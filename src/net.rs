//! Fetching pages and their resources over HTTP/1.1.
//!
//! The protocol is written here: URL resolution, requests, redirects, chunked
//! bodies, keep-alive and parallel downloads. TLS comes from rustls and gzip
//! from flate2.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Most redirects followed for one request.
const MAX_REDIRECTS: usize = 8;
/// Largest response body accepted.
const MAX_BODY: usize = 256 << 20;
/// Most connections downloading at once.
const MAX_PARALLEL: usize = 12;

/// An absolute `http` or `https` URL. `path` starts with `/` and keeps the query.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

fn strip_scheme<'a>(s: &'a str, scheme: &str) -> Option<&'a str> {
    let head = s.get(..scheme.len())?;
    head.eq_ignore_ascii_case(scheme).then(|| &s[scheme.len()..])
}

impl Url {
    pub fn parse(s: &str) -> Option<Url> {
        let s = s.trim();
        let (https, rest) = match (strip_scheme(s, "https://"), strip_scheme(s, "http://")) {
            (Some(rest), _) => (true, rest),
            (_, Some(rest)) => (false, rest),
            _ => return None,
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        let authority = authority.rsplit('@').next()?;
        let default = if https { 443 } else { 80 };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, p.parse().ok()?),
            _ => (authority, default),
        };
        if host.is_empty() {
            return None;
        }
        let tail = tail.split('#').next().unwrap_or("");
        let path = match tail {
            "" => "/".to_string(),
            t if t.starts_with('?') => format!("/{t}"),
            t => t.to_string(),
        };
        Some(Url { https, host: host.to_ascii_lowercase(), port, path: path.replace(' ', "%20") })
    }

    /// Resolves a reference found in this document: absolute, `//host/..`, `/root`,
    /// `relative/..` or `?query`. Other schemes (`data:`, `mailto:`) give `None`.
    pub fn join(&self, reference: &str) -> Option<Url> {
        let r = reference.trim().split('#').next().unwrap_or("");
        if r.is_empty() {
            return Some(self.clone());
        }
        if let Some(url) = Url::parse(r) {
            return Some(url);
        }
        if let Some(rest) = r.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", if self.https { "https" } else { "http" }));
        }
        let scheme_end = r.find([':', '/', '?']).filter(|&i| i > 0 && r.as_bytes()[i] == b':');
        if scheme_end.is_some_and(|i| r[..i].bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))) {
            return None;
        }
        let own = self.path.split('?').next().unwrap_or("/");
        let joined = if r.starts_with('/') {
            r.to_string()
        } else if r.starts_with('?') {
            format!("{own}{r}")
        } else {
            format!("{}{r}", &own[..own.rfind('/').map_or(0, |i| i + 1)])
        };
        let (plain, query) = match joined.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (joined.as_str(), None),
        };
        let mut segments: Vec<&str> = Vec::new();
        for segment in plain.split('/').skip(1) {
            match segment {
                "." => {}
                ".." => drop(segments.pop()),
                s => segments.push(s),
            }
        }
        let mut path = format!("/{}", segments.join("/"));
        if (plain.ends_with("/.") || plain.ends_with("/..")) && !path.ends_with('/') {
            path.push('/');
        }
        if let Some(q) = query {
            path.push('?');
            path.push_str(q);
        }
        Some(Url { path: path.replace(' ', "%20"), ..self.clone() })
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scheme = if self.https { "https" } else { "http" };
        if self.port == if self.https { 443 } else { 80 } {
            write!(f, "{scheme}://{}{}", self.host, self.path)
        } else {
            write!(f, "{scheme}://{}:{}{}", self.host, self.port, self.path)
        }
    }
}

/// True for a location that names a web page, not a local file.
pub fn is_url(location: &str) -> bool {
    strip_scheme(location.trim_start(), "http://").is_some() || strip_scheme(location.trim_start(), "https://").is_some()
}

pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// A header value by lower-case name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|h| h.0 == name).map(|h| h.1.as_str())
    }
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

type Conn = BufReader<Box<dyn Stream>>;

fn bad(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// One line without its line ending. An empty read is an error: the peer closed.
fn read_line(conn: &mut Conn) -> io::Result<String> {
    let mut line = Vec::new();
    if conn.take(64 << 10).read_until(b'\n', &mut line)? == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    while line.last().is_some_and(|b| b"\r\n".contains(b)) {
        line.pop();
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Sends one GET on an open connection and reads the whole response. The flag
/// tells whether the connection can carry another request.
fn exchange(conn: &mut Conn, url: &Url) -> io::Result<(Response, bool)> {
    let default_port = url.port == if url.https { 443 } else { 80 };
    let host = if default_port { url.host.clone() } else { format!("{}:{}", url.host, url.port) };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Mozilla/5.0 (Macintosh) render/0.1\r\nAccept: */*\r\nAccept-Encoding: gzip, identity\r\nConnection: keep-alive\r\n\r\n",
        url.path
    );
    conn.get_mut().write_all(request.as_bytes())?;
    conn.get_mut().flush()?;
    let (status, mut reusable, headers) = loop {
        let status_line = read_line(conn)?;
        let mut parts = status_line.split_whitespace();
        let version = parts.next().filter(|v| v.starts_with("HTTP/1.")).ok_or_else(|| bad("not an HTTP response"))?;
        let status: u16 = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| bad("bad status line"))?;
        let mut headers = Vec::new();
        loop {
            let line = read_line(conn)?;
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }
        // Interim responses (100 Continue, 103 Early Hints) precede the real one.
        if !(100..200).contains(&status) {
            break (status, version == "HTTP/1.1", headers);
        }
    };
    let mut response = Response { status, headers, body: Vec::new() };
    let has = |name: &str, token: &str| response.header(name).is_some_and(|v| v.to_ascii_lowercase().contains(token));
    reusable &= !has("connection", "close");
    let (chunked, gzip) = (has("transfer-encoding", "chunked"), has("content-encoding", "gzip"));
    let length = response.header("content-length").and_then(|v| v.parse::<usize>().ok());
    let mut body = Vec::new();
    if status == 204 || status == 304 {
    } else if chunked {
        loop {
            let line = read_line(conn)?;
            let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| bad("bad chunk size"))?;
            if size == 0 {
                // Trailer fields end with an empty line.
                while !read_line(conn)?.is_empty() {}
                break;
            }
            if body.len() + size > MAX_BODY {
                return Err(bad("response too large"));
            }
            let start = body.len();
            body.resize(start + size, 0);
            conn.read_exact(&mut body[start..])?;
            read_line(conn)?;
        }
    } else if let Some(length) = length {
        if length > MAX_BODY {
            return Err(bad("response too large"));
        }
        body.resize(length, 0);
        conn.read_exact(&mut body)?;
    } else {
        // No framing: the body runs until the peer closes. TLS peers often close
        // without a close_notify, which reads as an unexpected end.
        reusable = false;
        match conn.take(MAX_BODY as u64).read_to_end(&mut body) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {}
            Err(e) => return Err(e),
        }
    }
    if gzip {
        let mut plain = Vec::new();
        flate2::read::MultiGzDecoder::new(body.as_slice()).take(MAX_BODY as u64).read_to_end(&mut plain)?;
        body = plain;
    }
    response.body = body;
    Ok((response, reusable))
}

/// Root certificates and settings shared by every connection.
pub fn tls_config() -> Arc<rustls::ClientConfig> {
    // One configuration for the whole process: its session cache lets a later
    // connection to a known server resume instead of doing a full handshake.
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CONFIG.get_or_init(new_tls_config).clone()
}

fn new_tls_config() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

/// Keeps one open connection per origin and reuses it for later requests.
pub struct Client {
    tls: Arc<rustls::ClientConfig>,
    pool: HashMap<(bool, String, u16), Conn>,
}

impl Client {
    pub fn new(tls: Arc<rustls::ClientConfig>) -> Client {
        Client { tls, pool: HashMap::new() }
    }

    fn connect(&self, url: &Url) -> Result<Conn, String> {
        let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", url.host);
        let mut addresses: Vec<_> = (url.host.as_str(), url.port).to_socket_addrs().map_err(|e| fail(&e))?.collect();
        // IPv4 first: an IPv6 address without a working route costs a full timeout.
        addresses.sort_by_key(std::net::SocketAddr::is_ipv6);
        let mut tcp = Err(fail(&"no address"));
        for address in &addresses {
            tcp = TcpStream::connect_timeout(address, Duration::from_secs(5)).map_err(|e| fail(&e));
            if tcp.is_ok() {
                break;
            }
        }
        let tcp = tcp?;
        let timeout = Some(Duration::from_secs(20));
        tcp.set_read_timeout(timeout).and_then(|()| tcp.set_write_timeout(timeout)).and_then(|()| tcp.set_nodelay(true)).map_err(|e| fail(&e))?;
        let stream: Box<dyn Stream> = if url.https {
            let name = rustls::pki_types::ServerName::try_from(url.host.clone()).map_err(|e| fail(&e))?;
            let session = rustls::ClientConnection::new(self.tls.clone(), name).map_err(|e| fail(&e))?;
            Box::new(rustls::StreamOwned::new(session, tcp))
        } else {
            Box::new(tcp)
        };
        Ok(BufReader::with_capacity(64 << 10, stream))
    }

    fn send(&mut self, url: &Url) -> Result<Response, String> {
        let key = (url.https, url.host.clone(), url.port);
        let kept = self.pool.remove(&key).or_else(|| {
            let mut idle = IDLE.lock().expect("idle connections");
            idle.iter().position(|c| c.0 == key).map(|i| idle.swap_remove(i).1)
        });
        if let Some(mut conn) = kept {
            // A kept connection may have been closed by the server meanwhile; then
            // the request is sent again on a new one.
            if let Ok((response, reusable)) = exchange(&mut conn, url) {
                if reusable {
                    self.pool.insert(key, conn);
                }
                return Ok(response);
            }
        }
        let mut conn = self.connect(url)?;
        let (response, reusable) = exchange(&mut conn, url).map_err(|e| format!("{url}: {e}"))?;
        if reusable {
            self.pool.insert(key, conn);
        }
        Ok(response)
    }

    /// Fetches `url`, following redirects. Returns the final URL with the response.
    pub fn get(&mut self, url: &Url) -> Result<(Url, Response), String> {
        let mut url = url.clone();
        for _ in 0..=MAX_REDIRECTS {
            let response = self.send(&url)?;
            let target = response.header("location").filter(|_| matches!(response.status, 301 | 302 | 303 | 307 | 308));
            match target {
                Some(location) => url = url.join(location).ok_or_else(|| format!("{url}: bad redirect to {location}"))?,
                None => return Ok((url, response)),
            }
        }
        Err(format!("{url}: too many redirects"))
    }
}

/// Open connections no client is using. A client leaves its connections here when
/// it is done, so the next page from the same servers skips connecting again.
static IDLE: Mutex<Vec<((bool, String, u16), Conn)>> = Mutex::new(Vec::new());

/// Most idle connections kept.
const MAX_IDLE: usize = 32;

impl Drop for Client {
    fn drop(&mut self) {
        let mut idle = IDLE.lock().expect("idle connections");
        for kept in self.pool.drain() {
            if idle.len() < MAX_IDLE {
                idle.push(kept);
            }
        }
    }
}

/// File extension for a downloaded resource: image formats are told from their
/// first bytes, since many URLs carry no usable extension.
fn extension(url: &Url, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(&body[..body.len().min(1024)]);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if body.starts_with(b"\x89PNG") {
        "png".into()
    } else if body.starts_with(&[0xFF, 0xD8]) {
        "jpg".into()
    } else if body.starts_with(b"GIF8") {
        "gif".into()
    } else if body.starts_with(b"RIFF") && body.get(8..12) == Some(b"WEBP") {
        "webp".into()
    } else if text.starts_with("<svg") || (text.starts_with("<?xml") && text.contains("<svg")) {
        "svg".into()
    } else {
        let name = url.path.split('?').next().unwrap_or("").rsplit('/').next().unwrap_or("");
        match name.rsplit_once('.') {
            Some((_, ext)) if (1..=5).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphanumeric()) => ext.to_ascii_lowercase(),
            _ => "bin".into(),
        }
    }
}

/// A page opened from the web: its URL and the resources downloaded for it,
/// kept as files so the rest of the renderer reads them like local ones.
pub struct Site {
    pub url: Url,
    dir: PathBuf,
    tls: Arc<rustls::ClientConfig>,
    files: Mutex<HashMap<Url, Option<PathBuf>>>,
}

impl Site {
    /// Downloads the page at `location`. Returns the site and the page's HTML.
    pub fn open(location: &str) -> Result<(Arc<Site>, String), String> {
        let url = Url::parse(location).ok_or_else(|| format!("{location}: not an http or https URL"))?;
        let tls = tls_config();
        let (url, response) = Client::new(tls.clone()).get(&url)?;
        if response.status >= 400 {
            return Err(format!("{url}: HTTP {}", response.status));
        }
        static OPENED: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("render-{}-{}", std::process::id(), OPENED.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let html = String::from_utf8_lossy(&response.body).into_owned();
        Ok((Arc::new(Site { url, dir, tls, files: Mutex::new(HashMap::new()) }), html))
    }

    fn download(&self, client: &mut Client, url: &Url) -> Option<PathBuf> {
        let response = match client.get(url) {
            Ok((_, response)) if response.status < 400 => response,
            Ok((_, response)) => {
                eprintln!("[net] {url}: HTTP {}", response.status);
                return None;
            }
            Err(e) => {
                eprintln!("[net] {e}");
                return None;
            }
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(url, &mut hasher);
        let name = format!("{:016x}.{}", std::hash::Hasher::finish(&hasher), extension(url, &response.body));
        let path = self.dir.join(name);
        std::fs::write(&path, &response.body).ok()?;
        Some(path)
    }

    fn known(&self, url: &Url) -> Option<Option<PathBuf>> {
        self.files.lock().expect("downloaded files").get(url).cloned()
    }

    /// The local file holding the resource `src` refers to, downloaded now unless
    /// it already was. `None` when it cannot be fetched.
    pub fn file(&self, src: &str) -> Option<PathBuf> {
        let url = self.url.join(src)?;
        if let Some(path) = self.known(&url) {
            return path;
        }
        let path = self.download(&mut Client::new(self.tls.clone()), &url);
        self.files.lock().expect("downloaded files").insert(url, path.clone());
        path
    }

    /// Downloads the listed resources over several connections at once.
    pub fn prefetch(&self, srcs: &[String]) {
        let mut seen = HashSet::new();
        let urls: Vec<Url> = srcs.iter().filter_map(|s| self.url.join(s)).filter(|u| self.known(u).is_none() && seen.insert(u.clone())).collect();
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..urls.len().min(MAX_PARALLEL) {
                scope.spawn(|| {
                    let mut client = Client::new(self.tls.clone());
                    while let Some(url) = urls.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let path = self.download(&mut client, url);
                        self.files.lock().expect("downloaded files").insert(url.clone(), path);
                    }
                });
            }
        });
    }

    /// Like `prefetch`, without waiting for the downloads.
    pub fn prefetch_later(self: &Arc<Self>, srcs: Vec<String>) {
        let site = self.clone();
        std::thread::spawn(move || site.prefetch(&srcs));
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        // The downloads are only needed while the page is open.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::Url;

    #[test]
    fn urls_resolve_like_a_browser() {
        let base = Url::parse("https://Example.com/a/b/page.html?x=1#top").unwrap();
        assert_eq!(base.to_string(), "https://example.com/a/b/page.html?x=1");
        let cases = [
            ("style.css", "https://example.com/a/b/style.css"),
            ("../img/a b.png", "https://example.com/a/img/a%20b.png"),
            ("/root.js?v=2", "https://example.com/root.js?v=2"),
            ("//cdn.example.org/lib.js", "https://cdn.example.org/lib.js"),
            ("http://other.test:8080/x", "http://other.test:8080/x"),
            ("?y=2", "https://example.com/a/b/page.html?y=2"),
            ("./", "https://example.com/a/b/"),
            ("../../../up", "https://example.com/up"),
            ("#frag", "https://example.com/a/b/page.html?x=1"),
        ];
        for (reference, expected) in cases {
            assert_eq!(base.join(reference).unwrap().to_string(), expected, "{reference}");
        }
        assert!(base.join("data:image/png;base64,AAAA").is_none());
        assert!(base.join("mailto:someone@example.com").is_none());
        assert!(Url::parse("ftp://example.com/").is_none());
    }
}
