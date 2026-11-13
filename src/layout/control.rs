//! Form controls (HTML 4.10): what an `<input>`, `<select>` or `<textarea>` draws in
//! place of children, and the size WebKit gives one whose `width` or `height` is auto,
//! from its font and its `size`, `cols` and `rows` attributes.

use super::{clamp_w, face_of, is_abs, line_extent, rs, sides, spec_w, BoxDeco, Engine, Item, Kind, NAN};
use crate::css::{Color, Len, TextAlign};
use crate::dom::{atom, Dom, NodeId, NONE};
use crate::css::Display;
use crate::style::Style;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Control {
    /// A one-line text field: its value, or else its placeholder.
    Field,
    /// A push button made of an `<input>`: its label.
    Button,
    /// A box the platform draws with no text: checkbox, radio button, slider,
    /// colour well, file picker, image button. The built-in sheet sizes it.
    Widget,
    /// A drop-down list: its selected option.
    Select,
    /// A multi-line text field.
    Area,
}

/// The kind of form control `n` is, if it is one whose content is replaced.
pub(super) fn control(dom: &Dom, n: NodeId) -> Option<Control> {
    match dom.nodes[n as usize].tag {
        atom::INPUT => {
            let ty = dom.attr(n, atom::TYPE).unwrap_or("").trim();
            let is = |t: &str| ty.eq_ignore_ascii_case(t);
            Some(if is("submit") || is("button") || is("reset") {
                Control::Button
            } else if ["checkbox", "radio", "range", "color", "file", "image"].into_iter().any(is) {
                Control::Widget
            } else {
                Control::Field
            })
        }
        atom::SELECT => Some(Control::Select),
        atom::TEXTAREA => Some(Control::Area),
        _ => None,
    }
}

/// True for an element sized like a replaced element: block-level with `width: auto`,
/// it is as wide as its content, as far as its container allows, and it keeps clear
/// of floats. WebKit treats buttons so too.
pub(super) fn sized_by_content(dom: &Dom, n: NodeId) -> bool {
    dom.shows_image(n) || dom.nodes[n as usize].tag == atom::BUTTON || control(dom, n).is_some()
}

/// The legend a fieldset draws on its top border: its first child element when that
/// is a `<legend>` that is shown, neither floated nor absolutely positioned.
pub(super) fn rendered_legend(dom: &Dom, st: &[Style], n: NodeId) -> NodeId {
    let mut c = dom.nodes[n as usize].first_child;
    while c != NONE && !dom.is_element(c) {
        c = dom.nodes[c as usize].next;
    }
    let shown = |c: NodeId| {
        let s = &st[c as usize];
        s.display != Display::None && s.float == 0 && !is_abs(s)
    };
    if c != NONE && dom.nodes[c as usize].tag == atom::LEGEND && shown(c) {
        c
    } else {
        NONE
    }
}

/// What the platform draws in place of the CSS background and border of a native
/// control `n` styled `s`: WebKit's thin-edged text fields, grey rounded push buttons
/// and lists, blue submit buttons and filled checked boxes. None draws the CSS.
pub(super) fn native_deco(dom: &Dom, n: NodeId, s: &Style) -> Option<BoxDeco> {
    let is = |t: &str| dom.attr(n, atom::TYPE).is_some_and(|ty| ty.trim().eq_ignore_ascii_case(t));
    let blue = Color(0xFF00_7AFF);
    let (bg, edge, radius) = match control(dom, n) {
        Some(Control::Field | Control::Area) => (s.bg, Some(Color(0xFFC0_C0C0)), Len::Px(2.0)),
        Some(Control::Button) if is("submit") => (blue, None, Len::Px(5.0)),
        None if dom.nodes[n as usize].tag == atom::BUTTON => (Color(0xFFEB_EBEB), None, Len::Px(5.0)),
        Some(Control::Button | Control::Select) => (Color(0xFFEB_EBEB), None, Len::Px(5.0)),
        Some(Control::Widget) if is("checkbox") || is("radio") => {
            let checked = dom.atoms.get("checked").is_some_and(|a| dom.attr(n, a).is_some());
            (if checked { blue } else { s.bg }, Some(if checked { blue } else { Color(0xFF80_8080) }), s.radius[0])
        }
        _ => return None,
    };
    let o = s.opacity;
    Some(BoxDeco {
        bg: bg.with_opacity(o),
        bw: [if edge.is_some() { 1.0 } else { 0.0 }; 4],
        bc: [edge.unwrap_or(Color::TRANSPARENT).with_opacity(o); 4],
        radius: [radius; 4],
        grad: None,
        shadow: None,
    })
}

/// A positive integer attribute such as `size`, or `default` without a valid one.
fn count(dom: &Dom, n: NodeId, name: u32, default: f32) -> f32 {
    dom.attr(n, name).and_then(|v| v.trim().parse::<u32>().ok()).filter(|&v| v > 0).map_or(default, |v| v as f32)
}

/// The label of a push button made of an `<input>`.
fn label(dom: &Dom, n: NodeId) -> &str {
    dom.attr(n, atom::VALUE).unwrap_or_else(|| match dom.attr(n, atom::TYPE).map(str::trim) {
        Some(t) if t.eq_ignore_ascii_case("submit") => "Submit",
        Some(t) if t.eq_ignore_ascii_case("reset") => "Reset",
        _ => "",
    })
}

/// The text of each `<option>` of a `<select>`, and which one is shown: the last
/// one marked `selected`, or else the first.
fn options(dom: &Dom, n: NodeId) -> (Vec<String>, usize) {
    let (mut texts, mut shown) = (Vec::new(), 0);
    let mut c = dom.next_in(n, n);
    while c != NONE {
        if dom.nodes[c as usize].tag == atom::OPTION {
            let text = match dom.attr(c, atom::LABEL) {
                Some(l) => l.to_string(),
                None => {
                    let mut t = String::new();
                    dom.text_content(c, &mut t);
                    t.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
                }
            };
            if dom.attr(c, atom::SELECTED).is_some() {
                shown = texts.len();
            }
            texts.push(text);
        }
        c = dom.next_in(c, n);
    }
    (texts, shown)
}

impl Engine<'_> {
    /// Pushes what paints fieldset `n`'s box: its background and borders without the
    /// top one, then the two pieces of the top border beside the legend. `place_frame`
    /// puts them where they go once the legend and the height are known.
    pub(super) fn fieldset_frame(&mut self, n: NodeId, s: &Style, [x, y, w]: [f32; 3]) -> u32 {
        let frame = self.push_box(n, s, x, y, w, 0.0);
        self.out.boxes[self.out.items[frame as usize].data as usize].bw[0] = 0.0;
        let piece = BoxDeco { bg: s.bc[0].with_opacity(s.opacity), bw: [0.0; 4], bc: [Color::TRANSPARENT; 4], radius: [Len::Px(0.0); 4], grad: None, shadow: None };
        for _ in 0..2 {
            self.push_deco(n, x, y, 0.0, 0.0, piece.clone());
        }
        frame
    }

    /// Lays out `legend` at the top of its fieldset, whose border box starts at `y` and
    /// whose content box starts at `cx` and is `cw` wide. Returns the height of its margin
    /// box and where that box starts and ends across.
    pub(super) fn lay_legend(&mut self, legend: NodeId, cx: f32, y: f32, cw: f32) -> (f32, f32, f32) {
        let s = &self.st[legend as usize];
        let sd = sides(s, cw);
        let w = match spec_w(s, cw, &sd) {
            Some(w) => w,
            None => clamp_w(s, (self.intrinsic(legend) - sd.ml - sd.mr).min(cw - sd.ml - sd.mr), cw, &sd),
        };
        let r = self.layout_box(legend, cx + sd.ml, y + sd.mt, w, cw, NAN, NAN, true);
        (r.h + sd.mt + sd.mb, cx, cx + sd.ml + w + sd.mr)
    }

    /// Moves the painted box of a fieldset `h` high down to the middle of its legend,
    /// whose margin box is `band` high from `x0` to `x1`, and draws the top border on
    /// each side of it.
    pub(super) fn place_frame(&mut self, frame: u32, s: &Style, h: f32, (band, x0, x1): (f32, f32, f32)) {
        let bt = s.bw[0];
        let drop = ((band - bt) / 2.0).max(0.0);
        let it = &mut self.out.items[frame as usize];
        it.y += drop;
        it.h = h - drop;
        let (x, y, w) = (it.x, it.y, it.w);
        let pieces = [(x, x0.max(x)), (x1.min(x + w), x + w)];
        for (k, (a, b)) in pieces.into_iter().enumerate() {
            let p = &mut self.out.items[frame as usize + 1 + k];
            (p.x, p.y, p.w, p.h) = (a, y, (b - a).max(0.0), bt);
        }
    }

    /// The content-box width of control `n` when its `width` is auto. Text fields are
    /// as wide as `size` (or `cols`) times the advance of "0", which WebKit takes for
    /// the average character width of the system font.
    pub(super) fn control_width(&mut self, n: NodeId, s: &Style, c: Control) -> f32 {
        let dom = self.dom;
        let face = face_of(self.fonts, s);
        let zero = self.fonts.lookup(face, '0').2 * s.font_size;
        match c {
            Control::Field => (zero * count(dom, n, atom::SIZE, 20.0)).ceil(),
            Control::Area => (zero * count(dom, n, atom::COLS, 20.0)).ceil(),
            Control::Button => self.fonts.measure(face, label(dom, n), s.font_size, s.spacing),
            Control::Select => options(dom, n).0.iter().map(|t| self.fonts.measure(face, t, s.font_size, s.spacing)).fold(0.0, f32::max),
            Control::Widget => 0.0,
        }
    }

    /// Lays out what control `n` shows in its content box at (`cx`, `cy`), `cw` wide
    /// and `inner_h` high (NaN when auto), and returns the content height: one line,
    /// `rows` lines for a `<textarea>`. One line is centred in a taller box, and the
    /// control's baseline is that of its first line.
    pub(super) fn control_content(&mut self, n: NodeId, s: &Style, c: Control, [cx, cy, cw, inner_h]: [f32; 4]) -> f32 {
        let dom = self.dom;
        let face = face_of(self.fonts, s);
        let (asc, desc, gap) = self.fonts.metrics(face);
        let (above, below) = line_extent(s, asc, desc, gap);
        let lh = above + below;
        let h = match c {
            Control::Widget => 0.0,
            Control::Area => lh * count(dom, n, atom::ROWS, 2.0),
            _ => lh,
        };
        let box_h = if inner_h.is_nan() { h } else { inner_h };
        let top = if c == Control::Area { cy } else { cy + (box_h - lh) / 2.0 };
        match c {
            // WebKit puts a checkbox's baseline 2px above its bottom border edge, a
            // slider's on that edge and a colour well's 3px above it; a file picker's
            // is that of its button's label.
            Control::Widget => {
                let bottom = cy + box_h + rs(s.padding[2], 0.0) + s.bw[2];
                let is = |t: &str| dom.attr(n, atom::TYPE).is_some_and(|ty| ty.trim().eq_ignore_ascii_case(t));
                if is("checkbox") || is("radio") {
                    self.last_bl = bottom - 2.0;
                } else if is("range") {
                    self.last_bl = bottom;
                } else if is("color") {
                    self.last_bl = bottom - 3.0;
                } else if is("file") {
                    self.last_bl = top + above;
                }
                return 0.0;
            }
            // A `<textarea>` sits on its bottom margin edge, as an inline-block that clips.
            Control::Area => {}
            _ => self.last_bl = top + above,
        }
        let placeholder = Color(0xFFA9_A9A9);
        let mut owned = String::new();
        let (text, color) = match c {
            // WebKit draws the label of a blue submit button in white.
            Control::Button if s.native && dom.attr(n, atom::TYPE).is_some_and(|t| t.trim().eq_ignore_ascii_case("submit")) => (label(dom, n), Color::WHITE),
            Control::Button => (label(dom, n), s.color),
            Control::Select => {
                let (texts, shown) = options(dom, n);
                owned = texts.into_iter().nth(shown).unwrap_or_default();
                (owned.as_str(), s.color)
            }
            Control::Area => {
                dom.text_content(n, &mut owned);
                match owned.is_empty() {
                    true => (dom.attr(n, atom::PLACEHOLDER).unwrap_or(""), placeholder),
                    false => (owned.as_str(), s.color),
                }
            }
            _ => match dom.attr(n, atom::VALUE) {
                Some(v) if !v.is_empty() => {
                    if dom.attr(n, atom::TYPE).is_some_and(|t| t.trim().eq_ignore_ascii_case("password")) {
                        owned = "\u{2022}".repeat(v.chars().count());
                        (owned.as_str(), s.color)
                    } else {
                        (v, s.color)
                    }
                }
                _ => (dom.attr(n, atom::PLACEHOLDER).unwrap_or(""), placeholder),
            },
        };
        if text.is_empty() || !s.visible {
            return h;
        }
        // The text of a field is cut at its content box; a button's label is not.
        let clip = c != Control::Button;
        if clip {
            let data = self.out.boxes.len() as u32;
            self.out.boxes.push(BoxDeco { bg: Color::TRANSPARENT, bw: [0.0; 4], bc: [Color::TRANSPARENT; 4], radius: [Len::Px(0.0); 4], grad: None, shadow: None });
            self.out.items.push(Item { x: cx, y: cy, w: cw, h: box_h, kind: Kind::Clip, node: n, data });
        }
        let color = color.with_opacity(s.opacity);
        for (i, line) in text.split('\n').enumerate() {
            let y = top + i as f32 * lh;
            if i > 0 && (c != Control::Area || y >= cy + box_h) {
                break;
            }
            let w = self.fonts.measure(face, line, s.font_size, s.spacing);
            let free = cw - w;
            let dx = match s.text_align {
                TextAlign::Left => 0.0,
                TextAlign::Center => free / 2.0,
                TextAlign::Right => free,
            };
            let dx = if c == Control::Button { dx } else { dx.max(0.0) };
            self.emit_text(n, line, cx + dx, y + above, s, color);
        }
        if clip {
            self.out.items.push(Item { x: 0.0, y: 0.0, w: 0.0, h: 0.0, kind: Kind::PopClip, node: n, data: 0 });
        }
        h
    }
}
