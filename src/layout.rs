//! Layout: block flow, inline line boxes, flexbox and positioned boxes.
//!
//! Layout walks the DOM once and appends paint-ordered items with absolute
//! document coordinates. A box whose final position is only known after its
//! content is measured (line alignment, flex alignment, `bottom`) is laid out
//! at a provisional position and its item range is shifted afterwards.

use crate::css::*;
use crate::dom::{atom, Dom, NodeId, NONE};
use crate::font::{Fonts, Seg};
use crate::img::Images;
use crate::FxMap;
use crate::style::Style;
use std::rc::Rc;

mod float;
mod grid;
use float::FloatBox;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Box,
    Text,
    Clip,
    PopClip,
    /// `data` indexes `Layout::pictures`.
    Image,
    /// Border box of an element that paints nothing itself; only present
    /// when `Layout::record_boxes` is set.
    Area,
}

/// `data` indexes `Layout::boxes` for boxes and `Layout::texts` for text.
#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub kind: Kind,
    pub node: u32,
    pub data: u32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct BoxDeco {
    pub bg: Color,
    pub bw: [f32; 4],
    pub bc: [Color; 4],
    pub radius: [Len; 4],
    pub grad: Option<Rc<Gradient>>,
}

/// What an image item draws: a picture in `Images` and the opacity it is drawn at.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PicRef {
    pub id: u32,
    pub alpha: f32,
    /// For a background image, where the whole image lies, `[x, y, w, h]` from the
    /// item's origin: the item covers only the part inside the element's box.
    pub tile: Option<[f32; 4]>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TextRun {
    pub start: u32,
    pub len: u32,
    pub face: u8,
    pub deco: u8,
    pub size: f32,
    pub color: Color,
    pub spacing: f32,
    /// Baseline offset from the item's top.
    pub ascent: f32,
}

#[derive(Default)]
pub struct Layout {
    pub items: Vec<Item>,
    pub boxes: Vec<BoxDeco>,
    pub texts: Vec<TextRun>,
    pub pictures: Vec<PicRef>,
    pub text: String,
    pub width: f32,
    pub height: f32,
    pub canvas: Color,
    /// Keep the geometry of every box, for scripts and inspection.
    pub record_boxes: bool,
}

/// The attributes layout reads. A change of any other attribute, outside an
/// inline `<svg>` whose markup is drawn whole, leaves the layout as it was.
pub const READ_ATTRS: [u32; 11] = [
    atom::START,
    atom::ALT,
    atom::VALUE,
    atom::PLACEHOLDER,
    atom::TYPE,
    atom::MEDIA,
    atom::SRC,
    atom::SRCSET,
    atom::POSTER,
    atom::WIDTH,
    atom::HEIGHT,
];

impl Layout {
    /// Gives the items of `nodes` the colours of their styles in `st`, for styles
    /// that changed only as `Change::Paint` allows. False, with nothing changed,
    /// when one of them draws more than its own colours: replaced content, a list
    /// marker, generated content, which is credited to its element but styled on
    /// its own, or the canvas, which takes the background of `<html>` or `<body>`.
    pub fn recolour(&mut self, dom: &Dom, st: &[Style], nodes: &[NodeId]) -> bool {
        let mut set: FxMap<NodeId, ()> = FxMap::default();
        for &n in nodes {
            let (tag, s) = (dom.nodes[n as usize].tag, &st[n as usize]);
            let marker = s.display == Display::ListItem && s.list_style != ListStyle::None;
            let generated = s.rare().before.is_some() || s.rare().after.is_some();
            if marker || generated || matches!(tag, atom::INPUT | atom::HTML | atom::BODY) || dom.shows_image(n) {
                return false;
            }
            set.insert(n, ());
        }
        for it in &self.items {
            if !set.contains_key(&it.node) {
                continue;
            }
            let s = &st[it.node as usize];
            let o = s.opacity;
            match it.kind {
                Kind::Box => {
                    let d = &mut self.boxes[it.data as usize];
                    (d.bg, d.bc, d.radius, d.grad) = (s.bg.with_opacity(o), s.bc.map(|c| c.with_opacity(o)), s.radius, s.grad.clone());
                }
                Kind::Text => {
                    let t = &mut self.texts[it.data as usize];
                    (t.color, t.deco) = (s.color.with_opacity(o), s.deco);
                }
                Kind::Image => self.pictures[it.data as usize].alpha = o,
                Kind::Clip | Kind::PopClip | Kind::Area => {}
            }
        }
        true
    }

    /// The first way this layout differs from `other` in what it draws or tells
    /// scripts, for checks.
    pub fn difference(&self, other: &Layout) -> Option<String> {
        if (self.width, self.height, self.canvas) != (other.width, other.height, other.canvas) || self.text != other.text {
            return Some(format!("size {}x{} canvas {:?}, expected {}x{} canvas {:?}, or text", self.width, self.height, self.canvas, other.width, other.height, other.canvas));
        }
        if self.items.len() != other.items.len() {
            return Some(format!("{} items, expected {}", self.items.len(), other.items.len()));
        }
        for (i, (a, b)) in self.items.iter().zip(&other.items).enumerate() {
            let geometry = |it: &Item| [it.x, it.y, it.w, it.h].map(f32::to_bits);
            let data = match a.kind {
                Kind::Box => format!("{:?}", self.boxes[a.data as usize]) == format!("{:?}", other.boxes[b.data as usize]),
                Kind::Text => self.texts[a.data as usize] == other.texts[b.data as usize],
                Kind::Image => self.pictures[a.data as usize] == other.pictures[b.data as usize],
                Kind::Clip | Kind::PopClip | Kind::Area => true,
            };
            if a.kind != b.kind || a.node != b.node || geometry(a) != geometry(b) || !data {
                return Some(format!("item {i}: {a:?}, expected {b:?}"));
            }
        }
        None
    }
}

const NAN: f32 = f32::NAN;
/// Slack for float error when deciding whether content still fits a line.
const FIT: f32 = 0.05;

fn rs(l: Len, base: f32) -> f32 {
    match l {
        Len::Px(v) => v,
        Len::Pct(p) => p * base * 0.01,
        Len::Calc(p, v) => p * base * 0.01 + v,
        _ => 0.0,
    }
}

fn definite(l: Len, base: f32) -> Option<f32> {
    match l {
        Len::Auto => None,
        Len::Px(v) => Some(v),
        _ if base.is_nan() => None,
        l => Some(rs(l, base)),
    }
}

fn collapse(a: f32, b: f32) -> f32 {
    if a >= 0.0 && b >= 0.0 {
        a.max(b)
    } else if a < 0.0 && b < 0.0 {
        a.min(b)
    } else {
        a + b
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\n' | b'\t' | b'\r' | 0x0c)
}

fn is_ws(s: &str) -> bool {
    s.bytes().all(is_space)
}

/// Scripts that allow a line break between any two characters.
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0xD7FF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// The face text in this style is drawn with.
fn face_of(fonts: &mut Fonts, s: &Style) -> u8 {
    fonts.face(s.families.as_deref(), s.family, s.weight, s.italic)
}

fn is_abs(s: &Style) -> bool {
    matches!(s.position, Position::Absolute | Position::Fixed)
}

/// True for a box in normal flow that establishes a block formatting context of
/// its own, which holds its floats and keeps clear of those outside.
fn bfc_root(s: &Style) -> bool {
    s.overflow_hidden || s.flow_root || matches!(s.display, Display::Flex | Display::InlineFlex)
}

/// Line box extent above and below the baseline, with half-leading applied.
fn line_extent(s: &Style, asc: f32, desc: f32, gap: f32) -> (f32, f32) {
    let size = s.font_size;
    let lh = match s.line_height {
        LineHeight::Normal => (asc + desc + gap) * size,
        LineHeight::Number(n) => n * size,
        LineHeight::Len(l) => rs(l, size),
    };
    let half = (lh - (asc + desc) * size) / 2.0;
    (asc * size + half, desc * size + half)
}

#[derive(Clone, Copy, Default)]
struct Sides {
    mt: f32,
    mr: f32,
    mb: f32,
    ml: f32,
    pt: f32,
    pr: f32,
    pb: f32,
    pl: f32,
    auto_l: bool,
    auto_r: bool,
}

fn sides(s: &Style, base: f32) -> Sides {
    Sides {
        mt: rs(s.margin[0], base),
        mr: rs(s.margin[1], base),
        mb: rs(s.margin[2], base),
        ml: rs(s.margin[3], base),
        pt: rs(s.padding[0], base).max(0.0),
        pr: rs(s.padding[1], base).max(0.0),
        pb: rs(s.padding[2], base).max(0.0),
        pl: rs(s.padding[3], base).max(0.0),
        auto_l: s.margin[3] == Len::Auto,
        auto_r: s.margin[1] == Len::Auto,
    }
}

/// Horizontal padding plus border.
fn hextra(s: &Style, sd: &Sides) -> f32 {
    sd.pl + sd.pr + s.bw[1] + s.bw[3]
}

fn clamp_w(s: &Style, w: f32, cbw: f32, sd: &Sides) -> f32 {
    let extra = if s.border_box { 0.0 } else { hextra(s, sd) };
    let mut w = w;
    if s.max_w != Len::Auto {
        w = w.min(rs(s.max_w, cbw) + extra);
    }
    if s.min_w != Len::Auto {
        w = w.max(rs(s.min_w, cbw) + extra);
    }
    w.max(0.0)
}

/// Border-box width when `width` is not auto.
fn spec_w(s: &Style, cbw: f32, sd: &Sides) -> Option<f32> {
    if s.width == Len::Auto {
        return None;
    }
    let v = rs(s.width, cbw);
    Some(clamp_w(s, if s.border_box { v } else { v + hextra(s, sd) }, cbw, sd))
}

struct Res {
    h: f32,
    /// Bottom margin that still collapses with what follows.
    mb: f32,
    item: u32,
    /// Last line's baseline from the border-box top, NaN without lines.
    baseline: f32,
}

const F_TEXT: u8 = 0;
const F_ATOMIC: u8 = 1;
const F_DECO: u8 = 2;

/// A piece of the current line: an item range plus how it sits on the baseline.
struct LFrag {
    first: u32,
    end: u32,
    above: f32,
    below: f32,
    top: f32,
    kind: u8,
}

/// An inline element whose box is still open on the current line.
struct Open {
    node: NodeId,
    item: u32,
    start_x: f32,
    has_deco: bool,
    top: f32,
    h: f32,
}

#[derive(Default)]
struct Inl {
    cx: f32,
    cw: f32,
    x: f32,
    y: f32,
    above: f32,
    below: f32,
    align: f32,
    space_w: f32,
    begun: bool,
    content: bool,
    space: bool,
    wrap: bool,
    last_text: u32,
    /// A break is allowed before the next unit even without pending whitespace.
    brk: bool,
    frags: Vec<LFrag>,
    open: Vec<Open>,
    /// Where the current line starts and how far it may reach, from `cx`: floats
    /// beside it shorten it.
    lx: f32,
    right: f32,
    /// Floats met on the current line that did not fit beside its content; they
    /// are placed below it.
    queued: Vec<NodeId>,
}

#[derive(Clone, Copy)]
struct TStyle {
    node: NodeId,
    face: u8,
    size: f32,
    color: Color,
    deco: u8,
    spacing: f32,
    asc: f32,
    desc: f32,
    above: f32,
    below: f32,
    space_w: f32,
    visible: bool,
    transform: TextTransform,
}

struct FItem {
    node: NodeId,
    text: bool,
    sd: Sides,
    align: Align,
    grow: f32,
    shrink: f32,
    base: f32,
    w: f32,
    h: f32,
    first: u32,
    end: u32,
    item: u32,
}

/// Lengths of the layout output at one point, for [`Engine::rewind`].
#[derive(Clone, Copy)]
struct Snapshot {
    items: usize,
    boxes: usize,
    texts: usize,
    pictures: usize,
    text: usize,
    abs: usize,
    zranges: usize,
    floats: usize,
    next_ord: u32,
    last_bl: f32,
}

#[derive(Default)]
struct IState {
    cur: f32,
    max: f32,
    space: bool,
    content: bool,
}

impl IState {
    fn break_line(&mut self) {
        self.max = self.max.max(self.cur);
        self.cur = 0.0;
        self.space = false;
        self.content = false;
    }
}

struct Engine<'a> {
    dom: &'a Dom,
    st: &'a [Style],
    fonts: &'a mut Fonts,
    images: &'a mut Images,
    /// Resolved picture per replaced element: (image id, width, height).
    pics: FxMap<NodeId, Option<(u32, f32, f32)>>,
    /// Replaced elements whose image is still downloading.
    waiting: FxMap<NodeId, ()>,
    /// Positioned boxes not yet ordered by a stacking context: (first item, end, z-index).
    zranges: Vec<(u32, u32, i32)>,
    vw: f32,
    vh: f32,
    out: Layout,
    /// Out-of-flow boxes waiting for their containing block: (node, static x, static y).
    abs: Vec<(NodeId, f32, f32)>,
    /// Memoized max-content widths (margin box), NaN until computed.
    intr: Vec<f32>,
    segs: Vec<Seg>,
    pool: Vec<Inl>,
    last_bl: f32,
    next_ord: u32,
    /// Floats placed so far, in the block formatting contexts being laid out.
    floats: Vec<FloatBox>,
    /// Where the floats of the innermost block formatting context start in `floats`.
    bfc: usize,
}

pub fn layout(dom: &Dom, st: &[Style], fonts: &mut Fonts, images: &mut Images, vw: f32, vh: f32, mut reuse: Layout) -> Layout {
    reuse.items.clear();
    reuse.boxes.clear();
    reuse.texts.clear();
    reuse.pictures.clear();
    reuse.text.clear();
    let mut e = Engine {
        dom,
        st,
        fonts,
        images,
        pics: FxMap::default(),
        waiting: FxMap::default(),
        zranges: Vec::new(),
        vw,
        vh,
        out: reuse,
        abs: Vec::new(),
        intr: vec![NAN; dom.nodes.len()],
        segs: Vec::new(),
        pool: Vec::new(),
        last_bl: NAN,
        next_ord: 1,
        floats: Vec::new(),
        bfc: 0,
    };
    let (h, _) = e.layout_children(0, 0.0, vw, 0.0, vh, false, false);
    e.layout_abs_children(0, 0.0, 0.0, vw, h.max(vh));
    e.flush_z(0, 0);
    let mut out = e.out;
    out.height = out.items.iter().filter(|i| matches!(i.kind, Kind::Box | Kind::Text | Kind::Image)).fold(h, |m, i| m.max(i.y + i.h));
    out.width = vw;
    out.canvas = Color::WHITE;
    let html = dom.nodes[0].first_child;
    if html != NONE {
        let mut c = html;
        while c != NONE {
            let bg = st[c as usize].bg;
            if dom.is_element(c) && bg.a() > 0 {
                out.canvas = bg;
                break;
            }
            c = if c == html { dom.nodes[c as usize].first_child } else { dom.nodes[c as usize].next };
            while c != NONE && dom.nodes[c as usize].tag != atom::BODY {
                c = dom.nodes[c as usize].next;
            }
        }
    }
    out
}

impl<'a> Engine<'a> {
    fn shift(&mut self, a: u32, b: u32, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        for it in &mut self.out.items[a as usize..b as usize] {
            it.x += dx;
            it.y += dy;
        }
    }

    fn mark(&self) -> u32 {
        self.out.items.len() as u32
    }

    fn push_deco(&mut self, node: NodeId, x: f32, y: f32, w: f32, h: f32, deco: BoxDeco) -> u32 {
        let data = self.out.boxes.len() as u32;
        self.out.boxes.push(deco);
        self.out.items.push(Item { x, y, w, h, kind: Kind::Box, node, data });
        self.mark() - 1
    }

    fn push_box(&mut self, node: NodeId, s: &Style, x: f32, y: f32, w: f32, h: f32) -> u32 {
        let o = s.opacity;
        let deco = BoxDeco {
            bg: s.bg.with_opacity(o),
            bw: s.bw,
            bc: s.bc.map(|c| c.with_opacity(o)),
            radius: s.radius,
            grad: s.grad.clone(),
        };
        self.push_deco(node, x, y, w, h, deco)
    }

    fn push_picture(&mut self, node: NodeId, x: f32, y: f32, w: f32, h: f32, pic: PicRef) {
        let data = self.out.pictures.len() as u32;
        self.out.pictures.push(pic);
        self.out.items.push(Item { x, y, w, h, kind: Kind::Image, node, data });
    }

    /// Reserves the item that paints an element's background image, under its
    /// content; `place_background` sets it once the box size is known. Returns the
    /// item and the image's natural size.
    fn background_slot(&mut self, n: NodeId, s: &Style) -> Option<(u32, f32, f32)> {
        if !s.visible || s.opacity <= 0.0 {
            return None;
        }
        let id = self.images.file(s.bg_image.as_deref()?)?;
        let (iw, ih) = self.images.size(id);
        if iw <= 0.0 || ih <= 0.0 {
            return None;
        }
        self.push_picture(n, 0.0, 0.0, 0.0, 0.0, PicRef { id, alpha: s.opacity, tile: Some([0.0; 4]) });
        Some((self.mark() - 1, iw, ih))
    }

    /// Sizes and positions a background image in its element's padding box
    /// `[x, y, w, h]`. One copy is drawn: repeating backgrounds are not tiled.
    fn place_background(&mut self, slot: u32, s: &Style, pad: [f32; 4], iw: f32, ih: f32) {
        let [px, py, pw, ph] = pad;
        let (tw, th) = match s.bg_size {
            BgSize::Cover => (iw * (pw / iw).max(ph / ih), ih * (pw / iw).max(ph / ih)),
            BgSize::Contain => (iw * (pw / iw).min(ph / ih), ih * (pw / iw).min(ph / ih)),
            BgSize::Auto => (iw, ih),
            BgSize::Size(a, b) => match (definite(a, pw), definite(b, ph)) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, w * ih / iw),
                (None, Some(h)) => (h * iw / ih, h),
                (None, None) => (iw, ih),
            },
        };
        let (tx, ty) = (px + rs(s.bg_pos[0], pw - tw), py + rs(s.bg_pos[1], ph - th));
        let (x0, y0) = (tx.max(px), ty.max(py));
        let (x1, y1) = ((tx + tw).min(px + pw), (ty + th).min(py + ph));
        let it = &mut self.out.items[slot as usize];
        (it.x, it.y, it.w, it.h) = (x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0));
        self.out.pictures[it.data as usize].tile = Some([tx - x0, ty - y0, tw, th]);
    }

    /// Emits already-shaped text at a fixed position and returns its width.
    fn emit_text(&mut self, node: NodeId, text: &str, x: f32, baseline: f32, s: &Style, color: Color) -> f32 {
        let face = face_of(self.fonts, s);
        let size = s.font_size;
        let (asc, desc, _) = self.fonts.metrics(face);
        let mut segs = std::mem::take(&mut self.segs);
        segs.clear();
        self.fonts.segments(face, text, size, 0.0, &mut segs);
        let mut cx = x;
        for sg in &segs {
            let start = self.out.text.len() as u32;
            self.out.text.push_str(&text[sg.start as usize..sg.end as usize]);
            let data = self.out.texts.len() as u32;
            self.out.texts.push(TextRun { start, len: sg.end - sg.start, face: sg.face, deco: 0, size, color, spacing: 0.0, ascent: asc * size });
            self.out.items.push(Item { x: cx, y: baseline - asc * size, w: sg.w, h: (asc + desc) * size, kind: Kind::Text, node, data });
            cx += sg.w;
        }
        self.segs = segs;
        cx - x
    }

    /// Top margin of `n` after collapsing with its first in-flow block child.
    fn collapsed_top(&self, n: NodeId) -> f32 {
        let s = &self.st[n as usize];
        let m = rs(s.margin[0], 0.0);
        let plain = matches!(s.display, Display::Block | Display::ListItem)
            && s.padding[0] == Len::Px(0.0)
            && s.bw[0] == 0.0
            && !s.overflow_hidden;
        if !plain {
            return m;
        }
        let mut c = self.dom.nodes[n as usize].first_child;
        while c != NONE {
            let node = &self.dom.nodes[c as usize];
            if node.tag == atom::TEXT {
                if !is_ws(self.dom.text_of(c)) || s.white_space == WhiteSpace::Pre {
                    break;
                }
            } else {
                let cs = &self.st[c as usize];
                match cs.display {
                    Display::None => {}
                    _ if is_abs(cs) || cs.float != 0 => {}
                    Display::Block | Display::ListItem | Display::Flex => return collapse(m, self.collapsed_top(c)),
                    _ => break,
                }
            }
            c = node.next;
        }
        m
    }

    /// Lays out the children of a block container. Returns the content height
    /// and the trailing margin that collapses through the container's bottom.
    fn layout_children(&mut self, n: NodeId, cx: f32, cw: f32, y: f32, ch: f32, top_coll: bool, bottom_coll: bool) -> (f32, f32) {
        let dom = self.dom;
        let st = self.st;
        let pre = st[n as usize].white_space == WhiteSpace::Pre;
        let (mut cur, mut pend, mut first) = (y, 0.0f32, true);
        let mut ord = match dom.attr(n, atom::START) {
            Some(v) if dom.nodes[n as usize].tag == atom::OL => v.trim().parse().unwrap_or(1),
            _ => 1,
        };
        let mut c = dom.nodes[n as usize].first_child;
        while c != NONE {
            let node = &dom.nodes[c as usize];
            let inline = node.tag == atom::TEXT || {
                let cs = &st[c as usize];
                if cs.display == Display::None {
                    c = node.next;
                    continue;
                }
                if is_abs(cs) {
                    self.abs.push((c, cx, cur + pend));
                    c = node.next;
                    continue;
                }
                if cs.float != 0 {
                    self.place_float(c, cx, cw, cur + pend, ch);
                    c = node.next;
                    continue;
                }
                matches!(cs.display, Display::Inline | Display::InlineBlock | Display::InlineFlex)
            };
            if inline {
                let (mut e, mut blank) = (c, true);
                while e != NONE {
                    let en = &dom.nodes[e as usize];
                    if en.tag == atom::TEXT {
                        blank &= if pre { en.text.1 == 0 } else { is_ws(dom.text_of(e)) };
                    } else {
                        match st[e as usize].display {
                            Display::None => {}
                            // A float in a run of inline content is placed with its lines.
                            _ if st[e as usize].float != 0 => blank = false,
                            Display::Inline | Display::InlineBlock | Display::InlineFlex => blank = false,
                            _ => break,
                        }
                    }
                    e = en.next;
                }
                if !blank {
                    cur += pend;
                    pend = 0.0;
                    first = false;
                    cur += self.layout_inline(n, c, e, cx, cw, cur);
                }
                c = e;
                continue;
            }
            let mt = self.collapsed_top(c);
            let mut gap = if first && top_coll { 0.0 } else { collapse(pend, mt) };
            let clear = st[c as usize].clear;
            if clear != 0 && self.floats.len() > self.bfc {
                // Clearance puts the box's top below the floats it clears.
                gap = gap.max(self.clear_y(clear) - cur);
            }
            if st[c as usize].display == Display::ListItem {
                self.next_ord = ord;
                ord += 1;
            }
            let r = self.block_in_flow(c, cx, cw, cur + gap, ch);
            cur += gap + r.h;
            pend = r.mb;
            first = false;
            c = node.next;
        }
        if bottom_coll {
            (cur - y, pend)
        } else {
            (cur - y + pend, 0.0)
        }
    }

    fn block_in_flow(&mut self, n: NodeId, cx: f32, cw: f32, y: f32, ch: f32) -> Res {
        let st = self.st;
        let s = &st[n as usize];
        if self.floats.len() > self.bfc && (bfc_root(s) || self.dom.shows_image(n)) {
            return self.beside_floats(n, cx, cw, y, ch);
        }
        let sd = sides(s, cw);
        let (w, fixed) = match spec_w(s, cw, &sd) {
            Some(w) => (w, true),
            None => {
                let fill = (cw - sd.ml - sd.mr).max(0.0);
                let w = if self.dom.shows_image(n) { (self.intrinsic(n) - sd.ml - sd.mr).min(fill) } else { fill };
                let c = clamp_w(s, w, cw, &sd);
                (c, c != fill)
            }
        };
        let free = (cw - w - sd.ml - sd.mr).max(0.0);
        let ml = match (fixed, sd.auto_l, sd.auto_r) {
            (true, true, true) => free / 2.0,
            (true, true, false) => free,
            _ => sd.ml,
        };
        self.layout_box(n, cx + ml, y, w, cw, ch, NAN, false)
    }

    /// Lays out one box whose border box starts at (x, y) with width `w`.
    /// `bfc` marks boxes whose margins never collapse with their children.
    #[allow(clippy::too_many_arguments)]
    fn layout_box(&mut self, n: NodeId, x: f32, y: f32, w: f32, cbw: f32, ch: f32, forced_h: f32, bfc: bool) -> Res {
        let st = self.st;
        let s = &st[n as usize];
        let sd = sides(s, cbw);
        let [bt, br, bb, bl] = s.bw;
        let cx = x + bl + sd.pl;
        let cw = (w - bl - br - sd.pl - sd.pr).max(0.0);
        let cy = y + bt + sd.pt;
        let vextra = sd.pt + sd.pb + bt + bb;
        let tag = dom_tag(self.dom, n);
        let ord = self.next_ord;

        let first_item = self.mark();
        let item = if s.visible && s.has_deco() {
            self.push_box(n, s, x, y, w, 0.0)
        } else if self.out.record_boxes {
            self.out.items.push(Item { x, y, w, h: 0.0, kind: Kind::Area, node: n, data: 0 });
            self.mark() - 1
        } else {
            NONE
        };
        let bg = self.background_slot(n, s);
        let clip = if s.overflow_hidden {
            self.out.items.push(Item { x: x + bl, y: y + bt, w: w - bl - br, h: 0.0, kind: Kind::Clip, node: n, data: 0 });
            self.mark() - 1
        } else {
            NONE
        };
        let abs_mark = self.abs.len();
        let zmark = self.zranges.len();
        let content_start = self.mark();

        let hx = if s.border_box { 0.0 } else { vextra };
        let mut spec_h = forced_h;
        if spec_h.is_nan() {
            if let Some(v) = definite(s.height, ch) {
                spec_h = v + hx;
            } else if let Some((iw, ih)) = self.natural(n) {
                spec_h = cw * ih / iw + vextra;
            }
        }
        let min_h = definite(s.min_h, ch).map_or(NAN, |v| v + hx);
        let max_h = definite(s.max_h, ch).map_or(NAN, |v| v + hx);
        if !spec_h.is_nan() {
            spec_h = spec_h.min(if max_h.is_nan() { spec_h } else { max_h }).max(if min_h.is_nan() { 0.0 } else { min_h });
        }
        let inner_h = if spec_h.is_nan() { NAN } else { (spec_h - vextra).max(0.0) };

        let saved_bl = self.last_bl;
        self.last_bl = NAN;
        let outer_bfc = (bfc || bfc_root(s)).then(|| self.enter_bfc());
        let (mut content_h, mb_out) = if tag == atom::INPUT || self.dom.shows_image(n) {
            (self.replaced(n, s, cx, cy, cw, inner_h, tag), 0.0)
        } else if s.grid && s.rare().subgrid == 0 {
            (self.layout_grid(n, cx, cw, cy, inner_h), 0.0)
        } else if matches!(s.display, Display::Flex | Display::InlineFlex) {
            let inner_min = if min_h.is_nan() { NAN } else { (min_h - vextra).max(0.0) };
            (self.layout_flex(n, cx, cw, cy, inner_h, inner_min), 0.0)
        } else {
            let open = !bfc && !s.overflow_hidden;
            let top = open && sd.pt == 0.0 && bt == 0.0;
            let bottom = open && sd.pb == 0.0 && bb == 0.0 && spec_h.is_nan() && min_h.is_nan();
            self.layout_children(n, cx, cw, cy, inner_h, top, bottom)
        };
        if let Some(outer) = outer_bfc {
            // A block formatting context holds its floats.
            content_h = content_h.max(self.leave_bfc(outer) - cy);
        }
        let mut h = if spec_h.is_nan() { content_h + vextra } else { spec_h };
        if !max_h.is_nan() {
            h = h.min(max_h);
        }
        if !min_h.is_nan() {
            h = h.max(min_h);
        }

        if s.display == Display::ListItem && s.list_style != ListStyle::None && s.visible {
            self.marker(n, s, cx, cy, ord);
        }
        if item != NONE {
            self.out.items[item as usize].h = h;
        }
        if let Some((slot, iw, ih)) = bg {
            self.place_background(slot, s, [x + bl, y + bt, w - bl - br, h - bt - bb], iw, ih);
        }
        let baseline = if self.last_bl.is_nan() {
            self.last_bl = saved_bl;
            NAN
        } else {
            self.last_bl - y
        };
        if s.position != Position::Static {
            let keep = self.last_bl;
            self.layout_abs_children(abs_mark, x + bl, y + bt, w - bl - br, h - bt - bb);
            self.last_bl = keep;
        }
        if s.z_ctx || s.overflow_hidden || s.position == Position::Fixed {
            self.flush_z(zmark, content_start);
        }
        if clip != NONE {
            self.out.items[clip as usize].h = h - bt - bb;
            self.out.items.push(Item { x: 0.0, y: 0.0, w: 0.0, h: 0.0, kind: Kind::PopClip, node: n, data: 0 });
        }
        if s.position == Position::Relative {
            let dx = definite(s.inset[3], cbw).or(definite(s.inset[1], cbw).map(|v| -v)).unwrap_or(0.0);
            let dy = definite(s.inset[0], ch).or(definite(s.inset[2], ch).map(|v| -v)).unwrap_or(0.0);
            let end = self.mark();
            self.shift(first_item, end, dx, dy);
        }
        if s.translate != [Len::Px(0.0); 2] {
            let end = self.mark();
            self.shift(first_item, end, rs(s.translate[0], w), rs(s.translate[1], h));
        }
        if s.position != Position::Static || s.z_ctx {
            let end = self.mark();
            self.zranges.push((first_item, end, s.z));
        }
        Res { h, mb: if bfc { sd.mb } else { collapse(sd.mb, mb_out) }, item, baseline }
    }

    /// Content of `<img>` (alt text placeholder) and `<input>` (its value).
    #[allow(clippy::too_many_arguments)]
    fn replaced(&mut self, n: NodeId, s: &Style, cx: f32, cy: f32, cw: f32, inner_h: f32, tag: u32) -> f32 {
        let dom = self.dom;
        if tag != atom::INPUT {
            if let Some((id, _, _)) = self.picture(n) {
                let h = if inner_h.is_nan() { 0.0 } else { inner_h };
                if s.visible && s.opacity > 0.0 {
                    self.push_picture(n, cx, cy, cw, h, PicRef { id, alpha: s.opacity, tile: None });
                }
                return h;
            }
            if matches!(tag, atom::SVG | atom::VIDEO) {
                return 0.0;
            }
            if self.waiting.contains_key(&n) {
                // An empty box of the size its attributes give, until the image arrives.
                return if inner_h.is_nan() { 0.0 } else { inner_h };
            }
        }
        let face = face_of(self.fonts, s);
        let (asc, desc, gap) = self.fonts.metrics(face);
        let (above, below) = line_extent(s, asc, desc, gap);
        let (text, color) = if tag == atom::IMG {
            (dom.attr(n, atom::ALT).unwrap_or(""), s.color)
        } else {
            match dom.attr(n, atom::VALUE) {
                Some(v) if !v.is_empty() => (v, s.color),
                _ => (dom.attr(n, atom::PLACEHOLDER).unwrap_or(""), Color(0xFF75_7575)),
            }
        };
        let boxy = matches!(dom.attr(n, atom::TYPE), Some("checkbox" | "radio"));
        if !text.is_empty() && s.visible && !boxy {
            self.emit_text(n, text, cx, cy + above, s, color.with_opacity(s.opacity));
        }
        if boxy {
            0.0
        } else {
            above + below
        }
    }

    fn marker(&mut self, n: NodeId, s: &Style, cx: f32, cy: f32, ord: u32) {
        let face = face_of(self.fonts, s);
        let (asc, desc, gap) = self.fonts.metrics(face);
        let (above, _) = line_extent(s, asc, desc, gap);
        let size = s.font_size;
        let baseline = cy + above;
        let color = s.color.with_opacity(s.opacity);
        if s.list_style == ListStyle::Decimal {
            let label = format!("{ord}.");
            let w = self.fonts.measure(face, &label, size, 0.0);
            self.emit_text(n, &label, cx - w - size * 0.4, baseline, s, color);
            return;
        }
        let d = (size * 0.36).round();
        let hollow = s.list_style == ListStyle::Circle;
        let deco = BoxDeco {
            bg: if hollow { Color::TRANSPARENT } else { color },
            bw: [if hollow { 1.0 } else { 0.0 }; 4],
            bc: [color; 4],
            radius: [if s.list_style == ListStyle::Square { Len::Px(0.0) } else { Len::Pct(50.0) }; 4],
            grad: None,
        };
        self.push_deco(n, cx - size * 0.55 - d, baseline - size * 0.34 - d / 2.0, d, d, deco);
    }

    fn layout_abs_children(&mut self, mark: usize, px: f32, py: f32, pw: f32, ph: f32) {
        if self.abs.len() <= mark {
            return;
        }
        let pending: Vec<_> = self.abs.drain(mark..).collect();
        for (c, sx, sy) in pending {
            let st = self.st;
            let s = &st[c as usize];
            // A fixed box is placed against the viewport, not its positioned ancestor.
            let (px, py, pw, ph) = if s.position == Position::Fixed { (0.0, 0.0, self.vw, self.vh) } else { (px, py, pw, ph) };
            let sd = sides(s, pw);
            let l = definite(s.inset[3], pw);
            let r = definite(s.inset[1], pw);
            let t = definite(s.inset[0], ph);
            let b = definite(s.inset[2], ph);
            let by_ratio = |(iw, ih): (f32, f32)| {
                let h = definite(s.height, ph)?;
                Some(clamp_w(s, h * iw / ih + if s.border_box { 0.0 } else { hextra(s, &sd) }, pw, &sd))
            };
            let fixed = spec_w(s, pw, &sd).or_else(|| self.natural(c).and_then(by_ratio));
            let w = fixed.unwrap_or_else(|| match (l, r) {
                (Some(l), Some(r)) => clamp_w(s, (pw - l - r - sd.ml - sd.mr).max(0.0), pw, &sd),
                _ => {
                    let avail = (pw - sd.ml - sd.mr - l.or(r).unwrap_or(0.0)).max(0.0);
                    clamp_w(s, (self.intrinsic(c) - sd.ml - sd.mr).min(avail), pw, &sd)
                }
            });
            let x = match (l, r) {
                (Some(l), Some(r)) if fixed.is_some() && sd.auto_l && sd.auto_r => px + l + (pw - l - r - w).max(0.0) / 2.0,
                (Some(l), _) => px + l + sd.ml,
                (None, Some(r)) => px + pw - r - sd.mr - w,
                (None, None) => sx + sd.ml,
            };
            let forced_h = match (t, b) {
                (Some(t), Some(b)) if s.height == Len::Auto => (ph - t - b - sd.mt - sd.mb).max(0.0),
                _ => NAN,
            };
            let y0 = match (t, b) {
                (Some(t), _) => py + t + sd.mt,
                (None, Some(_)) => py,
                (None, None) => sy + sd.mt,
            };
            let first = self.mark();
            let res = self.layout_box(c, x, y0, w, pw, ph, forced_h, true);
            if let (None, Some(b)) = (t, b) {
                let end = self.mark();
                self.shift(first, end, 0.0, py + ph - b - sd.mb - res.h - y0);
            }
        }
    }

    fn layout_inline(&mut self, block: NodeId, first: NodeId, end: NodeId, cx: f32, cw: f32, y: f32) -> f32 {
        let st = self.st;
        let s = &st[block as usize];
        let face = face_of(self.fonts, s);
        let (asc, desc, gap) = self.fonts.metrics(face);
        let (above, below) = line_extent(s, asc, desc, gap);
        let mut il = self.pool.pop().unwrap_or_default();
        il.frags.clear();
        il.open.clear();
        il = Inl {
            cx,
            cw,
            x: 0.0,
            y,
            above,
            below,
            align: match s.text_align {
                TextAlign::Left => 0.0,
                TextAlign::Center => 0.5,
                TextAlign::Right => 1.0,
            },
            space_w: self.fonts.lookup(face, ' ').2 * s.font_size + s.spacing,
            begun: false,
            content: false,
            space: false,
            wrap: s.white_space == WhiteSpace::Normal,
            last_text: NONE,
            brk: false,
            lx: 0.0,
            right: cw,
            ..il
        };
        il.queued.clear();
        let mut c = first;
        while c != end && c != NONE {
            self.inline_node(&mut il, c);
            c = self.dom.nodes[c as usize].next;
        }
        self.finish_line(&mut il);
        let h = il.y - y;
        self.pool.push(il);
        h
    }

    fn open_deco(&mut self, il: &mut Inl, i: usize) {
        let st = self.st;
        let o = &mut il.open[i];
        let (node, top, h, x) = (o.node, o.top, o.h, il.cx + o.start_x);
        let idx = self.push_box(node, &st[node as usize], x, 0.0, 0.0, h);
        il.open[i].item = idx;
        il.frags.push(LFrag { first: idx, end: idx + 1, above: 0.0, below: 0.0, top, kind: F_DECO });
    }

    fn begin_line(&mut self, il: &mut Inl) {
        if il.begun {
            return;
        }
        il.begun = true;
        (il.lx, il.right) = (0.0, il.cw);
        if self.floats.len() > self.bfc {
            let (l, r) = self.band(il.y, il.above + il.below, il.cx, il.cw);
            (il.lx, il.right) = (l - il.cx, r - il.cx);
            il.x += il.lx;
        }
        for i in 0..il.open.len() {
            il.open[i].start_x = il.x;
            if il.open[i].has_deco {
                self.open_deco(il, i);
            }
        }
    }

    /// Closes the current line: fixes the baseline, aligns, and moves on.
    fn finish_line(&mut self, il: &mut Inl) {
        il.space = false;
        if !il.begun {
            return;
        }
        let items = &mut self.out.items;
        for o in &mut il.open {
            if o.item != NONE {
                let it = &mut items[o.item as usize];
                it.w = il.cx + il.x - it.x;
                o.item = NONE;
            }
        }
        let (mut above, mut below) = (il.above, il.below);
        for f in &il.frags {
            above = above.max(f.above);
            below = below.max(f.below);
        }
        let baseline = il.y + above;
        let dx = (il.right - il.x).max(0.0) * il.align;
        for f in &il.frags {
            let range = &mut items[f.first as usize..f.end as usize];
            if f.kind == F_ATOMIC {
                let dy = above - f.top;
                for it in range {
                    it.x += dx;
                    it.y += dy;
                }
            } else {
                for it in range {
                    it.x += dx;
                    it.y = baseline - f.top;
                }
            }
        }
        self.last_bl = baseline;
        il.y += above + below;
        il.x = 0.0;
        il.begun = false;
        il.content = false;
        il.last_text = NONE;
        il.frags.clear();
        let queued = std::mem::take(&mut il.queued);
        for &f in &queued {
            self.place_float(f, il.cx, il.cw, il.y, NAN);
        }
        il.queued = queued;
        il.queued.clear();
    }

    fn inline_node(&mut self, il: &mut Inl, n: NodeId) {
        let dom = self.dom;
        let st = self.st;
        let node = &dom.nodes[n as usize];
        if node.tag == atom::TEXT {
            self.inline_text(il, n);
            return;
        }
        let s = &st[n as usize];
        match s.display {
            Display::None => {}
            _ if is_abs(s) => self.abs.push((n, il.cx + il.x, il.y)),
            _ if s.float != 0 => self.inline_float(il, n),
            Display::Inline => {
                if node.tag == atom::BR {
                    il.begun = true;
                    self.finish_line(il);
                    return;
                }
                let sd = sides(s, il.cw);
                let (bl, br) = (s.bw[3], s.bw[1]);
                let has_deco = s.visible && s.has_deco();
                let lead = sd.ml + bl + sd.pl;
                if has_deco || lead != 0.0 {
                    if il.space && il.content {
                        il.x += il.space_w;
                        il.space = false;
                        il.brk = true;
                    }
                    self.begin_line(il);
                }
                let face = face_of(self.fonts, s);
                let (asc, desc, _) = self.fonts.metrics(face);
                il.x += sd.ml;
                il.open.push(Open {
                    node: n,
                    item: NONE,
                    start_x: il.x,
                    has_deco,
                    top: asc * s.font_size + sd.pt + s.bw[0],
                    h: (asc + desc) * s.font_size + sd.pt + sd.pb + s.bw[0] + s.bw[2],
                });
                if has_deco {
                    let i = il.open.len() - 1;
                    self.open_deco(il, i);
                }
                il.x += bl + sd.pl;
                let mut c = node.first_child;
                while c != NONE {
                    let cn = &dom.nodes[c as usize];
                    let block = cn.tag != atom::TEXT
                        && !is_abs(&st[c as usize])
                        && st[c as usize].float == 0
                        && matches!(st[c as usize].display, Display::Block | Display::Flex | Display::ListItem);
                    if block {
                        self.finish_line(il);
                        let mt = self.collapsed_top(c);
                        let r = self.block_in_flow(c, il.cx, il.cw, il.y + mt, NAN);
                        il.y += mt + r.h + r.mb;
                    } else {
                        self.inline_node(il, c);
                    }
                    c = cn.next;
                }
                il.x += sd.pr + br;
                let o = il.open.pop().unwrap();
                if o.item != NONE {
                    let it = &mut self.out.items[o.item as usize];
                    it.w = il.cx + il.x - it.x;
                }
                il.x += sd.mr;
            }
            _ => self.inline_atomic(il, n),
        }
    }

    fn inline_atomic(&mut self, il: &mut Inl, n: NodeId) {
        let st = self.st;
        let s = &st[n as usize];
        let sd = sides(s, il.cw);
        let avail = (il.cw - sd.ml - sd.mr).max(0.0);
        let w = match spec_w(s, il.cw, &sd) {
            Some(w) => w,
            None => clamp_w(s, (self.intrinsic(n) - sd.ml - sd.mr).min(avail), il.cw, &sd),
        };
        let outer_w = w + sd.ml + sd.mr;
        let ps = &st[self.dom.nodes[n as usize].parent as usize];
        let face = face_of(self.fonts, ps);
        let gap = self.fonts.lookup(face, ' ').2 * ps.font_size + ps.spacing;
        let mut space_w = if il.space && il.content { gap } else { 0.0 };
        if il.wrap && il.content && il.x + space_w + outer_w > il.right + FIT {
            self.finish_line(il);
            space_w = 0.0;
        }
        il.space = false;
        self.fit_line(il, outer_w);
        self.begin_line(il);
        let first = self.mark();
        let r = self.layout_box(n, il.cx + il.x + space_w + sd.ml, il.y + sd.mt, w, il.cw, NAN, NAN, true);
        let outer_h = r.h + sd.mt + sd.mb;
        let above = if r.baseline.is_nan() || s.overflow_hidden { outer_h } else { sd.mt + r.baseline };
        il.frags.push(LFrag { first, end: self.mark(), above, below: outer_h - above, top: above, kind: F_ATOMIC });
        il.x += space_w + outer_w;
        il.content = true;
        il.last_text = NONE;
        il.brk = false;
    }

    fn inline_text(&mut self, il: &mut Inl, n: NodeId) {
        let dom = self.dom;
        let st = self.st;
        let parent = dom.nodes[n as usize].parent;
        let s = &st[parent as usize];
        let text = dom.text_of(n);
        let face = face_of(self.fonts, s);
        let (asc, desc, gap) = self.fonts.metrics(face);
        let (above, below) = line_extent(s, asc, desc, gap);
        let size = s.font_size;
        let ts = TStyle {
            node: parent,
            face,
            size,
            color: s.color.with_opacity(s.opacity),
            deco: s.deco,
            spacing: s.spacing,
            asc: asc * size,
            desc: desc * size,
            above,
            below,
            space_w: self.fonts.lookup(face, ' ').2 * size + s.spacing,
            visible: s.visible,
            transform: s.transform,
        };
        if s.white_space == WhiteSpace::Pre {
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    il.begun = true;
                    self.finish_line(il);
                }
                let line = line.trim_end_matches('\r');
                if line.contains('\t') {
                    self.place_unit(il, &ts, &line.replace('\t', "    "), false);
                } else if !line.is_empty() {
                    self.place_unit(il, &ts, line, false);
                }
            }
            return;
        }
        let wrap = s.white_space == WhiteSpace::Normal;
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if is_space(b[i]) {
                il.space = true;
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && !is_space(b[i]) {
                i += 1;
            }
            let word = &text[start..i];
            if word.is_ascii() {
                self.place_unit(il, &ts, word, wrap);
                continue;
            }
            let mut from = 0;
            for (j, c) in word.char_indices() {
                if is_cjk(c) {
                    if j > from {
                        il.brk = from > 0 && word[from..].starts_with(char::is_alphanumeric);
                        self.place_unit(il, &ts, &word[from..j], wrap);
                    }
                    from = j + c.len_utf8();
                    il.brk = true;
                    self.place_unit(il, &ts, &word[j..from], wrap);
                }
            }
            if from < word.len() {
                il.brk = from > 0 && word[from..].starts_with(char::is_alphanumeric);
                self.place_unit(il, &ts, &word[from..], wrap);
            }
        }
    }

    /// Places one unbreakable unit on the current line, wrapping first if it
    /// does not fit. Adjacent units of the same style extend one text run.
    fn place_unit(&mut self, il: &mut Inl, ts: &TStyle, unit: &str, wrap: bool) {
        let cased;
        let unit = match ts.transform {
            TextTransform::None => unit,
            TextTransform::Upper => {
                cased = unit.to_uppercase();
                &cased
            }
            TextTransform::Lower => {
                cased = unit.to_lowercase();
                &cased
            }
            TextTransform::Capitalize => {
                cased = capitalize(unit, il.space || !il.content);
                &cased
            }
        };
        let mut segs = std::mem::take(&mut self.segs);
        segs.clear();
        self.fonts.segments(ts.face, unit, ts.size, ts.spacing, &mut segs);
        let total: f32 = segs.iter().map(|s| s.w).sum();
        let mut space_w = if il.space && il.content { ts.space_w } else { 0.0 };
        let can_break = il.space || il.brk;
        il.brk = false;
        if wrap && can_break && il.content && il.x + space_w + total > il.right + FIT {
            self.finish_line(il);
            space_w = 0.0;
        }
        il.space = false;
        self.fit_line(il, total);
        self.begin_line(il);
        for sg in &segs {
            let piece = &unit[sg.start as usize..sg.end as usize];
            let x = il.cx + il.x + space_w;
            if ts.visible {
                let out = &mut self.out;
                let mut merged = false;
                if il.last_text != NONE && il.last_text as usize + 1 == out.items.len() {
                    let it = &mut out.items[il.last_text as usize];
                    let run = &mut out.texts[it.data as usize];
                    let joins = it.node == ts.node
                        && run.face == sg.face
                        && run.size == ts.size
                        && run.color == ts.color
                        && (run.start + run.len) as usize == out.text.len()
                        && (it.x + it.w - (il.cx + il.x)).abs() < 0.01;
                    if joins {
                        if space_w > 0.0 {
                            out.text.push(' ');
                        }
                        out.text.push_str(piece);
                        run.len = out.text.len() as u32 - run.start;
                        it.w = x + sg.w - it.x;
                        merged = true;
                    }
                }
                if !merged {
                    let start = out.text.len() as u32;
                    out.text.push_str(piece);
                    let data = out.texts.len() as u32;
                    out.texts.push(TextRun {
                        start,
                        len: piece.len() as u32,
                        face: sg.face,
                        deco: ts.deco,
                        size: ts.size,
                        color: ts.color,
                        spacing: ts.spacing,
                        ascent: ts.asc,
                    });
                    let idx = out.items.len() as u32;
                    out.items.push(Item { x, y: 0.0, w: sg.w, h: ts.asc + ts.desc, kind: Kind::Text, node: ts.node, data });
                    il.frags.push(LFrag { first: idx, end: idx + 1, above: ts.above, below: ts.below, top: ts.asc, kind: F_TEXT });
                    il.last_text = idx;
                }
            }
            il.x += space_w + sg.w;
            space_w = 0.0;
        }
        il.content = true;
        self.segs = segs;
    }

    fn layout_flex(&mut self, n: NodeId, cx: f32, cw: f32, y: f32, inner_h: f32, min_h: f32) -> f32 {
        let dom = self.dom;
        let st = self.st;
        let s = &st[n as usize];
        let mut items: Vec<FItem> = Vec::new();
        let mut kids = Vec::new();
        self.flex_children(n, &mut kids);
        for &c in &kids {
            let node = &dom.nodes[c as usize];
            let mut it = FItem {
                node: c,
                text: true,
                sd: Sides::default(),
                align: s.align_items,
                grow: 0.0,
                shrink: 1.0,
                base: 0.0,
                w: 0.0,
                h: 0.0,
                first: 0,
                end: 0,
                item: NONE,
            };
            if node.tag == atom::TEXT {
                if !is_ws(dom.text_of(c)) {
                    items.push(it);
                }
            } else {
                let cs = &st[c as usize];
                if is_abs(cs) && cs.display != Display::None {
                    self.abs.push((c, cx, y));
                } else if cs.display != Display::None {
                    it.text = false;
                    it.sd = sides(cs, cw);
                    it.grow = cs.grow;
                    it.shrink = cs.shrink;
                    if cs.align_self != Align::Auto {
                        it.align = cs.align_self;
                    }
                    items.push(it);
                }
            }
        }
        if items.iter().any(|it| !it.text && st[it.node as usize].order != 0) {
            items.sort_by_key(|it| if it.text { 0 } else { st[it.node as usize].order });
        }
        self.order_grid_rows(&mut items);
        if s.flex_col {
            return self.flex_column(n, &mut items, cx, cw, y, inner_h, min_h);
        }

        for it in &mut items {
            it.base = if it.text {
                self.text_intrinsic(it.node)
            } else {
                let cs = &st[it.node as usize];
                let sd = it.sd;
                match cs.basis {
                    Len::Auto => spec_w(cs, cw, &sd)
                        .unwrap_or_else(|| clamp_w(cs, self.intrinsic(it.node) - sd.ml - sd.mr, cw, &sd)),
                    l => {
                        let v = rs(l, cw);
                        clamp_w(cs, if cs.border_box { v } else { v + hextra(cs, &sd) }, cw, &sd)
                    }
                }
            };
        }

        let gap = s.gap_col;
        let mut lines: Vec<(usize, usize)> = Vec::new();
        let (mut a, mut used) = (0, 0.0);
        if s.flex_wrap {
            for (i, it) in items.iter().enumerate() {
                let o = it.base + it.sd.ml + it.sd.mr;
                if i > a && used + gap + o > cw + FIT {
                    lines.push((a, i));
                    a = i;
                    used = o;
                } else {
                    used += o + if i > a { gap } else { 0.0 };
                }
            }
        }
        lines.push((a, items.len()));
        let single = lines.len() == 1;

        let mut cy = y;
        for (li, &(a, b)) in lines.iter().enumerate() {
            let line = &mut items[a..b];
            let k = line.len() as f32;
            if line.is_empty() {
                continue;
            }
            let used: f32 = line.iter().map(|i| i.base + i.sd.ml + i.sd.mr).sum::<f32>() + gap * (k - 1.0);
            let mut free = cw - used;
            let grow: f32 = line.iter().map(|i| i.grow).sum();
            let shrink: f32 = line.iter().map(|i| i.shrink * i.base).sum();
            for it in line.iter_mut() {
                it.w = it.base;
                if free > 0.0 && grow > 0.0 {
                    it.w += free * it.grow / grow.max(1.0);
                } else if free < 0.0 && shrink > 0.0 {
                    it.w = (it.base + free * it.shrink * it.base / shrink).max(0.0);
                }
                if !it.text {
                    it.w = clamp_w(&st[it.node as usize], it.w, cw, &it.sd);
                }
            }
            if (free > 0.0 && grow > 0.0) || (free < 0.0 && shrink > 0.0) {
                free = cw - line.iter().map(|i| i.w + i.sd.ml + i.sd.mr).sum::<f32>() - gap * (k - 1.0);
            }
            let autos = line.iter().map(|i| i.sd.auto_l as u32 + i.sd.auto_r as u32).sum::<u32>();
            let (mut lead, mut between, mut auto_m) = (0.0, gap, 0.0);
            if autos > 0 && free > 0.0 {
                auto_m = free / autos as f32;
            } else {
                match s.justify {
                    Justify::Start => {}
                    Justify::Center => lead = free / 2.0,
                    Justify::End => lead = free,
                    Justify::Between if free > 0.0 && k > 1.0 => between += free / (k - 1.0),
                    Justify::Around if free > 0.0 => {
                        between += free / k;
                        lead = free / k / 2.0;
                    }
                    Justify::Evenly if free > 0.0 => {
                        between += free / (k + 1.0);
                        lead = free / (k + 1.0);
                    }
                    _ => {}
                }
            }

            let mut cross = if single { inner_h } else { NAN };
            let snap = self.snapshot();
            let mut line_h;
            loop {
                let mut px = cx + lead;
                line_h = 0.0f32;
                for it in line.iter_mut() {
                    px += it.sd.ml + if it.sd.auto_l { auto_m } else { 0.0 };
                    it.first = self.mark();
                    if it.text {
                        let next = dom.nodes[it.node as usize].next;
                        it.h = self.layout_inline(n, it.node, next, px, it.w, cy);
                    } else {
                        let cs = &st[it.node as usize];
                        let stretch = matches!(it.align, Align::Stretch | Align::Auto) && cs.height == Len::Auto;
                        let fh = if stretch && !cross.is_nan() { (cross - it.sd.mt - it.sd.mb).max(0.0) } else { NAN };
                        let r = self.layout_box(it.node, px, cy + it.sd.mt, it.w, cw, inner_h, fh, true);
                        it.h = r.h;
                        it.item = r.item;
                    }
                    it.end = self.mark();
                    line_h = line_h.max(it.h + it.sd.mt + it.sd.mb);
                    px += it.w + it.sd.mr + if it.sd.auto_r { auto_m } else { 0.0 } + between;
                }
                if single {
                    if !inner_h.is_nan() {
                        line_h = inner_h;
                    } else if !min_h.is_nan() {
                        line_h = line_h.max(min_h);
                    }
                }
                if cross.is_nan() && self.restretch(line, line_h) {
                    self.rewind(snap);
                    cross = line_h;
                    continue;
                }
                break;
            }
            for it in line.iter() {
                let outer = it.h + it.sd.mt + it.sd.mb;
                match it.align {
                    Align::Center => self.shift(it.first, it.end, 0.0, (line_h - outer) / 2.0),
                    Align::End => self.shift(it.first, it.end, 0.0, line_h - outer),
                    Align::Start => {}
                    Align::Stretch | Align::Auto => {
                        let auto_h = !it.text && st[it.node as usize].height == Len::Auto;
                        if auto_h && it.item != NONE && outer < line_h {
                            self.out.items[it.item as usize].h = line_h - it.sd.mt - it.sd.mb;
                        }
                    }
                }
            }
            cy += line_h + if li + 1 < lines.len() { s.gap_row } else { 0.0 };
        }
        cy - y
    }

    #[allow(clippy::too_many_arguments)]
    fn flex_column(&mut self, n: NodeId, items: &mut [FItem], cx: f32, cw: f32, y: f32, inner_h: f32, min_h: f32) -> f32 {
        let dom = self.dom;
        let st = self.st;
        let s = &st[n as usize];
        let gap = s.gap_row;
        let mut cy = y;
        // Grid items placed on the same row share it: each starts at the
        // row's top and the next row starts below the tallest of them.
        let (mut row, mut row_top, mut row_next) = (0u16, y, y);
        for it in items.iter_mut() {
            it.first = self.mark();
            if it.text {
                let next = dom.nodes[it.node as usize].next;
                it.h = self.layout_inline(n, it.node, next, cx, cw, cy);
                cy += it.h + gap;
                row = 0;
            } else {
                let cs = &st[it.node as usize];
                let shared = cs.grid_row > 0 && cs.grid_row == row;
                if shared {
                    cy = row_top;
                } else {
                    (row, row_top) = (cs.grid_row, cy);
                }
                let sd = it.sd;
                let fill = (cw - sd.ml - sd.mr).max(0.0);
                let stretch = matches!(it.align, Align::Stretch | Align::Auto) && !sd.auto_l && !sd.auto_r;
                let w = spec_w(cs, cw, &sd).unwrap_or_else(|| {
                    let w = if stretch { fill } else { (self.intrinsic(it.node) - sd.ml - sd.mr).min(fill) };
                    clamp_w(cs, w, cw, &sd)
                });
                let free = (fill - w).max(0.0);
                let off = match (sd.auto_l, sd.auto_r, it.align) {
                    (true, true, _) | (false, false, Align::Center) => free / 2.0,
                    (true, false, _) | (false, false, Align::End) => free,
                    _ => 0.0,
                };
                let r = self.layout_box(it.node, cx + sd.ml + off, cy + sd.mt, w, cw, inner_h, NAN, true);
                it.h = r.h;
                it.item = r.item;
                cy += sd.mt + r.h + sd.mb + gap;
                if shared {
                    cy = cy.max(row_next);
                }
                row_next = cy;
            }
            it.end = self.mark();
        }
        let total = if items.is_empty() { 0.0 } else { cy - y - gap };
        let target = if inner_h.is_nan() { min_h } else { inner_h };
        if target.is_nan() || target <= total + FIT {
            return total;
        }
        let free = target - total;
        let grow: f32 = items.iter().map(|i| i.grow).sum();
        let k = items.len() as f32;
        let (lead, between) = match s.justify {
            _ if grow > 0.0 => (0.0, 0.0),
            Justify::Center => (free / 2.0, 0.0),
            Justify::End => (free, 0.0),
            Justify::Between if k > 1.0 => (0.0, free / (k - 1.0)),
            Justify::Around => (free / k / 2.0, free / k),
            Justify::Evenly => (free / (k + 1.0), free / (k + 1.0)),
            _ => (0.0, 0.0),
        };
        let mut off = lead;
        for it in items.iter() {
            self.shift(it.first, it.end, 0.0, off);
            off += between;
            if grow > 0.0 && it.grow > 0.0 {
                let add = free * it.grow / grow.max(1.0);
                if it.item != NONE {
                    self.out.items[it.item as usize].h += add;
                }
                off += add;
            }
        }
        target
    }

    /// Puts grid items in row order when some of them name their row with
    /// `grid-row`. The others are placed like CSS sparse auto-placement in
    /// one column: in document order, each on the next row no named item
    /// starts on.
    fn order_grid_rows(&self, items: &mut Vec<FItem>) {
        let row = |it: &FItem| self.st[it.node as usize].grid_row;
        if items.iter().any(|it| it.text) || items.iter().all(|it| row(it) == 0) {
            return;
        }
        let taken: Vec<u16> = items.iter().map(row).filter(|&r| r > 0).collect();
        let mut next = 1u16;
        let mut keys = Vec::with_capacity(items.len());
        for it in items.iter() {
            let r = row(it);
            if r > 0 {
                keys.push(r);
            } else {
                while taken.contains(&next) {
                    next += 1;
                }
                keys.push(next);
                next += 1;
            }
        }
        let mut keyed: Vec<(u16, FItem)> = keys.into_iter().zip(items.drain(..)).collect();
        keyed.sort_by_key(|k| k.0);
        items.extend(keyed.into_iter().map(|k| k.1));
    }

    /// Whether a flex line whose cross size was only known after laying
    /// out its items must be laid out again: a stretched column flex item
    /// shorter than the line places its own children (justify-content,
    /// flex-grow) against the stretched height, so it needs that height
    /// as a definite size, as CSS Flexbox 9.4 step 11 requires.
    fn restretch(&self, line: &[FItem], line_h: f32) -> bool {
        line.iter().any(|it| {
            let cs = &self.st[it.node as usize];
            !it.text
                && matches!(it.align, Align::Stretch | Align::Auto)
                && cs.height == Len::Auto
                && cs.flex_col
                && matches!(cs.display, Display::Flex | Display::InlineFlex)
                && it.h + it.sd.mt + it.sd.mb + FIT < line_h
        })
    }

    fn snapshot(&self) -> Snapshot {
        let o = &self.out;
        Snapshot {
            items: o.items.len(),
            boxes: o.boxes.len(),
            texts: o.texts.len(),
            pictures: o.pictures.len(),
            text: o.text.len(),
            abs: self.abs.len(),
            zranges: self.zranges.len(),
            floats: self.floats.len(),
            next_ord: self.next_ord,
            last_bl: self.last_bl,
        }
    }

    /// Drops everything laid out since `s`, so that part can be laid out again.
    fn rewind(&mut self, s: Snapshot) {
        let o = &mut self.out;
        o.items.truncate(s.items);
        o.boxes.truncate(s.boxes);
        o.texts.truncate(s.texts);
        o.pictures.truncate(s.pictures);
        o.text.truncate(s.text);
        self.abs.truncate(s.abs);
        self.zranges.truncate(s.zranges);
        self.floats.truncate(s.floats);
        self.next_ord = s.next_ord;
        self.last_bl = s.last_bl;
    }

    /// Children that take part in a flex container's layout: a
    /// `display: contents` child is replaced by its own children.
    fn flex_children(&self, n: NodeId, out: &mut Vec<NodeId>) {
        let mut c = self.dom.nodes[n as usize].first_child;
        while c != NONE {
            if self.dom.is_element(c) && self.st[c as usize].contents {
                self.flex_children(c, out);
            } else {
                out.push(c);
            }
            c = self.dom.nodes[c as usize].next;
        }
    }

    /// Puts the items of one stacking context into paint order: boxes with a
    /// negative z-index, then in-flow content, then positioned boxes by
    /// z-index. `zranges[zmark..]` holds the positioned boxes laid out inside
    /// the context as item ranges, which nest; an item belongs to the
    /// innermost range containing it.
    fn flush_z(&mut self, zmark: usize, lo: u32) {
        if self.zranges.len() <= zmark {
            return;
        }
        let mut ranges: Vec<(u32, u32, i32)> = self.zranges.drain(zmark..).collect();
        ranges.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let hi = self.mark();
        let span = |r: &(u32, u32, i32)| (r.0.max(lo) - lo) as usize..(r.1.min(hi).max(lo) - lo) as usize;
        let items = &mut self.out.items[lo as usize..hi as usize];
        let mut owner = vec![u32::MAX; items.len()];
        for (k, r) in ranges.iter().enumerate() {
            owner[span(r)].fill(k as u32);
        }
        let mut order: Vec<u32> = (0..ranges.len() as u32).collect();
        order.sort_by_key(|&k| (ranges[k as usize].2, k));
        let mut sorted: Vec<Item> = Vec::with_capacity(items.len());
        let emit = |k: u32, sorted: &mut Vec<Item>| {
            let r = span(&ranges[k as usize]);
            sorted.extend(items[r.clone()].iter().zip(&owner[r]).filter(|(_, o)| **o == k).map(|(i, _)| *i));
        };
        let split = order.partition_point(|&k| ranges[k as usize].2 < 0);
        for &k in &order[..split] {
            emit(k, &mut sorted);
        }
        sorted.extend(items.iter().zip(&owner).filter(|(_, o)| **o == u32::MAX).map(|(i, _)| *i));
        for &k in &order[split..] {
            emit(k, &mut sorted);
        }
        items.copy_from_slice(&sorted);
    }

    /// The image an `<img>` or inline `<svg>` shows, with its intrinsic size.
    fn picture(&mut self, n: NodeId) -> Option<(u32, f32, f32)> {
        if let Some(&p) = self.pics.get(&n) {
            return p;
        }
        let dom = self.dom;
        let node = &dom.nodes[n as usize];
        let (vw, vh) = (self.vw, self.vh);
        let misses = self.images.misses();
        let id = if node.tag == atom::SVG {
            let mut markup = String::new();
            dom.serialize(n, true, &mut markup);
            let s = &self.st[n as usize];
            self.images.svg(&svg_markup(markup, s.fill, s.color))
        } else {
            let mut found = None;
            let parent = node.parent;
            if parent != NONE && dom.nodes[parent as usize].tag == atom::PICTURE {
                let mut c = dom.nodes[parent as usize].first_child;
                while c != NONE && found.is_none() {
                    let usable = dom.nodes[c as usize].tag == atom::SOURCE && dom.attr(c, atom::MEDIA).is_none_or(|m| media_matches(m, vw, vh));
                    if usable {
                        found = dom.attr(c, atom::SRCSET).and_then(first_candidate).and_then(|u| self.images.file(u));
                    }
                    c = dom.nodes[c as usize].next;
                }
            }
            found
                .or_else(|| dom.attr(n, if node.tag == atom::VIDEO { atom::POSTER } else { atom::SRC }).and_then(|u| self.images.file(u)))
                .or_else(|| dom.attr(n, atom::SRCSET).and_then(first_candidate).and_then(|u| self.images.file(u)))
        };
        let p = id.map(|id| (id, self.images.size(id))).map(|(id, (w, h))| (id, w, h)).filter(|p| p.1 > 0.0 && p.2 > 0.0);
        if id.is_none() && self.images.misses() != misses {
            self.waiting.insert(n, ());
        }
        self.pics.insert(n, p);
        p
    }

    /// Natural size of a replaced element: `width`/`height` attributes win
    /// over the image's own size, keeping its aspect ratio.
    fn natural(&mut self, n: NodeId) -> Option<(f32, f32)> {
        let dom = self.dom;
        if !dom.shows_image(n) {
            return None;
        }
        let num = |a| dom.attr(n, a).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).filter(|v| *v > 0.0);
        let (aw, ah) = (num(atom::WIDTH), num(atom::HEIGHT));
        let pic = self.picture(n).map(|p| (p.1, p.2));
        match (aw, ah, pic) {
            (Some(w), Some(h), _) => Some((w, h)),
            (Some(w), None, Some((pw, ph))) => Some((w, w * ph / pw)),
            (None, Some(h), Some((pw, ph))) => Some((h * pw / ph, h)),
            (None, None, p) => p,
            _ => None,
        }
    }

    /// Max-content width of a box including its margins, memoized per node.
    fn intrinsic(&mut self, n: NodeId) -> f32 {
        let cached = self.intr[n as usize];
        if !cached.is_nan() {
            return cached;
        }
        let dom = self.dom;
        let st = self.st;
        let s = &st[n as usize];
        let sd = sides(s, 0.0);
        let extra = hextra(s, &sd);
        let tag = dom.nodes[n as usize].tag;
        let content = if let Len::Px(v) = s.width {
            if s.border_box { (v - extra).max(0.0) } else { v }
        } else if let Some((iw, ih)) = self.natural(n) {
            match s.height {
                Len::Px(h) => h * iw / ih,
                _ => iw,
            }
        } else if tag == atom::IMG || tag == atom::INPUT {
            let attr = if tag == atom::IMG { atom::ALT } else { atom::VALUE };
            let text = dom.attr(n, attr).unwrap_or("");
            match dom.attr(n, atom::WIDTH).and_then(|v| v.trim().parse::<f32>().ok()) {
                Some(w) if tag == atom::IMG => (w - extra).max(0.0),
                _ if self.waiting.contains_key(&n) => 0.0,
                _ => {
                    let face = face_of(self.fonts, s);
                    self.fonts.measure(face, text, s.font_size, 0.0).max(if tag == atom::IMG { 16.0 } else { 0.0 })
                }
            }
        } else if s.grid && s.rare().subgrid == 0 {
            self.grid_intrinsic(n)
        } else if matches!(s.display, Display::Flex | Display::InlineFlex) {
            let (mut sum, mut max, mut count) = (0.0f32, 0.0f32, 0.0f32);
            let mut kids = Vec::new();
            self.flex_children(n, &mut kids);
            for &c in &kids {
                let node = &dom.nodes[c as usize];
                let w = if node.tag == atom::TEXT {
                    if is_ws(dom.text_of(c)) { NAN } else { self.text_intrinsic(c) }
                } else if st[c as usize].display == Display::None || is_abs(&st[c as usize]) {
                    NAN
                } else {
                    self.intrinsic(c)
                };
                if !w.is_nan() {
                    sum += w;
                    max = max.max(w);
                    count += 1.0;
                }
            }
            if s.flex_col { max } else { sum + s.gap_col * (count - 1.0).max(0.0) }
        } else {
            let mut state = IState::default();
            self.intr_children(n, &mut state);
            state.max.max(state.cur)
        };
        let mut w = content + extra;
        let grow = if s.border_box { 0.0 } else { extra };
        if let Len::Px(m) = s.max_w {
            w = w.min(m + grow);
        }
        if let Len::Px(m) = s.min_w {
            w = w.max(m + grow);
        }
        let r = w + sd.ml + sd.mr;
        self.intr[n as usize] = r;
        r
    }

    fn text_intrinsic(&mut self, n: NodeId) -> f32 {
        let mut state = IState::default();
        self.intr_text(n, &mut state);
        state.max.max(state.cur)
    }

    fn intr_children(&mut self, n: NodeId, state: &mut IState) {
        let dom = self.dom;
        let st = self.st;
        let mut c = dom.nodes[n as usize].first_child;
        while c != NONE {
            let node = &dom.nodes[c as usize];
            if node.tag == atom::TEXT {
                self.intr_text(c, state);
            } else {
                let cs = &st[c as usize];
                match cs.display {
                    Display::None => {}
                    _ if is_abs(cs) => {}
                    // Floats sit beside each other and the line.
                    _ if cs.float != 0 => state.cur += self.intrinsic(c),
                    Display::Inline if node.tag == atom::BR => state.break_line(),
                    Display::Inline => {
                        let sd = sides(cs, 0.0);
                        state.cur += sd.ml + cs.bw[3] + sd.pl;
                        self.intr_children(c, state);
                        state.cur += sd.pr + cs.bw[1] + sd.mr;
                    }
                    Display::InlineBlock | Display::InlineFlex => {
                        let w = self.intrinsic(c);
                        if state.space && state.content {
                            let ps = &st[n as usize];
                            let face = face_of(self.fonts, ps);
                            state.cur += self.fonts.lookup(face, ' ').2 * ps.font_size + ps.spacing;
                        }
                        state.cur += w;
                        state.space = false;
                        state.content = true;
                    }
                    _ => {
                        state.break_line();
                        let w = self.intrinsic(c);
                        state.max = state.max.max(w);
                    }
                }
            }
            c = node.next;
        }
    }

    fn intr_text(&mut self, n: NodeId, state: &mut IState) {
        let dom = self.dom;
        let s = &self.st[dom.nodes[n as usize].parent as usize];
        let text = dom.text_of(n);
        let (face, size, spacing) = (face_of(self.fonts, s), s.font_size, s.spacing);
        let transform = s.transform;
        let measure = |fonts: &mut Fonts, w: &str| match transform {
            TextTransform::Upper => fonts.measure(face, &w.to_uppercase(), size, spacing),
            TextTransform::Capitalize => fonts.measure(face, &capitalize(w, true), size, spacing),
            _ => fonts.measure(face, w, size, spacing),
        };
        if s.white_space == WhiteSpace::Pre {
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    state.break_line();
                }
                state.cur += measure(self.fonts, &line.replace('\t', "    "));
            }
            return;
        }
        let space_w = self.fonts.lookup(face, ' ').2 * size + spacing;
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if is_space(b[i]) {
                state.space = true;
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && !is_space(b[i]) {
                i += 1;
            }
            if state.space && state.content {
                state.cur += space_w;
            }
            state.space = false;
            state.content = true;
            state.cur += measure(self.fonts, &text[start..i]);
        }
    }
}

/// First URL of a `srcset` list.
fn first_candidate(srcset: &str) -> Option<&str> {
    srcset.split(',').next()?.split_ascii_whitespace().next()
}

/// Makes serialized inline SVG parseable on its own: the HTML parser
/// lowercased case-sensitive SVG names, and fill comes from CSS.
fn svg_markup(mut m: String, fill: Color, color: Color) -> String {
    let hex = |c: Color| if c.a() == 0 { "none".to_string() } else { format!("#{:06x}", c.0 & 0x00FF_FFFF) };
    let head = &m[..m.find('>').unwrap_or(0)];
    let mut add = String::new();
    if !head.contains(" fill=") {
        add += &format!(" fill=\"{}\"", hex(fill));
    }
    if !head.contains(" color=") {
        add += &format!(" color=\"{}\"", hex(color));
    }
    if !head.contains("xmlns=") {
        add += " xmlns=\"http://www.w3.org/2000/svg\"";
    }
    if !head.contains("xmlns:xlink=") {
        add += " xmlns:xlink=\"http://www.w3.org/1999/xlink\"";
    }
    m.insert_str(4.min(m.len()), &add);
    const NAMES: [(&str, &str); 7] = [
        ("viewbox=", "viewBox="),
        ("preserveaspectratio=", "preserveAspectRatio="),
        ("lineargradient", "linearGradient"),
        ("radialgradient", "radialGradient"),
        ("clippath", "clipPath"),
        ("gradientunits=", "gradientUnits="),
        ("gradienttransform=", "gradientTransform="),
    ];
    for (from, to) in NAMES {
        if m.contains(from) {
            m = m.replace(from, to);
        }
    }
    m
}

fn dom_tag(dom: &Dom, n: NodeId) -> u32 {
    dom.nodes[n as usize].tag
}

fn capitalize(unit: &str, word_start: bool) -> String {
    let mut chars = unit.chars();
    match chars.next() {
        Some(c) if word_start => c.to_uppercase().chain(chars).collect(),
        _ => unit.to_string(),
    }
}
