//! Fetching pages and their resources over HTTP/1.1.
//!
//! The protocol is written here: URL resolution, requests, redirects, chunked
//! bodies, keep-alive and parallel downloads. TLS comes from rustls and gzip
//! from flate2.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Most redirects followed for one request.
const MAX_REDIRECTS: usize = 8;
/// Largest response body accepted.
const MAX_BODY: usize = 256 << 20;
/// Most connections downloading at once.
const MAX_PARALLEL: usize = 12;
/// Delay before trying the next address of a host while earlier attempts are still
/// connecting. Short enough that a dead address costs little, long enough that a
/// healthy first address usually wins alone.
const CONNECT_STAGGER: Duration = Duration::from_millis(250);
/// Most addresses of one host tried at once.
const MAX_ATTEMPTS: usize = 4;
/// Most hosts connected to ahead of their first request for one page.
const MAX_PRECONNECT: usize = 6;

/// A call that tells the owner of a page that something it waits for has arrived.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

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

/// Sees a response body while it downloads: called once with no bytes when the
/// headers have arrived, then with each piece of the body, already gunzipped.
type Watcher<'a> = dyn FnMut(&Url, &[u8]) + 'a;

/// Feeds a watcher, gunzipping on the way when the body is compressed.
struct Watch<'a, 'b> {
    see: &'a mut Watcher<'b>,
    gz: Option<flate2::write::MultiGzDecoder<Vec<u8>>>,
}

impl Watch<'_, '_> {
    fn feed(&mut self, url: &Url, raw: &[u8]) {
        match &mut self.gz {
            // A corrupt stream stops the watcher here; the full decode reports the error.
            Some(gz) => {
                if gz.write_all(raw).is_ok() {
                    let plain = std::mem::take(gz.get_mut());
                    (self.see)(url, &plain);
                }
            }
            None => (self.see)(url, raw),
        }
    }
}

/// Reads `n` more body bytes onto `body`, handing each piece to `feed` as it arrives.
fn read_into(conn: &mut Conn, body: &mut Vec<u8>, n: usize, feed: &mut dyn FnMut(&[u8])) -> io::Result<()> {
    let end = body.len() + n;
    while body.len() < end {
        let start = body.len();
        body.resize(start + (end - start).min(64 << 10), 0);
        let got = conn.read(&mut body[start..])?;
        if got == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        body.truncate(start + got);
        feed(&body[start..]);
    }
    Ok(())
}

/// The request for a GET of `url`. A page's resources name the page in
/// `Referer`, as browsers do; some servers refuse fonts and stylesheets without it.
fn request_head(url: &Url, referer: Option<&str>) -> String {
    let default_port = url.port == if url.https { 443 } else { 80 };
    let host = if default_port { url.host.clone() } else { format!("{}:{}", url.host, url.port) };
    let referer = referer.map(|r| format!("Referer: {r}\r\n")).unwrap_or_default();
    format!(
        "GET {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Mozilla/5.0 (Macintosh) render/0.1\r\nAccept: */*\r\nAccept-Encoding: gzip, identity\r\n{referer}Connection: keep-alive\r\n\r\n",
        url.path
    )
}

/// Sends one GET on an open connection and reads the whole response. The flag
/// tells whether the connection can carry another request. `watch` sees the body
/// of a final (not redirecting) response while it arrives.
fn exchange(conn: &mut Conn, url: &Url, referer: Option<&str>, watch: Option<&mut Watcher>) -> io::Result<(Response, bool)> {
    conn.get_mut().write_all(request_head(url, referer).as_bytes())?;
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
    let mut watch = watch.filter(|_| !(300..400).contains(&status)).map(|see| Watch { see, gz: gzip.then(|| flate2::write::MultiGzDecoder::new(Vec::new())) });
    let mut feed = |piece: &[u8]| {
        if let Some(w) = &mut watch {
            w.feed(url, piece);
        }
    };
    feed(&[]);
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
            read_into(conn, &mut body, size, &mut feed)?;
            read_line(conn)?;
        }
    } else if let Some(length) = length {
        if length > MAX_BODY {
            return Err(bad("response too large"));
        }
        read_into(conn, &mut body, length, &mut feed)?;
    } else {
        // No framing: the body runs until the peer closes. TLS peers often close
        // without a close_notify, which reads as an unexpected end.
        reusable = false;
        let mut piece = vec![0; 64 << 10];
        while body.len() < MAX_BODY {
            let room = piece.len().min(MAX_BODY - body.len());
            match conn.read(&mut piece[..room]) {
                Ok(0) => break,
                Ok(got) => {
                    body.extend_from_slice(&piece[..got]);
                    feed(&piece[..got]);
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
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

/// Connects to the first of `addresses` that answers. Attempts start one stagger
/// apart and overlap, so an address that never answers does not hold up the next.
fn connect_any(addresses: &[SocketAddr]) -> io::Result<TcpStream> {
    let timeout = Duration::from_secs(5);
    match addresses {
        [] => return Err(io::Error::new(io::ErrorKind::NotFound, "no address")),
        [only] => return TcpStream::connect_timeout(only, timeout),
        _ => {}
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let won = Arc::new(AtomicBool::new(false));
    for (i, &address) in addresses.iter().take(MAX_ATTEMPTS).enumerate() {
        let (tx, won) = (tx.clone(), won.clone());
        std::thread::spawn(move || {
            std::thread::sleep(CONNECT_STAGGER * i as u32);
            if !won.load(Ordering::Relaxed) {
                // The receiver is gone once another attempt has won; the stream then closes.
                let _ = tx.send(TcpStream::connect_timeout(&address, timeout));
            }
        });
    }
    drop(tx);
    let mut last = io::Error::new(io::ErrorKind::TimedOut, "no address answered");
    for attempt in rx {
        match attempt {
            Ok(tcp) => {
                won.store(true, Ordering::Relaxed);
                return Ok(tcp);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
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
    /// The page whose resources this client fetches.
    referer: Option<Arc<str>>,
}

impl Client {
    pub fn new(tls: Arc<rustls::ClientConfig>) -> Client {
        Client { tls, pool: HashMap::new(), referer: None }
    }

    /// A client fetching the resources of the page at `page`.
    fn referring(mut self, page: Arc<str>) -> Client {
        self.referer = Some(page);
        self
    }

    fn connect(&self, url: &Url) -> Result<Conn, String> {
        let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", url.host);
        let mut addresses: Vec<_> = (url.host.as_str(), url.port).to_socket_addrs().map_err(|e| fail(&e))?.collect();
        // IPv4 first: an IPv6 address without a working route then only costs a stagger.
        addresses.sort_by_key(SocketAddr::is_ipv6);
        let tcp = connect_any(&addresses).map_err(|e| fail(&e))?;
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

    fn send(&mut self, url: &Url, mut watch: Option<&mut Watcher>) -> Result<Response, String> {
        let key = (url.https, url.host.clone(), url.port);
        let kept = self.pool.remove(&key).or_else(|| {
            let mut idle = IDLE.lock().expect("idle connections");
            idle.iter().position(|c| c.0 == key).map(|i| idle.swap_remove(i).1)
        });
        if let Some(mut conn) = kept {
            // A kept connection may have been closed by the server meanwhile; then
            // the request is sent again on a new one.
            if let Ok((response, reusable)) = exchange(&mut conn, url, self.referer.as_deref(), watch.as_deref_mut()) {
                if reusable {
                    self.pool.insert(key, conn);
                }
                return Ok(response);
            }
        }
        let (mut conn, fresh) = self.open(url)?;
        let mut result = exchange(&mut conn, url, self.referer.as_deref(), watch.as_deref_mut());
        if result.is_err() && !fresh {
            // The idle connection had been closed by its server meanwhile.
            conn = self.connect(url)?;
            result = exchange(&mut conn, url, self.referer.as_deref(), watch);
        }
        let (response, reusable) = result.map_err(|e| format!("{url}: {e}"))?;
        if reusable {
            self.pool.insert(key, conn);
        }
        Ok(response)
    }

    /// A connection to `url`'s server: a new one, or an idle one parked while the new
    /// one is still connecting, whichever comes first; true for a new one. A
    /// stylesheet asked for while its page's HTML downloads then goes out on the
    /// HTML's connection as soon as that is free, which on a distant server is
    /// sooner than a new TCP and TLS handshake.
    fn open(&self, url: &Url) -> Result<(Conn, bool), String> {
        let key = (url.https, url.host.clone(), url.port);
        let attempt = Arc::new(Mutex::new(Attempt { result: None, abandoned: false }));
        let (tls, target, shared, own) = (self.tls.clone(), url.clone(), attempt.clone(), key.clone());
        std::thread::spawn(move || {
            let result = Client::new(tls).connect(&target);
            let mut a = shared.lock().expect("connection attempt");
            if a.abandoned {
                drop(a);
                if let Ok(conn) = result {
                    park(own, conn);
                }
                return;
            }
            a.result = Some(result);
            drop(a);
            // Signalled under the pool's lock, which the waiter holds while it looks.
            let _idle = IDLE.lock().expect("idle connections");
            IDLE_CHANGED.notify_all();
        });
        let mut idle = IDLE.lock().expect("idle connections");
        loop {
            let mut a = attempt.lock().expect("connection attempt");
            if let Some(result) = a.result.take() {
                return result.map(|conn| (conn, true));
            }
            if let Some(i) = idle.iter().position(|c| c.0 == key) {
                a.abandoned = true;
                return Ok((idle.swap_remove(i).1, false));
            }
            drop(a);
            idle = IDLE_CHANGED.wait(idle).expect("idle connections");
        }
    }

    /// Fetches `url`, following redirects. Returns the final URL with the response.
    pub fn get(&mut self, url: &Url) -> Result<(Url, Response), String> {
        self.get_watched(url, None)
    }

    /// Like `get`, letting `watch` see the final response's body while it arrives.
    fn get_watched(&mut self, url: &Url, mut watch: Option<&mut Watcher>) -> Result<(Url, Response), String> {
        let mut url = url.clone();
        for _ in 0..=MAX_REDIRECTS {
            let response = self.send(&url, watch.as_deref_mut())?;
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

/// Signalled when a connection is parked in `IDLE`, or a connection attempt ends.
static IDLE_CHANGED: Condvar = Condvar::new();

/// A new connection being made on its own thread for `Client::open`. When the
/// client took an idle connection instead, the new one is parked once ready.
struct Attempt {
    result: Option<Result<Conn, String>>,
    abandoned: bool,
}

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
        IDLE_CHANGED.notify_all();
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
    } else if body.get(4..8) == Some(b"ftyp") && matches!(body.get(8..12), Some(b"avif" | b"avis")) {
        "avif".into()
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

/// Puts a connection nobody is using into the idle pool, if there is room.
fn park(key: (bool, String, u16), conn: Conn) {
    let mut idle = IDLE.lock().expect("idle connections");
    if idle.len() < MAX_IDLE {
        idle.push((key, conn));
        IDLE_CHANGED.notify_all();
    }
}

/// Opens a connection to the server of `url`, TLS handshake included, on a
/// thread of its own and leaves it in the idle pool for the request that will need it.
fn preconnect(tls: Arc<rustls::ClientConfig>, url: Url) {
    std::thread::spawn(move || {
        let Ok(mut conn) = Client::new(tls).connect(&url) else { return };
        // Flushing a TLS stream completes its handshake.
        if conn.get_mut().flush().is_ok() {
            park((url.https, url.host.clone(), url.port), conn);
        }
    });
}

/// The `href` of a `<link rel=stylesheet>` tag, given as markup.
fn stylesheet_href(tag: &str) -> Option<String> {
    use crate::dom::{atom, parse_html, NodeId};
    let dom = parse_html(tag);
    let n = (0..dom.nodes.len() as NodeId).find(|&n| dom.nodes[n as usize].tag == atom::LINK)?;
    dom.attr(n, atom::REL).filter(|r| r.eq_ignore_ascii_case("stylesheet"))?;
    dom.attr(n, atom::HREF).map(str::to_string)
}

/// Finds `<link rel=stylesheet>` tags in a document while it downloads, so the
/// stylesheets can be requested before the rest of the HTML has arrived.
#[derive(Default)]
struct Preload {
    seen: Vec<u8>,
    /// Where scanning resumes: everything before it has been looked at.
    at: usize,
}

impl Preload {
    /// Takes the next piece of the document; returns the stylesheet hrefs it completed.
    fn feed(&mut self, piece: &[u8]) -> Vec<String> {
        self.seen.extend_from_slice(piece);
        let mut found = Vec::new();
        loop {
            let rest = &self.seen[self.at..];
            let Some(i) = memchr::memchr(b'<', rest) else {
                self.at = self.seen.len();
                break;
            };
            let tag = &rest[i..];
            if tag.len() < 5 {
                self.at += i;
                break;
            }
            if !tag[1..5].eq_ignore_ascii_case(b"link") {
                self.at += i + 1;
                continue;
            }
            let Some(end) = memchr::memchr(b'>', tag) else {
                self.at += i;
                break;
            };
            found.extend(stylesheet_href(&String::from_utf8_lossy(&tag[..=end])));
            self.at += i + end + 1;
        }
        found
    }
}

/// Where one resource stands: downloading, or done with its file (`None` when it failed).
enum Slot {
    Busy,
    Done(Option<PathBuf>),
}

/// The downloads of one page, shared by every thread fetching for it. Each resource
/// is downloaded once: a second request for it waits for the first.
struct Store {
    dir: PathBuf,
    tls: Arc<rustls::ClientConfig>,
    /// The page the resources belong to, sent as their `Referer`.
    page: Arc<str>,
    slots: Mutex<HashMap<Url, Slot>>,
    /// Signalled whenever a download finishes.
    done: Condvar,
    /// How many downloads have finished.
    finished: AtomicUsize,
    /// Called after each finished download.
    wake: Mutex<Option<Wake>>,
}

impl Store {
    fn new(tls: Arc<rustls::ClientConfig>, page: &Url) -> Result<Arc<Store>, String> {
        static OPENED: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("render-{}-{}", std::process::id(), OPENED.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let page = page.to_string().into();
        let store = Store { dir, tls, page, slots: Mutex::new(HashMap::new()), done: Condvar::new(), finished: AtomicUsize::new(0), wake: Mutex::new(None) };
        Ok(Arc::new(store))
    }

    /// A client for this page's resources.
    fn client(&self) -> Client {
        Client::new(self.tls.clone()).referring(self.page.clone())
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<Url, Slot>> {
        self.slots.lock().expect("downloads")
    }

    /// Marks the URLs nobody has started on as downloading and returns them.
    fn claim(&self, urls: impl IntoIterator<Item = Url>) -> Vec<Url> {
        let mut slots = self.slots();
        urls.into_iter()
            .filter(|url| {
                let new = !slots.contains_key(url);
                if new {
                    slots.insert(url.clone(), Slot::Busy);
                }
                new
            })
            .collect()
    }

    /// The file of a finished download; `None` while it runs or before it starts.
    fn ready(&self, url: &Url) -> Option<Option<PathBuf>> {
        match self.slots().get(url) {
            Some(Slot::Done(path)) => Some(path.clone()),
            _ => None,
        }
    }

    fn finish(&self, url: Url, path: Option<PathBuf>) {
        self.slots().insert(url, Slot::Done(path));
        self.finished.fetch_add(1, Ordering::Release);
        self.done.notify_all();
        let wake = self.wake.lock().expect("wake").clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Waits until none of `urls` is downloading any more, or until `deadline`.
    fn wait(&self, urls: &[Url], deadline: Option<Instant>) {
        let mut slots = self.slots();
        while urls.iter().any(|u| matches!(slots.get(u), Some(Slot::Busy))) {
            slots = match deadline {
                None => self.done.wait(slots).expect("downloads"),
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return;
                    }
                    self.done.wait_timeout(slots, left).expect("downloads").0
                }
            };
        }
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

    /// Downloads URLs this caller has claimed, over several connections at once.
    fn fetch(&self, urls: &[Url]) {
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..urls.len().min(MAX_PARALLEL) {
                scope.spawn(|| {
                    let mut client = self.client();
                    while let Some(url) = urls.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let path = self.download(&mut client, url);
                        self.finish(url.clone(), path);
                    }
                });
            }
        });
    }

    /// Starts downloading the URLs nobody has started on, without waiting.
    fn fetch_later(self: &Arc<Self>, urls: impl IntoIterator<Item = Url>) {
        let urls = self.claim(urls);
        if !urls.is_empty() {
            let store = self.clone();
            std::thread::spawn(move || store.fetch(&urls));
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // The downloads are only needed while the page is open.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A page opened from the web: its URL and the resources downloaded for it,
/// kept as files so the rest of the renderer reads them like local ones.
pub struct Site {
    pub url: Url,
    /// From the start of `open`: when the HTML's response headers arrived, and
    /// when its last byte did.
    pub first_byte: Duration,
    pub received: Duration,
    store: Arc<Store>,
}

impl Site {
    /// Downloads the page at `location`. Returns the site and the page's HTML.
    /// Stylesheets the HTML links to start downloading as soon as their tag arrives.
    pub fn open(location: &str) -> Result<(Arc<Site>, String), String> {
        let started = Instant::now();
        let url = Url::parse(location).ok_or_else(|| format!("{location}: not an http or https URL"))?;
        let tls = tls_config();
        let store = Store::new(tls.clone(), &url)?;
        // A second connection to the page's server, ready for the first stylesheet found.
        preconnect(tls.clone(), url.clone());
        let mut preload = Preload::default();
        let mut first_byte = None;
        let mut see = |at: &Url, piece: &[u8]| {
            first_byte.get_or_insert_with(|| started.elapsed());
            store.fetch_later(preload.feed(piece).iter().filter_map(|href| at.join(href)));
        };
        let (url, response) = Client::new(tls).get_watched(&url, Some(&mut see))?;
        let received = started.elapsed();
        if response.status >= 400 {
            return Err(format!("{url}: HTTP {}", response.status));
        }
        let html = String::from_utf8_lossy(&response.body).into_owned();
        let first_byte = first_byte.unwrap_or(received);
        Ok((Arc::new(Site { url, first_byte, received, store }), html))
    }

    /// The URLs `srcs` refer to, without repeats.
    fn urls(&self, srcs: &[String]) -> Vec<Url> {
        let mut seen = HashSet::new();
        srcs.iter().filter_map(|s| self.url.join(s)).filter(|u| seen.insert(u.clone())).collect()
    }

    /// The local file holding the resource `src` refers to, downloaded now unless
    /// it already was; one already downloading is waited for. `None` when it cannot be fetched.
    pub fn file(&self, src: &str) -> Option<PathBuf> {
        let url = self.url.join(src)?;
        if self.store.claim([url.clone()]).is_empty() {
            self.store.wait(std::slice::from_ref(&url), None);
            return self.store.ready(&url).flatten();
        }
        let path = self.store.download(&mut self.store.client(), &url);
        self.store.finish(url, path.clone());
        path
    }

    /// The local file for `src` once its download has finished, `Some(None)` when
    /// it failed or cannot be fetched. Before that, starts the download in the
    /// background if nothing has, and returns `None` without waiting.
    pub fn file_if_ready(&self, src: &str) -> Option<Option<PathBuf>> {
        let Some(url) = self.url.join(src) else { return Some(None) };
        let ready = self.store.ready(&url);
        if ready.is_none() {
            self.store.fetch_later([url]);
        }
        ready
    }

    /// True once the download of `src` has finished, or when it cannot be fetched.
    pub fn arrived(&self, src: &str) -> bool {
        self.url.join(src).is_none_or(|url| self.store.ready(&url).is_some())
    }

    /// Downloads the listed resources over several connections at once.
    pub fn prefetch(&self, srcs: &[String]) {
        let urls = self.urls(srcs);
        self.store.fetch(&self.store.claim(urls.iter().cloned()));
        // Some may have been downloading already, for an earlier request.
        self.store.wait(&urls, None);
    }

    /// Like `prefetch`, without waiting for the downloads.
    pub fn prefetch_later(&self, srcs: Vec<String>) {
        self.store.fetch_later(self.urls(&srcs));
    }

    /// Waits until the listed resources have downloaded or `deadline` has passed.
    /// Returns how many of them have arrived, and how many there are.
    pub fn wait_for(&self, srcs: &[String], deadline: Instant) -> (usize, usize) {
        let urls = self.urls(srcs);
        self.store.wait(&urls, Some(deadline));
        (urls.iter().filter(|u| self.store.ready(u).is_some()).count(), urls.len())
    }

    /// Opens connections to the servers of `srcs` other than the page's own, ahead
    /// of the requests, so those start without waiting for TCP and TLS handshakes.
    pub fn preconnect(&self, srcs: &[String]) {
        let own = (self.url.https, self.url.host.as_str(), self.url.port);
        let urls = self.urls(srcs);
        let mut hosts = HashSet::new();
        for url in &urls {
            let key = (url.https, url.host.as_str(), url.port);
            if key != own && hosts.len() < MAX_PRECONNECT && hosts.insert(key) {
                preconnect(self.store.tls.clone(), url.clone());
            }
        }
    }

    /// Calls `wake` from the downloading thread whenever a resource arrives.
    pub fn on_arrival(&self, wake: Wake) {
        *self.store.wake.lock().expect("wake") = Some(wake);
    }

    /// How many downloads have finished so far; it grows with every arrival.
    pub fn arrivals(&self) -> usize {
        self.store.finished.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::{request_head, Preload, Url};

    #[test]
    fn resources_name_their_page_in_referer() {
        let font = Url::parse("https://www.apple.com/wss/fonts?families=SF+Pro,v3|SF+Pro+Icons,v3").unwrap();
        let head = request_head(&font, Some("https://www.apple.com/"));
        assert!(head.starts_with("GET /wss/fonts?families=SF+Pro,v3|SF+Pro+Icons,v3 HTTP/1.1\r\nHost: www.apple.com\r\n"));
        assert_eq!(head.lines().filter(|l| *l == "Referer: https://www.apple.com/").count(), 1);
        assert!(head.ends_with("\r\n\r\n"));
        assert!(!request_head(&font, None).contains("Referer"));
    }

    #[test]
    fn preload_finds_stylesheets_split_across_pieces() {
        let html = b"<html><head><LINK rel=\"icon\" href=\"a.ico\"><link rel=stylesheet href='/s.css'><link\n rel=\"Stylesheet\" href=\"b.css\"></head>";
        let mut preload = Preload::default();
        let mut found = Vec::new();
        for piece in html.chunks(7) {
            found.extend(preload.feed(piece));
        }
        assert_eq!(found, ["/s.css", "b.css"]);
    }

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
