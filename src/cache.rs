//! An HTTP cache on disk for the answers to plain GET requests, kept between runs.
//!
//! A stored response is reused without asking the server while its
//! `Cache-Control: max-age` lasts. Once stale, or with `no-cache`, it is
//! revalidated with `If-None-Match` or `If-Modified-Since`, and a `304` answer
//! reuses the stored body. `no-store` responses are not kept, nor those that set
//! cookies, which only an answer from the network gives the jar, nor those that
//! vary on `Cookie`, `Referer` or everything (`*`): the plain GETs this cache
//! serves send every other header the same each time, or not at all, so a vary
//! on those always matches.
//! Past the size cap the least recently used entries are removed.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Most bytes the shared cache keeps on disk.
const CAPACITY: u64 = 256 << 20;

/// The first line of every entry, naming its format.
const MAGIC: &str = "render-cache 1";

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Turns the shared cache off for this process: nothing is read from it or written to it.
pub fn disable() {
    ENABLED.store(false, Ordering::Relaxed);
}

/// The cache every page from the web shares, in `render.http-cache` beside the
/// download directories in the system's temporary directory. `None` when
/// turned off, and in unit tests, which must not depend on earlier runs.
pub(crate) fn shared() -> Option<Arc<Cache>> {
    static SHARED: OnceLock<Arc<Cache>> = OnceLock::new();
    if cfg!(test) || !ENABLED.load(Ordering::Relaxed) {
        return None;
    }
    let make = || Arc::new(Cache::at(std::env::temp_dir().join("render.http-cache"), CAPACITY));
    Some(SHARED.get_or_init(make).clone())
}

/// A response as it arrived, to be stored.
pub(crate) struct Answer<'a> {
    pub status: u16,
    pub reason: &'a str,
    /// Names in lower case.
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
}

/// A response read back from the cache.
pub(crate) struct Stored {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// When it was stored or last revalidated.
    at: SystemTime,
}

/// What `Cache-Control` says about a response.
#[derive(Default)]
struct Control {
    no_store: bool,
    no_cache: bool,
    max_age: Option<u64>,
}

fn control(headers: &[(String, String)]) -> Control {
    let mut c = Control::default();
    for (_, value) in headers.iter().filter(|h| h.0 == "cache-control") {
        for directive in value.split(',') {
            let (name, arg) = directive.split_once('=').unwrap_or((directive, ""));
            match name.trim().to_ascii_lowercase().as_str() {
                "no-store" => c.no_store = true,
                "no-cache" => c.no_cache = true,
                "max-age" => c.max_age = arg.trim().trim_matches('"').parse().ok(),
                _ => {}
            }
        }
    }
    c
}

fn header<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers.iter().find(|h| h.0 == name).map(|h| h.1.as_str())
}

/// Whether a response may be stored and is worth it: reusable for a while, or
/// with a validator to revalidate it by.
fn storable(answer: &Answer) -> bool {
    let c = control(answer.headers);
    let varies = |name: &str| answer.headers.iter().filter(|h| h.0 == "vary").any(|h| h.1.split(',').any(|v| v.trim().eq_ignore_ascii_case(name)));
    let vary_ok = !["*", "cookie", "referer"].iter().any(|name| varies(name));
    let sets_cookie = header(answer.headers, "set-cookie").is_some();
    let validated = header(answer.headers, "etag").is_some() || header(answer.headers, "last-modified").is_some();
    answer.status == 200 && !c.no_store && vary_ok && !sets_cookie && (c.max_age.is_some_and(|age| age > 0) || validated)
}

impl Stored {
    /// True while it may be used without asking the server.
    pub fn fresh(&self, now: SystemTime) -> bool {
        let c = control(&self.headers);
        let before = header(&self.headers, "age").and_then(|a| a.trim().parse().ok()).unwrap_or(0);
        let age = Duration::from_secs(before) + now.duration_since(self.at).unwrap_or_default();
        !c.no_cache && c.max_age.is_some_and(|max| age < Duration::from_secs(max))
    }

    /// The request headers asking the server whether this copy is still current.
    pub fn validators(&self) -> Vec<(String, String)> {
        let etag = header(&self.headers, "etag").map(|v| ("if-none-match".to_string(), v.to_string()));
        let modified = header(&self.headers, "last-modified").map(|v| ("if-modified-since".to_string(), v.to_string()));
        etag.into_iter().chain(modified).collect()
    }
}

/// Entries in one directory, one file per URL.
pub struct Cache {
    dir: PathBuf,
    capacity: u64,
    /// Bytes on disk, counted at the first write.
    used: Mutex<Option<u64>>,
}

impl Cache {
    pub fn at(dir: PathBuf, capacity: u64) -> Cache {
        Cache { dir, capacity, used: Mutex::new(None) }
    }

    /// The file of `url`, named by its FNV-1a hash; the file names the URL again.
    fn path(&self, url: &str) -> PathBuf {
        let hash = url.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        self.dir.join(format!("{hash:016x}"))
    }

    /// The stored response for `url`, marked as just used.
    pub(crate) fn get(&self, url: &str) -> Option<Stored> {
        let path = self.path(url);
        let bytes = fs::read(&path).ok()?;
        let end = bytes.windows(2).position(|w| w == b"\n\n")?;
        let head = std::str::from_utf8(&bytes[..end]).ok()?;
        let mut lines = head.split('\n');
        if lines.next()? != MAGIC || lines.next()? != url {
            return None;
        }
        let at = UNIX_EPOCH + Duration::from_millis(lines.next()?.parse().ok()?);
        let (status, reason) = lines.next()?.split_once(' ')?;
        let headers = lines.filter_map(|l| l.split_once(": ")).map(|(n, v)| (n.to_string(), v.to_string())).collect();
        // The modification time orders entries for eviction; failing to set it only
        // makes this one an earlier candidate.
        let _ = File::options().append(true).open(&path).and_then(|f| f.set_modified(SystemTime::now()));
        let body = bytes[end + 2..].to_vec();
        Some(Stored { status: status.parse().ok()?, reason: reason.to_string(), headers, body, at })
    }

    /// Stores the answer for `url` when it may be stored; otherwise forgets any
    /// earlier answer, which this one replaces.
    pub(crate) fn put(&self, url: &str, answer: &Answer) {
        if !storable(answer) {
            let _ = fs::remove_file(self.path(url));
            return;
        }
        if let Err(e) = self.write(url, answer, SystemTime::now()) {
            eprintln!("[net] cache: {}: {e}", self.dir.display());
        }
    }

    /// Takes the headers of a `304` answer into a stored response, which is
    /// current again from now, and stores it.
    pub(crate) fn refresh(&self, url: &str, mut stored: Stored, update: &[(String, String)]) -> Stored {
        const KEEP: [&str; 4] = ["content-length", "content-encoding", "transfer-encoding", "connection"];
        for (name, value) in update.iter().filter(|h| !KEEP.contains(&h.0.as_str())) {
            stored.headers.retain(|h| h.0 != *name);
            stored.headers.push((name.clone(), value.clone()));
        }
        stored.at = SystemTime::now();
        let answer = Answer { status: stored.status, reason: &stored.reason, headers: &stored.headers, body: &stored.body };
        if let Err(e) = self.write(url, &answer, stored.at) {
            eprintln!("[net] cache: {}: {e}", self.dir.display());
        }
        stored
    }

    /// Writes an entry whole under a temporary name, then renames it into place,
    /// so a reader never sees half of one.
    fn write(&self, url: &str, answer: &Answer, at: SystemTime) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let millis = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        let mut head = format!("{MAGIC}\n{url}\n{millis}\n{} {}\n", answer.status, answer.reason);
        for (name, value) in answer.headers {
            head.push_str(&format!("{name}: {value}\n"));
        }
        head.push('\n');
        let path = self.path(url);
        let old = fs::metadata(&path).map_or(0, |m| m.len());
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut file = File::create(&temporary)?;
        file.write_all(head.as_bytes())?;
        file.write_all(answer.body)?;
        drop(file);
        fs::rename(&temporary, &path)?;
        let size = (head.len() + answer.body.len()) as u64;
        self.account(size, old)
    }

    /// Counts an entry of `added` bytes that replaced one of `removed`, and removes
    /// the least recently used entries once the total passes the capacity, down
    /// to nine tenths of it.
    fn account(&self, added: u64, removed: u64) -> io::Result<()> {
        let mut used = self.used.lock().expect("cache size");
        let total = match *used {
            Some(n) => (n + added).saturating_sub(removed),
            None => fs::read_dir(&self.dir)?.filter_map(|e| e.ok()?.metadata().ok()).map(|m| m.len()).sum(),
        };
        *used = Some(total);
        if total <= self.capacity {
            return Ok(());
        }
        let mut entries: Vec<_> = fs::read_dir(&self.dir)?
            .filter_map(|e| {
                let e = e.ok()?;
                let m = e.metadata().ok()?;
                Some((m.modified().ok()?, m.len(), e.path()))
            })
            .collect();
        entries.sort();
        let mut left = total;
        for (_, len, path) in entries {
            if left <= self.capacity / 10 * 9 {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                left -= len;
            }
        }
        *used = Some(left);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Answer, Cache};

    fn headers(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect()
    }

    #[test]
    fn only_reusable_or_revalidatable_answers_are_kept() {
        let dir = std::env::temp_dir().join(format!("render.http-cache-unit-{}", std::process::id()));
        let cache = Cache::at(dir.clone(), 1 << 20);
        // (status, headers, kept, fresh)
        let cases: [(u16, &[(&str, &str)], bool, bool); 13] = [
            (200, &[("cache-control", "public, max-age=600")], true, true),
            (200, &[("cache-control", "max-age=600"), ("age", "700")], true, false),
            (200, &[("cache-control", "max-age=0"), ("etag", "\"e\"")], true, false),
            (200, &[("cache-control", "no-cache, max-age=600")], true, false),
            (200, &[("cache-control", "max-age=600, no-store")], false, false),
            (200, &[("cache-control", "max-age=600"), ("vary", "Accept-Encoding")], true, true),
            (200, &[("cache-control", "max-age=600"), ("vary", "Accept-Encoding, Origin")], true, true),
            (200, &[("cache-control", "max-age=600"), ("vary", "accept-encoding, Referer")], false, false),
            (200, &[("cache-control", "max-age=600"), ("vary", "*")], false, false),
            (200, &[("cache-control", "max-age=600"), ("vary", "Cookie")], false, false),
            (200, &[("cache-control", "max-age=600"), ("set-cookie", "id=1")], false, false),
            (200, &[("date", "Tue, 10 Nov 2026 08:00:00 GMT")], false, false),
            (404, &[("cache-control", "max-age=600")], false, false),
        ];
        for (i, (status, list, kept, fresh)) in cases.into_iter().enumerate() {
            let url = format!("https://example.com/{i}");
            let h = headers(list);
            cache.put(&url, &Answer { status, reason: "OK", headers: &h, body: b"body" });
            let stored = cache.get(&url);
            assert_eq!(stored.is_some(), kept, "case {i}");
            assert_eq!(stored.as_ref().is_some_and(|s| s.fresh(std::time::SystemTime::now())), fresh, "case {i}");
            assert!(stored.is_none_or(|s| s.body == b"body"), "case {i}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_least_recently_used_entries_go_past_the_capacity() {
        let dir = std::env::temp_dir().join(format!("render.http-cache-cap-{}", std::process::id()));
        // Each entry takes some 970 bytes: three fit, a fourth sends one away.
        let cache = Cache::at(dir.clone(), 3500);
        let h = headers(&[("cache-control", "max-age=600")]);
        let body = vec![b'x'; 900];
        let at = |i: u64| std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + i);
        for i in 0..3 {
            let url = format!("https://example.com/{i}");
            cache.put(&url, &Answer { status: 200, reason: "OK", headers: &h, body: &body });
            std::fs::File::options().append(true).open(cache.path(&url)).unwrap().set_modified(at(i)).unwrap();
        }
        // Using the first entry makes the second the least recently used.
        assert!(cache.get("https://example.com/0").is_some());
        cache.put("https://example.com/3", &Answer { status: 200, reason: "OK", headers: &h, body: &body });
        let kept: Vec<bool> = (0..4).map(|i| cache.get(&format!("https://example.com/{i}")).is_some()).collect();
        assert_eq!(kept, [true, false, true, true]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
