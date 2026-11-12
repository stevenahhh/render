//! URLs as the URL Standard parses and resolves them, for the addresses
//! scripts see (`location`, `URL`, `a.href`) and the ones they load.
//! Hosts are lowercased but not converted to punycode.

/// A parsed absolute URL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Url {
    /// Lowercase, without the `:`.
    pub scheme: String,
    pub username: String,
    pub password: String,
    /// `None` for URLs without an authority, such as `mailto:` or `data:`.
    pub host: Option<String>,
    /// Absent when it is the scheme's default.
    pub port: Option<u16>,
    /// Starts with `/` for special schemes; opaque for others such as `data:`.
    pub path: String,
    /// Without the `?`.
    pub query: Option<String>,
    /// Without the `#`.
    pub fragment: Option<String>,
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

fn special(scheme: &str) -> bool {
    default_port(scheme).is_some() || scheme == "file"
}

/// Percent-encodes `s` onto `out`: bytes outside printable ASCII, and those in `set`.
fn encode(out: &mut String, s: &str, set: &[u8]) {
    for &b in s.as_bytes() {
        if b <= 0x20 || b >= 0x7f || set.contains(&b) {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(b as char);
        }
    }
}

const PATH_SET: &[u8] = b"\"#<>?`{}";
const USERINFO_SET: &[u8] = b"\"#<>?`{}/:;=@[\\]^|";
const FRAGMENT_SET: &[u8] = b"\"<>`";

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c))) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_host(s: &str, is_special: bool) -> Option<String> {
    if s.starts_with('[') {
        let inner = s.strip_prefix('[')?.strip_suffix(']')?;
        return inner.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.').then(|| format!("[{}]", inner.to_ascii_lowercase()));
    }
    if !is_special {
        let mut out = String::new();
        encode(&mut out, s, b"");
        return Some(out);
    }
    let host = percent_decode(s).to_lowercase();
    let forbidden = |c: char| c <= ' ' || "#%/:<>?@[\\]^|\u{7f}".contains(c);
    (!host.is_empty() && !host.contains(forbidden)).then_some(host)
}

/// Splits `rest` (what follows `//`) into authority and the remainder.
fn authority(rest: &str, is_special: bool) -> (&str, &str) {
    let end = rest.find(|c| c == '/' || c == '?' || c == '#' || (is_special && c == '\\')).unwrap_or(rest.len());
    rest.split_at(end)
}

impl Url {
    /// Parses `input`, resolving it against `base` when it is relative.
    pub fn parse(input: &str, base: Option<&Url>) -> Option<Url> {
        let trimmed = input.trim_matches(|c: char| c <= ' ');
        let cleaned: String;
        let input = if trimmed.contains(['\t', '\n', '\r']) {
            cleaned = trimmed.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
            &cleaned
        } else {
            trimmed
        };
        let scheme_end = input.find(':').filter(|&i| {
            let s = input.as_bytes();
            i > 0 && s[0].is_ascii_alphabetic() && s[1..i].iter().all(|&b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
        });
        let Some(i) = scheme_end else {
            return Url::relative(input, base?);
        };
        let scheme = input[..i].to_ascii_lowercase();
        let rest = &input[i + 1..];
        if scheme == "file" {
            // `file://host/path`, `file:///path` (no host) or `file:/path`.
            let mut url = Url { scheme, host: Some(String::new()), ..Url::default() };
            let tail = match rest.strip_prefix("//").or_else(|| rest.strip_prefix("\\\\")) {
                Some(after) => {
                    let end = after.find(['/', '\\', '?', '#']).unwrap_or(after.len());
                    let host = &after[..end];
                    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
                        url.host = Some(parse_host(host, true)?);
                    }
                    &after[end..]
                }
                None => rest,
            };
            let end = tail.find(['?', '#']).unwrap_or(tail.len());
            url.set_path(&tail[..end], None);
            url.query_fragment(&tail[end..]);
            return Some(url);
        }
        if special(&scheme) {
            let base = base.filter(|b| b.scheme == scheme);
            if !rest.starts_with(['/', '\\']) {
                if let Some(base) = base {
                    return Url::relative(rest, base);
                }
            }
            let mut url = Url { scheme, ..Url::default() };
            url.after_scheme(rest.trim_start_matches(['/', '\\']))?;
            return Some(url);
        }
        let mut url = Url { scheme, ..Url::default() };
        if let Some(after) = rest.strip_prefix("//") {
            url.after_scheme(after)?;
        } else {
            let end = rest.find(['?', '#']).unwrap_or(rest.len());
            if rest.starts_with('/') {
                url.set_path(&rest[..end], None);
            } else {
                encode(&mut url.path, &rest[..end], b"");
            }
            url.query_fragment(&rest[end..]);
        }
        Some(url)
    }

    /// Parses what follows `scheme://`: authority, path, query and fragment.
    fn after_scheme(&mut self, rest: &str) -> Option<()> {
        let is_special = special(&self.scheme);
        let (auth, tail) = authority(rest, is_special);
        let hostport = match auth.rsplit_once('@') {
            Some((info, hp)) => {
                let (user, pass) = info.split_once(':').unwrap_or((info, ""));
                encode(&mut self.username, user, USERINFO_SET);
                encode(&mut self.password, pass, USERINFO_SET);
                hp
            }
            None => auth,
        };
        let (host, port) = match hostport.rfind(':').filter(|&c| !hostport[c..].contains(']')) {
            Some(c) => (&hostport[..c], Some(&hostport[c + 1..])),
            None => (hostport, None),
        };
        if let Some(p) = port.filter(|p| !p.is_empty()) {
            let n: u16 = p.parse().ok().filter(|_| p.bytes().all(|b| b.is_ascii_digit()))?;
            self.port = (default_port(&self.scheme) != Some(n)).then_some(n);
        }
        self.host = match (host.is_empty(), self.scheme.as_str()) {
            (true, "file") => Some(String::new()),
            (true, _) if is_special => return None,
            (true, _) => Some(String::new()),
            _ => Some(parse_host(host, is_special)?),
        };
        let end = tail.find(['?', '#']).unwrap_or(tail.len());
        self.set_path(&tail[..end], None);
        self.query_fragment(&tail[end..]);
        Some(())
    }

    /// Reads `?query#fragment` (either part optional) from the start of `s`.
    fn query_fragment(&mut self, s: &str) {
        let (q, f) = match s.find('#') {
            Some(h) => (&s[..h], Some(&s[h + 1..])),
            None => (s, None),
        };
        self.query = q.strip_prefix('?').map(|q| {
            let mut out = String::new();
            let set: &[u8] = if special(&self.scheme) { b"\"#<>'" } else { b"\"#<>" };
            encode(&mut out, q, set);
            out
        });
        self.fragment = f.map(|f| {
            let mut out = String::new();
            encode(&mut out, f, FRAGMENT_SET);
            out
        });
    }

    /// Sets the path from `input`, relative to `dir` (a path ending in `/`) when given,
    /// removing `.` and `..` segments.
    fn set_path(&mut self, input: &str, dir: Option<&str>) {
        let is_special = special(&self.scheme);
        let mut segments: Vec<String> = Vec::new();
        let mut push = |raw: &str, last: bool| match raw.to_ascii_lowercase().as_str() {
            "." | "%2e" => {
                if last {
                    segments.push(String::new());
                }
            }
            ".." | ".%2e" | "%2e." | "%2e%2e" => {
                segments.pop();
                if last {
                    segments.push(String::new());
                }
            }
            _ => {
                let mut s = String::new();
                encode(&mut s, raw, PATH_SET);
                segments.push(s);
            }
        };
        let joined;
        let full = match dir {
            Some(d) => {
                joined = format!("{d}{input}");
                &joined
            }
            None => input,
        };
        let full = full.strip_prefix(['/', '\\']).filter(|_| is_special || full.starts_with('/')).unwrap_or(full);
        let parts: Vec<&str> = if is_special { full.split(['/', '\\']).collect() } else { full.split('/').collect() };
        let n = parts.len();
        for (i, p) in parts.into_iter().enumerate() {
            push(p, i + 1 == n);
        }
        self.path = if segments.is_empty() && !is_special && input.is_empty() { String::new() } else { format!("/{}", segments.join("/")) };
    }

    /// Resolves an input without a scheme against `base`.
    fn relative(input: &str, base: &Url) -> Option<Url> {
        let is_special = special(&base.scheme);
        if base.host.is_none() && !base.path.starts_with('/') {
            // A base with an opaque path, like `data:` or `about:blank`, only takes a fragment.
            let mut url = base.clone();
            url.fragment = Some(input.strip_prefix('#')?.to_string());
            return Some(url);
        }
        let mut url = Url { scheme: base.scheme.clone(), ..Url::default() };
        let slash = |s: &str| s.starts_with('/') || (is_special && s.starts_with('\\'));
        if slash(input) && slash(&input[1..]) {
            url.after_scheme(&input[2..])?;
            return Some(url);
        }
        url.username.clone_from(&base.username);
        url.password.clone_from(&base.password);
        url.host.clone_from(&base.host);
        url.port = base.port;
        let end = input.find(['?', '#']).unwrap_or(input.len());
        let (path, tail) = input.split_at(end);
        if path.is_empty() {
            url.path.clone_from(&base.path);
            url.query.clone_from(&base.query);
            url.query_fragment(tail);
            if !tail.starts_with('?') {
                url.query.clone_from(&base.query);
            }
            return Some(url);
        }
        if slash(path) {
            url.set_path(path, None);
        } else {
            let dir = &base.path[..base.path.rfind('/').map_or(0, |i| i + 1)];
            url.set_path(path, Some(dir));
        }
        url.query_fragment(tail);
        Some(url)
    }

    pub fn host_port(&self) -> String {
        let host = self.host.as_deref().unwrap_or("");
        match self.port {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        }
    }

    /// `scheme://host:port` for schemes that have origins, else `null`.
    pub fn origin(&self) -> String {
        match self.scheme.as_str() {
            "blob" => Url::parse(&self.path, None).filter(|u| u.scheme.starts_with("http")).map_or("null".to_string(), |u| u.origin()),
            s if default_port(s).is_some() => format!("{s}://{}", self.host_port()),
            _ => "null".to_string(),
        }
    }

    pub fn href(&self) -> String {
        let mut out = format!("{}:", self.scheme);
        if let Some(host) = &self.host {
            out.push_str("//");
            if !self.username.is_empty() || !self.password.is_empty() {
                out.push_str(&self.username);
                if !self.password.is_empty() {
                    out.push(':');
                    out.push_str(&self.password);
                }
                out.push('@');
            }
            out.push_str(host);
            if let Some(p) = self.port {
                out.push_str(&format!(":{p}"));
            }
        }
        out.push_str(&self.path);
        if let Some(q) = &self.query {
            out.push('?');
            out.push_str(q);
        }
        if let Some(f) = &self.fragment {
            out.push('#');
            out.push_str(f);
        }
        out
    }

    /// The `file:` URL of a local path.
    pub fn from_path(path: &std::path::Path) -> Url {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut url = Url { scheme: "file".into(), host: Some(String::new()), ..Url::default() };
        url.set_path(&abs.to_string_lossy(), None);
        url
    }

    /// The local path of a `file:` URL.
    pub fn to_path(&self) -> Option<std::path::PathBuf> {
        (self.scheme == "file").then(|| percent_decode(&self.path).into())
    }
}

#[cfg(test)]
mod tests {
    use super::Url;

    fn resolve(input: &str, base: &str) -> Option<String> {
        Url::parse(input, Some(&Url::parse(base, None).unwrap())).map(|u| u.href())
    }

    #[test]
    fn parses_like_a_browser() {
        let cases = [
            ("https://Example.COM:443/a/./b/../c?x=1 2#frag ment", "https://example.com/a/c?x=1%202#frag%20ment"),
            ("http://user:pa ss@host:8080/p", "http://user:pa%20ss@host:8080/p"),
            ("HTTP://host", "http://host/"),
            ("http:\\\\host\\a\\b", "http://host/a/b"),
            ("file:///Users/a b/x.html", "file:///Users/a%20b/x.html"),
            ("mailto:Someone@Example.com", "mailto:Someone@Example.com"),
            ("data:text/plain,hi there", "data:text/plain,hi%20there"),
            ("about:blank", "about:blank"),
            ("blob:https://a.test/uuid", "blob:https://a.test/uuid"),
            ("foo://Host/Path", "foo://Host/Path"),
        ];
        for (input, expected) in cases {
            assert_eq!(Url::parse(input, None).map(|u| u.href()).as_deref(), Some(expected), "{input}");
        }
        for bad in ["http://bad host/", "http://", "https://a:99999/", "noscheme", "http://a<b/"] {
            assert!(Url::parse(bad, None).is_none(), "{bad}");
        }
    }

    #[test]
    fn resolves_relative_references() {
        let base = "https://example.com/a/b/page.html?x=1#top";
        let cases = [
            ("style.css", "https://example.com/a/b/style.css"),
            ("../img/a b.png", "https://example.com/a/img/a%20b.png"),
            ("/root.js?v=2", "https://example.com/root.js?v=2"),
            ("//cdn.example.org/lib.js", "https://cdn.example.org/lib.js"),
            ("?y=2", "https://example.com/a/b/page.html?y=2"),
            ("#frag", "https://example.com/a/b/page.html?x=1#frag"),
            ("", "https://example.com/a/b/page.html?x=1"),
            ("./", "https://example.com/a/b/"),
            ("../../../up", "https://example.com/up"),
            ("https:other", "https://example.com/a/b/other"),
            ("http:other", "http://other/"),
        ];
        for (input, expected) in cases {
            assert_eq!(resolve(input, base).as_deref(), Some(expected), "{input}");
        }
        assert_eq!(resolve("pushed.html", "file:///Users/x/webapp/page.html").as_deref(), Some("file:///Users/x/webapp/pushed.html"));
        assert_eq!(resolve("#x", "about:blank").as_deref(), Some("about:blank#x"));
        assert!(resolve("x", "about:blank").is_none());
    }

    #[test]
    fn exposes_origin_and_host() {
        let url = Url::parse("https://a.test:8443/p?q", None).unwrap();
        assert_eq!((url.origin(), url.host_port()), ("https://a.test:8443".to_string(), "a.test:8443".to_string()));
        assert_eq!(Url::parse("file:///x", None).unwrap().origin(), "null");
        assert_eq!(Url::parse("blob:https://a.test/u", None).unwrap().origin(), "https://a.test");
        let file = Url::parse("file:///tmp/a%20b.html", None).unwrap();
        assert_eq!(file.to_path(), Some(std::path::PathBuf::from("/tmp/a b.html")));
    }
}
