//! System font loading, text measurement and the glyph coverage cache.

use crate::css::Family;
use crate::FxMap;
use ab_glyph::{point, Font, FontRef, GlyphId, PxScale};

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

pub struct Fonts {
    faces: Vec<Option<Face>>,
    tried: Vec<bool>,
    files: Vec<(&'static str, &'static [u8])>,
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
        let font = FontRef::try_from_slice_and_index(data, index).ok()?;
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
        for id in PRIMARY.len()..self.faces.len() {
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
