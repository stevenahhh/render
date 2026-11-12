//! The cookie jar of a run (RFC 6265): filled from `Set-Cookie` response
//! headers and `document.cookie`, read for the `Cookie` header of requests and
//! by `document.cookie`. It lives in memory only, shared by every page and
//! request of the process.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where a cookie is set or read: the parts of a URL the rules look at.
#[derive(Clone, Copy)]
pub struct Origin<'a> {
    pub https: bool,
    /// Lower case; empty for a `file:` URL.
    pub host: &'a str,
    /// The path, query left out.
    pub path: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameSite {
    Unset,
    Strict,
    Lax,
    None,
}

#[derive(Debug)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    /// Sent to `domain` only, not to its subdomains: the cookie named no `Domain`.
    host_only: bool,
    path: String,
    /// Seconds since the epoch; `None` for a session cookie.
    expires: Option<i64>,
    secure: bool,
    http_only: bool,
    same_site: SameSite,
}

#[derive(Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
}

static JAR: Mutex<Jar> = Mutex::new(Jar { cookies: Vec::new() });

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Stores a `Set-Cookie` header value, or with `script` a `document.cookie` assignment.
pub fn store(origin: Origin, line: &str, script: bool) {
    JAR.lock().expect("cookie jar").set(origin, line, script, now());
}

/// The `Cookie` header value for a request, or with `script` what `document.cookie` reads.
pub fn header(origin: Origin, script: bool) -> String {
    JAR.lock().expect("cookie jar").get(origin, script, now())
}

/// The directory of a request path, the default path of the cookies it sets.
fn default_path(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) if path.starts_with('/') => &path[..i],
        Some(_) => "/",
    }
}

fn domain_match(host: &str, domain: &str) -> bool {
    host == domain || (host.ends_with(domain) && host[..host.len() - domain.len()].ends_with('.') && host.parse::<std::net::IpAddr>().is_err())
}

fn path_match(path: &str, cookie: &str) -> bool {
    path == cookie || (path.starts_with(cookie) && (cookie.ends_with('/') || path[cookie.len()..].starts_with('/')))
}

impl Jar {
    fn set(&mut self, origin: Origin, line: &str, script: bool, now: i64) {
        let mut parts = line.split(';');
        let pair = parts.next().unwrap_or("");
        let (name, value) = match pair.split_once('=') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => ("", pair.trim()),
        };
        if name.is_empty() && value.is_empty() {
            return;
        }
        let mut cookie = Cookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: origin.host.to_string(),
            host_only: true,
            path: default_path(origin.path).to_string(),
            expires: None,
            secure: false,
            http_only: false,
            same_site: SameSite::Unset,
        };
        let mut max_age = None;
        for attr in parts {
            let (key, val) = attr.split_once('=').map_or((attr.trim(), ""), |(k, v)| (k.trim(), v.trim()));
            match key.to_ascii_lowercase().as_str() {
                "expires" => cookie.expires = cookie.expires.or(parse_date(val)),
                "max-age" => {
                    if let Ok(secs) = val.parse::<i64>() {
                        max_age = Some(if secs <= 0 { i64::MIN } else { now.saturating_add(secs) });
                    }
                }
                "domain" if !val.is_empty() => {
                    let domain = val.trim_start_matches('.').to_ascii_lowercase();
                    // A domain must cover the host, and name more than a top-level domain.
                    if !domain_match(origin.host, &domain) || !domain.contains('.') {
                        return;
                    }
                    cookie.domain = domain;
                    cookie.host_only = false;
                }
                "path" if val.starts_with('/') => cookie.path = val.to_string(),
                "secure" => cookie.secure = true,
                "httponly" => cookie.http_only = true,
                "samesite" => {
                    cookie.same_site = match val.to_ascii_lowercase().as_str() {
                        "strict" => SameSite::Strict,
                        "lax" => SameSite::Lax,
                        "none" => SameSite::None,
                        _ => SameSite::Unset,
                    }
                }
                _ => {}
            }
        }
        if max_age.is_some() {
            cookie.expires = max_age;
        }
        if (cookie.secure && !origin.https) || (script && cookie.http_only) {
            return;
        }
        let old = self.cookies.iter().position(|c| c.name == cookie.name && c.domain == cookie.domain && c.host_only == cookie.host_only && c.path == cookie.path);
        if let Some(i) = old {
            if script && self.cookies[i].http_only {
                return;
            }
            self.cookies.remove(i);
        }
        if cookie.expires.is_some_and(|e| e <= now) {
            return;
        }
        self.cookies.push(cookie);
    }

    fn get(&mut self, origin: Origin, script: bool, now: i64) -> String {
        self.cookies.retain(|c| c.expires.is_none_or(|e| e > now));
        let mut found: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| if c.host_only { c.domain == origin.host } else { domain_match(origin.host, &c.domain) })
            .filter(|c| path_match(origin.path, &c.path) && (!c.secure || origin.https) && !(script && c.http_only))
            .collect();
        // Longer paths first, as RFC 6265 asks; then by name, as WebKit orders them.
        found.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then_with(|| a.name.cmp(&b.name)));
        let mut out = String::new();
        for c in found {
            if !out.is_empty() {
                out.push_str("; ");
            }
            if !c.name.is_empty() {
                out.push_str(&c.name);
                out.push('=');
            }
            out.push_str(&c.value);
        }
        out
    }
}

/// Parses a cookie date (RFC 6265 section 5.1.1) into seconds since the epoch.
fn parse_date(s: &str) -> Option<i64> {
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for token in s.split(|c: char| !(c.is_ascii_alphanumeric() || c == ':')).filter(|t| !t.is_empty()) {
        let digits = token.bytes().take_while(u8::is_ascii_digit).count();
        if time.is_none() && token.contains(':') {
            let mut f = token.split(':').map(|p| p.parse::<i64>().ok());
            if let (Some(Some(h)), Some(Some(m)), Some(Some(sec))) = (f.next(), f.next(), f.next()) {
                time = Some(h * 3600 + m * 60 + sec);
                continue;
            }
        }
        if day.is_none() && (1..=2).contains(&digits) && digits == token.len() {
            day = token.parse::<i64>().ok();
        } else if month.is_none() && token.len() >= 3 && !token.as_bytes()[0].is_ascii_digit() {
            const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
            month = MONTHS.iter().position(|m| token[..3].eq_ignore_ascii_case(m)).map(|i| i as i64 + 1);
        } else if year.is_none() && (2..=4).contains(&digits) {
            year = token[..digits].parse::<i64>().ok().map(|y| match y {
                70..=99 => y + 1900,
                0..=69 => y + 2000,
                y => y,
            });
        }
    }
    let (time, day, month, year) = (time?, day?, month?, year?);
    if !(1..=31).contains(&day) || year < 1601 || time >= 24 * 3600 {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + time)
}

#[cfg(test)]
mod tests {
    use super::{parse_date, Jar, Origin};

    const SITE: Origin = Origin { https: true, host: "www.example.com", path: "/shop/cart" };

    #[test]
    fn dates_parse_in_the_forms_servers_send() {
        assert_eq!(parse_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
        assert_eq!(parse_date("Wednesday, 21-Oct-15 07:28:00 GMT"), Some(1_445_412_480));
        assert_eq!(parse_date("Wed Oct 21 07:28:00 2015"), Some(1_445_412_480));
        assert_eq!(parse_date("not a date"), None);
    }

    #[test]
    fn cookies_follow_domain_path_expiry_and_flags() {
        let mut jar = Jar::default();
        let set = |jar: &mut Jar, at: Origin, line: &str, script: bool| jar.set(at, line, script, 1000);
        set(&mut jar, SITE, "a=1", false);
        set(&mut jar, SITE, "root=r; Path=/", false);
        set(&mut jar, SITE, "wide=w; Domain=.example.com", false);
        set(&mut jar, SITE, "other=o; Domain=example.org", false);
        set(&mut jar, SITE, "tld=t; Domain=com", false);
        set(&mut jar, SITE, "s=1; Secure; SameSite=Lax", false);
        set(&mut jar, SITE, "h=1; HttpOnly", false);
        set(&mut jar, SITE, "gone=1; Max-Age=0", false);
        set(&mut jar, SITE, "old=1; Expires=Thu, 01 Jan 1970 00:00:00 GMT", false);
        set(&mut jar, SITE, "soon=1; Max-Age=10", false);
        assert_eq!(jar.get(SITE, false, 1000), "a=1; h=1; s=1; soon=1; wide=w; root=r", "longer paths first, then by name");
        assert_eq!(jar.get(SITE, true, 1000), "a=1; s=1; soon=1; wide=w; root=r", "HttpOnly is hidden from scripts");
        assert_eq!(jar.get(SITE, false, 1011), "a=1; h=1; s=1; wide=w; root=r");
        let plain = Origin { https: false, host: "api.example.com", path: "/shop" };
        assert_eq!(jar.get(plain, false, 1000), "wide=w", "host-only, path and Secure cookies stay behind");
        set(&mut jar, SITE, "h=script; Path=/shop", true);
        set(&mut jar, SITE, "a=2", true);
        set(&mut jar, plain, "x=1; Secure", false);
        assert_eq!(jar.get(SITE, false, 1000), "a=2; h=1; s=1; wide=w; root=r", "a script cannot replace an HttpOnly cookie");
        assert_eq!(jar.cookies.iter().find(|c| c.name == "s").map(|c| c.same_site), Some(super::SameSite::Lax));
    }
}
