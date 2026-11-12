//! Web font containers: WOFF and WOFF2 files rebuilt into the plain sfnt
//! (TrueType or OpenType) data the font library reads.

use std::io::Read;

/// Font data `ab_glyph` can read, rebuilt from a downloaded font file, with the
/// name of the container it came in. `None` when the data is not a font, or is
/// a WOFF2 font collection, which is not supported.
pub fn decode(data: &[u8]) -> Option<(Vec<u8>, &'static str)> {
    match data.get(..4)? {
        b"wOFF" => woff(data).map(|d| (d, "woff")),
        b"wOF2" => woff2(data).map(|d| (d, "woff2")),
        [0, 1, 0, 0] | b"true" => Some((data.to_vec(), "truetype")),
        b"OTTO" => Some((data.to_vec(), "opentype")),
        _ => None,
    }
}

/// Big-endian reads from a byte slice; every read is checked.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }

    fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn tag(&mut self) -> Option<[u8; 4]> {
        self.take(4).map(|s| [s[0], s[1], s[2], s[3]])
    }

    /// WOFF2 `UIntBase128`: 7 bits per byte, most significant first.
    fn base128(&mut self) -> Option<u32> {
        let mut v: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            if i == 0 && b == 0x80 {
                return None;
            }
            v = v.checked_mul(128)? | (b & 0x7f) as u32;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// WOFF2 `255UInt16`.
    fn u255(&mut self) -> Option<u16> {
        match self.u8()? {
            253 => self.u16(),
            254 => self.u8().map(|b| b as u16 + 506),
            255 => self.u8().map(|b| b as u16 + 253),
            b => Some(b as u16),
        }
    }
}

/// Assembles an sfnt file from its tables.
fn sfnt(flavor: u32, mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|t| t.0);
    let n = tables.len() as u16;
    let (mut pow, mut log) = (1u16, 0u16);
    while pow * 2 <= n {
        pow *= 2;
        log += 1;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&flavor.to_be_bytes());
    for v in [n, pow * 16, log, n * 16 - pow * 16] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    let mut offset = 12 + 16 * tables.len();
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        offset += (data.len() + 3) & !3;
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        out.resize((out.len() + 3) & !3, 0);
    }
    out
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, c| {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        sum.wrapping_add(u32::from_be_bytes(w))
    })
}

/// WOFF 1.0: each table on its own, zlib-compressed when that made it smaller.
fn woff(d: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(d);
    r.take(4)?;
    let flavor = r.u32()?;
    r.take(4)?;
    let n = r.u16()? as usize;
    let mut tables = Vec::with_capacity(n);
    for i in 0..n {
        let mut e = Reader::new(d.get(44 + 20 * i..)?);
        let tag = e.tag()?;
        let (offset, stored, length) = (e.u32()? as usize, e.u32()? as usize, e.u32()? as usize);
        let raw = d.get(offset..offset.checked_add(stored)?)?;
        let data = if stored < length {
            let mut out = Vec::with_capacity(length);
            flate2::read::ZlibDecoder::new(raw).take(length as u64).read_to_end(&mut out).ok()?;
            out
        } else {
            raw.to_vec()
        };
        if data.len() != length {
            return None;
        }
        tables.push((tag, data));
    }
    Some(sfnt(flavor, tables))
}

/// Tags a WOFF2 table directory names by index.
const KNOWN_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm", b"glyf", b"loca", b"prep", b"CFF ",
    b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern", b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS",
    b"GSUB", b"EBSC", b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar", b"bdat", b"bloc",
    b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty", b"just", b"lcar", b"mort", b"morx", b"opbd", b"prop",
    b"trak", b"Zapf", b"Silf", b"Glat", b"Gloc", b"Feat", b"Sill",
];

/// WOFF 2.0: all tables in one Brotli stream, `glyf`/`loca` and optionally
/// `hmtx` stored in transformed forms that are rebuilt here.
fn woff2(d: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(d);
    r.take(4)?;
    let flavor = r.u32()?;
    if flavor == u32::from_be_bytes(*b"ttcf") {
        return None;
    }
    r.take(4)?;
    let n = r.u16()? as usize;
    r.take(6)?;
    let compressed = r.u32()? as usize;
    r.take(24)?;
    // (tag, stored length, transformed)
    let mut dir = Vec::with_capacity(n);
    for _ in 0..n {
        let flags = r.u8()?;
        let tag = match flags & 63 {
            63 => r.tag()?,
            i => *KNOWN_TAGS[i as usize],
        };
        let length = r.base128()? as usize;
        let glyf_loca = &tag == b"glyf" || &tag == b"loca";
        let transformed = if glyf_loca { flags >> 6 == 0 } else { flags >> 6 != 0 };
        let stored = if transformed { r.base128()? as usize } else { length };
        dir.push((tag, stored, transformed));
    }
    let stream = d.get(r.at..r.at.checked_add(compressed)?)?;
    let total: usize = dir.iter().map(|e| e.1).sum();
    let mut data = Vec::with_capacity(total);
    brotli_decompressor::Decompressor::new(stream, 4096).take(total as u64).read_to_end(&mut data).ok()?;
    if data.len() != total {
        return None;
    }
    let mut at = 0;
    let mut raw: Vec<([u8; 4], &[u8], bool)> = Vec::with_capacity(n);
    for &(tag, stored, transformed) in &dir {
        raw.push((tag, &data[at..at + stored], transformed));
        at += stored;
    }
    let find = |want: &[u8; 4]| raw.iter().find(|t| &t.0 == want);
    let mut tables = Vec::with_capacity(n);
    let mut x_mins = None;
    if let Some(&(_, glyf_data, true)) = find(b"glyf") {
        let (glyf_table, loca, mins) = glyf(glyf_data)?;
        tables.push((*b"glyf", glyf_table));
        tables.push((*b"loca", loca));
        x_mins = Some(mins);
    }
    for &(tag, table, transformed) in &raw {
        match (&tag, transformed) {
            (b"glyf" | b"loca", true) => {}
            (b"hmtx", true) => {
                let glyphs = Reader::new(find(b"maxp")?.1.get(4..)?).u16()? as usize;
                let metrics = Reader::new(find(b"hhea")?.1.get(34..)?).u16()? as usize;
                tables.push((tag, hmtx(table, glyphs, metrics, x_mins.as_deref()?)?));
            }
            (_, true) => return None,
            _ => tables.push((tag, table.to_vec())),
        }
    }
    Some(sfnt(flavor, tables))
}

/// One point of a transformed glyph: its offset from the previous point, read
/// with the flag byte's encoding from the glyph stream.
fn triplet(flag: u8, s: &mut Reader) -> Option<(i32, i32)> {
    let sign = |f: u8, v: i32| if f & 1 != 0 { v } else { -v };
    let f = flag as i32;
    Some(if flag < 10 {
        (0, sign(flag, ((f & 14) << 7) + s.u8()? as i32))
    } else if flag < 20 {
        (sign(flag, (((f - 10) & 14) << 7) + s.u8()? as i32), 0)
    } else if flag < 84 {
        let (b0, b1) = (f - 20, s.u8()? as i32);
        (sign(flag, 1 + (b0 & 0x30) + (b1 >> 4)), sign(flag >> 1, 1 + ((b0 & 0x0c) << 2) + (b1 & 0x0f)))
    } else if flag < 120 {
        let b0 = f - 84;
        let (x, y) = (s.u8()? as i32, s.u8()? as i32);
        (sign(flag, 1 + ((b0 / 12) << 8) + x), sign(flag >> 1, 1 + (((b0 % 12) >> 2) << 8) + y))
    } else if flag < 124 {
        let (b0, b1, b2) = (s.u8()? as i32, s.u8()? as i32, s.u8()? as i32);
        (sign(flag, (b0 << 4) + (b1 >> 4)), sign(flag >> 1, ((b1 & 0x0f) << 8) + b2))
    } else {
        let (x, y) = (s.u16()? as i32, s.u16()? as i32);
        (sign(flag, x), sign(flag >> 1, y))
    })
}

fn put16(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&(v as u16).to_be_bytes());
}

/// Rebuilds the `glyf` and `loca` tables from a transformed `glyf`, and
/// returns each glyph's xMin, which a transformed `hmtx` leaves out.
fn glyf(t: &[u8]) -> Option<(Vec<u8>, Vec<u8>, Vec<i16>)> {
    let mut h = Reader::new(t);
    h.take(4)?;
    let glyphs = h.u16()? as usize;
    let long_loca = h.u16()? != 0;
    let mut at = h.at + 28;
    let mut stream = || -> Option<Reader> {
        let size = h.u32()? as usize;
        let s = t.get(at..at.checked_add(size)?)?;
        at += size;
        Some(Reader::new(s))
    };
    let (mut contours, mut points, mut flags, mut coords) = (stream()?, stream()?, stream()?, stream()?);
    let (mut composite, mut bbox, mut instructions) = (stream()?, stream()?, stream()?);
    let bitmap = bbox.take(4 * glyphs.div_ceil(32))?;
    let (mut out, mut offsets, mut x_mins) = (Vec::new(), Vec::with_capacity(glyphs + 1), Vec::with_capacity(glyphs));
    for i in 0..glyphs {
        offsets.push(out.len());
        let n = contours.i16()?;
        let explicit_bbox = bitmap[i >> 3] & (0x80 >> (i & 7)) != 0;
        let read_bbox = |b: &mut Reader| -> Option<[i16; 4]> { Some([b.i16()?, b.i16()?, b.i16()?, b.i16()?]) };
        if n == 0 {
            x_mins.push(0);
            continue;
        }
        let start = out.len();
        if n > 0 {
            let mut ends = Vec::with_capacity(n as usize);
            let mut total = 0usize;
            for _ in 0..n {
                total += points.u255()? as usize;
                ends.push(total.checked_sub(1)?);
            }
            let (mut x, mut y) = (0i32, 0i32);
            let mut pts = Vec::with_capacity(total);
            for _ in 0..total {
                let f = flags.u8()?;
                let (dx, dy) = triplet(f & 0x7f, &mut coords)?;
                x += dx;
                y += dy;
                pts.push((x, y, f & 0x80 == 0));
            }
            let code = coords.u255()? as usize;
            let code = instructions.take(code)?;
            let b = if explicit_bbox {
                read_bbox(&mut bbox)?
            } else {
                let fold = |f: fn(i32, i32) -> i32, v: fn(&(i32, i32, bool)) -> i32| pts.iter().map(v).reduce(f).unwrap_or(0) as i16;
                [fold(i32::min, |p| p.0), fold(i32::min, |p| p.1), fold(i32::max, |p| p.0), fold(i32::max, |p| p.1)]
            };
            put16(&mut out, n as i32);
            b.iter().for_each(|&v| put16(&mut out, v as i32));
            ends.iter().for_each(|&e| put16(&mut out, e as i32));
            put16(&mut out, code.len() as i32);
            out.extend_from_slice(code);
            out.extend(pts.iter().map(|p| p.2 as u8));
            let mut prev = 0;
            for p in &pts {
                put16(&mut out, p.0 - prev);
                prev = p.0;
            }
            prev = 0;
            for p in &pts {
                put16(&mut out, p.1 - prev);
                prev = p.1;
            }
        } else {
            let b = read_bbox(&mut bbox)?;
            put16(&mut out, -1);
            b.iter().for_each(|&v| put16(&mut out, v as i32));
            let mut has_code = false;
            loop {
                let f = composite.u16()?;
                let args = if f & 1 != 0 { 4 } else { 2 };
                let scale = if f & 8 != 0 { 2 } else if f & 0x40 != 0 { 4 } else if f & 0x80 != 0 { 8 } else { 0 };
                put16(&mut out, f as i32);
                out.extend_from_slice(composite.take(2 + args + scale)?);
                has_code |= f & 0x100 != 0;
                if f & 0x20 == 0 {
                    break;
                }
            }
            if has_code {
                let code = coords.u255()? as usize;
                put16(&mut out, code as i32);
                out.extend_from_slice(instructions.take(code)?);
            }
        }
        x_mins.push(i16::from_be_bytes([out[start + 2], out[start + 3]]));
        out.resize((out.len() + 3) & !3, 0);
    }
    offsets.push(out.len());
    let mut loca = Vec::with_capacity(offsets.len() * 4);
    for o in offsets {
        if long_loca {
            loca.extend_from_slice(&(o as u32).to_be_bytes());
        } else {
            loca.extend_from_slice(&((o / 2) as u16).to_be_bytes());
        }
    }
    Some((out, loca, x_mins))
}

/// Rebuilds `hmtx` from its transformed form, which may leave out left side
/// bearings that equal the glyph's xMin.
fn hmtx(t: &[u8], glyphs: usize, metrics: usize, x_mins: &[i16]) -> Option<Vec<u8>> {
    let mut r = Reader::new(t);
    let flags = r.u8()?;
    let advances: Vec<u16> = (0..metrics).map(|_| r.u16()).collect::<Option<_>>()?;
    let mut bearings = |range: std::ops::Range<usize>, absent: bool| -> Option<Vec<i16>> {
        match absent {
            true => x_mins.get(range).map(<[i16]>::to_vec),
            false => range.map(|_| r.i16()).collect(),
        }
    };
    let proportional = bearings(0..metrics, flags & 1 != 0)?;
    let mono = bearings(metrics..glyphs, flags & 2 != 0)?;
    let mut out = Vec::with_capacity(metrics * 4 + mono.len() * 2);
    for (a, l) in advances.iter().zip(&proportional) {
        out.extend_from_slice(&a.to_be_bytes());
        out.extend_from_slice(&l.to_be_bytes());
    }
    mono.iter().for_each(|l| out.extend_from_slice(&l.to_be_bytes()));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::decode;
    use ab_glyph::{Font, FontRef, GlyphId};

    // A subset of Fira Sans Regular (SIL Open Font License 1.1, see
    // testdata/OFL-FiraSans.txt) in the three formats Google Fonts serves.
    const TTF: &[u8] = include_bytes!("testdata/fira-subset.ttf");
    const WOFF: &[u8] = include_bytes!("testdata/fira-subset.woff");
    const WOFF2: &[u8] = include_bytes!("testdata/fira-subset.woff2");

    fn tags(sfnt: &[u8]) -> Vec<String> {
        let n = u16::from_be_bytes([sfnt[4], sfnt[5]]) as usize;
        (0..n).map(|i| String::from_utf8_lossy(&sfnt[12 + 16 * i..][..4]).into_owned()).collect()
    }

    /// Every glyph's advance and outline bounds, the values text layout and
    /// rasterizing read from the font.
    fn glyphs(sfnt: &[u8]) -> Vec<(f32, Option<[f32; 4]>)> {
        let font = FontRef::try_from_slice(sfnt).expect("rebuilt font parses");
        (0..font.glyph_count() as u16)
            .map(|g| {
                let id = GlyphId(g);
                let b = font.outline(id).map(|o| [o.bounds.min.x, o.bounds.min.y, o.bounds.max.x, o.bounds.max.y]);
                (font.h_advance_unscaled(id), b)
            })
            .collect()
    }

    #[test]
    fn woff_and_woff2_rebuild_the_same_glyphs_as_the_truetype_file() {
        let (woff, format) = decode(WOFF).expect("woff decodes");
        assert_eq!(format, "woff");
        let (woff2, format) = decode(WOFF2).expect("woff2 decodes");
        assert_eq!(format, "woff2");
        assert_eq!(tags(&woff), tags(TTF));
        for tag in ["cmap", "glyf", "head", "hhea", "hmtx", "loca", "maxp"] {
            assert!(tags(&woff2).iter().any(|t| t == tag), "woff2 lacks {tag}");
        }
        let expected = glyphs(TTF);
        assert_eq!(expected.len(), 24);
        assert_eq!(glyphs(&woff), expected);
        assert_eq!(glyphs(&woff2), expected);
        let font = FontRef::try_from_slice(&woff2).unwrap();
        assert_ne!(font.glyph_id('H'), GlyphId(0));
    }

    #[test]
    fn non_fonts_are_refused() {
        assert!(decode(b"<html>").is_none());
        assert!(decode(&WOFF2[..60]).is_none());
    }
}
