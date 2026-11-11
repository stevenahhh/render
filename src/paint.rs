//! Software rasterizer: paints a `Layout` into a 0x00RRGGBB pixel buffer.

use crate::css::{Color, Gradient, Len, DECO_LINE_THROUGH, DECO_UNDERLINE};
use crate::font::Fonts;
use crate::img::{Images, Picture};
use crate::layout::{BoxDeco, Item, Kind, Layout};

pub struct Target<'a> {
    pub buf: &'a mut [u32],
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Copy)]
struct Rect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Rect {
    fn intersect(self, o: Rect) -> Rect {
        Rect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) }
    }
    fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
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

/// Coverage of the pixel centred at (px, py) by a rounded rectangle, from
/// its signed distance.
#[inline]
fn rr_cov(px: f32, py: f32, r: (f32, f32, f32, f32), rad: f32) -> f32 {
    let (hw, hh) = ((r.2 - r.0) * 0.5, (r.3 - r.1) * 0.5);
    let qx = (px - (r.0 + hw)).abs() - (hw - rad);
    let qy = (py - (r.1 + hh)).abs() - (hh - rad);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    let d = (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - rad;
    (0.5 - d).clamp(0.0, 1.0)
}

/// Fills a rectangle with optional rounded corners and an optional rounded
/// hole (for border rings). Only the corner columns are evaluated per pixel;
/// everything else is filled as solid spans.
fn fill_shape(t: &mut Target, clip: Rect, r: Rect, rad: f32, hole: Option<(Rect, f32)>, src: &Src) {
    let c = clip.intersect(r);
    if c.is_empty() {
        return;
    }
    if rad < 0.5 && hole.is_none() {
        for y in c.y0..c.y1 {
            span(t, y, c.x0, c.x1, src);
        }
        return;
    }
    let outer = (r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32);
    let rz = rad.ceil() as i32;
    let (mut lz, mut rzs) = (r.x0 + rz, r.x1 - rz);
    let inner = hole.map(|(h, hr)| {
        let hz = hr.ceil() as i32;
        lz = lz.max(h.x0 + hz);
        rzs = rzs.min(h.x1 - hz);
        ((h.x0 as f32, h.y0 as f32, h.x1 as f32, h.y1 as f32), hr, h)
    });
    if lz > rzs {
        lz = r.x1;
        rzs = r.x1;
    }
    for y in c.y0..c.y1 {
        let py = y as f32 + 0.5;
        let in_hole = inner.is_some_and(|(_, _, h)| y >= h.y0 && y < h.y1);
        let band = py < outer.1 + rad || py > outer.3 - rad;
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

fn draw_box(t: &mut Target, clip: Rect, d: &BoxDeco, r: Rect, scale: f32) {
    let (w, h) = ((r.x1 - r.x0) as f32, (r.y1 - r.y0) as f32);
    let rad = match d.radius {
        Len::Px(v) => v * scale,
        Len::Pct(p) => p * 0.01 * w.min(h),
        _ => 0.0,
    }
    .min(w.min(h) / 2.0);

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
    if rad < 0.5 {
        let edges = [
            Rect { x0: r.x0, y0: r.y0, x1: r.x1, y1: r.y0 + bt },
            Rect { x0: r.x1 - br, y0: r.y0 + bt, x1: r.x1, y1: r.y1 - bb },
            Rect { x0: r.x0, y0: r.y1 - bb, x1: r.x1, y1: r.y1 },
            Rect { x0: r.x0, y0: r.y0 + bt, x1: r.x0 + bl, y1: r.y1 - bb },
        ];
        for (e, c) in edges.into_iter().zip(d.bc) {
            if c.a() > 0 {
                fill_shape(t, clip, e, 0.0, None, &solid(c));
            }
        }
        return;
    }
    let side = bw.iter().position(|w| *w > 0).unwrap();
    let hole = Rect { x0: r.x0 + bl, y0: r.y0 + bt, x1: r.x1 - br, y1: r.y1 - bb };
    let widest = *bw.iter().max().unwrap() as f32;
    let hole = (!hole.is_empty()).then_some((hole, (rad - widest).max(0.0)));
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
        let mut line = |y: i32| fill_shape(t, clip, Rect { x0, y0: y, x1, y1: y + thick }, 0.0, None, &solid(run.color));
        if run.deco & DECO_UNDERLINE != 0 {
            line(baseline + ((size * 0.1).round() as i32).max(1));
        }
        if run.deco & DECO_LINE_THROUGH != 0 {
            line(baseline - (size * 0.28).round() as i32);
        }
    }
}

/// Copies a picture already scaled to `r`, blending premultiplied alpha.
fn draw_image(t: &mut Target, clip: Rect, pic: &Picture, r: Rect) {
    let c = clip.intersect(r);
    if c.is_empty() {
        return;
    }
    let n = (c.x1 - c.x0) as usize;
    for y in c.y0..c.y1 {
        let src = &pic.px[((y - r.y0) as u32 * pic.w + (c.x0 - r.x0) as u32) as usize..][..n];
        let dst = &mut t.buf[(y * t.w + c.x0) as usize..][..n];
        for (d, &s) in dst.iter_mut().zip(src) {
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
    t.buf.fill(l.canvas.0 & 0x00FF_FFFF);
    let sy = (scroll * scale).round() as i32;
    let full = Rect { x0: 0, y0: 0, x1: t.w, y1: t.h };
    let dev = |it: &Item| Rect {
        x0: (it.x * scale).round() as i32,
        y0: (it.y * scale).round() as i32 - sy,
        x1: ((it.x + it.w) * scale).round() as i32,
        y1: ((it.y + it.h) * scale).round() as i32 - sy,
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
                    wants.push((it.data, (r.x1 - r.x0) as u32, (r.y1 - r.y0) as u32));
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
            _ if it.y + it.h < top || it.y > bottom || clip.is_empty() => {}
            Kind::Box => draw_box(t, clip, &l.boxes[it.data as usize], dev(it), scale),
            Kind::Text => draw_text(t, clip, l, fonts, it, sy, scale),
            Kind::Image => {
                let r = dev(it);
                if !r.is_empty() {
                    if let Some(pic) = images.get(it.data, (r.x1 - r.x0) as u32, (r.y1 - r.y0) as u32) {
                        draw_image(t, clip, pic, r);
                    }
                }
            }
        }
    }
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
    fill_shape(t, full, r, 3.5 * scale, None, &Src::Solid { rgb: 0, a: 110 });
}
