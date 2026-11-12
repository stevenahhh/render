//! Cascade and inheritance: one computed `Style` per DOM node.

use crate::css::*;
use crate::dom::{atom, Atom, Dom, NodeId, NONE};
use crate::FxMap;
use std::rc::Rc;

pub type Vars = FxMap<Box<str>, Box<str>>;

/// Computed style. Lengths are `Auto`, `Px`, `Pct` or `Calc` only; font
/// relative and viewport units are already resolved.
#[derive(Clone)]
pub struct Style {
    pub display: Display,
    pub position: Position,
    pub width: Len,
    pub height: Len,
    pub min_w: Len,
    pub max_w: Len,
    pub min_h: Len,
    pub max_h: Len,
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub inset: [Len; 4],
    /// Used border widths: zero where the border style is none.
    pub bw: [f32; 4],
    pub bc: [Color; 4],
    bc_set: u8,
    bs: u8,
    pub radius: Len,
    pub border_box: bool,
    pub bg: Color,
    pub grad: Option<Rc<Gradient>>,
    pub bg_image: Option<Rc<str>>,
    pub bg_size: BgSize,
    /// `background-position`; percentages refer to the free space around the image.
    pub bg_pos: [Len; 2],
    clip_text: bool,
    pub overflow_hidden: bool,
    /// `transform: translate()`; percentages refer to the box itself.
    pub translate: [Len; 2],
    /// `display: contents` inside a flex container: the box is skipped and
    /// its children become flex items of that container.
    pub contents: bool,
    pub z: i32,
    z_set: bool,
    /// The box forms a stacking context ordered by `z`.
    pub z_ctx: bool,
    /// Column count of a grid container, kept so its items can size themselves.
    grid_cols: u8,
    grid_even: bool,
    grid_flow_col: bool,
    /// Row lines of a grid container, so its items can resolve `grid-row`.
    row_lines: Option<Rc<RowLines>>,
    /// The box is a grid container (laid out as flex).
    grid: bool,
    /// Start row line this grid item is placed on; 0 when placed automatically.
    pub grid_row: u16,
    /// `order`: flex and grid items are laid out by ascending order.
    pub order: i32,
    own_opacity: f32,
    pub flex_col: bool,
    pub flex_wrap: bool,
    pub justify: Justify,
    pub align_items: Align,
    pub align_self: Align,
    pub grow: f32,
    pub shrink: f32,
    pub basis: Len,
    pub gap_row: f32,
    pub gap_col: f32,

    pub color: Color,
    pub font_size: f32,
    /// `font-weight`, 1 to 1000.
    pub weight: u16,
    pub italic: bool,
    pub family: Family,
    /// The `font-family` list as written, matched against `@font-face` rules.
    pub families: Option<Rc<str>>,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    pub white_space: WhiteSpace,
    pub transform: TextTransform,
    pub spacing: f32,
    pub list_style: ListStyle,
    pub visible: bool,
    pub pointer: bool,
    pub deco: u8,
    pub fill: Color,
    /// Product of this element's opacity and all ancestors'.
    pub opacity: f32,
    pub vars: Option<Rc<Vars>>,
}

impl Style {
    pub fn initial() -> Style {
        Style {
            display: Display::Inline,
            position: Position::Static,
            width: Len::Auto,
            height: Len::Auto,
            min_w: Len::Auto,
            max_w: Len::Auto,
            min_h: Len::Auto,
            max_h: Len::Auto,
            margin: [Len::Px(0.0); 4],
            padding: [Len::Px(0.0); 4],
            inset: [Len::Auto; 4],
            bw: [3.0; 4],
            bc: [Color::BLACK; 4],
            bc_set: 0,
            bs: 0,
            radius: Len::Px(0.0),
            border_box: false,
            bg: Color::TRANSPARENT,
            grad: None,
            bg_image: None,
            bg_size: BgSize::Auto,
            bg_pos: [Len::Pct(0.0); 2],
            clip_text: false,
            overflow_hidden: false,
            translate: [Len::Px(0.0); 2],
            contents: false,
            z: 0,
            z_set: false,
            z_ctx: false,
            grid_cols: 1,
            grid_even: false,
            grid_flow_col: false,
            row_lines: None,
            grid: false,
            grid_row: 0,
            order: 0,
            own_opacity: 1.0,
            flex_col: false,
            flex_wrap: false,
            justify: Justify::Start,
            align_items: Align::Stretch,
            align_self: Align::Auto,
            grow: 0.0,
            shrink: 1.0,
            basis: Len::Auto,
            gap_row: 0.0,
            gap_col: 0.0,
            color: Color::BLACK,
            font_size: 16.0,
            weight: 400,
            italic: false,
            family: Family::Serif,
            families: None,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Left,
            white_space: WhiteSpace::Normal,
            transform: TextTransform::None,
            spacing: 0.0,
            list_style: ListStyle::Disc,
            visible: true,
            pointer: false,
            deco: 0,
            fill: Color::BLACK,
            opacity: 1.0,
            vars: None,
        }
    }

    fn inherit(p: &Style) -> Style {
        Style {
            color: p.color,
            font_size: p.font_size,
            weight: p.weight,
            italic: p.italic,
            family: p.family,
            families: p.families.clone(),
            line_height: p.line_height,
            text_align: p.text_align,
            white_space: p.white_space,
            transform: p.transform,
            spacing: p.spacing,
            list_style: p.list_style,
            visible: p.visible,
            pointer: p.pointer,
            deco: p.deco,
            fill: p.fill,
            vars: p.vars.clone(),
            ..Style::initial()
        }
    }

    /// Computed values under their CSS property names, `name:value;` each,
    /// so a tool can compare them with a browser's `getComputedStyle`.
    pub fn css(&self) -> String {
        let len = |l: Len| match l {
            Len::Auto => "auto".to_string(),
            Len::Px(v) => format!("{v}px"),
            Len::Pct(p) => format!("{p}%"),
            Len::Calc(p, v) => format!("calc({p}% + {v}px)"),
            _ => "?".to_string(),
        };
        let display = match self.display {
            Display::None => "none",
            Display::Block => "block",
            Display::Inline => "inline",
            Display::InlineBlock => "inline-block",
            Display::Flex => "flex",
            Display::InlineFlex => "inline-flex",
            Display::ListItem => "list-item",
            Display::Grid | Display::Contents | Display::Inherit => "?",
        };
        let position = match self.position {
            Position::Static => "static",
            Position::Relative => "relative",
            Position::Absolute => "absolute",
            Position::Fixed => "fixed",
        };
        let justify = match self.justify {
            Justify::Start => "start",
            Justify::Center => "center",
            Justify::End => "end",
            Justify::Between => "space-between",
            Justify::Around => "space-around",
            Justify::Evenly => "space-evenly",
        };
        let align = match self.align_items {
            Align::Auto | Align::Stretch => "stretch",
            Align::Start => "start",
            Align::Center => "center",
            Align::End => "end",
        };
        let text_align = match self.text_align {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
        };
        let white_space = match self.white_space {
            WhiteSpace::Normal => "normal",
            WhiteSpace::NoWrap => "nowrap",
            WhiteSpace::Pre => "pre",
        };
        let line_height = match self.line_height {
            LineHeight::Normal => "normal".to_string(),
            LineHeight::Number(n) => format!("{}px", n * self.font_size),
            LineHeight::Len(l) => len(l),
        };
        let z = if self.z_set { self.z.to_string() } else { "auto".to_string() };
        let mut out = format!(
            "display:{display};position:{position};box-sizing:{};width:{};height:{};min-height:{};max-width:{};",
            if self.border_box { "border-box" } else { "content-box" },
            len(self.width),
            len(self.height),
            len(self.min_h),
            len(self.max_w)
        );
        for (i, side) in ["top", "right", "bottom", "left"].iter().enumerate() {
            out += &format!(
                "margin-{side}:{};padding-{side}:{};border-{side}-width:{}px;{side}:{};",
                len(self.margin[i]),
                len(self.padding[i]),
                self.bw[i],
                len(self.inset[i])
            );
        }
        out += &format!(
            "font-size:{}px;font-weight:{};line-height:{line_height};text-align:{text_align};white-space:{white_space};opacity:{};z-index:{z};",
            self.font_size,
            if self.weight >= 600 { "bold" } else { "normal" },
            self.own_opacity
        );
        out += &format!(
            "flex-direction:{};flex-wrap:{};flex-grow:{};flex-shrink:{};justify-content:{justify};align-items:{align}",
            if self.flex_col { "column" } else { "row" },
            if self.flex_wrap { "wrap" } else { "nowrap" },
            self.grow,
            self.shrink
        );
        out
    }

    pub fn has_deco(&self) -> bool {
        self.bg.a() > 0 || self.grad.is_some() || self.bw.iter().any(|w| *w > 0.0)
    }
}

struct Ctx {
    root_fs: f32,
    vw: f32,
    vh: f32,
}

fn resolve(l: Len, em: f32, c: &Ctx) -> Len {
    match l {
        Len::Em(v) => Len::Px(v * em),
        Len::Rem(v) => Len::Px(v * c.root_fs),
        Len::Vw(v) => Len::Px(v * c.vw * 0.01),
        Len::Vh(v) => Len::Px(v * c.vh * 0.01),
        o => o,
    }
}

/// `background-clip: text` shows the background only through the glyphs. Glyphs
/// are not used as a clip here, so the box paints no background and transparent
/// text takes one colour of it: the opaque gradient stop nearest the middle, or
/// the background colour.
fn text_from_background(st: &mut Style) {
    let middle = |s: &&(f32, Color)| ((s.0 - 0.5).abs() * 1e4) as u32;
    let grad = st.grad.take();
    let from = grad.as_ref().and_then(|g| g.stops.iter().filter(|s| s.1.a() > 0).min_by_key(middle)).map_or(st.bg, |s| s.1);
    if st.color.a() == 0 && from.a() > 0 {
        st.color = from;
    }
    st.bg = Color::TRANSPARENT;
}

/// Pixels per `unit` in a value resolved while styling: viewport units and `rem`
/// always, `em` only when its size is given (in `font-size`, the parent's).
fn unit_px(unit: &str, em: Option<f32>, c: &Ctx) -> Option<f32> {
    match unit {
        "vw" => Some(c.vw * 0.01),
        "vh" => Some(c.vh * 0.01),
        "vmin" => Some(c.vw.min(c.vh) * 0.01),
        "vmax" => Some(c.vw.max(c.vh) * 0.01),
        "rem" => Some(c.root_fs),
        "em" => em,
        _ => None,
    }
}

fn px(l: Len, em: f32, c: &Ctx) -> f32 {
    match resolve(l, em, c) {
        Len::Px(v) => v,
        _ => 0.0,
    }
}

fn apply_font_size(st: &mut Style, parent: &Style, fs: FontSize, c: &Ctx) {
    st.font_size = match fs {
        FontSize::Larger => parent.font_size * 1.2,
        FontSize::Smaller => parent.font_size / 1.2,
        FontSize::Len(Len::Pct(p)) => parent.font_size * p * 0.01,
        FontSize::Len(l) => px(l, parent.font_size, c),
    };
}

fn apply(st: &mut Style, parent: &Style, d: &Decl, c: &Ctx) {
    let em = st.font_size;
    let r = |l: Len| resolve(l, em, c);
    match *d {
        Decl::Display(v) => st.display = v,
        Decl::Position(v) => st.position = v,
        Decl::Width(l) => st.width = r(l),
        Decl::Height(l) => st.height = r(l),
        Decl::MinWidth(l) => st.min_w = r(l),
        Decl::MaxWidth(l) => st.max_w = r(l),
        Decl::MinHeight(l) => st.min_h = r(l),
        Decl::MaxHeight(l) => st.max_h = r(l),
        Decl::Margin(i, l) => st.margin[i as usize] = r(l),
        Decl::Padding(i, l) => st.padding[i as usize] = r(l),
        Decl::Inset(i, l) => st.inset[i as usize] = r(l),
        Decl::BorderWidth(i, l) => st.bw[i as usize] = px(l, em, c).max(0.0),
        Decl::BorderStyle(i, on) => st.bs = st.bs & !(1 << i) | (on as u8) << i,
        Decl::BorderColor(i, col) => match col {
            Some(col) => {
                st.bc[i as usize] = col;
                st.bc_set |= 1 << i;
            }
            None => st.bc_set &= !(1 << i),
        },
        Decl::Radius(l) => st.radius = r(l),
        Decl::BoxSizing(b) => st.border_box = b,
        Decl::Background(col) => st.bg = col,
        Decl::Gradient(ref g) => st.grad = g.clone(),
        Decl::Color(col) => st.color = col.unwrap_or(parent.color),
        Decl::Weight(w) => st.weight = relative_weight(w, parent.weight),
        Decl::Italic(b) => st.italic = b,
        Decl::Family(f, ref list) => (st.family, st.families) = (f, Some(list.clone())),
        Decl::LineHeight(lh) => {
            st.line_height = match lh {
                LineHeight::Len(Len::Pct(p)) => LineHeight::Len(Len::Px(p * 0.01 * em)),
                LineHeight::Len(l) => LineHeight::Len(Len::Px(px(l, em, c))),
                o => o,
            }
        }
        Decl::TextAlign(v) => st.text_align = v,
        Decl::TextDeco(v) => st.deco = v,
        Decl::WhiteSpace(v) => st.white_space = v,
        Decl::TextTransform(v) => st.transform = v,
        Decl::LetterSpacing(l) => st.spacing = px(l, em, c),
        Decl::ListStyle(v) => st.list_style = v,
        Decl::FlexDir(v) => st.flex_col = v,
        Decl::FlexWrap(v) => st.flex_wrap = v,
        Decl::Justify(v) => st.justify = v,
        Decl::AlignItems(v) => st.align_items = v,
        Decl::AlignSelf(v) => st.align_self = v,
        Decl::Grow(v) => st.grow = v.max(0.0),
        Decl::Shrink(v) => st.shrink = v.max(0.0),
        Decl::Basis(l) => st.basis = r(l),
        Decl::RowGap(l) => st.gap_row = px(l, em, c),
        Decl::ColGap(l) => st.gap_col = px(l, em, c),
        Decl::Overflow(v) => st.overflow_hidden = v,
        Decl::BgImage(ref u) => st.bg_image = u.clone(),
        Decl::BgSize(v) => {
            st.bg_size = match v {
                BgSize::Size(w, h) => BgSize::Size(r(w), r(h)),
                v => v,
            }
        }
        Decl::BgPos(x, y) => st.bg_pos = [r(x), r(y)],
        Decl::ClipText(v) => st.clip_text = v,
        Decl::Visible(v) => st.visible = v,
        Decl::Opacity(v) => st.own_opacity = v,
        Decl::Pointer(v) => st.pointer = v,
        Decl::Fill(c) => st.fill = c.unwrap_or(st.color),
        Decl::Translate(x, y) => st.translate = [r(x), r(y)],
        Decl::ZIndex(z) => (st.z, st.z_set) = (z.unwrap_or(0), z.is_some()),
        Decl::GridCols(n, even) => (st.grid_cols, st.grid_even) = (n, even),
        Decl::GridFlowCol(v) => st.grid_flow_col = v,
        Decl::GridRows(ref l) => st.row_lines = l.clone().or_else(|| parent.row_lines.clone()),
        Decl::GridRow(ref g) => st.grid_row = g.as_ref().map_or(0, |g| row_line(parent, g)),
        Decl::Order(v) => st.order = v,
        Decl::FontSize(_) | Decl::Var(..) | Decl::Deferred(..) => {}
    }
}

/// A `font-weight` against the parent's, with CSS Fonts 4's table for
/// `bolder` and `lighter`.
fn relative_weight(w: FontWeight, parent: u16) -> u16 {
    match w {
        FontWeight::Abs(n) => n,
        FontWeight::Bolder if parent < 350 => 400,
        FontWeight::Bolder if parent < 550 => 700,
        FontWeight::Bolder => parent.max(900),
        FontWeight::Lighter if parent < 550 => parent.min(100),
        FontWeight::Lighter if parent < 750 => 400,
        FontWeight::Lighter => 700,
    }
}

/// The line number a grid item's `grid-row` start refers to in its parent
/// grid, or 0 when the parent is not a grid or has no such line.
fn row_line(grid: &Style, g: &GridLine) -> u16 {
    if !grid.grid {
        return 0;
    }
    let lines = grid.row_lines.as_deref();
    match g {
        GridLine::Name(n) => lines.map_or(0, |l| {
            let find = |want: &str| l.names.iter().find(|(name, _)| &**name == want).map(|&(_, i)| i);
            find(n).or_else(|| find(&format!("{n}-start"))).unwrap_or(0)
        }),
        &GridLine::Num(k) if k > 0 => k as u16,
        &GridLine::Num(k) => lines.map_or(0, |l| (l.count as i32 + 1 + k as i32).max(0) as u16),
    }
}

fn subst(v: &str, vars: Option<&Vars>, depth: u32) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let mut out = String::new();
    let mut rest = v;
    while let Some(p) = rest.find("var(") {
        out.push_str(&rest[..p]);
        let b = rest.as_bytes();
        let (mut i, mut d) = (p + 4, 1);
        while i < b.len() {
            if b[i] == b'(' {
                d += 1;
            } else if b[i] == b')' {
                d -= 1;
                if d == 0 {
                    break;
                }
            }
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        let inner = &rest[p + 4..i];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        match vars.and_then(|m| m.get(name)) {
            Some(val) => out.push_str(&subst(val, vars, depth + 1)?),
            None => out.push_str(&subst(fallback?, vars, depth + 1)?),
        }
        rest = &rest[i + 1..];
    }
    out.push_str(rest);
    Some(out)
}

struct Matcher<'a> {
    dom: &'a Dom,
    hover: &'a [NodeId],
    /// Position of each element among its element siblings, filled per
    /// parent on first use.
    idx: Vec<Sib>,
}

/// 1-based position among element siblings, and among those sharing the tag,
/// with the count of each. `pos == 0` marks a node not filled in yet.
#[derive(Clone, Copy, Default)]
struct Sib {
    pos: u32,
    count: u32,
    type_pos: u32,
    type_count: u32,
}

fn nth(a: i32, b: i32, i: i32) -> bool {
    if a == 0 {
        return i == b;
    }
    let d = i - b;
    d % a == 0 && d / a >= 0
}

impl<'a> Matcher<'a> {
    /// Where element `n` stands among its parent's element children. One walk
    /// over the children fills every sibling, so structural pseudo-classes
    /// cost O(children) per parent instead of O(children) per element.
    fn sib(&mut self, n: NodeId) -> Sib {
        let nodes = &self.dom.nodes;
        let parent = nodes[n as usize].parent;
        if parent == NONE {
            return Sib { pos: 1, count: 1, type_pos: 1, type_count: 1 };
        }
        if self.idx.is_empty() {
            self.idx = vec![Sib::default(); nodes.len()];
        }
        if self.idx[n as usize].pos == 0 {
            let first = nodes[parent as usize].first_child;
            let mut types: FxMap<Atom, u32> = FxMap::default();
            let (mut c, mut k) = (first, 0);
            while c != NONE {
                if self.dom.is_element(c) {
                    k += 1;
                    let t = types.entry(nodes[c as usize].tag).or_insert(0);
                    *t += 1;
                    self.idx[c as usize] = Sib { pos: k, count: 0, type_pos: *t, type_count: 0 };
                }
                c = nodes[c as usize].next;
            }
            c = first;
            while c != NONE {
                if self.dom.is_element(c) {
                    let s = &mut self.idx[c as usize];
                    (s.count, s.type_count) = (k, types[&nodes[c as usize].tag]);
                }
                c = nodes[c as usize].next;
            }
        }
        self.idx[n as usize]
    }

    fn pseudo(&mut self, p: &Pseudo, n: NodeId) -> bool {
        let node = &self.dom.nodes[n as usize];
        match p {
            Pseudo::Root => node.parent == 0,
            Pseudo::FirstChild => self.sib(n).pos == 1,
            Pseudo::LastChild => self.sib(n).pos == self.sib(n).count,
            Pseudo::OnlyChild => self.sib(n).count == 1,
            Pseudo::NthChild(a, b) => nth(*a, *b, self.sib(n).pos as i32),
            Pseudo::NthLastChild(a, b) => nth(*a, *b, (self.sib(n).count - self.sib(n).pos + 1) as i32),
            Pseudo::FirstOfType => self.sib(n).type_pos == 1,
            Pseudo::LastOfType => self.sib(n).type_pos == self.sib(n).type_count,
            Pseudo::NthOfType(a, b) => nth(*a, *b, self.sib(n).type_pos as i32),
            Pseudo::Not(c) => !self.compound(c, n),
            Pseudo::Has(parts) => {
                let mut c = node.first_child;
                while c != NONE {
                    if self.dom.is_element(c) && self.matches(parts, c) {
                        return true;
                    }
                    c = dfs_next(self.dom, c, n, true);
                }
                false
            }
            Pseudo::Hover => self.hover.contains(&n),
            Pseudo::Link => node.tag == atom::A && self.dom.attr(n, atom::HREF).is_some(),
            Pseudo::Empty => node.first_child == NONE,
            Pseudo::Never => false,
        }
    }

    fn compound(&mut self, c: &Compound, n: NodeId) -> bool {
        let node = &self.dom.nodes[n as usize];
        if (c.tag != 0 && c.tag != node.tag) || (c.id != 0 && c.id != node.id) {
            return false;
        }
        if !c.classes.is_empty() {
            let have = self.dom.classes_of(n);
            if !c.classes.iter().all(|cl| have.contains(cl)) {
                return false;
            }
        }
        for a in &c.attrs {
            let Some(v) = self.dom.attr(n, a.name) else { return false };
            let want = &*a.value;
            let ok = match a.op {
                AttrOp::Exists => true,
                AttrOp::Eq => v == want,
                AttrOp::Includes => v.split_ascii_whitespace().any(|w| w == want),
                AttrOp::Dash => v == want || (v.starts_with(want) && v[want.len()..].starts_with('-')),
                AttrOp::Prefix => !want.is_empty() && v.starts_with(want),
                AttrOp::Suffix => !want.is_empty() && v.ends_with(want),
                AttrOp::Substr => !want.is_empty() && v.contains(want),
            };
            if !ok {
                return false;
            }
        }
        c.pseudos.iter().all(|p| self.pseudo(p, n))
    }

    /// `parts` runs right to left; descendant and sibling combinators backtrack.
    fn matches(&mut self, parts: &[Part], n: NodeId) -> bool {
        if !self.compound(&parts[0].c, n) {
            return false;
        }
        if parts.len() == 1 {
            return true;
        }
        let nodes = &self.dom.nodes;
        let rest = &parts[1..];
        match parts[0].comb {
            Comb::Child => {
                let p = nodes[n as usize].parent;
                p != 0 && p != NONE && self.matches(rest, p)
            }
            Comb::Descendant => {
                let mut p = nodes[n as usize].parent;
                while p != 0 && p != NONE {
                    if self.matches(rest, p) {
                        return true;
                    }
                    p = nodes[p as usize].parent;
                }
                false
            }
            Comb::Adjacent | Comb::Sibling => {
                let mut s = nodes[n as usize].prev;
                while s != NONE {
                    if self.dom.is_element(s) {
                        if self.matches(rest, s) {
                            return true;
                        }
                        if parts[0].comb == Comb::Adjacent {
                            return false;
                        }
                    }
                    s = nodes[s as usize].prev;
                }
                false
            }
            Comb::None => false,
        }
    }
}

/// Next node of a depth-first walk below `root`. With `descend` false the
/// subtree of `n` is skipped.
fn dfs_next(dom: &Dom, n: NodeId, root: NodeId, descend: bool) -> NodeId {
    let nodes = &dom.nodes;
    if descend && nodes[n as usize].first_child != NONE {
        return nodes[n as usize].first_child;
    }
    let mut c = n;
    while c != root && c != NONE {
        if nodes[c as usize].next != NONE {
            return nodes[c as usize].next;
        }
        c = nodes[c as usize].parent;
    }
    NONE
}

/// Elements below `root` that match a selector list, in document order.
pub fn query(dom: &mut Dom, root: NodeId, selector: &str, first_only: bool) -> Vec<NodeId> {
    let sels = parse_selector_list(selector, &mut dom.atoms);
    let dom = &*dom;
    let mut m = Matcher { dom, hover: &[], idx: Vec::new() };
    let mut out = Vec::new();
    let mut i = if sels.is_empty() { NONE } else { dom.nodes[root as usize].first_child };
    while i != NONE {
        if dom.is_element(i) && sels.iter().any(|p| m.matches(p, i)) {
            out.push(i);
            if first_only {
                break;
            }
        }
        i = dfs_next(dom, i, root, true);
    }
    out
}

pub fn matches(dom: &mut Dom, n: NodeId, selector: &str) -> bool {
    let sels = parse_selector_list(selector, &mut dom.atoms);
    let dom = &*dom;
    let mut m = Matcher { dom, hover: &[], idx: Vec::new() };
    dom.is_element(n) && sels.iter().any(|p| m.matches(p, n))
}

/// Adds or removes an element's tag, id and classes in the counting filter.
/// A counter that saturates stays set, which only costs a wasted match.
fn bloom_update(bloom: &mut [u8; 4096], dom: &Dom, n: NodeId, add: bool) {
    let node = &dom.nodes[n as usize];
    for a in [node.tag, node.id].into_iter().chain(dom.classes_of(n).iter().copied()) {
        let c = &mut bloom[bloom_slot(a) as usize];
        if a != 0 && *c != 255 {
            *c = if add { *c + 1 } else { c.saturating_sub(1) };
        }
    }
}

/// Visits declarations in cascade order: matched rules, inline style, then
/// the `!important` halves of both.
fn each<'s>(sheet: &'s Sheet, matched: &[(u64, u32)], inl: &'s (Vec<Decl>, Vec<Decl>, bool), mut f: impl FnMut(&'s Decl)) {
    for &(_, r) in matched {
        sheet.rules[r as usize].decls.iter().for_each(&mut f);
    }
    inl.0.iter().for_each(&mut f);
    for &(_, r) in matched {
        sheet.rules[r as usize].important.iter().for_each(&mut f);
    }
    inl.1.iter().for_each(&mut f);
}

/// Everything a `Deferred` stylesheet declaration's expansion depends on: the
/// declaration, the custom properties it reads, the `em` size it resolves
/// against (`font-size` only) and the root font size. The viewport is fixed
/// for one `compute_styles` call.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct DeferredKey {
    decl: usize,
    vars: usize,
    em: Option<u32>,
    root_fs: u32,
}

/// Computes a style for every node. Nodes are stored in document order, so a
/// single forward pass always sees the parent's style first.
pub fn compute_styles(dom: &Dom, sheet: &Sheet, vw: f32, vh: f32, hover: NodeId) -> Vec<Style> {
    let n = dom.nodes.len();
    let mut styles = vec![Style::initial(); n];
    let media_ok: Vec<bool> = sheet.media.iter().map(|m| m.matches(vw, vh)).collect();
    let mut chain = Vec::new();
    let mut h = hover;
    while h != NONE && h != 0 {
        chain.push(h);
        h = dom.nodes[h as usize].parent;
    }
    let mut m = Matcher { dom, hover: &chain, idx: Vec::new() };
    let mut matched: Vec<(u64, u32)> = Vec::new();
    let mut expanded: Vec<Decl> = Vec::new();
    // Every `Vars` map stays alive in `styles` until this call returns, so its
    // address identifies its contents for the whole pass.
    let mut deferred: FxMap<DeferredKey, Vec<Decl>> = FxMap::default();
    // Custom-property maps by (inherited map, custom-property declarations added).
    let mut own_vars: FxMap<(usize, Vec<usize>), Rc<Vars>> = FxMap::default();
    let mut var_decls: Vec<usize> = Vec::new();
    let mut ctx = Ctx { root_fs: 16.0, vw, vh };
    let no_inline = (Vec::new(), Vec::new(), false);
    let mut bloom = [0u8; 4096];
    let mut ancestors: Vec<NodeId> = Vec::new();

    let mut i = dom.nodes[0].first_child;
    while i != NONE {
        let node = &dom.nodes[i as usize];
        if node.tag == atom::TEXT {
            i = dfs_next(dom, i, 0, false);
            continue;
        }
        while let Some(&top) = ancestors.last() {
            if top == node.parent {
                break;
            }
            ancestors.pop();
            bloom_update(&mut bloom, dom, top, false);
        }
        let parent = &styles[node.parent as usize];
        let mut st = Style::inherit(parent);

        matched.clear();
        let mut test = |list: Option<&Vec<u32>>, m: &mut Matcher| {
            for &si in list.into_iter().flatten() {
                let sel = &sheet.sels[si as usize];
                let media = sheet.rules[sel.rule as usize].media;
                if media != u32::MAX && !media_ok[media as usize] {
                    continue;
                }
                if sel.anc[..sel.anc_n as usize].iter().any(|&h| bloom[h as usize] == 0) {
                    continue;
                }
                if m.matches(&sel.parts, i) {
                    matched.push(((sel.level as u64) << 60 | (sel.spec as u64) << 32 | si as u64, sel.rule));
                }
            }
        };
        if node.id != 0 {
            test(sheet.by_id.get(&node.id), &mut m);
        }
        for cl in dom.classes_of(i) {
            test(sheet.by_class.get(cl), &mut m);
        }
        test(sheet.by_tag.get(&node.tag), &mut m);
        test(Some(&sheet.universal), &mut m);
        matched.sort_unstable();

        let inline;
        let inl = match dom.attr(i, atom::STYLE) {
            Some(s) if !s.is_empty() => {
                inline = parse_declarations(s);
                &inline
            }
            _ => &no_inline,
        };

        if inl.2 || matched.iter().any(|&(_, r)| sheet.rules[r as usize].dynamic) {
            var_decls.clear();
            let mut inline_var = false;
            each(sheet, &matched, inl, |d| {
                if let Decl::Var(..) = d {
                    let ptr = d as *const Decl;
                    inline_var |= inl.0.as_ptr_range().contains(&ptr) || inl.1.as_ptr_range().contains(&ptr);
                    var_decls.push(ptr as usize);
                }
            });
            if !var_decls.is_empty() {
                let build = || {
                    let mut own = st.vars.as_deref().cloned().unwrap_or_default();
                    each(sheet, &matched, inl, |d| {
                        if let Decl::Var(k, v) = d {
                            own.insert(k.clone(), v.clone());
                        }
                    });
                    Rc::new(own)
                };
                // Elements that inherit the same map and add the same stylesheet
                // declarations share one map, so `deferred` keys repeat.
                st.vars = Some(if inline_var {
                    build()
                } else {
                    let key = (st.vars.as_ref().map_or(0, |m| Rc::as_ptr(m) as usize), var_decls.clone());
                    own_vars.entry(key).or_insert_with(build).clone()
                });
            }
            expanded.clear();
            let vars = st.vars.as_deref();
            each(sheet, &matched, inl, |d| match d {
                Decl::Var(..) => {}
                Decl::Deferred(name, v) => {
                    let em = (&**name == "font-size").then_some(parent.font_size);
                    let expand = |out: &mut Vec<Decl>| {
                        if let Some(sv) = subst(v, vars, 0) {
                            let sv = crate::css::absolute_lengths(&sv, |u| unit_px(u, em, &ctx)).unwrap_or(sv);
                            parse_decl(name, &sv, out);
                        }
                    };
                    // Inline declarations are freed after each element, so only
                    // stylesheet declarations have an address that stays unique.
                    let ptr = d as *const Decl;
                    if inl.0.as_ptr_range().contains(&ptr) || inl.1.as_ptr_range().contains(&ptr) {
                        expand(&mut expanded);
                    } else {
                        let key = DeferredKey {
                            decl: ptr as usize,
                            vars: vars.map_or(0, |m| m as *const Vars as usize),
                            em: em.map(f32::to_bits),
                            root_fs: ctx.root_fs.to_bits(),
                        };
                        let out = deferred.entry(key).or_insert_with(|| {
                            let mut out = Vec::new();
                            expand(&mut out);
                            out
                        });
                        expanded.extend(out.iter().cloned());
                    }
                }
                d => expanded.push(d.clone()),
            });
            for d in &expanded {
                if let Decl::FontSize(fs) = d {
                    apply_font_size(&mut st, parent, *fs, &ctx);
                }
            }
            for d in &expanded {
                apply(&mut st, parent, d, &ctx);
            }
        } else {
            each(sheet, &matched, inl, |d| {
                if let Decl::FontSize(fs) = d {
                    apply_font_size(&mut st, parent, *fs, &ctx);
                }
            });
            each(sheet, &matched, inl, |d| apply(&mut st, parent, d, &ctx));
        }

        for s in 0..4 {
            if st.bs & (1 << s) == 0 {
                st.bw[s] = 0.0;
            }
            if st.bc_set & (1 << s) == 0 {
                st.bc[s] = st.color;
            }
        }
        if st.clip_text {
            text_from_background(&mut st);
        }
        st.opacity = parent.opacity * st.own_opacity;
        let parent_flex = matches!(parent.display, Display::Flex | Display::InlineFlex);
        if st.display == Display::Inherit {
            st.display = match parent.display {
                _ if node.parent == 0 => Display::Block,
                _ if parent.contents => Display::Contents,
                d => d,
            };
        }
        match st.display {
            // No box of its own. Flex layout skips it; elsewhere it falls
            // back to a container that repeats the parent's flex settings.
            Display::Contents if parent_flex => {
                st.display = Display::Flex;
                st.contents = true;
                (st.flex_col, st.justify, st.align_items) = (parent.flex_col, parent.justify, parent.align_items);
                (st.gap_row, st.gap_col, st.grow) = (parent.gap_row, parent.gap_col, 1.0);
            }
            Display::Contents => st.display = Display::Block,
            // Grids are laid out as flex: one column stacks, several columns wrap.
            Display::Grid => {
                st.display = Display::Flex;
                st.grid = true;
                st.flex_col = st.grid_cols <= 1 && !st.grid_flow_col;
                st.flex_wrap = st.grid_cols > 1 && st.grid_even;
            }
            _ => {}
        }
        if parent_flex && parent.grid_cols > 1 && parent.grid_even {
            let n = parent.grid_cols as f32;
            st.basis = Len::Calc(100.0 / n, -parent.gap_col * (n - 1.0) / n);
            st.grow = 0.0;
        }
        // A grid that places its items in columns adds a track per item and
        // overflows rather than squeezing them.
        if parent_flex && parent.grid_flow_col {
            st.shrink = 0.0;
        }
        st.z_ctx = st.z_set && (st.position != Position::Static || parent_flex);
        let blockify = node.parent == 0
            || matches!(st.position, Position::Absolute | Position::Fixed)
            || matches!(parent.display, Display::Flex | Display::InlineFlex);
        if blockify {
            st.display = match st.display {
                Display::Inline | Display::InlineBlock => Display::Block,
                Display::InlineFlex => Display::Flex,
                d => d,
            };
        }
        let replaced = node.tag == atom::INPUT || dom.shows_image(i);
        if replaced && st.display == Display::Inline {
            st.display = Display::InlineBlock;
        }
        if node.tag == atom::HTML {
            ctx.root_fs = st.font_size;
        }
        ancestors.push(i);
        bloom_update(&mut bloom, dom, i, true);
        let descend = st.display != Display::None && node.tag != atom::SVG;
        styles[i as usize] = st;
        i = dfs_next(dom, i, 0, descend);
    }
    styles
}

#[cfg(test)]
mod tests {
    use super::{compute_styles, query, Style};
    use crate::css::Len;
    use crate::page::Page;

    /// Computed styles of the elements matching `sel`, in document order.
    fn styles_of(html: &str, sel: &str) -> Vec<Style> {
        let mut page = Page::from_html(html, None);
        let styles = compute_styles(&page.dom, &page.sheet, 800.0, 600.0, u32::MAX);
        query(&mut page.dom, 0, sel, false).into_iter().map(|n| styles[n as usize].clone()).collect()
    }

    fn widths(styles: &[Style]) -> Vec<Len> {
        styles.iter().map(|s| s.width).collect()
    }

    #[test]
    fn same_rules_under_different_custom_properties_do_not_share_an_expansion() {
        let html = "<style>.a{--w:10px} .b{--w:20px} .x{width:var(--w)}</style>\
                    <div class=a><p class=x></p></div><div class=b><p class=x></p></div>\
                    <div class=a><p class=x></p></div><div class=a><p class=x style=\"--w:5px\"></p><p class=x></p></div>";
        assert_eq!(widths(&styles_of(html, "p")), [Len::Px(10.0), Len::Px(20.0), Len::Px(10.0), Len::Px(5.0), Len::Px(10.0)]);
    }

    #[test]
    fn the_same_custom_property_rule_under_different_inherited_maps_does_not_share_a_map() {
        let html = "<style>:root{--w:1px} .a{--w:10px} .in{--h:3px} .x{width:var(--w);height:var(--h,7px)}</style>\
                    <div class=a><div class=in><p class=x></p></div></div><div class=in><p class=x></p></div><p class=x></p>";
        let s = styles_of(html, "p");
        assert_eq!(widths(&s), [Len::Px(10.0), Len::Px(1.0), Len::Px(1.0)]);
        assert_eq!(s.iter().map(|s| s.height).collect::<Vec<_>>(), [Len::Px(3.0), Len::Px(3.0), Len::Px(7.0)]);
    }

    #[test]
    fn structural_pseudo_classes_count_element_siblings_per_parent_and_tag() {
        let html = "<div id=a><p id=p1></p>text<span id=s1></span><p id=p2></p><!-- c --><span id=s2></span><p id=p3></p></div>\
                    <div id=b><em id=e1></em></div><div id=c><p id=q1></p><span id=t1></span></div>";
        let mut page = Page::from_html(html, None);
        let ids = |page: &mut Page, sel: &str| {
            let found = query(&mut page.dom, 0, sel, false);
            found.iter().map(|&n| page.dom.attr(n, crate::dom::atom::ID).unwrap_or("").to_string()).collect::<Vec<_>>()
        };
        let cases: [(&str, &[&str]); 9] = [
            ("div > :last-child", &["p3", "e1", "t1"]),
            ("div > :only-child", &["e1"]),
            ("div > :nth-child(2n)", &["s1", "s2", "t1"]),
            ("div > :nth-last-child(2)", &["s2", "q1"]),
            ("div > :first-of-type", &["p1", "s1", "e1", "q1", "t1"]),
            ("div > p:last-of-type", &["p3", "q1"]),
            ("div > span:last-of-type", &["s2", "t1"]),
            ("div > :nth-of-type(2)", &["p2", "s2"]),
            ("div > p:nth-of-type(3)", &["p3"]),
        ];
        for (sel, want) in cases {
            assert_eq!(ids(&mut page, sel), want, "{sel}");
        }
    }

    #[test]
    fn a_deferred_font_size_resolves_against_each_parent() {
        let html = "<style>.big{font-size:20px} .small{font-size:10px} .f{font-size:calc(1em + 1px)}</style>\
                    <div class=big><p class=f></p></div><div class=small><p class=f></p></div><div class=big><p class=f></p></div>";
        assert_eq!(styles_of(html, "p").iter().map(|s| s.font_size).collect::<Vec<_>>(), [21.0, 11.0, 21.0]);
    }
}
