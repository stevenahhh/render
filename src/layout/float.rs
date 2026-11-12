//! Floats (CSS 2.1 section 9.5): boxes taken out of the flow and put at the left
//! or right of their block formatting context, beside which line boxes shorten
//! and boxes that establish a formatting context of their own are placed.

use super::{clamp_w, sides, spec_w, Engine, Inl, Res, FIT, NAN};
use crate::css::{CLEAR_LEFT, CLEAR_RIGHT, FLOAT_LEFT};
use crate::dom::NodeId;

/// A float's margin box, in document coordinates.
#[derive(Clone, Copy)]
pub(super) struct FloatBox {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    left: bool,
}

impl Engine<'_> {
    /// Starts a block formatting context; returns what `leave_bfc` takes.
    pub(super) fn enter_bfc(&mut self) -> usize {
        std::mem::replace(&mut self.bfc, self.floats.len())
    }

    /// Ends the innermost block formatting context and drops its floats; returns
    /// the bottom of the lowest one, or minus infinity without floats.
    pub(super) fn leave_bfc(&mut self, outer: usize) -> f32 {
        if self.floats.len() == self.bfc {
            self.bfc = outer;
            return f32::NEG_INFINITY;
        }
        let bottom = self.floats[self.bfc..].iter().map(|f| f.y + f.h).fold(f32::NEG_INFINITY, f32::max);
        self.floats.truncate(self.bfc);
        self.bfc = outer;
        bottom
    }

    /// The floats of the current context that overlap `[y, y + h)`.
    fn overlapping(&self, y: f32, h: f32) -> impl Iterator<Item = &FloatBox> {
        self.floats[self.bfc..].iter().filter(move |f| f.y < y + h.max(FIT) && f.y + f.h > y)
    }

    /// The part of `[cx, cx + cw]` beside the floats that overlap `[y, y + h)`.
    pub(super) fn band(&self, y: f32, h: f32, cx: f32, cw: f32) -> (f32, f32) {
        let (mut l, mut r) = (cx, cx + cw);
        for f in self.overlapping(y, h) {
            if f.left {
                l = l.max(f.x + f.w);
            } else {
                r = r.min(f.x);
            }
        }
        (l, r.max(l))
    }

    /// The highest top from `y` down at which `[top, top + h)` leaves `w` of
    /// `[cx, cx + cw]` beside the floats, with that band: below the floats when
    /// nothing narrower fits.
    fn fit(&self, mut y: f32, h: f32, w: f32, cx: f32, cw: f32) -> (f32, f32, f32) {
        loop {
            let (l, r) = self.band(y, h, cx, cw);
            if r - l + FIT >= w {
                return (y, l, r);
            }
            match self.overlapping(y, h).map(|f| f.y + f.h).min_by(f32::total_cmp) {
                Some(bottom) if bottom > y => y = bottom,
                _ => return (y, l, r),
            }
        }
    }

    /// The lowest bottom of the floats a box with `clear` must be below.
    pub(super) fn clear_y(&self, clear: u8) -> f32 {
        let side = |f: &FloatBox| if f.left { CLEAR_LEFT } else { CLEAR_RIGHT };
        self.floats[self.bfc..].iter().filter(|f| clear & side(f) != 0).map(|f| f.y + f.h).fold(f32::NEG_INFINITY, f32::max)
    }

    /// Border-box width of float `n` in a containing block `cw` wide: its width,
    /// or its content's up to what fits.
    fn float_width(&mut self, n: NodeId, cw: f32) -> f32 {
        let st = self.st;
        let s = &st[n as usize];
        let sd = sides(s, cw);
        match spec_w(s, cw, &sd) {
            Some(w) => w,
            None => {
                let content = self.intrinsic(n) - sd.ml - sd.mr;
                clamp_w(s, content.min((cw - sd.ml - sd.mr).max(0.0)), cw, &sd)
            }
        }
    }

    /// Lays out float `n` and puts it in `[cx, cx + cw]` at `y` or below, as far
    /// left or right as the floats before it allow (CSS 2.1 section 9.5.1).
    pub(super) fn place_float(&mut self, n: NodeId, cx: f32, cw: f32, y: f32, ch: f32) {
        let st = self.st;
        let s = &st[n as usize];
        let sd = sides(s, cw);
        let w = self.float_width(n, cw);
        // Not above an earlier float, nor beside the floats it clears.
        let mut top = self.floats[self.bfc..].iter().map(|f| f.y).fold(y, f32::max);
        if s.clear != 0 {
            top = top.max(self.clear_y(s.clear));
        }
        let (first, abs) = (self.mark(), self.abs.len());
        let r = self.layout_box(n, cx + sd.ml, top + sd.mt, w, cw, ch, NAN, true);
        let (ow, oh) = (w + sd.ml + sd.mr, r.h + sd.mt + sd.mb);
        let (fy, l, right) = self.fit(top, oh, ow, cx, cw);
        let left = s.float == FLOAT_LEFT;
        let fx = if left { l } else { right - ow };
        let (dx, dy) = (fx - cx, fy - top);
        let end = self.mark();
        self.shift(first, end, dx, dy);
        for a in &mut self.abs[abs..] {
            (a.1, a.2) = (a.1 + dx, a.2 + dy);
        }
        self.floats.push(FloatBox { x: fx, y: fy, w: ow, h: oh, left });
    }

    /// A float met in a line of inline content: put at the top of the line when it
    /// fits beside what the line holds, else below the line.
    pub(super) fn inline_float(&mut self, il: &mut Inl, n: NodeId) {
        if il.content {
            let s = &self.st[n as usize];
            let sd = sides(s, il.cw);
            let ow = self.float_width(n, il.cw) + sd.ml + sd.mr;
            if il.x + ow > il.right + FIT {
                il.queued.push(n);
                return;
            }
        }
        self.place_float(n, il.cx, il.cw, il.y, NAN);
        if il.begun {
            // The line moves right past a new left float, and ends before a right one.
            let (l, r) = self.band(il.y, il.above + il.below, il.cx, il.cw);
            let d = (l - il.cx - il.lx).max(0.0);
            if d > 0.0 {
                for f in &il.frags {
                    self.shift(f.first, f.end, d, 0.0);
                }
                for o in &mut il.open {
                    o.start_x += d;
                }
                il.x += d;
                il.lx += d;
            }
            il.right = r - il.cx;
        }
    }

    /// Moves a line that has not begun down past floats until `need` fits beside them.
    #[inline]
    pub(super) fn fit_line(&self, il: &mut Inl, need: f32) {
        if !il.begun && self.floats.len() > self.bfc {
            self.move_line_down(il, need);
        }
    }

    #[cold]
    #[inline(never)]
    fn move_line_down(&self, il: &mut Inl, need: f32) {
        il.y = self.fit(il.y, il.above + il.below, need, il.cx, il.cw).0;
    }

    /// Starts a line beside the floats of the context: it begins after the left
    /// ones and ends before the right ones.
    #[cold]
    #[inline(never)]
    pub(super) fn shorten_line(&self, il: &mut Inl) {
        let (l, r) = self.band(il.y, il.above + il.below, il.cx, il.cw);
        (il.lx, il.right) = (l - il.cx, r - il.cx);
        il.x += il.lx;
    }

    /// Places the floats a line that just ended left for below it.
    #[cold]
    #[inline(never)]
    pub(super) fn place_queued(&mut self, il: &mut Inl) {
        let queued = std::mem::take(&mut il.queued);
        for &f in &queued {
            self.place_float(f, il.cx, il.cw, il.y, NAN);
        }
        il.queued = queued;
        il.queued.clear();
    }

    /// Lays out a box in normal flow that establishes a formatting context of its
    /// own, or is replaced: its border box goes beside the floats, as high as its
    /// width fits.
    pub(super) fn beside_floats(&mut self, n: NodeId, cx: f32, cw: f32, y: f32, ch: f32) -> Res {
        let st = self.st;
        let s = &st[n as usize];
        let sd = sides(s, cw);
        let fixed = spec_w(s, cw, &sd).or_else(|| self.dom.shows_image(n).then(|| self.intrinsic(n) - sd.ml - sd.mr));
        let need = fixed.map_or(0.0, |w| w + sd.ml + sd.mr);
        let (top, l, r) = self.fit(y, FIT, need, cx, cw);
        let w = fixed.unwrap_or_else(|| clamp_w(s, (r - l - sd.ml - sd.mr).max(0.0), cw, &sd));
        let mut res = self.layout_box(n, l + sd.ml, top, w, cw, ch, NAN, false);
        res.h += top - y;
        res
    }
}
