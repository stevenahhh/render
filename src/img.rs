//! Image loading: JPEG/PNG/WebP files, the first frame of GIF files, AVIF files on
//! macOS, and SVG (files or inline markup).
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
    /// For a page opened from the web: where its images are downloaded from, and
    /// whether a lookup waits for the download.
    site: Option<(Arc<crate::net::Site>, bool)>,
    /// Images a lookup found still downloading, by URL as written in the page.
    missing: Vec<Box<str>>,
    /// How many lookups found their image still downloading.
    misses: usize,
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

    /// Image URLs are from now on resolved against `site` and downloaded. Without
    /// `wait`, an image still downloading is looked up as missing, its download goes
    /// on in the background, and `arrived` later tells when it can be laid out.
    pub fn fetch_from(&mut self, site: Arc<crate::net::Site>, wait: bool) {
        self.site = Some((site, wait));
    }

    /// How many lookups have found their image still downloading. Layout compares
    /// it around a lookup to tell a late image from a broken one.
    pub fn misses(&self) -> usize {
        self.misses
    }

    /// How many images found missing have arrived since the last call. Their next
    /// lookup finds them, so a page with arrivals needs laying out again.
    pub fn arrived(&mut self) -> usize {
        let Some((site, _)) = &self.site else { return 0 };
        let before = self.missing.len();
        self.missing.retain(|src| !site.arrived(src));
        before - self.missing.len()
    }

    fn add(&mut self, key: &str, source: Option<(Source, f32, f32)>) -> Option<u32> {
        let id = source.map(|(source, w, h)| {
            self.list.push(Entry { source, w, h, drawn: None, at: [0; 4], failed: false });
            self.list.len() as u32 - 1
        });
        self.by_key.insert(key.into(), id);
        id
    }

    /// Resolves an image URL relative to the page. Without a site only local files
    /// load; a root-relative URL is looked up below the page's directory.
    pub fn file(&mut self, src: &str) -> Option<u32> {
        if let Some(&id) = self.by_key.get(src) {
            return id;
        }
        let local = !src.is_empty() && !src.starts_with("data:") && !src.contains("://");
        let file = src.split(['?', '#']).next().unwrap_or(src).trim_start_matches('/');
        let path = match &self.site {
            Some(_) if src.is_empty() || src.starts_with("data:") => None,
            Some((site, true)) => site.file(src),
            Some((site, false)) => match site.file_if_ready(src) {
                Some(path) => path,
                None => {
                    // Not remembered: the next lookup after it arrives finds it.
                    self.misses += 1;
                    if !self.missing.iter().any(|m| &**m == src) {
                        self.missing.push(src.into());
                    }
                    return None;
                }
            },
            None => self.base.as_ref().filter(|_| local).map(|b| b.join(file)),
        };
        let source = path.and_then(|path| {
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
                let tree = parse_svg(&std::fs::read_to_string(&path).ok()?)?;
                let size = tree.size();
                Some((Source::Svg(tree), size.width(), size.height()))
            } else {
                let (w, h) = dimensions(&path)?;
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

fn is_avif(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("avif"))
}

/// Width and height of an image file, read from its header.
fn dimensions(path: &std::path::Path) -> Option<(u32, u32)> {
    if is_avif(path) {
        return system::dimensions(&std::fs::read(path).ok()?);
    }
    image::image_dimensions(path).ok()
}

/// An image file's pixels as straight RGBA.
fn open(path: &std::path::Path) -> Option<image::RgbaImage> {
    if is_avif(path) {
        return system::decode(&std::fs::read(path).ok()?);
    }
    Some(image::open(path).ok()?.into_rgba8())
}

/// AVIF through ImageIO, the decoder macOS ships since macOS 13.
#[cfg(target_os = "macos")]
mod system {
    use objc2_core_foundation::{CFData, CFDictionary, CFRetained, CFType, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo};
    use std::ptr::NonNull;

    #[link(name = "ImageIO", kind = "framework")]
    extern "C-unwind" {
        fn CGImageSourceCreateWithData(data: &CFData, options: Option<&CFDictionary>) -> Option<NonNull<CFType>>;
        fn CGImageSourceCreateImageAtIndex(source: &CFType, index: usize, options: Option<&CFDictionary>) -> Option<NonNull<CGImage>>;
    }

    /// The first image in an encoded file; its pixels are decoded when drawn.
    fn image(bytes: &[u8]) -> Option<CFRetained<CGImage>> {
        let data = CFData::from_bytes(bytes);
        // SAFETY: `data` is a live CFData and no options are passed. The function follows
        // the Create rule: the source comes back retained once, and `from_raw` takes over
        // that one reference, released when it drops.
        let source = unsafe { CFRetained::from_raw(CGImageSourceCreateWithData(&data, None)?) };
        // SAFETY: `source` is the CGImageSource just created, alive for the call; an index
        // with no image gives NULL, which is `None`. The image follows the Create rule as above.
        Some(unsafe { CFRetained::from_raw(CGImageSourceCreateImageAtIndex(&source, 0, None)?) })
    }

    /// Width and height of an encoded image, without decoding its pixels.
    pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
        let img = image(bytes)?;
        Some((CGImage::width(Some(&img)) as u32, CGImage::height(Some(&img)) as u32))
    }

    /// The pixels of an encoded image, straight RGBA.
    pub fn decode(bytes: &[u8]) -> Option<image::RgbaImage> {
        let img = image(bytes)?;
        let (w, h) = (CGImage::width(Some(&img)), CGImage::height(Some(&img)));
        let mut px = vec![0u8; w.checked_mul(h)?.checked_mul(4)?];
        let space = CGColorSpace::new_device_rgb()?;
        // SAFETY: `px` holds exactly w * h * 4 bytes, matching the width, height, 8 bits per
        // component and w * 4 bytes per row given here. It is neither moved nor read until
        // `ctx`, the only writer, has been dropped below.
        let ctx = unsafe { CGBitmapContextCreate(px.as_mut_ptr().cast(), w, h, 8, w * 4, Some(&space), CGImageAlphaInfo::PremultipliedLast.0) }?;
        CGContext::draw_image(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)), Some(&img));
        drop(ctx);
        // The context keeps premultiplied alpha; the rest of the pipeline takes straight alpha.
        for p in px.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a != 0 && a != 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        image::RgbaImage::from_raw(w as u32, h as u32, px)
    }
}

/// Elsewhere AVIF is not decoded and the image shows its alt text.
#[cfg(not(target_os = "macos"))]
mod system {
    /// Always `None`: there is no AVIF decoder here.
    pub fn dimensions(_: &[u8]) -> Option<(u32, u32)> {
        None
    }

    /// Always `None`: there is no AVIF decoder here.
    pub fn decode(_: &[u8]) -> Option<image::RgbaImage> {
        None
    }
}

fn decode(path: &std::path::Path, w: u32, h: u32) -> Option<Picture> {
    let img = open(path)?;
    let img = if (img.width(), img.height()) == (w, h) {
        img
    } else if w <= img.width() && h <= img.height() {
        image::imageops::thumbnail(&img, w, h)
    } else {
        return Some(Picture { w, h, px: enlarge(&img, w, h) });
    };
    Some(Picture { w, h, px: img.as_raw().chunks_exact(4).map(premultiply).collect() })
}

/// Output rows `enlarge` resamples at a time.
const BAND: u32 = 32;

/// Where the source pixels of output pixel `o` start, and their weights, for a
/// triangle filter scaling `n_in` pixels to `n_out` (`image::imageops::resize`'s sums).
fn triangle_weights(n_in: u32, n_out: u32, o: u32) -> (u32, Vec<f32>) {
    let ratio = n_in as f32 / n_out as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let input = (o as f32 + 0.5) * ratio;
    let left = ((input - sratio).floor() as i64).clamp(0, i64::from(n_in) - 1);
    let right = ((input + sratio).ceil() as i64).clamp(left + 1, i64::from(n_in));
    let input = input - 0.5;
    let mut ws: Vec<f32> = (left..right)
        .map(|i| {
            let x = (i as f32 - input) / sratio;
            if x.abs() < 1.0 { 1.0 - x.abs() } else { 0.0 }
        })
        .collect();
    let sum = ws.iter().fold(0.0f32, |s, w| s + w);
    for w in &mut ws {
        *w /= sum;
    }
    (left as u32, ws)
}

/// `image::imageops::resize` with the triangle filter, premultiplied: the same sums in
/// the same order, so the same pixels, but the vertical pass is kept for `BAND` rows at
/// a time instead of in a float copy of the whole image at its new height.
fn enlarge(img: &image::RgbaImage, w: u32, h: u32) -> Vec<u32> {
    let (sw, sh) = img.dimensions();
    let src = img.as_raw();
    let cols: Vec<(u32, Vec<f32>)> = (0..w).map(|x| triangle_weights(sw, w, x)).collect();
    let mut out = Vec::with_capacity(w as usize * h as usize);
    let mut band = vec![[0.0f32; 4]; sw as usize * BAND.min(h) as usize];
    for y0 in (0..h).step_by(BAND as usize) {
        let rows = BAND.min(h - y0);
        for r in 0..rows {
            let (top, ws) = triangle_weights(sh, h, y0 + r);
            for x in 0..sw as usize {
                let mut t = [0.0f32; 4];
                for (i, w) in ws.iter().enumerate() {
                    let p = (top as usize + i) * sw as usize * 4 + x * 4;
                    for c in 0..4 {
                        t[c] += f32::from(src[p + c]) * w;
                    }
                }
                band[r as usize * sw as usize + x] = t;
            }
        }
        for r in 0..rows as usize {
            for (left, ws) in &cols {
                let mut t = [0.0f32; 4];
                for (i, w) in ws.iter().enumerate() {
                    let v = band[r * sw as usize + *left as usize + i];
                    for c in 0..4 {
                        t[c] += v[c] * w;
                    }
                }
                let px = t.map(|v| (if v < 0.0 { 0.0 } else if v > 255.0 { 255.0 } else { v }).round() as u8);
                out.push(premultiply(&px));
            }
        }
    }
    out
}

/// The part `keep` (`[x0, y0, x1, y1]`) of the image scaled to `w` x `h`: the matching
/// source rectangle is cut out first, so the scaled copy is only as large as the part.
fn decode_part(path: &std::path::Path, w: u32, h: u32, keep: [u32; 4]) -> Option<Picture> {
    let img = open(path)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webp_and_the_first_frame_of_a_gif_are_decoded() {
        use image::codecs::{gif::GifEncoder, webp::WebPEncoder};
        use image::{Delay, Frame, Rgba, RgbaImage};
        let dir = std::env::temp_dir().join(format!("render-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let red = RgbaImage::from_pixel(3, 2, Rgba([255, 0, 0, 255]));
        let webp = dir.join("a.webp");
        red.write_with_encoder(WebPEncoder::new_lossless(std::fs::File::create(&webp).unwrap())).unwrap();
        let gif = dir.join("a.gif");
        let frames = [[0, 0, 255, 255], [0, 255, 0, 255]].map(|c| Frame::from_parts(RgbaImage::from_pixel(4, 5, Rgba(c)), 0, 0, Delay::from_numer_denom_ms(100, 1)));
        GifEncoder::new(std::fs::File::create(&gif).unwrap()).encode_frames(frames).unwrap();
        assert_eq!(dimensions(&webp), Some((3, 2)));
        assert_eq!(decode(&webp, 3, 2).unwrap().px, [0xFFFF_0000; 6]);
        assert_eq!(dimensions(&gif), Some((4, 5)));
        assert_eq!(decode(&gif, 4, 5).unwrap().px, [0xFF00_00FF; 20]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn enlarging_in_bands_gives_the_pixels_of_a_whole_image_resize() {
        let src = image::RgbaImage::from_fn(37, 23, |x, y| image::Rgba([(x * 7 + y * 3) as u8, (x * y) as u8, (255 - x * 5) as u8, if (x + y) % 5 == 0 { 255 } else { (x * 11 + y * 13) as u8 }]));
        for (w, h) in [(80, 23), (37, 71), (90, 100), (50, 10), (20, 90), (41, 24)] {
            let want: Vec<u32> = image::imageops::resize(&src, w, h, image::imageops::FilterType::Triangle).as_raw().chunks_exact(4).map(premultiply).collect();
            assert_eq!(enlarge(&src, w, h), want, "{w}x{h}");
        }
    }
}
