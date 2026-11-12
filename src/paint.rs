//! Software rasterizer: paints a `Layout` into a 0x00RRGGBB pixel buffer.

use crate::css::{Color, Gradient, Len, DECO_LINE_THROUGH, DECO_UNDERLINE};
use crate::font::Fonts;
use crate::img::{Images, Picture};
use crate::layout::{BoxDeco, Item, Kind, Layout, TextRun};

pub struct Target<'a> {
    pub buf: &'a mut [u32],
    pub w: i32,
    pub h: i32,
}

/// Device pixels from `x0, y0` up to but not including `x1, y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Rect {
    pub fn intersect(self, o: Rect) -> Rect {
        Rect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) }
    }
    pub fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    fn union(self, o: Rect) -> Rect {
        Rect { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }
    fn area(self) -> i64 {
        if self.is_empty() { 0 } else { (self.x1 - self.x0) as i64 * (self.y1 - self.y0) as i64 }
    }
    fn grow(self, by: i32) -> Rect {
        Rect { x0: self.x0 - by, y0: self.y0 - by, x1: self.x1 + by, y1: self.y1 + by }
    }
}

enum Src<'a> {
    Solid { rgb: u32, a: u32 },
    /// t = t0 + x * dx + y * dy at pixel centres, looked up in a 256-entry ramp.
    Grad { lut: &'a [u32; 256], t0: f32, dx: f32, dy: f32 },
}

/// Blends `rgb` over `dst` with alpha 0..=255, red/blue and green in two
/// multiplies instead of three.
#[inline(always)]
fn blend(dst: u32, rgb: u32, a: u32) -> u32 {
    let a = a + (a >> 7);
    let ia = 256 - a;
    let rb = ((rgb & 0x00FF_00FF) * a + (dst & 0x00FF_00FF) * ia) >> 8 & 0x00FF_00FF;
    let g = ((rgb & 0x0000_FF00) * a + (dst & 0x0000_FF00) * ia) >> 8 & 0x0000_FF00;
    rb | g
}

impl Src<'_> {
    #[inline]
    fn at(&self, x: i32, y: i32) -> (u32, u32) {
        match *self {
            Src::Solid { rgb, a } => (rgb, a),
            Src::Grad { lut, t0, dx, dy } => {
                let t = t0 + (x as f32 + 0.5) * dx + (y as f32 + 0.5) * dy;
                let c = lut[(t.clamp(0.0, 1.0) * 255.0) as usize];
                (c & 0x00FF_FFFF, c >> 24)
            }
        }
    }
}

fn span(t: &mut Target, y: i32, xa: i32, xb: i32, src: &Src) {
    if xa >= xb {
        return;
    }
    let row = &mut t.buf[(y * t.w + xa) as usize..(y * t.w + xb) as usize];
    match *src {
        Src::Solid { rgb, a } => {
            if a >= 255 {
                row.fill(rgb);
            } else if a > 0 {
                for p in row {
                    *p = blend(*p, rgb, a);
                }
            }
        }
        Src::Grad { lut, t0, dx, dy } => {
            let mut tt = t0 + (xa as f32 + 0.5) * dx + (y as f32 + 0.5) * dy;
            for p in row {
                let c = lut[(tt.clamp(0.0, 1.0) * 255.0) as usize];
                let a = c >> 24;
                *p = if a == 255 { c & 0x00FF_FFFF } else { blend(*p, c & 0x00FF_FFFF, a) };
                tt += dx;
            }
        }
    }
}

/// Coverage of the pixel centred at (px, py) by a rounded rectangle whose
/// top-left, top-right, bottom-right and bottom-left corners have the radii
/// `rad`, from its signed distance.
#[inline]
fn rr_cov(px: f32, py: f32, r: (f32, f32, f32, f32), rad: [f32; 4]) -> f32 {
    let (hw, hh) = ((r.2 - r.0) * 0.5, (r.3 - r.1) * 0.5);
    let (dx, dy) = (px - (r.0 + hw), py - (r.1 + hh));
    let rad = match (dx < 0.0, dy < 0.0) {
        (true, true) => rad[0],
        (false, true) => rad[1],
        (false, false) => rad[2],
        (true, false) => rad[3],
    };
    let qx = dx.abs() - (hw - rad);
    let qy = dy.abs() - (hh - rad);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    let d = (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - rad;
    (0.5 - d).clamp(0.0, 1.0)
}

/// Fills a rectangle with optional rounded corners and an optional rounded
/// hole (for border rings), radii in the order of `rr_cov`. Only the corner
/// columns are evaluated per pixel; everything else is filled as solid spans.
fn fill_shape(t: &mut Target, clip: Rect, r: Rect, rad: [f32; 4], hole: Option<(Rect, [f32; 4])>, src: &Src) {
    let c = clip.intersect(r);
    if c.is_empty() {
        return;
    }
    if rad.iter().all(|&v| v < 0.5) && hole.is_none() {
        for y in c.y0..c.y1 {
            span(t, y, c.x0, c.x1, src);
        }
        return;
    }
    let outer = (r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32);
    let (mut lz, mut rzs) = (r.x0 + rad[0].max(rad[3]).ceil() as i32, r.x1 - rad[1].max(rad[2]).ceil() as i32);
    let inner = hole.map(|(h, hr)| {
        lz = lz.max(h.x0 + hr[0].max(hr[3]).ceil() as i32);
        rzs = rzs.min(h.x1 - hr[1].max(hr[2]).ceil() as i32);
        ((h.x0 as f32, h.y0 as f32, h.x1 as f32, h.y1 as f32), hr, h)
    });
    if lz > rzs {
        lz = r.x1;
        rzs = r.x1;
    }
    let (top, bottom) = (outer.1 + rad[0].max(rad[1]), outer.3 - rad[2].max(rad[3]));
    for y in c.y0..c.y1 {
        let py = y as f32 + 0.5;
        let in_hole = inner.is_some_and(|(_, _, h)| y >= h.y0 && y < h.y1);
        let band = py < top || py > bottom;
        if !band && !in_hole {
            span(t, y, c.x0, c.x1, src);
            continue;
        }
        for (xa, xb) in [(c.x0, lz.min(c.x1)), (rzs.max(c.x0), c.x1)] {
            for x in xa..xb {
                let px = x as f32 + 0.5;
                let mut cov = rr_cov(px, py, outer, rad);
                if let Some((hr, hrad, _)) = inner {
                    cov *= 1.0 - rr_cov(px, py, hr, hrad);
                }
                if cov > 0.0 {
                    let (rgb, a) = src.at(x, y);
                    let a = (a as f32 * cov + 0.5) as u32;
                    let p = &mut t.buf[(y * t.w + x) as usize];
                    *p = blend(*p, rgb, a);
                }
            }
        }
        if !in_hole {
            span(t, y, lz.max(c.x0), rzs.min(c.x1), src);
        }
    }
}

fn solid(c: Color) -> Src<'static> {
    Src::Solid { rgb: c.0 & 0x00FF_FFFF, a: c.a() }
}

fn gradient_lut(g: &Gradient) -> [u32; 256] {
    let mut lut = [0u32; 256];
    let mut k = 0;
    for (i, slot) in lut.iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        while k + 2 < g.stops.len() && t > g.stops[k + 1].0 {
            k += 1;
        }
        let ((p0, c0), (p1, c1)) = (g.stops[k], g.stops[k + 1]);
        let f = if p1 > p0 { ((t - p0) / (p1 - p0)).clamp(0.0, 1.0) } else { (t >= p1) as u32 as f32 };
        let ch = |shift: u32| {
            let (a, b) = ((c0.0 >> shift & 0xFF) as f32, (c1.0 >> shift & 0xFF) as f32);
            ((a + (b - a) * f + 0.5) as u32) << shift
        };
        *slot = ch(24) | ch(16) | ch(8) | ch(0);
    }
    lut
}

/// Corner radii in device pixels of a box `w` x `h` device pixels, in the order of
/// `rr_cov`. When the two radii on a side add up to more than the side, all of them
/// shrink by the same factor, as CSS has it.
fn radii(rad: &[Len; 4], w: f32, h: f32, scale: f32) -> [f32; 4] {
    let mut out = rad.map(|l| match l {
        Len::Px(v) => v * scale,
        Len::Pct(p) => p * 0.01 * w.min(h),
        _ => 0.0,
    });
    if out.iter().all(|&v| v == out[0]) {
        return [out[0].min(w.min(h) / 2.0); 4];
    }
    out = out.map(|v| v.max(0.0));
    let f = [(w, out[0] + out[1]), (h, out[1] + out[2]), (w, out[2] + out[3]), (h, out[3] + out[0])]
        .into_iter()
        .fold(1.0f32, |f, (side, sum)| if sum > side { f.min(side / sum) } else { f });
    out.map(|v| v * f)
}

fn draw_box(t: &mut Target, clip: Rect, d: &BoxDeco, r: Rect, scale: f32) {
    let (w, h) = ((r.x1 - r.x0) as f32, (r.y1 - r.y0) as f32);
    let rad = radii(&d.radius, w, h, scale);

    if let Some(g) = &d.grad {
        let lut = gradient_lut(g);
        let (sin, cos) = g.angle.to_radians().sin_cos();
        let len = (w * sin).abs() + (h * cos).abs();
        let (dx, dy) = (sin / len.max(1.0), -cos / len.max(1.0));
        let (cx, cy) = ((r.x0 + r.x1) as f32 * 0.5, (r.y0 + r.y1) as f32 * 0.5);
        fill_shape(t, clip, r, rad, None, &Src::Grad { lut: &lut, t0: 0.5 - cx * dx - cy * dy, dx, dy });
    } else if d.bg.a() > 0 {
        fill_shape(t, clip, r, rad, None, &solid(d.bg));
    }

    let bw = d.bw.map(|v| if v > 0.0 { ((v * scale).round() as i32).max(1) } else { 0 });
    let [bt, br, bb, bl] = bw;
    if bt + br + bb + bl == 0 {
        return;
    }
    if rad.iter().all(|&v| v < 0.5) {
        let edges = [
            Rect { x0: r.x0, y0: r.y0, x1: r.x1, y1: r.y0 + bt },
            Rect { x0: r.x1 - br, y0: r.y0 + bt, x1: r.x1, y1: r.y1 - bb },
            Rect { x0: r.x0, y0: r.y1 - bb, x1: r.x1, y1: r.y1 },
            Rect { x0: r.x0, y0: r.y0 + bt, x1: r.x0 + bl, y1: r.y1 - bb },
        ];
        for (e, c) in edges.into_iter().zip(d.bc) {
            if c.a() > 0 {
                fill_shape(t, clip, e, [0.0; 4], None, &solid(c));
            }
        }
        return;
    }
    let side = bw.iter().position(|w| *w > 0).unwrap();
    let hole = Rect { x0: r.x0 + bl, y0: r.y0 + bt, x1: r.x1 - br, y1: r.y1 - bb };
    let widest = *bw.iter().max().unwrap() as f32;
    let hole = (!hole.is_empty()).then_some((hole, rad.map(|v| (v - widest).max(0.0))));
    fill_shape(t, clip, r, rad, hole, &solid(d.bc[side]));
}

fn blit(t: &mut Target, clip: Rect, cov: &[u8], gw: i32, gx: i32, gy: i32, color: Color) {
    let gh = cov.len() as i32 / gw;
    let c = clip.intersect(Rect { x0: gx, y0: gy, x1: gx + gw, y1: gy + gh });
    if c.is_empty() {
        return;
    }
    let (rgb, alpha) = (color.0 & 0x00FF_FFFF, color.a());
    let n = (c.x1 - c.x0) as usize;
    for y in c.y0..c.y1 {
        let src = &cov[((y - gy) * gw + (c.x0 - gx)) as usize..][..n];
        let dst = &mut t.buf[(y * t.w + c.x0) as usize..][..n];
        for (d, &cv) in dst.iter_mut().zip(src) {
            if cv == 0 {
                continue;
            }
            let a = if alpha == 255 { cv as u32 } else { (cv as u32 * alpha + 127) / 255 };
            *d = if a == 255 { rgb } else { blend(*d, rgb, a) };
        }
    }
}

fn draw_text(t: &mut Target, clip: Rect, l: &Layout, fonts: &mut Fonts, it: &Item, sy: i32, scale: f32) {
    let run = &l.texts[it.data as usize];
    if run.color.a() == 0 {
        return;
    }
    let text = &l.text[run.start as usize..(run.start + run.len) as usize];
    let size = run.size * scale;
    let baseline = ((it.y + run.ascent) * scale).round() as i32 - sy;
    let spacing = run.spacing * scale;
    let mut pen = it.x * scale;
    for c in text.chars() {
        let (_, gid, adv) = fonts.lookup(run.face, c);
        if c != ' ' {
            let ipx = pen.floor();
            let g = fonts.glyph(run.face, gid, size, ((pen - ipx) * 4.0) as u8);
            if g.w > 0 {
                let cov = &fonts.atlas[g.off as usize..(g.off + g.w * g.h) as usize];
                blit(t, clip, cov, g.w as i32, ipx as i32 + g.left, baseline + g.top, run.color);
            }
        }
        pen += adv * size + spacing;
    }
    if run.deco != 0 {
        let thick = ((size / 14.0).round() as i32).max(1);
        let (x0, x1) = ((it.x * scale).round() as i32, ((it.x + it.w) * scale).round() as i32);
        let mut line = |y: i32| fill_shape(t, clip, Rect { x0, y0: y, x1, y1: y + thick }, [0.0; 4], None, &solid(run.color));
        if run.deco & DECO_UNDERLINE != 0 {
            line(baseline + ((size * 0.1).round() as i32).max(1));
        }
        if run.deco & DECO_LINE_THROUGH != 0 {
            line(baseline - (size * 0.28).round() as i32);
        }
    }
}

/// Scales a premultiplied ARGB pixel by `k` / 256.
fn fade(s: u32, k: u32) -> u32 {
    ((s & 0x00FF_00FF) * k >> 8 & 0x00FF_00FF) | ((s >> 8 & 0x00FF_00FF) * k & 0xFF00_FF00)
}

/// Copies a picture already scaled to `r`, blending premultiplied alpha after
/// scaling every pixel by `opacity`.
fn draw_image(t: &mut Target, clip: Rect, pic: &Picture, r: Rect, opacity: f32) {
    let c = clip.intersect(r);
    if c.is_empty() {
        return;
    }
    let n = (c.x1 - c.x0) as usize;
    let k = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
    for y in c.y0..c.y1 {
        let src = &pic.px[((y - r.y0) as u32 * pic.w + (c.x0 - r.x0) as u32) as usize..][..n];
        let dst = &mut t.buf[(y * t.w + c.x0) as usize..][..n];
        for (d, &s) in dst.iter_mut().zip(src) {
            let s = if k == 256 { s } else { fade(s, k) };
            let a = s >> 24;
            if a == 255 {
                *d = s & 0x00FF_FFFF;
            } else if a > 0 {
                let ia = 256 - (a + (a >> 7));
                let rb = (s & 0x00FF_00FF) + (((*d & 0x00FF_00FF) * ia) >> 8 & 0x00FF_00FF);
                let g = (s & 0x0000_FF00) + (((*d & 0x0000_FF00) * ia) >> 8 & 0x0000_FF00);
                *d = rb | g;
            }
        }
    }
}

/// Paints the part of the document visible at `scroll` (CSS px) into `t`,
/// which is `scale` device pixels per CSS px.
pub fn paint(l: &Layout, fonts: &mut Fonts, images: &mut Images, t: &mut Target, scroll: f32, scale: f32) {
    let full = Rect { x0: 0, y0: 0, x1: t.w, y1: t.h };
    paint_part(l, fonts, images, t, scroll, scale, full);
}

/// Like `paint`, but only the pixels of `part`: the others keep what they had, and
/// each pixel inside comes out exactly as `paint` makes it. Images are looked up
/// for the whole frame's view of them, so the parts kept decoded do not depend on
/// `part`.
pub fn paint_part(l: &Layout, fonts: &mut Fonts, images: &mut Images, t: &mut Target, scroll: f32, scale: f32, part: Rect) {
    let full = Rect { x0: 0, y0: 0, x1: t.w, y1: t.h };
    let part = part.intersect(full);
    if part.is_empty() {
        return;
    }
    let canvas = l.canvas.0 & 0x00FF_FFFF;
    for y in part.y0..part.y1 {
        t.buf[(y * t.w + part.x0) as usize..(y * t.w + part.x1) as usize].fill(canvas);
    }
    let sy = (scroll * scale).round() as i32;
    let dev = |it: &Item| Rect {
        x0: (it.x * scale).round() as i32,
        y0: (it.y * scale).round() as i32 - sy,
        x1: ((it.x + it.w) * scale).round() as i32,
        y1: ((it.y + it.h) * scale).round() as i32 - sy,
    };
    // Where the whole image of an image item lies: the item itself, or for a
    // background image the tile it shows part of.
    let whole = |it: &Item, r: Rect| match l.pictures[it.data as usize].tile {
        Some([dx, dy, w, h]) => Rect {
            x0: ((it.x + dx) * scale).round() as i32,
            y0: ((it.y + dy) * scale).round() as i32 - sy,
            x1: ((it.x + dx + w) * scale).round() as i32,
            y1: ((it.y + dy + h) * scale).round() as i32 - sy,
        },
        None => r,
    };
    let (top, bottom) = (scroll - 4.0, scroll + t.h as f32 / scale + 4.0);
    // Decode every image this frame draws up front, in parallel.
    let mut wants: Vec<(u32, u32, u32)> = Vec::new();
    let (mut clips, mut clip): (Vec<Rect>, Rect) = (Vec::new(), full);
    for it in &l.items {
        match it.kind {
            Kind::Clip => {
                clips.push(clip);
                clip = clip.intersect(dev(it));
            }
            Kind::PopClip => clip = clips.pop().unwrap_or(full),
            Kind::Image if !(it.y + it.h < top || it.y > bottom || clip.is_empty()) => {
                let r = dev(it);
                if !r.is_empty() {
                    let r = whole(it, r);
                    wants.push((l.pictures[it.data as usize].id, (r.x1 - r.x0) as u32, (r.y1 - r.y0) as u32));
                }
            }
            _ => {}
        }
    }
    images.prepare(&wants);
    let mut clips: Vec<Rect> = Vec::new();
    let mut clip = full;
    for it in &l.items {
        match it.kind {
            Kind::Clip => {
                clips.push(clip);
                clip = clip.intersect(dev(it));
            }
            Kind::PopClip => clip = clips.pop().unwrap_or(full),
            Kind::Area => {}
            _ if it.y + it.h < top || it.y > bottom || clip.is_empty() || (part != full && reach(l, it, dev(it), scale).intersect(part).is_empty()) => {}
            Kind::Box => draw_box(t, clip.intersect(part), &l.boxes[it.data as usize], dev(it), scale),
            Kind::Text => draw_text(t, clip.intersect(part), l, fonts, it, sy, scale),
            Kind::Image => {
                let (item, r) = (dev(it), whole(it, dev(it)));
                if !item.is_empty() {
                    let clip = clip.intersect(item);
                    let v = clip.intersect(r);
                    let from = |a: i32, origin: i32| (a - origin).max(0) as u32;
                    let visible = [from(v.x0, r.x0), from(v.y0, r.y0), from(v.x1, r.x0), from(v.y1, r.y0)];
                    let p = l.pictures[it.data as usize];
                    if let Some((pic, x, y)) = images.get(p.id, (r.x1 - r.x0) as u32, (r.y1 - r.y0) as u32, visible) {
                        let (x0, y0) = (r.x0 + x as i32, r.y0 + y as i32);
                        draw_image(t, clip.intersect(part), pic, Rect { x0, y0, x1: x0 + pic.w as i32, y1: y0 + pic.h as i32 }, p.alpha);
                    }
                }
            }
        }
    }
}

/// The device pixels an item can paint: its box, and for text a margin of twice its
/// size around it, where glyphs and decorations may reach past the line box.
fn reach(l: &Layout, it: &Item, r: Rect, scale: f32) -> Rect {
    match it.kind {
        Kind::Text => r.grow((l.texts[it.data as usize].size * scale * 2.0).ceil() as i32 + 2),
        _ => r.grow(1),
    }
}

/// True when `a` and `b` paint the same pixels: the same kind, place and content.
fn same_item(old: &Layout, a: &Item, new: &Layout, b: &Item) -> bool {
    if a.kind != b.kind || (a.x, a.y, a.w, a.h) != (b.x, b.y, b.w, b.h) {
        return false;
    }
    let (i, j) = (a.data as usize, b.data as usize);
    match a.kind {
        Kind::Box => old.boxes[i] == new.boxes[j],
        Kind::Text => {
            fn text(l: &Layout, t: TextRun) -> &str {
                &l.text[t.start as usize..(t.start + t.len) as usize]
            }
            let (r, s) = (old.texts[i], new.texts[j]);
            TextRun { start: 0, ..r } == TextRun { start: 0, ..s } && text(old, r) == text(new, s)
        }
        Kind::Image => old.pictures[i] == new.pictures[j],
        Kind::Clip | Kind::PopClip | Kind::Area => true,
    }
}

/// Where the frames of `old` and `new` can differ, both painted at `scroll` into `w` x `h`
/// device pixels: the old and the new place of every item whose kind, place or content
/// changed. `None` asks for a full repaint, when the lists do not line up item for
/// item or the changes cover much of the frame.
pub fn damage(old: &Layout, new: &Layout, scroll: f32, scale: f32, w: i32, h: i32) -> Option<Vec<Rect>> {
    if old.items.len() != new.items.len() || old.canvas != new.canvas {
        return None;
    }
    let sy = (scroll * scale).round() as i32;
    let dev = |it: &Item| Rect {
        x0: (it.x * scale).round() as i32,
        y0: (it.y * scale).round() as i32 - sy,
        x1: ((it.x + it.w) * scale).round() as i32,
        y1: ((it.y + it.h) * scale).round() as i32 - sy,
    };
    let frame = Rect { x0: 0, y0: 0, x1: w, y1: h };
    let mut rects: Vec<Rect> = Vec::new();
    for (a, b) in old.items.iter().zip(&new.items) {
        if same_item(old, a, new, b) {
            continue;
        }
        for r in [reach(old, a, dev(a), scale), reach(new, b, dev(b), scale)] {
            let r = r.intersect(frame);
            if r.is_empty() {
                continue;
            }
            // Overlapping rectangles merge, so few are left to paint.
            merge(&mut rects, r);
        }
        if rects.len() > 16 || rects.iter().map(|r| r.area()).sum::<i64>() * 2 > frame.area() {
            return None;
        }
    }
    Some(rects)
}

/// Adds `r` to `rects`, merged with every rectangle it touches.
fn merge(rects: &mut Vec<Rect>, mut r: Rect) {
    while let Some(i) = rects.iter().position(|q| !q.intersect(r.grow(1)).is_empty()) {
        r = r.union(rects.swap_remove(i));
    }
    rects.push(r);
}

/// The parts to repaint so that `rects` come out exactly as in a full repaint, in a
/// frame `w` pixels wide: a part that meets a gradient spans the whole width, as a
/// gradient's pixels depend on where its rows start.
pub fn exact_parts(l: &Layout, scroll: f32, scale: f32, w: i32, rects: &[Rect]) -> Vec<Rect> {
    let sy = (scroll * scale).round() as i32;
    let gradients: Vec<Rect> = l
        .items
        .iter()
        .filter(|it| it.kind == Kind::Box && l.boxes[it.data as usize].grad.is_some())
        .map(|it| Rect {
            x0: (it.x * scale).round() as i32,
            y0: (it.y * scale).round() as i32 - sy,
            x1: ((it.x + it.w) * scale).round() as i32,
            y1: ((it.y + it.h) * scale).round() as i32 - sy,
        })
        .collect();
    let mut parts: Vec<Rect> = rects.to_vec();
    loop {
        let mut next: Vec<Rect> = Vec::new();
        for &r in &parts {
            let wide = gradients.iter().any(|g| !g.intersect(r).is_empty());
            merge(&mut next, if wide { Rect { x0: 0, x1: w, ..r } } else { r });
        }
        if next.len() == parts.len() && next.iter().all(|r| parts.contains(r)) {
            return next;
        }
        parts = next;
    }
}

/// A copy of what a layout paints, to compare the next frame's with.
pub fn copy(l: &Layout) -> Layout {
    Layout {
        items: l.items.clone(),
        boxes: l.boxes.clone(),
        texts: l.texts.clone(),
        pictures: l.pictures.clone(),
        text: l.text.clone(),
        width: l.width,
        height: l.height,
        canvas: l.canvas,
        record_boxes: l.record_boxes,
    }
}

/// Where the scrollbar can be drawn: the strip along the right edge.
pub fn scrollbar_strip(t: &Target, scale: f32) -> Rect {
    Rect { x0: t.w - (10.0 * scale) as i32 - 1, y0: 0, x1: t.w, y1: t.h }
}

/// Overlay scrollbar thumb, drawn only when the document overflows the view.
pub fn scrollbar(t: &mut Target, scroll: f32, doc_h: f32, scale: f32) {
    let view = t.h as f32 / scale;
    if doc_h <= view + 0.5 {
        return;
    }
    let thumb = (view * view / doc_h).max(32.0);
    let y = scroll / (doc_h - view) * (view - thumb - 4.0) + 2.0;
    let r = Rect {
        x0: t.w - (10.0 * scale) as i32,
        y0: (y * scale) as i32,
        x1: t.w - (3.0 * scale) as i32,
        y1: ((y + thumb) * scale) as i32,
    };
    let full = Rect { x0: 0, y0: 0, x1: t.w, y1: t.h };
    fill_shape(t, full, r, [3.5 * scale; 4], None, &Src::Solid { rgb: 0, a: 110 });
}
