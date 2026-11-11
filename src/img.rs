//! Image loading: JPEG/PNG files and SVG (files or inline markup).
//!
//! Layout only needs intrinsic sizes, which come from file headers. Pixels
//! are decoded when an image is first painted and cached at the size drawn.

use crate::FxMap;
use resvg::{tiny_skia, usvg};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Premultiplied 0xAARRGGBB pixels.
pub struct Picture {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

enum Source {
    File(PathBuf),
    Svg(usvg::Tree),
}

struct Entry {
    source: Source,
    w: f32,
    h: f32,
    drawn: Option<Picture>,
    failed: bool,
}

#[derive(Default)]
pub struct Images {
    base: Option<PathBuf>,
    by_key: FxMap<Box<str>, Option<u32>>,
    list: Vec<Entry>,
    /// Set by `decode_in_background`: where worker threads send finished pictures,
    /// and the call that tells the owner one has arrived.
    background: Option<(Sender<Decoded>, Receiver<Decoded>, Wake)>,
    /// Images a worker thread is decoding right now.
    pending: Vec<u32>,
}

type Decoded = (u32, Option<Picture>);
type Wake = Arc<dyn Fn() + Send + Sync>;

/// Most worker threads decoding at once; further images wait for the next frame.
const MAX_DECODERS: usize = 8;

fn parse_svg(markup: &str) -> Option<usvg::Tree> {
    usvg::Tree::from_str(markup, &usvg::Options::default()).ok()
}

impl Images {
    pub fn new(base: Option<PathBuf>) -> Images {
        Images { base, ..Images::default() }
    }

    fn add(&mut self, key: &str, source: Option<(Source, f32, f32)>) -> Option<u32> {
        let id = source.map(|(source, w, h)| {
            self.list.push(Entry { source, w, h, drawn: None, failed: false });
            self.list.len() as u32 - 1
        });
        self.by_key.insert(key.into(), id);
        id
    }

    /// Resolves an image URL relative to the page. Only local files load;
    /// a root-relative URL is looked up below the page's directory.
    pub fn file(&mut self, src: &str) -> Option<u32> {
        if let Some(&id) = self.by_key.get(src) {
            return id;
        }
        let local = !src.is_empty() && !src.starts_with("data:") && !src.contains("://");
        let file = src.split(['?', '#']).next().unwrap_or(src).trim_start_matches('/');
        let path = self.base.as_ref().filter(|_| local).map(|b| b.join(file));
        let source = path.and_then(|path| {
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
                let tree = parse_svg(&std::fs::read_to_string(&path).ok()?)?;
                let size = tree.size();
                Some((Source::Svg(tree), size.width(), size.height()))
            } else {
                let (w, h) = image::image_dimensions(&path).ok()?;
                Some((Source::File(path), w as f32, h as f32))
            }
        });
        self.add(src, source)
    }

    /// Registers inline SVG markup.
    pub fn svg(&mut self, markup: &str) -> Option<u32> {
        if let Some(&id) = self.by_key.get(markup) {
            return id;
        }
        let source = parse_svg(markup).map(|tree| {
            let size = tree.size();
            (Source::Svg(tree), size.width(), size.height())
        });
        self.add(markup, source)
    }

    /// Intrinsic size in CSS pixels.
    pub fn size(&self, id: u32) -> (f32, f32) {
        let e = &self.list[id as usize];
        (e.w, e.h)
    }

    /// From now on file images are decoded on worker threads: `get` returns `None`
    /// until a picture is ready, `wake` is called from the worker when one is, and
    /// `poll` then takes it in. This keeps decoding out of the time to a frame.
    pub fn decode_in_background(&mut self, wake: Wake) {
        let (tx, rx) = channel();
        self.background = Some((tx, rx, wake));
    }

    /// Takes in the pictures worker threads have finished. True when any arrived.
    pub fn poll(&mut self) -> bool {
        let Some((_, rx, _)) = &self.background else { return false };
        let mut any = false;
        while let Ok((id, picture)) = rx.try_recv() {
            self.pending.retain(|&p| p != id);
            let e = &mut self.list[id as usize];
            e.failed = picture.is_none();
            e.drawn = picture;
            any = true;
        }
        any
    }

    /// Decodes the listed `(id, w, h)` file images that are not cached at that size,
    /// on several threads at once. `get` then finds them ready; the pixels are the
    /// ones it would have produced itself.
    pub fn prepare(&mut self, wants: &[(u32, u32, u32)]) {
        if self.background.is_some() {
            return;
        }
        let mut jobs: Vec<(u32, PathBuf, u32, u32)> = Vec::new();
        for &(id, w, h) in wants {
            let Some(e) = self.list.get(id as usize) else { continue };
            let cached = e.drawn.as_ref().is_some_and(|p| p.w == w && p.h == h);
            if e.failed || cached || w == 0 || h == 0 || w > 16384 || h > 16384 || jobs.iter().any(|j| j.0 == id) {
                continue;
            }
            if let Source::File(path) = &e.source {
                jobs.push((id, path.clone(), w, h));
            }
        }
        if jobs.len() < 2 {
            return;
        }
        let threads = std::thread::available_parallelism().map_or(4, usize::from).min(jobs.len());
        let next = std::sync::atomic::AtomicUsize::new(0);
        let done = std::sync::Mutex::new(Vec::with_capacity(jobs.len()));
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    while let Some((id, path, w, h)) = jobs.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                        let picture = decode(path, *w, *h);
                        done.lock().expect("decode results").push((*id, picture));
                    }
                });
            }
        });
        for (id, picture) in done.into_inner().expect("decode results") {
            let e = &mut self.list[id as usize];
            e.failed = picture.is_none();
            e.drawn = picture;
        }
    }

    /// Pixels scaled to exactly `w` x `h` device pixels.
    pub fn get(&mut self, id: u32, w: u32, h: u32) -> Option<&Picture> {
        let e = self.list.get_mut(id as usize)?;
        if e.failed || w == 0 || h == 0 || w > 16384 || h > 16384 {
            return None;
        }
        if !e.drawn.as_ref().is_some_and(|p| p.w == w && p.h == h) {
            if let (Some((tx, _, wake)), Source::File(path)) = (&self.background, &e.source) {
                if !self.pending.contains(&id) && self.pending.len() < MAX_DECODERS {
                    self.pending.push(id);
                    let (tx, wake, path) = (tx.clone(), wake.clone(), path.clone());
                    std::thread::spawn(move || {
                        // The receiver is gone when the page was replaced meanwhile.
                        if tx.send((id, decode(&path, w, h))).is_ok() {
                            wake();
                        }
                    });
                }
                return None;
            }
            e.drawn = match &e.source {
                Source::File(path) => decode(path, w, h),
                Source::Svg(tree) => rasterize(tree, w, h),
            };
            e.failed = e.drawn.is_none();
        }
        e.drawn.as_ref()
    }
}

fn premultiply(rgba: &[u8]) -> u32 {
    let a = rgba[3] as u32;
    let m = |c: u8| (c as u32 * a + 127) / 255;
    a << 24 | m(rgba[0]) << 16 | m(rgba[1]) << 8 | m(rgba[2])
}

fn decode(path: &std::path::Path, w: u32, h: u32) -> Option<Picture> {
    let img = image::open(path).ok()?.into_rgba8();
    let img = if (img.width(), img.height()) == (w, h) {
        img
    } else if w <= img.width() && h <= img.height() {
        image::imageops::thumbnail(&img, w, h)
    } else {
        image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
    };
    Some(Picture { w, h, px: img.as_raw().chunks_exact(4).map(premultiply).collect() })
}

fn rasterize(tree: &usvg::Tree, w: u32, h: u32) -> Option<Picture> {
    let size = tree.size();
    let mut pm = tiny_skia::Pixmap::new(w, h)?;
    let scale = tiny_skia::Transform::from_scale(w as f32 / size.width(), h as f32 / size.height());
    resvg::render(tree, scale, &mut pm.as_mut());
    let px = pm.data().chunks_exact(4).map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect();
    Some(Picture { w, h, px })
}
