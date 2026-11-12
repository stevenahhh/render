//! System and web font loading, font matching, text measurement and the
//! glyph coverage cache.

use crate::css::{builtin_family, family_names, Family, FontFace};
use crate::FxMap;
use ab_glyph::{point, Font, FontRef, GlyphId, PxScale, VariableFont};
use std::hash::Hasher;
use std::rc::Rc;

/// Metrics are in em units: multiply by the font size.
#[derive(Clone)]
struct Face {
    font: FontRef<'static>,
    ascent: f32,
    descent: f32,
    gap: f32,
    upem: f32,
    /// ab_glyph scales by ascent-to-descent height, not by em.
    hscale: f32,
    /// (glyph id, advance) for ASCII, so Latin text never touches the cmap.
    ascii: [(u16, f32); 128],
}

#[derive(Clone, Copy)]
pub struct Glyph {
    pub left: i32,
    pub top: i32,
    pub w: u32,
    pub h: u32,
    /// Offset of the 8-bit coverage bitmap in `Fonts::atlas`.
    pub off: u32,
}

/// A run of one word that renders with a single face.
#[derive(Clone, Copy)]
pub struct Seg {
    pub face: u8,
    pub start: u32,
    pub end: u32,
    pub w: f32,
}

type Candidates = &'static [(&'static str, u32)];

const SUPP: &str = "/System/Library/Fonts/Supplemental/";

macro_rules! faces {
    ($mac:expr, $linux:expr, $win:expr) => {
        &[
            (concat!("/System/Library/Fonts/Supplemental/", $mac), 0),
            (concat!("/usr/share/fonts/truetype/dejavu/", $linux), 0),
            (concat!("C:\\Windows\\Fonts\\", $win), 0),
        ]
    };
}

/// Index = family * 4 + bold + 2 * italic.
static PRIMARY: [Candidates; 12] = [
    faces!("Times New Roman.ttf", "DejaVuSerif.ttf", "times.ttf"),
    faces!("Times New Roman Bold.ttf", "DejaVuSerif-Bold.ttf", "timesbd.ttf"),
    faces!("Times New Roman Italic.ttf", "DejaVuSerif-Italic.ttf", "timesi.ttf"),
    faces!("Times New Roman Bold Italic.ttf", "DejaVuSerif-BoldItalic.ttf", "timesbi.ttf"),
    faces!("Arial.ttf", "DejaVuSans.ttf", "arial.ttf"),
    faces!("Arial Bold.ttf", "DejaVuSans-Bold.ttf", "arialbd.ttf"),
    faces!("Arial Italic.ttf", "DejaVuSans-Oblique.ttf", "ariali.ttf"),
    faces!("Arial Bold Italic.ttf", "DejaVuSans-BoldOblique.ttf", "arialbi.ttf"),
    &[("/System/Library/Fonts/Menlo.ttc", 0), ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0), ("C:\\Windows\\Fonts\\consola.ttf", 0)],
    &[("/System/Library/Fonts/Menlo.ttc", 1), ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf", 0), ("C:\\Windows\\Fonts\\consolab.ttf", 0)],
    &[("/System/Library/Fonts/Menlo.ttc", 2), ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Oblique.ttf", 0), ("C:\\Windows\\Fonts\\consolai.ttf", 0)],
    &[("/System/Library/Fonts/Menlo.ttc", 3), ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono-BoldOblique.ttf", 0), ("C:\\Windows\\Fonts\\consolaz.ttf", 0)],
];

/// Tried in order for characters the primary face lacks.
static FALLBACK: Candidates = &[
    ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0),
    ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
    ("/System/Library/Fonts/Apple Symbols.ttf", 0),
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc", 0),
    ("C:\\Windows\\Fonts\\malgun.ttf", 0),
    ("C:\\Windows\\Fonts\\seguisym.ttf", 0),
];

/// A web font file: on its way, unusable, or its sfnt data.
enum WebFile {
    Pending,
    Failed,
    Ready(&'static [u8]),
}

/// Containers `webfont::decode` reads, as `format()` hints name them.
const FORMATS: [&str; 4] = ["woff2", "woff", "truetype", "opentype"];

/// Most faces: ids are a byte, and the last value is never handed out.
const MAX_FACES: usize = 255;

pub struct Fonts {
    faces: Vec<Option<Face>>,
    tried: Vec<bool>,
    files: Vec<(&'static str, &'static [u8])>,
    /// The current page's `@font-face` rules.
    web: Rc<[FontFace]>,
    /// Web font files by URL, kept for the life of the process.
    web_files: FxMap<Box<str>, WebFile>,
    /// Face ids of web font instances by URL and weight (0 for a static font).
    instances: FxMap<(Box<str>, u16), u8>,
    /// Face chosen for a family list, weight, style and generic family.
    chosen: FxMap<u64, u8>,
    /// Web font URLs layout asked for since `take_wanted`.
    wanted: Vec<Box<str>>,
    /// Print a line for every web font file taken in.
    pub log: bool,
    /// (primary face, char) -> (face that has it, glyph id, advance in em).
    cmap: FxMap<u64, (u8, u16, f32)>,
    glyphs: FxMap<u64, Glyph>,
    pub atlas: Vec<u8>,
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

impl Fonts {
    pub fn new() -> Fonts {
        let n = PRIMARY.len() + FALLBACK.len();
        let _ = SUPP;
        Fonts {
            faces: vec![None; n],
            tried: vec![false; n],
            files: Vec::new(),
            web: Rc::from([]),
            web_files: FxMap::default(),
            instances: FxMap::default(),
            chosen: FxMap::default(),
            wanted: Vec::new(),
            log: false,
            cmap: FxMap::default(),
            glyphs: FxMap::default(),
            atlas: Vec::new(),
        }
    }

    #[inline]
    pub fn face_id(family: Family, bold: bool, italic: bool) -> u8 {
        family as u8 * 4 + bold as u8 + 2 * italic as u8
    }

    fn load(&mut self, path: &'static str, index: u32) -> Option<Face> {
        let data = match self.files.iter().find(|f| f.0 == path) {
            Some(f) => f.1,
            None => {
                // Font data lives for the whole process, so the mapping is leaked to give
                // FontRef a 'static slice. Mapping instead of reading keeps a 50 MB CJK
                // font out of the heap: only the pages a page's glyphs touch are loaded.
                let file = std::fs::File::open(path).ok()?;
                // SAFETY: the mapping is private and read-only. These are system font
                // files, which are not rewritten in place while programs use them.
                let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
                let d: &'static [u8] = Box::leak(Box::new(map));
                self.files.push((path, d));
                d
            }
        };
        face(FontRef::try_from_slice_and_index(data, index).ok()?)
    }

    /// Makes `faces` the `@font-face` rules that family names are matched against.
    /// Downloads asked for under other rules are forgotten: the page that asked
    /// for them takes them in, so the new rules ask again.
    pub fn set_web_faces(&mut self, faces: &Rc<[FontFace]>) {
        if !Rc::ptr_eq(&self.web, faces) {
            self.web = faces.clone();
            self.chosen.clear();
            self.web_files.retain(|_, f| !matches!(f, WebFile::Pending));
        }
    }

    /// The face for text in a `font-family` list at a weight and style: the
    /// first family with an `@font-face` whose file has arrived, or a system
    /// family the name stands for. A web font still downloading is asked for
    /// (see `take_wanted`) and the next family is used meanwhile.
    pub fn face(&mut self, families: Option<&str>, generic: Family, weight: u16, italic: bool) -> u8 {
        let builtin = Fonts::face_id(generic, weight >= 600, italic);
        let Some(list) = families.filter(|_| !self.web.is_empty()) else { return builtin };
        let mut h = crate::FxHasher::default();
        h.write(list.as_bytes());
        h.write_u64((weight as u64) << 16 | (italic as u64) << 8 | generic as u64);
        let key = h.finish();
        if let Some(&id) = self.chosen.get(&key) {
            return id;
        }
        let id = self.choose(list, weight, italic).unwrap_or(builtin);
        self.chosen.insert(key, id);
        id
    }

    fn choose(&mut self, list: &str, weight: u16, italic: bool) -> Option<u8> {
        let web = self.web.clone();
        for name in family_names(list) {
            let candidates = web.iter().filter(|f| *f.family == *name);
            match best_face(candidates, weight, italic) {
                Some(face) => {
                    if let Some(id) = self.instance(face, weight) {
                        return Some(id);
                    }
                }
                None => {
                    if let Some(f) = builtin_family(&name) {
                        return Some(Fonts::face_id(f, weight >= 600, italic));
                    }
                }
            }
        }
        None
    }

    /// The face of an `@font-face` rule at a weight, from the first of its
    /// sources that loads. `None` while that one is downloading, or when none loads.
    fn instance(&mut self, rule: &FontFace, weight: u16) -> Option<u8> {
        let usable = rule.srcs.iter().filter(|(_, f)| f.as_deref().is_none_or(|f| FORMATS.iter().any(|k| f.starts_with(k))));
        for (url, _) in usable {
            let data = match self.web_files.get(url) {
                Some(WebFile::Ready(data)) => *data,
                Some(WebFile::Failed) => continue,
                Some(WebFile::Pending) => return None,
                None => {
                    self.web_files.insert(url.clone(), WebFile::Pending);
                    self.wanted.push(url.clone());
                    return None;
                }
            };
            let mut font = FontRef::try_from_slice(data).ok()?;
            let axis = font.variations().into_iter().find(|a| &a.tag == b"wght");
            let wght = axis.map_or(0, |a| (weight.clamp(rule.weight.0, rule.weight.1) as f32).clamp(a.min_value, a.max_value) as u16);
            let key = (url.clone(), wght);
            if let Some(&id) = self.instances.get(&key) {
                return Some(id);
            }
            if self.faces.len() >= MAX_FACES {
                return None;
            }
            if wght > 0 {
                font.set_variation(b"wght", wght as f32);
            }
            let id = self.faces.len() as u8;
            self.faces.push(Some(face(font)?));
            self.tried.push(true);
            self.instances.insert(key, id);
            return Some(id);
        }
        None
    }

    /// Web font URLs that text asked for since the last call; each is asked for once.
    pub fn take_wanted(&mut self) -> Vec<Box<str>> {
        std::mem::take(&mut self.wanted)
    }

    /// Takes in a web font file that was asked for, or `None` when it could not
    /// be fetched. Text chooses its faces again afterwards.
    pub fn provide(&mut self, url: &str, file: Option<Vec<u8>>) {
        let decoded = file.as_deref().and_then(crate::webfont::decode);
        if self.log {
            let size = file.as_ref().map_or(0, Vec::len);
            for rule in self.web.iter().filter(|r| r.srcs.iter().any(|s| *s.0 == *url)) {
                let (lo, hi) = rule.weight;
                let style = if rule.italic { "italic" } else { "normal" };
                match &decoded {
                    Some((_, format)) => eprintln!("[font] {} {lo}-{hi} {style}: {format}, {size} bytes, {url}", rule.family),
                    None => eprintln!("[font] {} {lo}-{hi} {style}: unusable ({size} bytes), {url}", rule.family),
                }
            }
        }
        // A font stays in use as long as any page shows text in it, so its data
        // lives for the rest of the process, like the mapped system fonts.
        let state = match decoded {
            Some((data, _)) => WebFile::Ready(Box::leak(data.into_boxed_slice())),
            None => WebFile::Failed,
        };
        self.web_files.insert(url.into(), state);
        self.chosen.clear();
    }

    fn ensure(&mut self, id: usize) -> bool {
        if self.faces[id].is_some() {
            return true;
        }
        if self.tried[id] {
            return false;
        }
        self.tried[id] = true;
        let cands: Candidates = if id < PRIMARY.len() { PRIMARY[id] } else { &FALLBACK[id - PRIMARY.len()..][..1] };
        for &(path, index) in cands {
            if let Some(f) = self.load(path, index) {
                self.faces[id] = Some(f);
                return true;
            }
        }
        false
    }

    /// Makes a primary face usable, substituting the nearest face that loads.
    fn prime(&mut self, id: u8) {
        let id = id as usize;
        if self.faces[id].is_some() || self.ensure(id) {
            return;
        }
        let sub = std::iter::once(id & !3)
            .chain(0..PRIMARY.len())
            .find(|&o| self.ensure(o))
            .expect("no usable system font found");
        self.faces[id] = self.faces[sub].clone();
    }

    /// (ascent, descent, line gap) in em units.
    pub fn metrics(&mut self, id: u8) -> (f32, f32, f32) {
        self.prime(id);
        let f = self.faces[id as usize].as_ref().unwrap();
        (f.ascent, f.descent, f.gap)
    }

    /// Resolves a character to (face, glyph id, advance in em), falling back
    /// to other fonts when the primary face has no glyph for it.
    #[inline]
    pub fn lookup(&mut self, primary: u8, c: char) -> (u8, u16, f32) {
        if self.faces[primary as usize].is_none() {
            self.prime(primary);
        }
        if (c as u32) < 128 {
            let (g, a) = self.faces[primary as usize].as_ref().unwrap().ascii[c as usize];
            return (primary, g, a);
        }
        let key = (primary as u64) << 32 | c as u64;
        if let Some(&v) = self.cmap.get(&key) {
            return v;
        }
        let v = self.lookup_slow(primary, c);
        self.cmap.insert(key, v);
        v
    }

    fn lookup_slow(&mut self, primary: u8, c: char) -> (u8, u16, f32) {
        let own = {
            let f = self.faces[primary as usize].as_ref().unwrap();
            let g = f.font.glyph_id(c);
            (primary, g.0, f.font.h_advance_unscaled(g) / f.upem)
        };
        if own.1 != 0 {
            return own;
        }
        for id in PRIMARY.len()..PRIMARY.len() + FALLBACK.len() {
            if !self.ensure(id) {
                continue;
            }
            let f = self.faces[id].as_ref().unwrap();
            let g = f.font.glyph_id(c);
            if g.0 != 0 {
                return (id as u8, g.0, f.font.h_advance_unscaled(g) / f.upem);
            }
        }
        own
    }

    /// Splits a word into runs of a single face and measures each.
    pub fn segments(&mut self, primary: u8, s: &str, size: f32, spacing: f32, out: &mut Vec<Seg>) {
        self.prime(primary);
        if s.is_ascii() {
            let f = self.faces[primary as usize].as_ref().unwrap();
            let w: f32 = s.bytes().map(|c| f.ascii[c as usize].1).sum();
            out.push(Seg { face: primary, start: 0, end: s.len() as u32, w: w * size + spacing * s.len() as f32 });
            return;
        }
        let (mut cur, mut start, mut w) = (primary, 0, 0.0);
        for (i, c) in s.char_indices() {
            let (face, _, adv) = self.lookup(primary, c);
            if i == 0 {
                cur = face;
            } else if face != cur {
                out.push(Seg { face: cur, start, end: i as u32, w });
                (cur, start, w) = (face, i as u32, 0.0);
            }
            w += adv * size + spacing;
        }
        out.push(Seg { face: cur, start, end: s.len() as u32, w });
    }

    pub fn measure(&mut self, primary: u8, s: &str, size: f32, spacing: f32) -> f32 {
        self.prime(primary);
        if s.is_ascii() {
            let f = self.faces[primary as usize].as_ref().unwrap();
            let w: f32 = s.bytes().map(|c| f.ascii[c as usize].1).sum();
            return w * size + spacing * s.len() as f32;
        }
        s.chars().map(|c| self.lookup(primary, c).2 * size + spacing).sum()
    }

    /// Rasterized coverage for a glyph at a device pixel size. `sub` is the
    /// horizontal subpixel offset in quarters of a pixel.
    #[inline]
    pub fn glyph(&mut self, face: u8, gid: u16, size: f32, sub: u8) -> Glyph {
        let key = (face as u64) << 56 | (gid as u64) << 40 | (sub as u64) << 32 | size.to_bits() as u64;
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let g = self.rasterize(face, gid, size, sub);
        self.glyphs.insert(key, g);
        g
    }

    fn rasterize(&mut self, face: u8, gid: u16, size: f32, sub: u8) -> Glyph {
        let empty = Glyph { left: 0, top: 0, w: 0, h: 0, off: 0 };
        let Some(f) = self.faces[face as usize].as_ref() else { return empty };
        let glyph = GlyphId(gid).with_scale_and_position(PxScale::from(size * f.hscale), point(sub as f32 * 0.25, 0.0));
        let Some(outline) = f.font.outline_glyph(glyph) else { return empty };
        let b = outline.px_bounds();
        let (w, h) = (b.width() as usize, b.height() as usize);
        if w == 0 || h == 0 {
            return empty;
        }
        let off = self.atlas.len();
        self.atlas.resize(off + w * h, 0);
        let dst = &mut self.atlas[off..];
        outline.draw(|x, y, c| {
            if let Some(p) = dst.get_mut(y as usize * w + x as usize) {
                *p = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        });
        Glyph { left: b.min.x as i32, top: b.min.y as i32, w: w as u32, h: h as u32, off: off as u32 }
    }
}

/// The `@font-face` rule CSS font matching picks for a weight and style:
/// the right style first, then a face covering the weight, else the nearest
/// one in the direction CSS Fonts 4 prescribes for that weight.
fn best_face<'a>(rules: impl Iterator<Item = &'a FontFace>, weight: u16, italic: bool) -> Option<&'a FontFace> {
    let rank = |r: &FontFace| {
        let (lo, hi) = r.weight;
        let w = if (lo..=hi).contains(&weight) {
            (0, 0)
        } else if (400..=500).contains(&weight) {
            match () {
                _ if lo > weight && lo <= 500 => (1, lo - weight),
                _ if hi < weight => (2, weight - hi),
                _ => (3, lo - weight),
            }
        } else if weight < 400 {
            if hi < weight { (1, weight - hi) } else { (2, lo - weight) }
        } else if lo > weight {
            (1, lo - weight)
        } else {
            (2, weight - hi)
        };
        (r.italic != italic, w)
    };
    rules.min_by_key(|r| rank(r))
}

/// Metrics and ASCII advances of a parsed font.
fn face(font: FontRef<'static>) -> Option<Face> {
    let upem = font.units_per_em()?;
    let mut ascii = [(0u16, 0f32); 128];
    for (c, slot) in ascii.iter_mut().enumerate().skip(32) {
        let g = font.glyph_id(c as u8 as char);
        *slot = (g.0, font.h_advance_unscaled(g) / upem);
    }
    Some(Face {
        ascent: font.ascent_unscaled() / upem,
        descent: -font.descent_unscaled() / upem,
        gap: font.line_gap_unscaled() / upem,
        hscale: font.height_unscaled() / upem,
        upem,
        ascii,
        font,
    })
}

#[cfg(test)]
mod tests {
    use super::best_face;
    use crate::css::FontFace;

    fn face(lo: u16, hi: u16, italic: bool) -> FontFace {
        FontFace { family: "f".into(), srcs: Vec::new(), weight: (lo, hi), italic }
    }

    /// The weight of the face CSS font matching picks from `faces`.
    fn pick(faces: &[FontFace], weight: u16, italic: bool) -> (u16, bool) {
        let f = best_face(faces.iter(), weight, italic).unwrap();
        (f.weight.0, f.italic)
    }

    #[test]
    fn weights_match_like_css_fonts_4() {
        let set: Vec<_> = [300, 400, 600, 700].iter().map(|&w| face(w, w, false)).collect();
        assert_eq!(pick(&set, 400, false), (400, false));
        // 500 tries heavier up to 500, then lighter, then heavier.
        assert_eq!(pick(&set, 500, false), (400, false));
        // Below 400 lighter comes first; above 500 heavier does.
        assert_eq!(pick(&set, 350, false), (300, false));
        assert_eq!(pick(&set, 200, false), (300, false));
        assert_eq!(pick(&set, 650, false), (700, false));
        assert_eq!(pick(&set, 900, false), (700, false));
        let thin: Vec<_> = [100, 500, 900].iter().map(|&w| face(w, w, false)).collect();
        assert_eq!(pick(&thin, 400, false), (500, false));
        assert_eq!(pick(&thin, 450, false), (500, false));
        // A variable face covers its whole range.
        assert_eq!(pick(&[face(100, 900, false), face(700, 700, false)], 700, false), (100, false));
    }

    #[test]
    fn style_outranks_weight() {
        let set = [face(400, 400, false), face(700, 700, true)];
        assert_eq!(pick(&set, 400, true), (700, true));
        assert_eq!(pick(&set, 700, false), (400, false));
    }
}
