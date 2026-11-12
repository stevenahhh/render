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
    /// What `drawn` holds: its offset `[x, y]` inside the picture scaled to `[w, h]`.
    /// A whole picture is `[0, 0, w, h]`.
    at: [u32; 4],
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
    /// For a page opened from the web: where its images are downloaded from.
    site: Option<Arc<crate::net::Site>>,
}

type Decoded = (u32, Option<Picture>, [u32; 4]);

/// Device pixels kept around the visible part of a partly shown image.
const MARGIN: u32 = 512;
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

    /// Image URLs are from now on resolved against `site` and downloaded.
    pub fn fetch_from(&mut self, site: Arc<crate::net::Site>) {
        self.site = Some(site);
    }

    fn add(&mut self, key: &str, source: Option<(Source, f32, f32)>) -> Option<u32> {
        let id = source.map(|(source, w, h)| {
            self.list.push(Entry { source, w, h, drawn: None, at: [0; 4], failed: false });
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
        let path = match &self.site {
            Some(site) => Some(src).filter(|s| !s.is_empty() && !s.starts_with("data:")).and_then(|s| site.file(s)),
            None => self.base.as_ref().filter(|_| local).map(|b| b.join(file)),
        };
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
        while let Ok((id, picture, at)) = rx.try_recv() {
            self.pending.retain(|&p| p != id);
            let e = &mut self.list[id as usize];
            e.failed = picture.is_none();
            e.drawn = picture;
            e.at = at;
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
            let cached = e.drawn.is_some() && e.at == [0, 0, w, h];
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
                        done.lock().expect("decode results").push((*id, picture, [0, 0, *w, *h]));
                    }
                });
            }
        });
        for (id, picture, at) in done.into_inner().expect("decode results") {
            let e = &mut self.list[id as usize];
            e.failed = picture.is_none();
            e.drawn = picture;
            e.at = at;
        }
    }

    /// Pixels of the image scaled to `w` x `h` device pixels, with the offset of the
    /// returned picture inside that scaled image. `visible` is the part `[x0, y0, x1, y1]`
    /// the caller will draw. With background decoding only that part and a margin are
    /// kept when the image is mostly off screen, so a huge image costs what is shown.
    pub fn get(&mut self, id: u32, w: u32, h: u32, visible: [u32; 4]) -> Option<(&Picture, u32, u32)> {
        let e = self.list.get_mut(id as usize)?;
        if e.failed || w == 0 || h == 0 || w > 16384 || h > 16384 {
            return None;
        }
        let sized = e.drawn.is_some() && e.at[2] == w && e.at[3] == h;
        let covers = sized
            && e.drawn.as_ref().is_some_and(|p| {
                e.at[0] <= visible[0] && e.at[1] <= visible[1] && e.at[0] + p.w >= visible[2].min(w) && e.at[1] + p.h >= visible[3].min(h)
            });
        if !covers {
            if let (Some((tx, _, wake)), Source::File(path)) = (&self.background, &e.source) {
                if !self.pending.contains(&id) && self.pending.len() < MAX_DECODERS {
                    self.pending.push(id);
                    let (x0, y0) = (visible[0].saturating_sub(MARGIN), visible[1].saturating_sub(MARGIN));
                    let (x1, y1) = ((visible[2] + MARGIN).min(w), (visible[3] + MARGIN).min(h));
                    let part = x1.saturating_sub(x0) as u64 * y1.saturating_sub(y0) as u64;
                    let whole = part == 0 || part * 10 >= w as u64 * h as u64 * 6;
                    let keep = if whole { [0, 0, w, h] } else { [x0, y0, x1, y1] };
                    let (tx, wake, path) = (tx.clone(), wake.clone(), path.clone());
                    std::thread::spawn(move || {
                        let picture = if whole { decode(&path, w, h) } else { decode_part(&path, w, h, keep) };
                        // The receiver is gone when the page was replaced meanwhile.
                        if tx.send((id, picture, [keep[0], keep[1], w, h])).is_ok() {
                            wake();
                        }
                    });
                }
                // Until the new part arrives, an older part at this size is still drawn.
                if !sized {
                    return None;
                }
            } else {
                e.drawn = match &e.source {
                    Source::File(path) => decode(path, w, h),
                    Source::Svg(tree) => rasterize(tree, w, h),
                };
                e.at = [0, 0, w, h];
                e.failed = e.drawn.is_none();
            }
        }
        e.drawn.as_ref().map(|p| (p, e.at[0], e.at[1]))
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

/// The part `keep` (`[x0, y0, x1, y1]`) of the image scaled to `w` x `h`: the matching
/// source rectangle is cut out first, so the scaled copy is only as large as the part.
fn decode_part(path: &std::path::Path, w: u32, h: u32, keep: [u32; 4]) -> Option<Picture> {
    let img = image::open(path).ok()?.into_rgba8();
    let (nw, nh) = img.dimensions();
    let to_src = |v: u32, full: u32, n: u32, up: bool| {
        let s = v as u64 * n as u64;
        (if up { s.div_ceil(full as u64) } else { s / full as u64 }).min(n as u64) as u32
    };
    let (sx0, sy0) = (to_src(keep[0], w, nw, false), to_src(keep[1], h, nh, false));
    let (sx1, sy1) = (to_src(keep[2], w, nw, true).max(sx0 + 1).min(nw), to_src(keep[3], h, nh, true).max(sy0 + 1).min(nh));
    let part = image::imageops::crop_imm(&img, sx0, sy0, sx1 - sx0, sy1 - sy0).to_image();
    drop(img);
    let (pw, ph) = (keep[2] - keep[0], keep[3] - keep[1]);
    let part = if (part.width(), part.height()) == (pw, ph) {
        part
    } else if pw <= part.width() && ph <= part.height() {
        image::imageops::thumbnail(&part, pw, ph)
    } else {
        image::imageops::resize(&part, pw, ph, image::imageops::FilterType::Triangle)
    };
    Some(Picture { w: pw, h: ph, px: part.as_raw().chunks_exact(4).map(premultiply).collect() })
}

fn rasterize(tree: &usvg::Tree, w: u32, h: u32) -> Option<Picture> {
    let size = tree.size();
    let mut pm = tiny_skia::Pixmap::new(w, h)?;
    let scale = tiny_skia::Transform::from_scale(w as f32 / size.width(), h as f32 / size.height());
    resvg::render(tree, scale, &mut pm.as_mut());
    let px = pm.data().chunks_exact(4).map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect();
    Some(Picture { w, h, px })
}
