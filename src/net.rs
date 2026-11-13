//! Fetching pages and their resources over HTTP/1.1.
//!
//! The protocol is written here: URL resolution, requests, redirects, chunked
//! bodies, keep-alive and parallel downloads. TLS comes from rustls, gzip from
//! flate2 and brotli from brotli-decompressor.

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
/// Most requests a page has in flight to one server, as browsers allow.
const PER_HOST: usize = 6;
/// Delay before trying the next address of a host while earlier attempts are still
/// connecting. Short enough that a dead address costs little, long enough that a
/// healthy first address usually wins alone.
const CONNECT_STAGGER: Duration = Duration::from_millis(250);
/// Most addresses of one host tried at once.
const MAX_ATTEMPTS: usize = 4;
/// Most hosts connected to ahead of their first request for one page.
const MAX_PRECONNECT: usize = 6;

/// How long images wait for the stylesheets found before them, at most.
const STYLESHEETS_FIRST: Duration = Duration::from_millis(1000);

/// The browser this renderer claims to be, in every request's `User-Agent`.
/// Sites pick their builds by it: as `render/0.1` YouTube sent its 10.8 MB ES5
/// bundle, as Safari a module build of about 2.1 MB. Scripts must see the same
/// string in `navigator.userAgent` (src/prelude.js keeps a copy until it reads this).
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15 render/0.1";

/// The languages every request asks for in `Accept-Language`, as Safari set to
/// US English sends them: the language `navigator.languages` reports. Without it
/// sites pick a language by the visitor's address, and scripts that assume
/// `navigator.language` get a page in another one.
pub const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";

/// A call that tells the owner of a page that something it waits for has arrived.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// With `RENDER_NET_TIMELINE=1` every connection, request and `mark` is reported on
/// stderr as a `[net]` line, its times in milliseconds since the first report.
fn timeline() -> Option<Instant> {
    static START: std::sync::OnceLock<Option<Instant>> = std::sync::OnceLock::new();
    *START.get_or_init(|| (std::env::var_os("RENDER_NET_TIMELINE").is_some_and(|v| v == "1")).then(Instant::now))
}

fn since(start: Instant, at: Instant) -> f64 {
    at.saturating_duration_since(start).as_secs_f64() * 1e3
}

/// Puts a point of the page's loading on the network timeline, when it is on.
pub fn mark(what: &str) {
    if let Some(start) = timeline() {
        eprintln!("[net] {:8.1} mark {what}", since(start, Instant::now()));
    }
}

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

/// A request method a script can send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
    Patch,
    Options,
}

impl Method {
    /// The method called `name` in any case, as `fetch` and `XMLHttpRequest.open` take it.
    pub fn parse(name: &str) -> Option<Method> {
        use Method::*;
        [Get, Head, Post, Put, Delete, Patch, Options].into_iter().find(|m| m.as_str().eq_ignore_ascii_case(name.trim()))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Patch => "PATCH",
            Method::Options => "OPTIONS",
        }
    }
}

/// Why a request failed. Each carries a message naming the URL or host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The host name did not resolve.
    Dns(String),
    Connect(String),
    Tls(String),
    /// The server stopped answering for longer than the read timeout.
    Timeout(String),
    /// The answer was not valid HTTP, or the connection broke in the middle of it.
    Protocol(String),
    TooManyRedirects(String),
    UnsupportedScheme(String),
    /// A `file:` URL that could not be read.
    File(String),
    /// The request's abort flag was set.
    Aborted,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Dns(m)
            | FetchError::Connect(m)
            | FetchError::Tls(m)
            | FetchError::Timeout(m)
            | FetchError::Protocol(m)
            | FetchError::TooManyRedirects(m)
            | FetchError::File(m) => f.write_str(m),
            FetchError::UnsupportedScheme(url) => write!(f, "{url}: unsupported scheme"),
            FetchError::Aborted => f.write_str("aborted"),
        }
    }
}

impl std::error::Error for FetchError {}

/// What failed in an exchange with the server of `url`.
fn failure(url: &Url, e: &io::Error) -> FetchError {
    let message = format!("{url}: {e}");
    if e.get_ref().is_some_and(|inner| inner.is::<rustls::Error>()) {
        FetchError::Tls(message)
    } else if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) {
        FetchError::Timeout(message)
    } else {
        FetchError::Protocol(message)
    }
}

/// A request for `fetch`.
pub struct Request {
    pub method: Method,
    /// An absolute `http:`, `https:` or `file:` URL.
    pub url: String,
    /// Sent as given, except those a page may not set (`Host`, `Cookie`,
    /// `Content-Length` and the other forbidden names of the Fetch standard).
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// The page making the request, sent as `Referer`.
    pub referrer: Option<String>,
    /// Set from any thread to stop the request; it is looked at between reads.
    pub abort: Option<Arc<AtomicBool>>,
}

impl Request {
    /// A plain GET of `url`.
    pub fn get(url: &str) -> Request {
        Request { method: Method::Get, url: url.to_string(), headers: Vec::new(), body: None, referrer: None, abort: None }
    }
}

/// The final response to a `fetch`, after redirects.
pub struct Fetched {
    /// The URL that answered, after redirects.
    pub url: String,
    pub status: u16,
    pub status_text: String,
    /// In the order received, names in lower case, repeated names kept apart.
    pub headers: Vec<(String, String)>,
    /// Already decoded from its `Content-Encoding`.
    pub body: Vec<u8>,
}

/// Request headers a page may not set (the Fetch standard's forbidden names),
/// and values that would break the request apart.
fn allowed_header(name: &str, value: &str) -> bool {
    const FORBIDDEN: [&str; 21] = [
        "accept-charset", "accept-encoding", "access-control-request-headers", "access-control-request-method", "connection",
        "content-length", "cookie", "cookie2", "date", "dnt", "expect", "host", "keep-alive", "origin", "referer", "te",
        "trailer", "transfer-encoding", "upgrade", "user-agent", "via",
    ];
    let name = name.trim().to_ascii_lowercase();
    let token = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    token && !value.contains(['\r', '\n', '\0']) && !FORBIDDEN.contains(&name.as_str()) && !name.starts_with("proxy-") && !name.starts_with("sec-")
}

/// Sends `request` and waits for the whole response, following redirects as a
/// browser does: 303 turns any method but HEAD into GET, 301 and 302 turn POST
/// into GET, 307 and 308 repeat the method and body. Blocks the calling thread;
/// meant for a worker thread behind a script's `fetch` or `XMLHttpRequest`.
/// Connections come from, and go back to, the pool page downloads use.
///
/// Cookies come from and go to the jar in `cookie`, on every redirect hop.
///
/// ```no_run
/// use render::net::{fetch, Method, Request};
/// use std::sync::{atomic::AtomicBool, Arc};
///
/// let abort = Arc::new(AtomicBool::new(false));
/// let request = Request {
///     method: Method::Post,
///     headers: vec![("Content-Type".into(), "application/json".into())],
///     body: Some(br#"{"q":1}"#.to_vec()),
///     referrer: Some("https://example.com/".into()),
///     abort: Some(abort.clone()),
///     ..Request::get("https://example.com/api")
/// };
/// match fetch(request) {
///     Ok(r) => println!("{} {} {}: {} bytes", r.url, r.status, r.status_text, r.body.len()),
///     Err(e) => eprintln!("{e}"),
/// }
/// ```
pub fn fetch(request: Request) -> Result<Fetched, FetchError> {
    let location = request.url.trim();
    if let Some(rest) = strip_scheme(location, "file://") {
        return fetch_file(location, rest, request.method);
    }
    let url = match Url::parse(location) {
        Some(url) => url,
        None if is_url(location) => return Err(FetchError::Protocol(format!("{location}: bad URL"))),
        None => return Err(FetchError::UnsupportedScheme(location.to_string())),
    };
    let headers = request.headers.into_iter().filter(|(n, v)| allowed_header(n, v)).map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()));
    let ask = Ask { method: request.method, url, headers: headers.collect(), body: request.body, abort: request.abort };
    let _slot = ServerSlot::take(&ask)?;
    let mut client = Client::new(tls_config());
    if let Some(page) = request.referrer {
        client = client.referring(page.into());
    }
    let (url, response) = client.follow(ask, None)?;
    Ok(Fetched { url: url.to_string(), status: response.status, status_text: response.reason, headers: response.headers, body: response.body })
}

/// Script requests in flight to each server. A script that asks for a hundred icons at
/// once would otherwise open a hundred connections to one server within milliseconds,
/// which fonts.gstatic.com answers by not answering this machine for a while. Like a
/// browser, which keeps six HTTP/1.1 connections per server, at most `PER_HOST` go
/// out at once and the rest wait for one of them to end; they then reuse its connection.
static IN_FLIGHT: Mutex<Vec<((bool, String, u16), usize)>> = Mutex::new(Vec::new());

/// Signalled when a script request ends.
static SLOT_FREED: Condvar = Condvar::new();

/// One of the `PER_HOST` places a script request to a server holds while in flight.
struct ServerSlot((bool, String, u16));

impl ServerSlot {
    /// Waits for a place for `ask` on its server; `Aborted` if the request is aborted meanwhile.
    fn take(ask: &Ask) -> Result<ServerSlot, FetchError> {
        let key = (ask.url.https, ask.url.host.clone(), ask.url.port);
        let mut busy = IN_FLIGHT.lock().expect("script requests");
        loop {
            if ask.aborted() {
                return Err(FetchError::Aborted);
            }
            match busy.iter_mut().find(|b| b.0 == key) {
                Some(b) if b.1 >= PER_HOST => {}
                Some(b) => {
                    b.1 += 1;
                    return Ok(ServerSlot(key));
                }
                None => {
                    busy.push((key.clone(), 1));
                    return Ok(ServerSlot(key));
                }
            }
            // Woken when a slot frees; an abort is noticed within the timeout.
            busy = SLOT_FREED.wait_timeout(busy, Duration::from_millis(50)).expect("script requests").0;
        }
    }
}

impl Drop for ServerSlot {
    fn drop(&mut self) {
        let mut busy = IN_FLIGHT.lock().expect("script requests");
        if let Some(i) = busy.iter().position(|b| b.0 == self.0) {
            busy[i].1 -= 1;
            if busy[i].1 == 0 {
                busy.swap_remove(i);
            }
        }
        SLOT_FREED.notify_all();
    }
}

/// A local file read for `fetch`. `rest` follows `file://`: an optional host, then
/// a percent-encoded absolute path.
fn fetch_file(location: &str, rest: &str, method: Method) -> Result<Fetched, FetchError> {
    if !matches!(method, Method::Get | Method::Head) {
        return Err(FetchError::File(format!("{location}: {} is not allowed", method.as_str())));
    }
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let encoded = rest.as_bytes();
    let mut path = Vec::with_capacity(encoded.len());
    let mut i = rest.find('/').unwrap_or(rest.len());
    while i < encoded.len() {
        let hex = rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (encoded[i], hex) {
            (b'%', Some(byte)) => {
                path.push(byte);
                i += 3;
            }
            (byte, _) => {
                path.push(byte);
                i += 1;
            }
        }
    }
    let path = String::from_utf8_lossy(&path).into_owned();
    let body = std::fs::read(&path).map_err(|e| FetchError::File(format!("{location}: {e}")))?;
    let body = if method == Method::Head { Vec::new() } else { body };
    Ok(Fetched { url: location.to_string(), status: 200, status_text: "OK".into(), headers: Vec::new(), body })
}

pub struct Response {
    /// When the status line of the final response arrived.
    head_at: Instant,
    pub status: u16,
    /// The reason phrase of the status line, like `OK`.
    pub reason: String,
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

/// What a watcher is told about a response while it downloads.
enum Arrival<'p> {
    /// The headers have arrived. `early_links` holds the `Link` header values of
    /// the 103 Early Hints responses that came before them.
    Head { early_links: &'p [String] },
    /// The next piece of the body, already decoded.
    Body(&'p [u8]),
}

/// Sees a response while it downloads: once its head, then each piece of its body.
type Watcher<'a> = dyn FnMut(&Url, Arrival<'_>) + 'a;

/// The content codings asked for. Each must be decoded both while a body streams
/// (`Watch`) and once it is whole (`exchange`).
const ACCEPT_ENCODING: &str = "gzip, br";

/// A decoder for a body in transit.
enum Decoder {
    Gzip(flate2::write::MultiGzDecoder<Vec<u8>>),
    Brotli(Box<brotli_decompressor::DecompressorWriter<Vec<u8>>>),
}

/// Feeds a watcher, decoding on the way when the body is compressed.
struct Watch<'a, 'b> {
    see: &'a mut Watcher<'b>,
    decoder: Option<Decoder>,
}

impl Watch<'_, '_> {
    fn feed(&mut self, url: &Url, raw: &[u8]) {
        // A corrupt stream stops the watcher here; the full decode reports the error.
        let plain = match &mut self.decoder {
            Some(Decoder::Gzip(d)) => d.write_all(raw).map(|()| std::mem::take(d.get_mut())),
            Some(Decoder::Brotli(d)) => d.write_all(raw).map(|()| std::mem::take(d.get_mut())),
            None => return (self.see)(url, Arrival::Body(raw)),
        };
        if let Ok(plain) = plain {
            (self.see)(url, Arrival::Body(&plain));
        }
    }
}

/// Reads `n` more body bytes onto `body`, handing each piece to `feed` as it
/// arrives; an error from `feed` stops the reading.
fn read_into(conn: &mut Conn, body: &mut Vec<u8>, n: usize, feed: &mut dyn FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
    let end = body.len() + n;
    while body.len() < end {
        let start = body.len();
        body.resize(start + (end - start).min(64 << 10), 0);
        let got = conn.read(&mut body[start..])?;
        if got == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        body.truncate(start + got);
        feed(&body[start..])?;
    }
    Ok(())
}

/// One request as it goes out: a page's GET, or a script's request.
struct Ask {
    method: Method,
    url: Url,
    /// Extra headers, names in lower case, already checked by `allowed_header`.
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    abort: Option<Arc<AtomicBool>>,
}

impl Ask {
    fn get(url: Url) -> Ask {
        Ask { method: Method::Get, url, headers: Vec::new(), body: None, abort: None }
    }

    fn aborted(&self) -> bool {
        self.abort.as_ref().is_some_and(|a| a.load(Ordering::Relaxed))
    }

    /// Whether the request may go out again after failing with `e` on a kept
    /// connection. A POST or PATCH is repeated only when the server closed the
    /// connection, never after a timeout: it may already have acted on the first.
    fn may_resend(&self, e: &io::Error) -> bool {
        let closed = matches!(
            e.kind(),
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted | io::ErrorKind::BrokenPipe
        );
        closed || !matches!(self.method, Method::Post | Method::Patch)
    }
}

/// The request line and headers of `ask`. A page's resources name the page in
/// `Referer`, as browsers do; some servers refuse fonts and stylesheets without it.
fn request_head(ask: &Ask, referer: Option<&str>) -> String {
    let url = &ask.url;
    let default_port = url.port == if url.https { 443 } else { 80 };
    let host = if default_port { url.host.clone() } else { format!("{}:{}", url.host, url.port) };
    let mut head = format!("{} {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {USER_AGENT}\r\n", ask.method.as_str(), url.path);
    if !ask.headers.iter().any(|h| h.0 == "accept") {
        head.push_str("Accept: */*\r\n");
    }
    if !ask.headers.iter().any(|h| h.0 == "accept-language") {
        head.push_str(&format!("Accept-Language: {ACCEPT_LANGUAGE}\r\n"));
    }
    head.push_str(&format!("Accept-Encoding: {ACCEPT_ENCODING}\r\n"));
    if let Some(r) = referer {
        head.push_str(&format!("Referer: {r}\r\n"));
    }
    for (name, value) in &ask.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    match &ask.body {
        Some(body) => head.push_str(&format!("Content-Length: {}\r\n", body.len())),
        // Servers may refuse a body-carrying method without a length.
        None if matches!(ask.method, Method::Post | Method::Put | Method::Patch) => head.push_str("Content-Length: 0\r\n"),
        None => {}
    }
    head.push_str("Connection: keep-alive\r\n\r\n");
    head
}

/// Sends one request on an open connection and reads the whole response. The
/// flag tells whether the connection can carry another request. `watch` sees the
/// body of a final (not redirecting) response while it arrives.
fn exchange(conn: &mut Conn, ask: &Ask, referer: Option<&str>, watch: Option<&mut Watcher>) -> io::Result<(Response, bool)> {
    let url = &ask.url;
    conn.get_mut().write_all(request_head(ask, referer).as_bytes())?;
    if let Some(body) = &ask.body {
        conn.get_mut().write_all(body)?;
    }
    conn.get_mut().flush()?;
    let mut early_links = Vec::new();
    let mut head_at;
    let (status, reason, mut reusable, headers) = loop {
        let status_line = read_line(conn)?;
        head_at = Instant::now();
        let (version, rest) = status_line.split_once(' ').ok_or_else(|| bad("not an HTTP response"))?;
        if !version.starts_with("HTTP/1.") {
            return Err(bad("not an HTTP response"));
        }
        let (code, reason) = rest.trim_start().split_once(' ').unwrap_or((rest.trim(), ""));
        let status: u16 = code.parse().map_err(|_| bad("bad status line"))?;
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
        if status == 103 {
            early_links.extend(headers.into_iter().filter(|h| h.0 == "link").map(|h| h.1));
        } else if !(100..200).contains(&status) {
            break (status, reason.trim().to_string(), version == "HTTP/1.1", headers);
        }
    };
    let mut response = Response { head_at, status, reason, headers, body: Vec::new() };
    let has = |name: &str, token: &str| response.header(name).is_some_and(|v| v.to_ascii_lowercase().contains(token));
    reusable &= !has("connection", "close");
    let chunked = has("transfer-encoding", "chunked");
    let gzip = has("content-encoding", "gzip");
    let brotli = !gzip && response.header("content-encoding").is_some_and(|v| v.trim().eq_ignore_ascii_case("br"));
    let length = response.header("content-length").and_then(|v| v.parse::<usize>().ok());
    let decoder = || match (gzip, brotli) {
        (true, _) => Some(Decoder::Gzip(flate2::write::MultiGzDecoder::new(Vec::new()))),
        (_, true) => Some(Decoder::Brotli(Box::new(brotli_decompressor::DecompressorWriter::new(Vec::new(), 64 << 10)))),
        _ => None,
    };
    let mut watch = watch.filter(|_| !(300..400).contains(&status)).map(|see| Watch { see, decoder: decoder() });
    if ask.aborted() {
        return Err(io::Error::other("aborted"));
    }
    if let Some(w) = &mut watch {
        (w.see)(url, Arrival::Head { early_links: &early_links });
    }
    let mut feed = |piece: &[u8]| {
        if ask.aborted() {
            return Err(io::Error::other("aborted"));
        }
        if let Some(w) = &mut watch {
            w.feed(url, piece);
        }
        Ok(())
    };
    let mut body = Vec::new();
    if status == 204 || status == 304 || ask.method == Method::Head {
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
                    feed(&piece[..got])?;
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
    } else if brotli && !body.is_empty() {
        let mut plain = Vec::new();
        brotli_decompressor::Decompressor::new(body.as_slice(), 64 << 10).take(MAX_BODY as u64).read_to_end(&mut plain)?;
        body = plain;
    }
    response.body = body;
    Ok((response, reusable))
}

fn loopback_only() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RENDER_LOOPBACK_ONLY").is_some_and(|v| v == "1"))
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
    /// Where plain GET answers are kept and looked up.
    cache: Option<Arc<crate::cache::Cache>>,
}

impl Client {
    pub fn new(tls: Arc<rustls::ClientConfig>) -> Client {
        Client { tls, pool: HashMap::new(), referer: None, cache: crate::cache::shared() }
    }

    #[cfg(test)]
    fn caching(mut self, cache: Option<Arc<crate::cache::Cache>>) -> Client {
        self.cache = cache;
        self
    }

    /// A client fetching the resources of the page at `page`.
    fn referring(mut self, page: Arc<str>) -> Client {
        self.referer = Some(page);
        self
    }

    fn connect(&self, url: &Url) -> Result<Conn, FetchError> {
        let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", url.host);
        let started = Instant::now();
        let mut addresses: Vec<_> = (url.host.as_str(), url.port).to_socket_addrs().map_err(|e| FetchError::Dns(fail(&e)))?.collect();
        let resolved = Instant::now();
        // RENDER_LOOPBACK_ONLY=1 keeps a replayed page from reaching the network.
        if loopback_only() {
            addresses.retain(|a| a.ip().is_loopback());
            if addresses.is_empty() {
                return Err(FetchError::Connect(fail(&"not a loopback address (RENDER_LOOPBACK_ONLY)")));
            }
        }
        // IPv4 first: an IPv6 address without a working route then only costs a stagger.
        addresses.sort_by_key(SocketAddr::is_ipv6);
        let tcp = connect_any(&addresses).map_err(|e| match e.kind() {
            io::ErrorKind::TimedOut => FetchError::Timeout(fail(&e)),
            _ => FetchError::Connect(fail(&e)),
        })?;
        let timeout = Some(Duration::from_secs(20));
        let set = tcp.set_read_timeout(timeout).and_then(|()| tcp.set_write_timeout(timeout)).and_then(|()| tcp.set_nodelay(true));
        set.map_err(|e| FetchError::Connect(fail(&e)))?;
        let connected = Instant::now();
        let mut stream: Box<dyn Stream> = if url.https {
            let name = rustls::pki_types::ServerName::try_from(url.host.clone()).map_err(|e| FetchError::Tls(fail(&e)))?;
            let session = rustls::ClientConnection::new(self.tls.clone(), name).map_err(|e| FetchError::Tls(fail(&e)))?;
            Box::new(rustls::StreamOwned::new(session, tcp))
        } else {
            Box::new(tcp)
        };
        // Flushing a TLS stream completes its handshake, which the request would
        // otherwise wait for in its first write.
        stream.flush().map_err(|e| FetchError::Tls(fail(&e)))?;
        if let Some(start) = timeline() {
            let (dns, tcp, tls) = (since(started, resolved), since(resolved, connected), since(connected, Instant::now()));
            eprintln!("[net] {:8.1} connect {}:{} dns {dns:.1} tcp {tcp:.1} tls {tls:.1}", since(start, started), url.host, url.port);
        }
        Ok(BufReader::with_capacity(64 << 10, stream))
    }

    fn send(&mut self, ask: &Ask, watch: Option<&mut Watcher>) -> Result<Response, FetchError> {
        if ask.aborted() {
            return Err(FetchError::Aborted);
        }
        let started = Instant::now();
        let result = self.send_cached(ask, watch);
        if let (Some(start), Ok((response, how))) = (timeline(), &result) {
            let (head, end) = (since(start, response.head_at), since(start, Instant::now()));
            let bytes = response.body.len();
            eprintln!("[net] {:8.1} {} {} {} head {head:.1} end {end:.1} bytes {bytes} conn {how}", since(start, started), ask.method.as_str(), response.status, ask.url);
        } else if let (Some(start), Err(e)) = (timeline(), &result) {
            eprintln!("[net] {:8.1} {} failed {} at {:.1}: {e}", since(start, started), ask.method.as_str(), ask.url, since(start, Instant::now()));
        }
        result.map(|(response, _)| response)
    }

    /// `send_on_any` through the cache for a plain GET: a fresh stored answer is
    /// used as it is, a stale one with validators is revalidated, and a new answer
    /// that may be kept is stored. `watch` sees a stored body as if it arrived.
    fn send_cached(&mut self, ask: &Ask, mut watch: Option<&mut Watcher>) -> Result<(Response, &'static str), FetchError> {
        // The jar's cookies may go with a plain GET; a script's own headers may not.
        let plain = ask.method == Method::Get && ask.headers.iter().all(|h| h.0 == "cookie") && ask.body.is_none();
        let Some(cache) = self.cache.clone().filter(|_| plain) else { return self.send_on_any(ask, watch) };
        let key = ask.url.to_string();
        let stored = match cache.get(&key) {
            Some(s) if s.fresh(std::time::SystemTime::now()) => return Ok((replay(&ask.url, s, watch), "cache")),
            s => s,
        };
        let validators = stored.as_ref().map(crate::cache::Stored::validators).unwrap_or_default();
        let headers = ask.headers.iter().cloned().chain(validators).collect();
        let conditional = Ask { headers, abort: ask.abort.clone(), ..Ask::get(ask.url.clone()) };
        let (response, how) = self.send_on_any(&conditional, watch.as_deref_mut())?;
        if let (304, Some(s)) = (response.status, stored) {
            let s = cache.refresh(&key, s, &response.headers);
            return Ok((replay(&ask.url, s, watch), "revalidated"));
        }
        let answer = crate::cache::Answer { status: response.status, reason: &response.reason, headers: &response.headers, body: &response.body };
        cache.put(&key, &answer);
        Ok((response, how))
    }

    /// `send`, also telling which connection carried the request: one this client
    /// kept, an idle one from the pool, or a new one.
    fn send_on_any(&mut self, ask: &Ask, mut watch: Option<&mut Watcher>) -> Result<(Response, &'static str), FetchError> {
        let url = &ask.url;
        let key = (url.https, url.host.clone(), url.port);
        let mine = self.pool.contains_key(&key);
        let kept = self.pool.remove(&key).or_else(|| {
            let mut idle = IDLE.lock().expect("idle connections");
            idle.iter().position(|c| c.0 == key).map(|i| idle.swap_remove(i).1)
        });
        if let Some(mut conn) = kept {
            // A kept connection may have been closed by the server meanwhile; then
            // the request is sent again on a new one.
            match exchange(&mut conn, ask, self.referer.as_deref(), watch.as_deref_mut()) {
                Ok((response, reusable)) => {
                    if reusable {
                        self.pool.insert(key, conn);
                    }
                    return Ok((response, if mine { "kept" } else { "idle" }));
                }
                Err(_) if ask.aborted() => return Err(FetchError::Aborted),
                Err(e) if !ask.may_resend(&e) => return Err(failure(url, &e)),
                Err(_) => {}
            }
        }
        let (mut conn, fresh) = self.open(url)?;
        let mut result = exchange(&mut conn, ask, self.referer.as_deref(), watch.as_deref_mut());
        if !fresh && result.as_ref().is_err_and(|e| !ask.aborted() && ask.may_resend(e)) {
            // The idle connection had been closed by its server meanwhile.
            conn = self.connect(url)?;
            result = exchange(&mut conn, ask, self.referer.as_deref(), watch);
        }
        if result.is_err() && ask.aborted() {
            return Err(FetchError::Aborted);
        }
        let (response, reusable) = result.map_err(|e| failure(url, &e))?;
        if reusable {
            self.pool.insert(key, conn);
        }
        Ok((response, if fresh { "new" } else { "idle" }))
    }

    /// A connection to `url`'s server: a new one, or an idle one parked while the new
    /// one is still connecting, whichever comes first; true for a new one. A
    /// stylesheet asked for while its page's HTML downloads then goes out on the
    /// HTML's connection as soon as that is free, which on a distant server is
    /// sooner than a new TCP and TLS handshake.
    fn open(&self, url: &Url) -> Result<(Conn, bool), FetchError> {
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
    fn get_watched(&mut self, url: &Url, watch: Option<&mut Watcher>) -> Result<(Url, Response), String> {
        self.follow(Ask::get(url.clone()), watch).map_err(|e| e.to_string())
    }

    /// Sends `ask` and follows its redirects, with the cookies of the jar, which
    /// keeps those every response sets. Returns the final URL with the response.
    fn follow(&mut self, mut ask: Ask, mut watch: Option<&mut Watcher>) -> Result<(Url, Response), FetchError> {
        for _ in 0..=MAX_REDIRECTS {
            let origin = crate::cookie::Origin { https: ask.url.https, host: &ask.url.host, path: ask.url.path.split('?').next().unwrap_or("/") };
            let cookies = crate::cookie::header(origin, false);
            ask.headers.retain(|h| h.0 != "cookie");
            if !cookies.is_empty() {
                ask.headers.push(("cookie".into(), cookies));
            }
            let response = self.send(&ask, watch.as_deref_mut())?;
            for (_, line) in response.headers.iter().filter(|h| h.0 == "set-cookie") {
                crate::cookie::store(origin, line, false);
            }
            let target = response.header("location").filter(|_| matches!(response.status, 301 | 302 | 303 | 307 | 308));
            let Some(location) = target else { return Ok((ask.url, response)) };
            let next = ask.url.join(location).ok_or_else(|| FetchError::Protocol(format!("{}: bad redirect to {location}", ask.url)))?;
            let to_get = match response.status {
                303 => ask.method != Method::Head,
                301 | 302 => ask.method == Method::Post,
                _ => false,
            };
            if to_get {
                ask.method = Method::Get;
                ask.body = None;
                ask.headers.retain(|h| !matches!(h.0.as_str(), "content-type" | "content-encoding" | "content-language" | "content-location"));
            }
            if (next.https, &next.host, next.port) != (ask.url.https, &ask.url.host, ask.url.port) {
                ask.headers.retain(|h| h.0 != "authorization");
            }
            ask.url = next;
        }
        Err(FetchError::TooManyRedirects(format!("{}: too many redirects", ask.url)))
    }
}

/// A stored answer as a response, shown to `watch` as if it had just arrived.
fn replay(url: &Url, stored: crate::cache::Stored, watch: Option<&mut Watcher>) -> Response {
    if let Some(see) = watch {
        see(url, Arrival::Head { early_links: &[] });
        if !stored.body.is_empty() {
            see(url, Arrival::Body(&stored.body));
        }
    }
    Response { head_at: Instant::now(), status: stored.status, reason: stored.reason, headers: stored.headers, body: stored.body }
}

/// Open connections no client is using. A client leaves its connections here when
/// it is done, so the next page from the same servers skips connecting again.
static IDLE: Mutex<Vec<((bool, String, u16), Conn)>> = Mutex::new(Vec::new());

/// Signalled when a connection is parked in `IDLE`, or a connection attempt ends.
static IDLE_CHANGED: Condvar = Condvar::new();

/// A new connection being made on its own thread for `Client::open`. When the
/// client took an idle connection instead, the new one is parked once ready.
struct Attempt {
    result: Option<Result<Conn, FetchError>>,
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

/// Resources a preload scanner found, as written in the document (relative URLs
/// unresolved), grouped by when they are fetched.
#[derive(Debug, Default, PartialEq, Eq)]
struct Found {
    /// Stylesheets: the first frame waits for them, so they are fetched at once.
    first: Vec<String>,
    /// Scripts, fonts and module preloads, fetched at once too.
    then: Vec<String>,
    /// Images: fetched once the stylesheets found before them are in.
    images: Vec<String>,
    /// Origins to connect to ahead of their first request.
    connect: Vec<String>,
}

impl Found {
    /// A `<link>` tag or a `Link` header entry. A stylesheet must have `rel` exactly
    /// `stylesheet`, as the style code requires; the other kinds are rel tokens.
    fn link(&mut self, rel: &str, href: &str, kind: &str) {
        if href.is_empty() {
            return;
        }
        if rel.trim().eq_ignore_ascii_case("stylesheet") {
            return self.first.push(href.to_string());
        }
        for token in rel.split_ascii_whitespace() {
            let list = match token.to_ascii_lowercase().as_str() {
                "preload" if kind.eq_ignore_ascii_case("style") => &mut self.first,
                "preload" if ["script", "font"].iter().any(|k| kind.eq_ignore_ascii_case(k)) => &mut self.then,
                "preload" if kind.eq_ignore_ascii_case("image") => &mut self.images,
                "modulepreload" => &mut self.then,
                "preconnect" => &mut self.connect,
                _ => continue,
            };
            return list.push(href.to_string());
        }
    }

    /// A `<link>`, `<script>` or `<img>` start tag, given as markup. An image
    /// counts with its `src` and the first candidate of its `srcset`, the ones
    /// the page code asks for.
    fn tag(&mut self, markup: &str) {
        use crate::dom::{atom, parse_html, NodeId};
        let mut dom = parse_html(markup);
        let kind = dom.atoms.intern("as");
        let tags = [atom::LINK, atom::SCRIPT, atom::IMG];
        let Some(n) = (0..dom.nodes.len() as NodeId).find(|&n| tags.contains(&dom.nodes[n as usize].tag)) else { return };
        let attr = |name| dom.attr(n, name).map(str::trim).filter(|v| !v.is_empty());
        match dom.nodes[n as usize].tag {
            atom::LINK => self.link(attr(atom::REL).unwrap_or(""), attr(atom::HREF).unwrap_or(""), attr(kind).unwrap_or("")),
            atom::SCRIPT => self.then.extend(attr(atom::SRC).map(str::to_string)),
            _ => {
                self.images.extend(attr(atom::SRC).map(str::to_string));
                let first = attr(atom::SRCSET).and_then(|set| set.split(',').next()?.split_ascii_whitespace().next());
                self.images.extend(first.map(str::to_string));
            }
        }
    }

    /// The value of a `Link` header: `<href>; rel=...; as=..., <href>; ...`.
    fn header(&mut self, value: &str) {
        let mut rest = value;
        while let Some(open) = rest.find('<') {
            let Some(close) = rest[open..].find('>') else { return };
            let href = rest[open + 1..open + close].trim();
            let after = &rest[open + close + 1..];
            let end = after.find('<').unwrap_or(after.len());
            let (mut rel, mut kind) = ("", "");
            for param in after[..end].split([';', ',']) {
                let Some((name, value)) = param.split_once('=') else { continue };
                let value = value.trim().trim_matches('"');
                match name.trim().to_ascii_lowercase().as_str() {
                    "rel" => rel = value,
                    "as" => kind = value,
                    _ => {}
                }
            }
            self.link(rel, href, kind);
            rest = &after[end..];
        }
    }
}

/// Finds the resources a document names in `<link>`, `<script src>` and `<img>`
/// tags while it downloads, so they can be requested before the rest of the HTML
/// has arrived. Comments and the insides of `<script>` and `<style>` are skipped.
#[derive(Default)]
struct Preload {
    seen: Vec<u8>,
    /// Where scanning resumes: everything before it has been looked at.
    at: usize,
    /// Inside a comment or raw text: the marker that ends it.
    until: Option<&'static [u8]>,
}

impl Preload {
    /// Takes the next piece of the document; returns what the tags it completed name.
    fn feed(&mut self, piece: &[u8]) -> Found {
        self.seen.extend_from_slice(piece);
        let mut found = Found::default();
        loop {
            let rest = &self.seen[self.at..];
            if let Some(close) = self.until {
                match rest.windows(close.len()).position(|w| w.eq_ignore_ascii_case(close)) {
                    Some(i) => {
                        self.at += i + close.len();
                        self.until = None;
                        continue;
                    }
                    // The marker may be split across pieces: its start is looked at again.
                    None => {
                        self.at += rest.len().saturating_sub(close.len() - 1);
                        break;
                    }
                }
            }
            let Some(i) = memchr::memchr(b'<', rest) else {
                self.at = self.seen.len();
                break;
            };
            let tag = &rest[i..];
            if tag.len() < 4 {
                self.at += i;
                break;
            }
            if tag.starts_with(b"<!--") {
                self.at += i + 4;
                self.until = Some(b"-->");
                continue;
            }
            let Some(name_end) = tag[1..].iter().position(|b| !b.is_ascii_alphanumeric()) else {
                self.at += i;
                break;
            };
            let name = &tag[1..=name_end];
            let raw_end: &'static [u8] = if name.eq_ignore_ascii_case(b"script") {
                b"</script"
            } else if name.eq_ignore_ascii_case(b"style") {
                b"</style"
            } else if name.eq_ignore_ascii_case(b"link") || name.eq_ignore_ascii_case(b"img") {
                b""
            } else {
                self.at += i + 1;
                continue;
            };
            let Some(end) = memchr::memchr(b'>', tag) else {
                self.at += i;
                break;
            };
            found.tag(&String::from_utf8_lossy(&tag[..=end]));
            self.at += i + end + 1;
            self.until = Some(raw_end).filter(|m| !m.is_empty());
        }
        found
    }
}

/// Where one resource stands: downloading, or done with its file (`None` when it failed).
enum Slot {
    Busy,
    Done(Option<PathBuf>),
}

/// When a queued download may go out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Priority {
    /// As soon as its server has a free slot: stylesheets, scripts, fonts, preloads.
    Now,
    /// Once the stylesheets found so far are in, or `STYLESHEETS_FIRST` after the
    /// page opened, so images leave the bandwidth to those: images.
    AfterStyles,
}

type Origin = (bool, String, u16);

fn origin(url: &Url) -> Origin {
    (url.https, url.host.clone(), url.port)
}

/// Downloads waiting for a free slot on their server, in the order they were found,
/// and how many requests each server has in flight.
#[derive(Default)]
struct Queue {
    waiting: Vec<(Url, Priority)>,
    busy: HashMap<Origin, usize>,
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
    /// The servers connected to ahead of their requests, the page's own first.
    connected: Mutex<HashSet<(bool, String, u16)>>,
    queue: Mutex<Queue>,
    /// The stylesheets found in the page, which images wait for until `styles_by`.
    stylesheets: Mutex<Vec<Url>>,
    styles_by: Instant,
    /// Set once a thread waits for `styles_by` to let the images go.
    timer: AtomicBool,
}

impl Store {
    fn new(tls: Arc<rustls::ClientConfig>, page: &Url) -> Result<Arc<Store>, String> {
        static OPENED: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("render-{}-{}", std::process::id(), OPENED.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let connected = Mutex::new(HashSet::from([(page.https, page.host.clone(), page.port)]));
        let page = page.to_string().into();
        let store = Store {
            dir,
            tls,
            page,
            slots: Mutex::new(HashMap::new()),
            done: Condvar::new(),
            finished: AtomicUsize::new(0),
            wake: Mutex::new(None),
            connected,
            queue: Mutex::default(),
            stylesheets: Mutex::default(),
            styles_by: Instant::now() + STYLESHEETS_FIRST,
            timer: AtomicBool::new(false),
        };
        Ok(Arc::new(store))
    }

    /// Opens connections to the servers of `urls` not connected to yet, up to
    /// `MAX_PRECONNECT` besides the page's own.
    fn connect_ahead<'u>(&self, urls: impl IntoIterator<Item = &'u Url>) {
        let mut connected = self.connected.lock().expect("connected hosts");
        for url in urls {
            if connected.len() > MAX_PRECONNECT {
                return;
            }
            if connected.insert((url.https, url.host.clone(), url.port)) {
                preconnect(self.tls.clone(), url.clone());
            }
        }
    }

    /// Queues the URLs nobody has started on. Each goes out once its server has
    /// fewer than `PER_HOST` requests in flight and `priority` allows.
    fn enqueue(self: &Arc<Self>, urls: impl IntoIterator<Item = Url>, priority: Priority) {
        let urls = self.claim(urls);
        if urls.is_empty() {
            return;
        }
        if priority == Priority::AfterStyles && !self.timer.swap(true, Ordering::Relaxed) {
            let (store, at) = (Arc::downgrade(self), self.styles_by);
            std::thread::spawn(move || {
                std::thread::sleep(at.saturating_duration_since(Instant::now()));
                if let Some(store) = store.upgrade() {
                    store.pump();
                }
            });
        }
        self.queue.lock().expect("download queue").waiting.extend(urls.into_iter().map(|u| (u, priority)));
        self.pump();
    }

    /// Starts a download thread for every queued URL that may go out now.
    fn pump(self: &Arc<Self>) {
        while let Some(url) = self.next() {
            let store = self.clone();
            std::thread::spawn(move || store.work(url));
        }
    }

    /// Takes the first queued URL that may go out now, the `Now` ones first, and
    /// counts it against its server.
    fn next(&self) -> Option<Url> {
        let styled = Instant::now() >= self.styles_by || {
            let stylesheets = self.stylesheets.lock().expect("stylesheets");
            let slots = self.slots();
            stylesheets.iter().all(|u| !matches!(slots.get(u), Some(Slot::Busy)))
        };
        let mut queue = self.queue.lock().expect("download queue");
        let Queue { waiting, busy } = &mut *queue;
        let free = |url: &Url| busy.get(&origin(url)).copied().unwrap_or(0) < PER_HOST;
        let now = waiting.iter().position(|(u, p)| *p == Priority::Now && free(u));
        let i = now.or_else(|| waiting.iter().position(|(u, _)| styled && free(u)))?;
        let (url, _) = waiting.remove(i);
        *busy.entry(origin(&url)).or_default() += 1;
        Some(url)
    }

    /// Downloads `url`, then whatever queued URL may go out next, on one client so
    /// its connections are reused; starts more threads when several may go.
    fn work(self: Arc<Self>, mut url: Url) {
        let mut client = self.client();
        loop {
            let path = self.download(&mut client, &url);
            if let Some(n) = self.queue.lock().expect("download queue").busy.get_mut(&origin(&url)) {
                *n -= 1;
            }
            self.finish(url, path);
            let Some(next) = self.next() else { break };
            url = next;
            self.pump();
        }
        // The connections go back to the idle pool before other threads look there.
        drop(client);
        self.pump();
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
    /// The page was served as an XML document, XHTML.
    pub xml: bool,
    store: Arc<Store>,
}

impl Site {
    /// Downloads the page at `location`. Returns the site and the page's HTML.
    /// Stylesheets the HTML links to start downloading as soon as their tag arrives.
    pub fn open(location: &str) -> Result<(Arc<Site>, String), String> {
        mark(&format!("open {location}"));
        let started = Instant::now();
        let url = Url::parse(location).ok_or_else(|| format!("{location}: not an http or https URL"))?;
        let tls = tls_config();
        let store = Store::new(tls.clone(), &url)?;
        // A second connection to the page's server, ready for the first stylesheet found.
        preconnect(tls.clone(), url.clone());
        let mut preload = Preload::default();
        let mut first_byte = None;
        let mut see = |at: &Url, arrival: Arrival| {
            first_byte.get_or_insert_with(|| started.elapsed());
            let found = match arrival {
                Arrival::Head { early_links } => {
                    let mut found = Found::default();
                    early_links.iter().for_each(|value| found.header(value));
                    found
                }
                Arrival::Body(piece) => preload.feed(piece),
            };
            let join = |hrefs: Vec<String>| hrefs.iter().filter_map(|href| at.join(href)).collect::<Vec<_>>();
            let (first, then, images) = (join(found.first), join(found.then), join(found.images));
            if timeline().is_some() {
                first.iter().chain(&then).chain(&images).for_each(|u| mark(&format!("found {u}")));
            }
            store.stylesheets.lock().expect("stylesheets").extend(first.iter().cloned());
            store.connect_ahead(join(found.connect).iter().chain(&then).chain(&images));
            store.enqueue(first, Priority::Now);
            store.enqueue(then, Priority::Now);
            store.enqueue(images, Priority::AfterStyles);
        };
        let (url, response) = Client::new(tls).get_watched(&url, Some(&mut see))?;
        let received = started.elapsed();
        if response.status >= 400 {
            return Err(format!("{url}: HTTP {}", response.status));
        }
        let html = crate::encoding::decode_document(&response.body, response.header("content-type"), false);
        let first_byte = first_byte.unwrap_or(received);
        let essence = response.header("content-type").and_then(|t| t.split(';').next()).map(|t| t.trim().to_ascii_lowercase());
        let xml = matches!(essence.as_deref(), Some("application/xhtml+xml" | "application/xml" | "text/xml"));
        Ok((Arc::new(Site { url, first_byte, received, xml, store }), html))
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
        // Images may have waited for this one.
        self.store.pump();
        path
    }

    /// The local file for `src` once its download has finished, `Some(None)` when
    /// it failed or cannot be fetched. Before that, starts the download in the
    /// background if nothing has, and returns `None` without waiting.
    pub fn file_if_ready(&self, src: &str) -> Option<Option<PathBuf>> {
        let Some(url) = self.url.join(src) else { return Some(None) };
        let ready = self.store.ready(&url);
        if ready.is_none() {
            self.store.enqueue([url], Priority::Now);
        }
        ready
    }

    /// True once the download of `src` has finished, or when it cannot be fetched.
    pub fn arrived(&self, src: &str) -> bool {
        self.url.join(src).is_none_or(|url| self.store.ready(&url).is_some())
    }

    /// Downloads the listed resources, `PER_HOST` at once to each server, and waits for them.
    pub fn prefetch(&self, srcs: &[String]) {
        let urls = self.urls(srcs);
        self.store.enqueue(urls.iter().cloned(), Priority::Now);
        // Some may have been downloading already, for an earlier request.
        self.store.wait(&urls, None);
    }

    /// Like `prefetch`, without waiting for the downloads.
    pub fn prefetch_later(&self, srcs: Vec<String>) {
        self.store.enqueue(self.urls(&srcs), Priority::Now);
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
        self.store.connect_ahead(&self.urls(srcs));
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
    use super::{fetch, request_head, Arrival, Ask, Client, FetchError, Found, Method, Preload, Request, Url};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};

    /// A server on a local port that answers requests with `responses` in turn,
    /// over as many connections as the client opens, and stops after the last.
    /// The requests it read come back on the channel, body included.
    fn serve(responses: Vec<Vec<u8>>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut responses = responses.into_iter();
            for stream in listener.incoming() {
                let mut conn = BufReader::new(stream.unwrap());
                loop {
                    let mut request = String::new();
                    while conn.read_line(&mut request).unwrap_or(0) > 0 && !request.ends_with("\r\n\r\n") {}
                    if request.is_empty() {
                        break;
                    }
                    let length = request.lines().find_map(|l| l.strip_prefix("Content-Length: ")).map_or(0, |n| n.parse().unwrap());
                    let mut body = vec![0; length];
                    conn.read_exact(&mut body).unwrap();
                    request.push_str(&String::from_utf8_lossy(&body));
                    tx.send(request).unwrap();
                    let Some(response) = responses.next() else { return };
                    conn.get_mut().write_all(&response).unwrap();
                    if responses.len() == 0 {
                        return;
                    }
                }
            }
        });
        (origin, rx)
    }

    #[test]
    fn resources_name_their_page_in_referer() {
        let font = Ask::get(Url::parse("https://www.apple.com/wss/fonts?families=SF+Pro,v3|SF+Pro+Icons,v3").unwrap());
        let head = request_head(&font, Some("https://www.apple.com/"));
        assert!(head.starts_with("GET /wss/fonts?families=SF+Pro,v3|SF+Pro+Icons,v3 HTTP/1.1\r\nHost: www.apple.com\r\n"));
        assert_eq!(head.lines().filter(|l| *l == "Referer: https://www.apple.com/").count(), 1);
        assert!(head.ends_with("\r\n\r\n"));
        assert!(!request_head(&font, None).contains("Referer"));
    }

    #[test]
    fn scripts_see_the_user_agent_requests_send() {
        assert!(include_str!("prelude.js").contains(&format!("userAgent: '{}'", super::USER_AGENT)));
        let head = request_head(&Ask::get(Url::parse("https://example.com/").unwrap()), None);
        assert!(head.contains(&format!("\r\nUser-Agent: {}\r\n", super::USER_AGENT)));
        // The first language asked for is the one scripts read from navigator.language.
        let first = super::ACCEPT_LANGUAGE.split(',').next().unwrap();
        assert!(include_str!("prelude.js").contains(&format!("language: '{first}'")));
        assert!(head.contains(&format!("\r\nAccept-Language: {}\r\n", super::ACCEPT_LANGUAGE)));
    }

    #[test]
    fn fetch_sends_method_headers_and_body() {
        let reply = b"HTTP/1.1 201 Created Here\r\nContent-Length: 2\r\nSet-Cookie: a=1\r\nX-Id: 7\r\nSet-Cookie: b=2\r\n\r\nok";
        let (origin, requests) = serve(vec![reply.to_vec()]);
        let headers = [("Content-Type", "text/plain"), ("Cookie", "secret=1"), ("Host", "evil.test"), ("X-Split", "a\r\nInjected: 1"), ("X-Ok", "yes")];
        let request = Request {
            method: Method::parse("post").unwrap(),
            headers: headers.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect(),
            body: Some(b"a=1&b=2".to_vec()),
            referrer: Some("http://page.test/".into()),
            ..Request::get(&format!("{origin}/submit?x=1"))
        };
        let response = fetch(request).unwrap();
        assert_eq!((response.status, response.status_text.as_str(), response.body.as_slice()), (201, "Created Here", &b"ok"[..]));
        assert_eq!(response.url, format!("{origin}/submit?x=1"));
        let names: Vec<_> = response.headers.iter().map(|h| format!("{}={}", h.0, h.1)).collect();
        assert_eq!(names, ["content-length=2", "set-cookie=a=1", "x-id=7", "set-cookie=b=2"]);
        let sent = requests.recv().unwrap();
        assert!(sent.starts_with("POST /submit?x=1 HTTP/1.1\r\n"), "{sent}");
        assert!(sent.contains("\r\ncontent-type: text/plain\r\n") && sent.contains("\r\nx-ok: yes\r\n"), "{sent}");
        assert!(sent.contains("\r\nContent-Length: 7\r\n") && sent.contains("\r\nReferer: http://page.test/\r\n"), "{sent}");
        assert!(sent.ends_with("\r\n\r\na=1&b=2"), "{sent}");
        assert_eq!(sent.matches("Host:").count(), 1);
        assert!(!sent.to_ascii_lowercase().contains("cookie") && !sent.contains("Injected") && !sent.contains("evil"), "{sent}");
    }

    #[test]
    fn cookies_from_responses_go_back_on_later_requests_and_redirects() {
        let (origin, requests) = serve(vec![
            b"HTTP/1.1 302 Found\r\nLocation: /next\r\nSet-Cookie: hop=1; Path=/\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nSet-Cookie: id=7; HttpOnly\r\nSet-Cookie: deep=x; Path=/other\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ]);
        // A host of its own, so that no other test's cookies are sent with these.
        let origin = origin.replace("127.0.0.1", "localhost");
        fetch(Request::get(&format!("{origin}/start"))).unwrap();
        fetch(Request::get(&format!("{origin}/again"))).unwrap();
        let sent: Vec<String> = requests.iter().take(3).map(|r| r.lines().filter(|l| l.starts_with("cookie:")).collect::<Vec<_>>().join("|")).collect();
        assert_eq!(sent, ["", "cookie: hop=1", "cookie: hop=1; id=7"]);
        let page = crate::cookie::Origin { https: false, host: "localhost", path: "/again" };
        assert_eq!(crate::cookie::header(page, true), "hop=1", "scripts do not see HttpOnly cookies");
    }

    #[test]
    fn fetch_redirects_keep_or_drop_the_body_like_a_browser() {
        let (origin, requests) = serve(vec![
            b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /b\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 303 See Other\r\nLocation: /c\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone".to_vec(),
        ]);
        let request = Request {
            method: Method::Put,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(b"{}".to_vec()),
            ..Request::get(&format!("{origin}/a"))
        };
        let response = fetch(request).unwrap();
        assert_eq!((response.url, response.body), (format!("{origin}/c"), b"done".to_vec()));
        let sent: Vec<String> = requests.iter().take(3).collect();
        assert!(sent[0].starts_with("PUT /a ") && sent[0].ends_with("\r\n\r\n{}"), "{}", sent[0]);
        assert!(sent[1].starts_with("PUT /b ") && sent[1].ends_with("\r\n\r\n{}"), "{}", sent[1]);
        assert!(sent[2].starts_with("GET /c ") && sent[2].ends_with("\r\n\r\n"), "{}", sent[2]);
        assert!(!sent[2].contains("content-type") && !sent[2].contains("Content-Length"), "{}", sent[2]);
    }

    #[test]
    fn fetch_head_reads_no_body_and_keeps_the_connection() {
        let (origin, requests) = serve(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec(),
        ]);
        let head = fetch(Request { method: Method::Head, ..Request::get(&origin) }).unwrap();
        assert_eq!((head.status, head.body.len()), (200, 0));
        assert_eq!(fetch(Request::get(&origin)).unwrap().body, b"hello");
        assert!(requests.recv().unwrap().starts_with("HEAD / HTTP/1.1\r\n"));
    }

    #[test]
    fn scripts_keep_six_requests_in_flight_on_each_server() {
        use std::sync::{Condvar, Mutex};
        use std::time::Duration;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        // Requests in flight, the most at once, how many arrived, and connections.
        let state = Arc::new((Mutex::new((0usize, 0usize, 0usize, 0usize)), Condvar::new()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let state = shared.clone();
                state.0.lock().unwrap().3 += 1;
                std::thread::spawn(move || {
                    let mut conn = BufReader::new(stream.unwrap());
                    loop {
                        let mut request = String::new();
                        while conn.read_line(&mut request).unwrap_or(0) > 0 && !request.ends_with("\r\n\r\n") {}
                        if request.is_empty() {
                            return;
                        }
                        let (lock, changed) = &*state;
                        let mut s = lock.lock().unwrap();
                        s.0 += 1;
                        s.1 = s.1.max(s.0);
                        s.2 += 1;
                        changed.notify_all();
                        // Held until all ten were asked for, or for a while.
                        let mut s = changed.wait_timeout_while(s, Duration::from_millis(1500), |s| s.2 < 10).unwrap().0;
                        s.0 -= 1;
                        drop(s);
                        conn.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx").unwrap();
                    }
                });
            }
        });
        let asks: Vec<_> = (0..10).map(|i| {
            let url = format!("{origin}/i{i}");
            std::thread::spawn(move || fetch(Request::get(&url)).unwrap().body)
        }).collect();
        for ask in asks {
            assert_eq!(ask.join().unwrap(), b"x");
        }
        let s = *state.0.lock().unwrap();
        assert_eq!((s.1, s.2), (6, 10));
        assert!(s.3 <= 6, "{} connections", s.3);
    }

    #[test]
    fn fetch_errors_say_what_failed() {
        assert!(matches!(fetch(Request::get("ftp://example.com/")), Err(FetchError::UnsupportedScheme(_))));
        let loop_back = b"HTTP/1.1 302 Found\r\nLocation: /\r\nContent-Length: 0\r\n\r\n".to_vec();
        let (origin, _requests) = serve(vec![loop_back; 9]);
        assert!(matches!(fetch(Request::get(&origin)), Err(FetchError::TooManyRedirects(_))));
        let (origin, _requests) = serve(vec![b"hello there\r\n\r\n".to_vec()]);
        assert!(matches!(fetch(Request::get(&origin)), Err(FetchError::Protocol(_))));
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(matches!(fetch(Request::get(&format!("http://127.0.0.1:{closed}/"))), Err(FetchError::Connect(_))));
        assert!(matches!(fetch(Request::get("http://no-such-host.invalid/")), Err(FetchError::Dns(_))));
    }

    #[test]
    fn aborting_stops_a_fetch_in_the_middle_of_its_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/slow", listener.local_addr().unwrap().port());
        let (go, more) = mpsc::channel::<()>();
        let (started, has_started) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nfirst").unwrap();
            started.send(()).unwrap();
            more.recv().unwrap();
            // The rest of the body: a reader that never looks at the flag would take it all.
            drop(stream.write_all(&[b'x'; 995]));
        });
        let abort = Arc::new(AtomicBool::new(false));
        let request = Request { abort: Some(abort.clone()), ..Request::get(&url) };
        let client = std::thread::spawn(move || fetch(request));
        has_started.recv().unwrap();
        abort.store(true, Ordering::Relaxed);
        go.send(()).unwrap();
        assert!(matches!(client.join().unwrap(), Err(FetchError::Aborted)));
        server.join().unwrap();
    }

    #[test]
    fn brotli_bodies_decode_while_streaming_and_whole() {
        let packed = include_bytes!("testdata/lines.html.br");
        let plain: String = (0..400).map(|i| format!("<p>line {i}</p>\n")).collect();
        let mut reply = b"HTTP/1.1 200 OK\r\nContent-Encoding: br\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for chunk in packed.chunks(37) {
            reply.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            reply.extend_from_slice(chunk);
            reply.extend_from_slice(b"\r\n");
        }
        reply.extend_from_slice(b"0\r\n\r\n");
        let (origin, requests) = serve(vec![reply]);
        let mut streamed = Vec::new();
        let mut see = |_: &Url, arrival: Arrival| {
            if let Arrival::Body(piece) = arrival {
                streamed.extend_from_slice(piece);
            }
        };
        let (_, response) = Client::new(super::tls_config()).get_watched(&Url::parse(&origin).unwrap(), Some(&mut see)).unwrap();
        assert_eq!(String::from_utf8(response.body).unwrap(), plain);
        assert_eq!(String::from_utf8(streamed).unwrap(), plain);
        assert!(requests.recv().unwrap().contains("\r\nAccept-Encoding: gzip, br\r\n"));
    }

    #[test]
    fn fetch_reads_file_urls() {
        let path = std::env::temp_dir().join(format!("net-test-{} page.html", std::process::id()));
        std::fs::write(&path, "<p>local</p>").unwrap();
        let url = format!("file://{}", path.display()).replace(' ', "%20");
        let response = fetch(Request::get(&url));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(response.unwrap().body, b"<p>local</p>");
        assert!(matches!(fetch(Request::get(&url)), Err(FetchError::File(_))));
    }

    #[test]
    fn preload_finds_stylesheets_split_across_pieces() {
        let html = b"<html><head><LINK rel=\"icon\" href=\"a.ico\"><link rel=stylesheet href='/s.css'><link\n rel=\"Stylesheet\" href=\"b.css\"></head>";
        let mut preload = Preload::default();
        let mut found = Vec::new();
        for piece in html.chunks(7) {
            found.extend(preload.feed(piece).first);
        }
        assert_eq!(found, ["/s.css", "b.css"]);
    }

    #[test]
    fn preload_finds_scripts_images_and_hinted_links_but_not_inside_comments_or_scripts() {
        let html = concat!(
            "<head><!-- <img src=commented.png> --><link rel=preconnect href=https://cdn.test>",
            "<link rel=\"alternate stylesheet\" href=alt.css><link rel=preload as=style href=early.css>",
            "<link rel=preload as=font href=f.woff2 crossorigin><link rel=preload as=fetch href=data.json>",
            "<link rel=modulepreload href=m.js><link rel=dns-prefetch href=//dns.test>",
            "<script>document.write('<img src=written.png>')</script><style>a{background:url(x.png)}</style>",
            "<SCRIPT src=/app.js defer></SCRIPT></head><body><img src=a.png srcset='a1.png 1x, a2.png 2x'>",
            "<noscript><img src=fallback.jpg></noscript><img srcset=only.png><img src=''>",
        );
        let expected = Found {
            first: vec!["early.css".into()],
            then: ["f.woff2", "m.js", "/app.js"].map(String::from).to_vec(),
            images: ["a.png", "a1.png", "fallback.jpg", "only.png"].map(String::from).to_vec(),
            connect: vec!["https://cdn.test".into()],
        };
        for size in [1, 7, html.len()] {
            let mut preload = Preload::default();
            let mut found = Found::default();
            for piece in html.as_bytes().chunks(size) {
                let more = preload.feed(piece);
                found.first.extend(more.first);
                found.then.extend(more.then);
                found.images.extend(more.images);
                found.connect.extend(more.connect);
            }
            assert_eq!(found, expected, "pieces of {size}");
        }
    }

    #[test]
    fn link_headers_of_early_hints_reach_the_watcher_before_the_body() {
        let reply = concat!(
            "HTTP/1.1 103 Early Hints\r\nLink: </s.css>; rel=preload; as=style, <https://cdn.test>; rel=preconnect\r\n\r\n",
            "HTTP/1.1 103 Early Hints\r\nLink: </app.js>; rel=\"modulepreload\"\r\n\r\n",
            "HTTP/1.1 200 OK\r\nLink: </late.css>; rel=preload; as=style\r\nContent-Length: 4\r\n\r\nbody",
        );
        let (origin, _requests) = serve(vec![reply.as_bytes().to_vec()]);
        let mut seen = Vec::new();
        let mut see = |_: &Url, arrival: Arrival| match arrival {
            Arrival::Head { early_links } => seen.extend(early_links.iter().map(|l| format!("head {l}"))),
            Arrival::Body(piece) => seen.push(format!("body {}", String::from_utf8_lossy(piece))),
        };
        Client::new(super::tls_config()).get_watched(&Url::parse(&origin).unwrap(), Some(&mut see)).unwrap();
        assert_eq!(seen, [
            "head </s.css>; rel=preload; as=style, <https://cdn.test>; rel=preconnect",
            "head </app.js>; rel=\"modulepreload\"",
            "body body",
        ]);
        let mut found = Found::default();
        seen[..2].iter().for_each(|l| found.header(&l["head ".len()..]));
        assert_eq!(found, Found { first: vec!["/s.css".into()], then: vec!["/app.js".into()], images: Vec::new(), connect: vec!["https://cdn.test".into()] });
    }

    #[test]
    fn preloaded_images_wait_for_the_stylesheets() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let css_sent = Arc::new(AtomicBool::new(false));
        // Whether the stylesheet had been answered when the image was asked for.
        let (tx, image_asked) = mpsc::channel::<bool>();
        let flag = css_sent.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let (flag, tx) = (flag.clone(), tx.clone());
                std::thread::spawn(move || {
                    let mut conn = BufReader::new(stream.unwrap());
                    loop {
                        let mut request = String::new();
                        while conn.read_line(&mut request).unwrap_or(0) > 0 && !request.ends_with("\r\n\r\n") {}
                        let Some(path) = request.split_whitespace().nth(1).map(str::to_string) else { return };
                        let body = match path.as_str() {
                            "/" => "<link rel=stylesheet href=s.css><body><img src=i.png>",
                            "/s.css" => "p{color:red}",
                            _ => {
                                tx.send(flag.load(Ordering::SeqCst)).unwrap();
                                "png"
                            }
                        };
                        // Set before the stylesheet goes out: the client cannot have
                        // read it any earlier.
                        if path == "/s.css" {
                            flag.store(true, Ordering::SeqCst);
                        }
                        let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                        conn.get_mut().write_all(reply.as_bytes()).unwrap();
                    }
                });
            }
        });
        let (site, html) = super::Site::open(&origin).unwrap();
        assert!(html.contains("i.png"));
        assert_eq!(image_asked.recv(), Ok(true));
        let (arrived, all) = site.wait_for(&["i.png".into(), "s.css".into()], std::time::Instant::now() + std::time::Duration::from_secs(10));
        assert_eq!((arrived, all), (2, 2));
    }

    #[test]
    fn a_page_keeps_six_requests_in_flight_on_each_server() {
        use std::sync::{Condvar, Mutex};
        use std::time::{Duration, Instant};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        // Script requests in flight, the most at once, and how many arrived.
        let state = Arc::new((Mutex::new((0usize, 0usize, 0usize)), Condvar::new()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let state = shared.clone();
                std::thread::spawn(move || {
                    let mut conn = BufReader::new(stream.unwrap());
                    loop {
                        let mut request = String::new();
                        while conn.read_line(&mut request).unwrap_or(0) > 0 && !request.ends_with("\r\n\r\n") {}
                        let Some(path) = request.split_whitespace().nth(1).map(str::to_string) else { return };
                        let body = if path == "/" {
                            (0..10).map(|i| format!("<script src=s{i}.js></script>")).collect()
                        } else {
                            let (lock, changed) = &*state;
                            let mut s = lock.lock().unwrap();
                            s.0 += 1;
                            s.1 = s.1.max(s.0);
                            s.2 += 1;
                            changed.notify_all();
                            // Holds every answer until all scripts were asked for, or for a
                            // while, so the requests a client sends at once are all in flight.
                            let mut s = changed.wait_timeout_while(s, Duration::from_millis(1500), |s| s.2 < 10).unwrap().0;
                            s.0 -= 1;
                            "x".to_string()
                        };
                        let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                        conn.get_mut().write_all(reply.as_bytes()).unwrap();
                    }
                });
            }
        });
        let (site, _) = super::Site::open(&origin).unwrap();
        let scripts: Vec<String> = (0..10).map(|i| format!("s{i}.js")).collect();
        site.prefetch(&scripts);
        assert_eq!(site.wait_for(&scripts, Instant::now()), (10, 10));
        assert_eq!(state.0.lock().unwrap().1, 6);
    }

    #[test]
    fn responses_are_reused_while_fresh_and_revalidated_when_stale() {
        let dir = std::env::temp_dir().join(format!("render.http-cache-test-{}", std::process::id()));
        let cache = Arc::new(crate::cache::Cache::at(dir.clone(), 1 << 20));
        let (origin, requests) = serve(vec![
            b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 5\r\n\r\nfresh".to_vec(),
            b"HTTP/1.1 200 OK\r\nCache-Control: no-cache\r\nETag: \"v1\"\r\nContent-Length: 4\r\n\r\netag".to_vec(),
            b"HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nLast-Modified: Tue, 10 Nov 2026 08:00:00 GMT\r\nContent-Length: 2\r\n\r\nlm".to_vec(),
            b"HTTP/1.1 304 Not Modified\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nCache-Control: no-store, max-age=60\r\nContent-Length: 1\r\n\r\nx".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\ny".to_vec(),
            b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nVary: Referer\r\nContent-Length: 1\r\n\r\nz".to_vec(),
            b"HTTP/1.1 200 OK\r\nCache-Control: max-age=60\r\nContent-Length: 1\r\n\r\nw".to_vec(),
        ]);
        let mut client = Client::new(super::tls_config()).caching(Some(cache));
        let mut get = |path: &str| {
            let (_, response) = client.get(&Url::parse(&format!("{origin}{path}")).unwrap()).unwrap();
            (response.status, String::from_utf8(response.body).unwrap())
        };
        let bodies: Vec<_> = ["/a", "/a", "/b", "/b", "/c", "/c", "/d", "/d", "/e", "/e"].map(|p| get(p)).to_vec();
        let expected = ["fresh", "fresh", "etag", "etag", "lm", "lm", "x", "y", "z", "w"];
        assert_eq!(bodies, expected.map(|b| (200, b.to_string())).to_vec());
        drop(client);
        let sent: Vec<String> = requests.iter().take(9).collect();
        let lines: Vec<&str> = sent.iter().map(|r| r.lines().next().unwrap()).collect();
        let paths = ["/a", "/b", "/b", "/c", "/c", "/d", "/d", "/e", "/e"];
        assert_eq!(lines, paths.map(|p| format!("GET {p} HTTP/1.1")).to_vec());
        assert!(sent[2].contains("\r\nif-none-match: \"v1\"\r\n"), "{}", sent[2]);
        assert!(sent[4].contains("\r\nif-modified-since: Tue, 10 Nov 2026 08:00:00 GMT\r\n"), "{}", sent[4]);
        assert!(!sent[6].contains("\r\nif-") && !sent[8].contains("\r\nif-"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_post_is_sent_again_only_when_the_server_closed_the_connection() {
        let post = Request { method: Method::Post, ..Request::get("http://example.com/") };
        let ask = |r: &Request| Ask { method: r.method, ..Ask::get(Url::parse(&r.url).unwrap()) };
        let (post, get) = (ask(&post), ask(&Request::get("http://example.com/")));
        for kind in [std::io::ErrorKind::UnexpectedEof, std::io::ErrorKind::ConnectionReset, std::io::ErrorKind::BrokenPipe] {
            assert!(post.may_resend(&kind.into()) && get.may_resend(&kind.into()), "{kind:?}");
        }
        for kind in [std::io::ErrorKind::WouldBlock, std::io::ErrorKind::TimedOut, std::io::ErrorKind::InvalidData] {
            assert!(!post.may_resend(&kind.into()) && get.may_resend(&kind.into()), "{kind:?}");
        }
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
