//! Cascade and inheritance: one computed `Style` per DOM node.

use crate::css::*;
use crate::dom::{atom, Atom, Dom, NodeId, NONE};
use crate::FxMap;
use std::rc::Rc;

pub type Vars = FxMap<Box<str>, Box<str>>;

/// Computed style. Lengths are `Auto`, `Px`, `Pct` or `Calc` only; font
/// relative and viewport units are already resolved.
#[derive(Clone, PartialEq, Debug)]
pub struct Style {
    pub display: Display,
    pub position: Position,
    pub width: Len,
    pub height: Len,
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub inset: [Len; 4],
    /// Used border widths: zero where the border style is none.
    pub bw: [f32; 4],
    pub bc: [Color; 4],
    bc_set: u8,
    bs: u8,
    /// Top-left, top-right, bottom-right and bottom-left.
    pub radius: [Len; 4],
    pub border_box: bool,
    pub bg: Color,
    clip_text: bool,
    pub overflow_hidden: bool,
    /// `transform` scales the element to nothing: it is drawn as with opacity 0.
    pub scaled_away: bool,
    /// `clip` (bit 0, for absolutely positioned boxes) or `clip-path` (bit 1) cuts the
    /// element to nothing: it is drawn as with opacity 0.
    pub cut_away: u8,
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
    pub grid_flow_col: bool,
    /// The box is a grid container: laid out as a grid, or as flex where it
    /// takes a parent grid's tracks (`subgrid`).
    pub grid: bool,
    pub(crate) own_opacity: f32,
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
    pub webkit_box: bool,
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
    pub justify_items: Align,
    pub justify_self: Align,
    /// `float`: 0, `FLOAT_LEFT` or `FLOAT_RIGHT`; 0 where it does not apply.
    pub float: u8,
    /// `clear`: `CLEAR_LEFT` and `CLEAR_RIGHT` bits.
    pub clear: u8,
    /// The box establishes a block formatting context of its own.
    pub flow_root: bool,
    /// What few elements have, kept out of line: a style is built, copied and
    /// dropped for every node in every pass, so each byte here costs every page.
    rare: RareRef,
}

/// The rare parts of a style, shared between the styles that have the same.
/// None stands for the initial values, and compares equal to them.
#[derive(Clone, Debug, Default)]
struct RareRef(Option<Rc<Rare>>);

impl PartialEq for RareRef {
    fn eq(&self, other: &RareRef) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b) || **a == **b,
            (None, None) => true,
            (Some(r), None) | (None, Some(r)) => **r == NO_RARE,
        }
    }
}

/// The parts of a computed style that few elements have.
#[derive(Clone, PartialEq, Debug)]
pub struct Rare {
    pub min_w: Len,
    pub max_w: Len,
    pub min_h: Len,
    pub max_h: Len,
    pub grad: Option<Rc<Gradient>>,
    /// Outer box shadows, lengths in px.
    pub shadow: Option<Rc<[Shadow]>>,
    pub bg_image: Option<Rc<str>>,
    pub bg_size: BgSize,
    /// `background-position`; percentages refer to the free space around the image.
    pub bg_pos: [Len; 2],
    /// `transform: translate()`; percentages refer to the box itself.
    pub translate: [Len; 2],
    /// Row lines of a grid container, so its items can resolve `grid-row`.
    row_lines: Option<Rc<RowLines>>,
    /// Start row line this grid item is placed on; 0 when placed automatically.
    pub grid_row: u16,
    /// `order`: flex and grid items are laid out by ascending order.
    pub order: i32,
    /// `-webkit-line-clamp`, which counts only for a `-webkit-box`.
    pub line_clamp: u16,
    /// A grid's track lists for rows (0) and columns (1), its areas, the sizes of
    /// its implicit rows and columns, and whether it fills holes (`dense`).
    pub tracks: [Option<Rc<TrackList>>; 2],
    pub areas: Option<Rc<Areas>>,
    pub auto_tracks: [Option<Rc<[Track]>>; 2],
    pub grid_dense: bool,
    /// Bits for rows (1) and columns (2) taken from the parent grid, which only
    /// the flex fallback follows.
    pub subgrid: u8,
    /// Where a grid item goes: row start, column start, row end, column end.
    pub place: Option<Rc<[GridPos; 4]>>,
    /// The element's `::before` and `::after` boxes.
    pub before: Option<Rc<Generated>>,
    pub after: Option<Rc<Generated>>,
    /// `content`, read while styling a `::before` or `::after` box.
    content: Option<Rc<[Content]>>,
}

/// The rare parts of a style that has none of them.
const NO_RARE: Rare = Rare {
    min_w: Len::Auto,
    max_w: Len::Auto,
    min_h: Len::Auto,
    max_h: Len::Auto,
    grad: None,
    shadow: None,
    bg_image: None,
    bg_size: BgSize::Auto,
    bg_pos: [Len::Pct(0.0); 2],
    translate: [Len::Px(0.0); 2],
    row_lines: None,
    grid_row: 0,
    order: 0,
    line_clamp: 0,
    tracks: [None, None],
    areas: None,
    auto_tracks: [None, None],
    grid_dense: false,
    subgrid: 0,
    place: None,
    before: None,
    after: None,
    content: None,
};

/// Sets a field of the rare parts, leaving a style without them as it is
/// when the value is the initial one.
fn put_rare<T: PartialEq>(st: &mut Style, field: fn(&mut Rare) -> &mut T, v: T) {
    if st.rare.0.is_none() {
        let mut initial = NO_RARE;
        if *field(&mut initial) == v {
            return;
        }
    }
    *field(st.rare_mut()) = v;
}

/// A `::before` or `::after` box: its style and what it shows.
#[derive(Clone, PartialEq, Debug)]
pub struct Generated {
    pub style: Style,
    pub content: Box<[Piece]>,
}

/// A piece of generated content, with attribute values filled in.
#[derive(Clone, PartialEq, Debug)]
pub enum Piece {
    Text(Box<str>),
    /// An image, by its URL.
    Image(Rc<str>),
}

impl Generated {
    /// The style of an image that generated content shows, in a box styled `parent`.
    pub(crate) fn image_style(parent: &Style) -> Style {
        Style { display: Display::InlineBlock, opacity: parent.opacity, ..Style::inherit(parent) }
    }
}

impl Style {
    pub fn initial() -> Style {
        Style {
            display: Display::Inline,
            position: Position::Static,
            width: Len::Auto,
            height: Len::Auto,
            margin: [Len::Px(0.0); 4],
            padding: [Len::Px(0.0); 4],
            inset: [Len::Auto; 4],
            bw: [3.0; 4],
            bc: [Color::BLACK; 4],
            bc_set: 0,
            bs: 0,
            radius: [Len::Px(0.0); 4],
            border_box: false,
            bg: Color::TRANSPARENT,
            clip_text: false,
            overflow_hidden: false,
            scaled_away: false,
            cut_away: 0,
            contents: false,
            z: 0,
            z_set: false,
            z_ctx: false,
            grid_cols: 1,
            grid_even: false,
            grid_flow_col: false,
            grid: false,
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
            webkit_box: false,
            transform: TextTransform::None,
            spacing: 0.0,
            list_style: ListStyle::Disc,
            visible: true,
            pointer: false,
            deco: 0,
            fill: Color::BLACK,
            opacity: 1.0,
            vars: None,
            justify_items: Align::Stretch,
            justify_self: Align::Auto,
            float: 0,
            clear: 0,
            flow_root: false,
            rare: RareRef(None),
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

    /// Grid templates and placement, and generated boxes.
    pub fn rare(&self) -> &Rare {
        self.rare.0.as_deref().unwrap_or(&NO_RARE)
    }

    fn rare_mut(&mut self) -> &mut Rare {
        Rc::make_mut(self.rare.0.get_or_insert_with(|| Rc::new(NO_RARE)))
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
            WhiteSpace::PreWrap => "pre-wrap",
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
            len(self.rare().min_h),
            len(self.rare().max_w)
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
        self.bg.a() > 0 || self.rare().grad.is_some() || self.bw.iter().any(|w| *w > 0.0) || self.rare().shadow.is_some()
    }

    /// How the page drawn with this style changes when it becomes `new`.
    pub fn change(&self, new: &Style) -> Change {
        if self == new {
            return Change::None;
        }
        // This style with everything that only colours what is drawn taken from `new`.
        let mut recoloured = Style {
            bg: new.bg,
            bc: new.bc,
            bc_set: new.bc_set,
            radius: new.radius,
            color: new.color,
            fill: new.fill,
            deco: new.deco,
            opacity: new.opacity,
            own_opacity: new.own_opacity,
            pointer: new.pointer,
            vars: new.vars.clone(),
            ..self.clone()
        };
        let (was, now) = (self.rare(), new.rare());
        if was.grad != now.grad || was.shadow != now.shadow {
            let rare = recoloured.rare_mut();
            (rare.grad, rare.shadow) = (now.grad.clone(), now.shadow.clone());
        }
        // A box that gains or loses its decoration, or stops or starts being fully
        // transparent, gains or loses items in the layout.
        if recoloured != *new || self.has_deco() != new.has_deco() || (self.opacity <= 0.0) != (new.opacity <= 0.0) {
            return Change::Layout;
        }
        // The cursor and custom properties draw nothing themselves.
        if (Style { pointer: new.pointer, vars: new.vars.clone(), ..self.clone() }) == *new {
            Change::None
        } else {
            Change::Paint
        }
    }
}

/// How a changed style changes the drawn page.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing drawn differs.
    None,
    /// Only colours, opacity, border colours and radius, or text decoration
    /// differ: every box keeps its place and size.
    Paint,
    /// Something layout reads differs.
    Layout,
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
    let grad = if st.rare().grad.is_some() { st.rare_mut().grad.take() } else { None };
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
        Decl::Display(v) => (st.display, st.flow_root, st.webkit_box) = (v, false, false),
        Decl::FlowRoot(v) => st.flow_root = v,
        Decl::Float(v) => st.float = v,
        Decl::Clear(v) => st.clear = v,
        Decl::Position(v) => st.position = v,
        Decl::Width(l) => st.width = r(l),
        Decl::Height(l) => st.height = r(l),
        Decl::MinWidth(l) => put_rare(st, |r| &mut r.min_w, r(l)),
        Decl::MaxWidth(l) => put_rare(st, |r| &mut r.max_w, r(l)),
        Decl::MinHeight(l) => put_rare(st, |r| &mut r.min_h, r(l)),
        Decl::MaxHeight(l) => put_rare(st, |r| &mut r.max_h, r(l)),
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
        Decl::Radius(i, l) => st.radius[i as usize] = r(l),
        Decl::BoxSizing(b) => st.border_box = b,
        Decl::Background(col) => st.bg = col,
        Decl::Gradient(ref g) => put_rare(st, |r| &mut r.grad, g.clone()),
        Decl::BoxShadow(ref s) => {
            let px = |l: Len| Len::Px(px(l, em, c));
            let shadow = s.as_ref().map(|l| l.iter().map(|sh| Shadow { x: px(sh.x), y: px(sh.y), blur: px(sh.blur), spread: px(sh.spread), color: sh.color }).collect());
            put_rare(st, |r| &mut r.shadow, shadow);
        }
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
        Decl::LineClamp(n) => put_rare(st, |r| &mut r.line_clamp, n),
        Decl::WebkitBox(b) => st.webkit_box = b,
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
        Decl::BgImage(ref u) => put_rare(st, |r| &mut r.bg_image, u.clone()),
        Decl::BgSize(v) => {
            let size = match v {
                BgSize::Size(w, h) => BgSize::Size(r(w), r(h)),
                v => v,
            };
            put_rare(st, |r| &mut r.bg_size, size);
        }
        Decl::BgPos(x, y) => put_rare(st, |r| &mut r.bg_pos, [r(x), r(y)]),
        Decl::ClipText(v) => st.clip_text = v,
        Decl::Visible(v) => st.visible = v.unwrap_or(parent.visible),
        // The font size was settled in its own pass, `all` included; custom properties stay.
        Decl::AllUnset => {
            let (vars, font_size) = (st.vars.take(), st.font_size);
            *st = Style::inherit(parent);
            (st.vars, st.font_size) = (vars, font_size);
        }
        Decl::Opacity(v) => st.own_opacity = v,
        Decl::Pointer(v) => st.pointer = v,
        Decl::Fill(c) => st.fill = c.unwrap_or(st.color),
        Decl::Translate(x, y) => put_rare(st, |r| &mut r.translate, [r(x), r(y)]),
        Decl::ScaledAway(b) => st.scaled_away = b,
        Decl::CutAway(i, b) => st.cut_away = st.cut_away & !(1 << i) | (b as u8) << i,
        Decl::ZIndex(z) => (st.z, st.z_set) = (z.unwrap_or(0), z.is_some()),
        Decl::GridCols(n, even) => (st.grid_cols, st.grid_even) = (n, even),
        Decl::GridFlowCol(v) => st.grid_flow_col = v,
        Decl::GridRows(ref l) => put_rare(st, |r| &mut r.row_lines, l.clone().or_else(|| parent.rare().row_lines.clone())),
        Decl::GridRow(ref g) => put_rare(st, |r| &mut r.grid_row, g.as_ref().map_or(0, |g| row_line(parent, g))),
        Decl::Order(v) => put_rare(st, |r| &mut r.order, v),
        Decl::Content(ref c) => st.rare_mut().content = c.clone(),
        Decl::GridTracks(axis, ref l) => {
            st.rare_mut().tracks[axis as usize] = l.as_ref().map(|l| {
                let auto = l.auto.as_ref().map(|(at, t, fit)| (*at, resolve_tracks(t, em, c), *fit));
                Rc::new(TrackList { tracks: resolve_tracks(&l.tracks, em, c), names: l.names.clone(), auto })
            })
        }
        Decl::GridAreas(ref a) => st.rare_mut().areas = a.clone(),
        Decl::GridAuto(axis, ref t) => st.rare_mut().auto_tracks[axis as usize] = t.as_ref().map(|t| resolve_tracks(t, em, c).into()),
        Decl::GridDense(v) => st.rare_mut().grid_dense = v,
        Decl::Subgrid(axis, v) => {
            let rare = st.rare_mut();
            rare.subgrid = rare.subgrid & !(1 << axis) | (v as u8) << axis;
        }
        Decl::GridPlace(k, ref p) => Rc::make_mut(st.rare_mut().place.get_or_insert_with(Default::default))[k as usize] = p.clone(),
        Decl::JustifyItems(v) => st.justify_items = v,
        Decl::JustifySelf(v) => st.justify_self = v,
        Decl::FontSize(_) | Decl::Var(..) | Decl::Deferred(..) => {}
    }
}

/// Grid tracks with their font- and viewport-relative lengths in px.
fn resolve_tracks(tracks: &[Track], em: f32, c: &Ctx) -> Vec<Track> {
    let b = |b: Breadth| match b {
        Breadth::Len(l) => Breadth::Len(resolve(l, em, c)),
        Breadth::Fit(l) => Breadth::Fit(resolve(l, em, c)),
        b => b,
    };
    tracks.iter().map(|t| Track { min: b(t.min), max: b(t.max) }).collect()
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
    let lines = grid.rare().row_lines.as_deref();
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
            Pseudo::Not(list) => !list.iter().any(|parts| self.matches(parts, n)),
            Pseudo::Is(list) => list.iter().any(|parts| self.matches(parts, n)),
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
            Pseudo::Defined => !self.dom.atoms.name(node.tag).contains('-') || self.dom.defined.contains(&node.tag),
            Pseudo::Modal => self.dom.modal.contains(&n),
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

/// The `href` of the first element `query(dom, 0, "base[href]", true)` finds,
/// found by the same walk without parsing and matching a selector.
pub fn base_href(dom: &Dom) -> Option<&str> {
    if !dom.has_base {
        return None;
    }
    let mut i = dom.nodes[0].first_child;
    while i != NONE {
        if dom.nodes[i as usize].tag == atom::BASE && dom.is_element(i) {
            if let Some(href) = dom.attr(i, atom::HREF) {
                return Some(href);
            }
        }
        i = dfs_next(dom, i, 0, true);
    }
    None
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
/// for one pass.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct DeferredKey {
    decl: usize,
    vars: usize,
    em: Option<u32>,
    root_fs: u32,
}

/// The hovered element and its ancestors: the elements `:hover` matches.
fn hover_chain(dom: &Dom, hover: NodeId) -> Vec<NodeId> {
    let mut chain = Vec::new();
    let mut h = hover;
    while h != NONE && h != 0 {
        chain.push(h);
        h = dom.nodes[h as usize].parent;
    }
    chain
}

/// One styling pass: the matcher, the ancestor filter, and the caches that let
/// elements share work. Elements are styled in document order, each after its parent.
struct Styler<'a> {
    dom: &'a Dom,
    sheet: &'a Sheet,
    media_ok: Vec<bool>,
    m: Matcher<'a>,
    matched: Vec<(u64, u32)>,
    /// Rules of the element's `::before` and `::after`, with which of the two.
    pseudo: Vec<(u64, u32, u8)>,
    expanded: Vec<Decl>,
    /// Expansions of `Deferred` declarations. A `Vars` map in a key is held by a
    /// style that stays in place until the pass ends, so its address identifies
    /// its contents for the whole pass.
    deferred: FxMap<DeferredKey, Vec<Decl>>,
    /// Custom-property maps by (inherited map, custom-property declarations added).
    own_vars: FxMap<(usize, Vec<usize>), Rc<Vars>>,
    var_decls: Vec<usize>,
    ctx: Ctx,
    bloom: [u8; 4096],
    ancestors: Vec<NodeId>,
}

/// An element's own declarations: the normal and `!important` ones of its
/// `style` attribute, and whether they use custom properties.
type Inline = (Vec<Decl>, Vec<Decl>, bool);

impl<'a> Styler<'a> {
    fn new(dom: &'a Dom, sheet: &'a Sheet, vw: f32, vh: f32, hover: &'a [NodeId]) -> Styler<'a> {
        Styler {
            dom,
            sheet,
            media_ok: sheet.media.iter().map(|m| m.matches(vw, vh)).collect(),
            m: Matcher { dom, hover, idx: Vec::new() },
            matched: Vec::new(),
            pseudo: Vec::new(),
            expanded: Vec::new(),
            deferred: FxMap::default(),
            own_vars: FxMap::default(),
            var_decls: Vec::new(),
            ctx: Ctx { root_fs: 16.0, vw, vh },
            bloom: [0u8; 4096],
            ancestors: Vec::new(),
        }
    }

    /// Forgets the ancestors of the element styled last that are not ancestors
    /// of the next one, a child of `parent`.
    fn enter(&mut self, parent: NodeId) {
        while let Some(&top) = self.ancestors.last() {
            if top == parent {
                break;
            }
            self.ancestors.pop();
            bloom_update(&mut self.bloom, self.dom, top, false);
        }
    }

    /// Makes element `i` an ancestor of the elements styled next.
    fn push(&mut self, i: NodeId) {
        self.ancestors.push(i);
        bloom_update(&mut self.bloom, self.dom, i, true);
    }

    /// The computed style of element `i`, whose parent's style is `parent`.
    fn style(&mut self, i: NodeId, parent: &Style) -> Style {
        let (mut matched, mut pseudo) = (std::mem::take(&mut self.matched), std::mem::take(&mut self.pseudo));
        self.collect(i, &mut matched, &mut pseudo);
        let dom = self.dom;
        let no_inline = (Vec::new(), Vec::new(), false);
        let inline;
        // The end state of finished animations that fill forwards follows the inline style.
        let animated;
        let text = match (dom.attr(i, atom::STYLE), (!dom.animated.is_empty()).then(|| dom.animated.get(&i)).flatten()) {
            (text, Some(end)) => {
                animated = format!("{};{end}", text.unwrap_or(""));
                Some(animated.as_str())
            }
            (text, None) => text,
        };
        let inl = match text {
            Some(s) if !s.is_empty() => {
                inline = parse_declarations(s);
                &inline
            }
            _ => &no_inline,
        };
        let mut st = Style::inherit(parent);
        self.cascade(&mut st, parent, &matched, inl);
        let node = &dom.nodes[i as usize];
        finish(&mut st, parent, node.parent == 0, node.tag == atom::INPUT || dom.shows_image(i));
        if !pseudo.is_empty() {
            let (before, after) = (self.generate(i, &st, &pseudo, BEFORE), self.generate(i, &st, &pseudo, AFTER));
            if before.is_some() || after.is_some() {
                let rare = st.rare_mut();
                (rare.before, rare.after) = (before, after);
            }
        }
        (self.matched, self.pseudo) = (matched, pseudo);
        st
    }

    /// The rules whose selectors match element `i`, in cascade order: those of the
    /// element in `matched`, those of its `::before` and `::after` in `pseudo`.
    fn collect(&mut self, i: NodeId, matched: &mut Vec<(u64, u32)>, pseudo: &mut Vec<(u64, u32, u8)>) {
        let Styler { dom, sheet, media_ok, m, bloom, .. } = self;
        let (dom, sheet) = (*dom, *sheet);
        let node = &dom.nodes[i as usize];
        matched.clear();
        pseudo.clear();
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
                    let key = (sel.level as u64) << 60 | (sel.spec as u64) << 32 | si as u64;
                    if sel.elem == 0 {
                        matched.push((key, sel.rule));
                    } else {
                        pseudo.push((key, sel.rule, sel.elem));
                    }
                }
            }
        };
        if node.id != 0 {
            test(sheet.by_id.get(&node.id), m);
        }
        for cl in dom.classes_of(i) {
            test(sheet.by_class.get(cl), m);
        }
        test(sheet.by_tag.get(&node.tag), m);
        test(Some(&sheet.universal), m);
        matched.sort_unstable();
        pseudo.sort_unstable();
    }

    /// Applies the declarations of the rules in `matched` and then of `inl`,
    /// normal before `!important`, to `st`, whose parent's style is `parent`.
    fn cascade(&mut self, st: &mut Style, parent: &Style, matched: &[(u64, u32)], inl: &Inline) {
        let Styler { sheet, expanded, deferred, own_vars, var_decls, ctx, .. } = self;
        let (sheet, ctx) = (*sheet, &*ctx);
        if inl.2 || matched.iter().any(|&(_, r)| sheet.rules[r as usize].dynamic) {
            var_decls.clear();
            let mut inline_var = false;
            each(sheet, matched, inl, |d| {
                if let Decl::Var(..) = d {
                    let ptr = d as *const Decl;
                    inline_var |= inl.0.as_ptr_range().contains(&ptr) || inl.1.as_ptr_range().contains(&ptr);
                    var_decls.push(ptr as usize);
                }
            });
            if !var_decls.is_empty() {
                let inherited = st.vars.clone();
                let build = || {
                    let mut own = inherited.as_deref().cloned().unwrap_or_default();
                    each(sheet, matched, inl, |d| {
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
                    let key = (inherited.as_ref().map_or(0, |m| Rc::as_ptr(m) as usize), var_decls.clone());
                    own_vars.entry(key).or_insert_with(build).clone()
                });
            }
            expanded.clear();
            let vars = st.vars.as_deref();
            each(sheet, matched, inl, |d| match d {
                Decl::Var(..) => {}
                Decl::Deferred(name, v) => {
                    let em = (&**name == "font-size").then_some(parent.font_size);
                    let expand = |out: &mut Vec<Decl>| {
                        if let Some(sv) = subst(v, vars, 0) {
                            let sv = crate::css::absolute_lengths(&sv, |u| unit_px(u, em, ctx)).unwrap_or(sv);
                            parse_decl(name, &sv, out);
                        }
                    };
                    // Inline declarations are freed after each element, so only
                    // stylesheet declarations have an address that stays unique.
                    let ptr = d as *const Decl;
                    if inl.0.as_ptr_range().contains(&ptr) || inl.1.as_ptr_range().contains(&ptr) {
                        expand(expanded);
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
            for d in expanded.iter() {
                match d {
                    Decl::FontSize(fs) => apply_font_size(st, parent, *fs, ctx),
                    Decl::AllUnset => st.font_size = parent.font_size,
                    _ => {}
                }
            }
            for d in expanded.iter() {
                apply(st, parent, d, ctx);
            }
        } else {
            each(sheet, matched, inl, |d| match d {
                Decl::FontSize(fs) => apply_font_size(st, parent, *fs, ctx),
                Decl::AllUnset => st.font_size = parent.font_size,
                _ => {}
            });
            each(sheet, matched, inl, |d| apply(st, parent, d, ctx));
        }
    }

    /// The `::before` or `::after` box (`elem`) of element `i`, whose style is
    /// `st`, from the rules in `pseudo`: none when they give it no content.
    fn generate(&mut self, i: NodeId, st: &Style, pseudo: &[(u64, u32, u8)], elem: u8) -> Option<Rc<Generated>> {
        let rules: Vec<(u64, u32)> = pseudo.iter().filter(|p| p.2 == elem).map(|p| (p.0, p.1)).collect();
        if rules.is_empty() {
            return None;
        }
        let mut gs = Style::inherit(st);
        self.cascade(&mut gs, st, &rules, &(Vec::new(), Vec::new(), false));
        finish(&mut gs, st, false, false);
        let content = gs.rare.0.as_mut().and_then(|r| Rc::make_mut(r).content.take())?;
        if gs.display == Display::None {
            return None;
        }
        let dom = self.dom;
        let (mut pieces, mut text) = (Vec::new(), String::new());
        for c in content.iter() {
            match c {
                Content::Text(t) => text.push_str(t),
                Content::Attr(name) => text.push_str(dom.atoms.get(name).and_then(|a| dom.attr(i, a)).unwrap_or("")),
                Content::Url(u) => {
                    if !text.is_empty() {
                        pieces.push(Piece::Text(std::mem::take(&mut text).into()));
                    }
                    pieces.push(Piece::Image(u.clone()));
                }
            }
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text.into()));
        }
        Some(Rc::new(Generated { style: gs, content: pieces.into() }))
    }
}

/// The parts of a computed style that follow from the rest and from the box's
/// place: used border widths and colours, opacity, and the display its parent
/// and position give it. `root` is set for the root element, `replaced` for one
/// whose content is an image or a form control.
fn finish(st: &mut Style, parent: &Style, root: bool, replaced: bool) {
    for s in 0..4 {
        if st.bs & (1 << s) == 0 {
            st.bw[s] = 0.0;
        }
        if st.bc_set & (1 << s) == 0 {
            st.bc[s] = st.color;
        }
    }
    if st.clip_text {
        text_from_background(st);
    }
    let cut = st.cut_away & 2 != 0 || (st.cut_away & 1 != 0 && matches!(st.position, Position::Absolute | Position::Fixed));
    st.opacity = if st.scaled_away || cut { 0.0 } else { parent.opacity * st.own_opacity };
    let parent_flex = matches!(parent.display, Display::Flex | Display::InlineFlex);
    if st.display == Display::Inherit {
        st.display = match parent.display {
            _ if root => Display::Block,
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
    // A flex item or a positioned box does not float.
    if parent_flex || matches!(st.position, Position::Absolute | Position::Fixed) {
        st.float = 0;
    }
    let blockify = root || matches!(st.position, Position::Absolute | Position::Fixed) || parent_flex || st.float != 0;
    if blockify {
        st.display = match st.display {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            d => d,
        };
    }
    if replaced && st.display == Display::Inline {
        st.display = Display::InlineBlock;
    }
}

/// Computes a style for every node. Nodes are stored in document order, so a
/// single forward pass always sees the parent's style first.
pub fn compute_styles(dom: &Dom, sheet: &Sheet, vw: f32, vh: f32, hover: NodeId) -> Vec<Style> {
    // Built from constants, which costs less than cloning one style for every node.
    let mut styles = Vec::with_capacity(dom.nodes.len());
    styles.resize_with(dom.nodes.len(), Style::initial);
    let chain = hover_chain(dom, hover);
    let mut s = Styler::new(dom, sheet, vw, vh, &chain);
    let mut i = dom.nodes[0].first_child;
    while i != NONE {
        let node = &dom.nodes[i as usize];
        if node.tag == atom::TEXT {
            i = dfs_next(dom, i, 0, false);
            continue;
        }
        s.enter(node.parent);
        let st = s.style(i, &styles[node.parent as usize]);
        if node.tag == atom::HTML {
            s.ctx.root_fs = st.font_size;
        }
        s.push(i);
        let descend = st.display != Display::None && node.tag != atom::SVG;
        styles[i as usize] = st;
        i = dfs_next(dom, i, 0, descend);
    }
    styles
}

/// Marks on elements whose style must be computed again: what changed since the
/// last pass, and while a pass runs, what it found depends on a style that changed.
#[derive(Default)]
pub struct Dirty {
    flags: Vec<u8>,
    marked: Vec<NodeId>,
}

/// The element's own style.
pub const OWN: u8 = 1;
/// The style of the element and of everything below it.
pub const TREE: u8 = 2;
/// Something below the element is marked.
const BELOW: u8 = 4;
/// Found while restyling: the element's style changed, so its children are styled again.
const KIDS: u8 = 8;
/// Found while restyling: everything below the element is styled again.
const KID_TREES: u8 = 16;
/// Set while restyling on the elements the pass reached.
const SEEN: u8 = 32;

impl Dirty {
    pub fn is_empty(&self) -> bool {
        self.marked.is_empty()
    }

    fn get(&self, n: NodeId) -> u8 {
        self.flags.get(n as usize).copied().unwrap_or(0)
    }

    fn set(&mut self, n: NodeId, f: u8) {
        let i = n as usize;
        if i >= self.flags.len() {
            self.flags.resize(i + 1, 0);
        }
        if self.flags[i] == 0 {
            self.marked.push(n);
        }
        self.flags[i] |= f;
    }

    /// Marks element `n` with `OWN` or `TREE`, and the path to it from the root,
    /// up to the first node already on a marked path: a node that moves is marked
    /// again where it lands, so the path above a marked node is marked too.
    pub fn mark(&mut self, dom: &Dom, n: NodeId, f: u8) {
        self.set(n, f);
        let mut p = dom.nodes[n as usize].parent;
        while p != NONE && self.get(p) & BELOW == 0 {
            self.set(p, BELOW);
            p = dom.nodes[p as usize].parent;
        }
    }

    pub fn clear(&mut self) {
        for n in self.marked.drain(..) {
            self.flags[n as usize] = 0;
        }
    }
}

/// What a `restyle` pass changed in what is drawn.
#[derive(Default)]
pub struct Restyled {
    /// Some style changed in something layout reads.
    pub layout: bool,
    /// Elements whose style changed only as `Change::Paint` allows.
    pub paint: Vec<NodeId>,
}

/// Styles again the elements `dirty` marks and, below them, those whose parent's
/// style changed, keeping every other style in `styles`. Afterwards every element
/// that `compute_styles` reaches has the style it computes. `None` when only
/// styling every element again is known to give that: the root's font size, which
/// `rem` lengths everywhere resolve against, changed, or the document holds more
/// than its `<html>` element.
pub fn restyle(dom: &Dom, sheet: &Sheet, vw: f32, vh: f32, hover: NodeId, styles: &mut Vec<Style>, dirty: &mut Dirty) -> Option<Restyled> {
    let root = dom.nodes[0].first_child;
    if root == NONE || dom.nodes[root as usize].tag != atom::HTML || dom.nodes[root as usize].next != NONE {
        return None;
    }
    styles.resize(dom.nodes.len(), Style::initial());
    let chain = hover_chain(dom, hover);
    let mut s = Styler::new(dom, sheet, vw, vh, &chain);
    let mut out = Restyled::default();
    // Custom-property maps replaced by equal ones the elements had, kept to the
    // end of the pass: `Deferred` expansions are cached by the address of a map.
    let mut replaced = Vec::new();
    let mut i = root;
    while i != NONE {
        let node = &dom.nodes[i as usize];
        let (f, pf) = (dirty.get(i), dirty.get(node.parent));
        let tree = f & TREE != 0 || pf & KID_TREES != 0;
        let again = tree || f & OWN != 0 || pf & KIDS != 0;
        if node.tag == atom::TEXT || (!again && f & BELOW == 0) {
            i = dfs_next(dom, i, 0, false);
            continue;
        }
        s.enter(node.parent);
        dirty.set(i, SEEN);
        if again {
            let mut new = s.style(i, &styles[node.parent as usize]);
            let old = &styles[i as usize];
            // The map the element had, when equal, is the one it keeps: the elements
            // below then inherit a map they can tell is theirs without comparing it.
            if let (Some(was), Some(now)) = (&old.vars, &new.vars) {
                if !Rc::ptr_eq(was, now) && was == now {
                    replaced.push(new.vars.replace(was.clone()));
                }
            }
            if node.tag == atom::HTML && new.font_size != old.font_size {
                return None;
            }
            // Elements below one that stops being shown keep no style, as a full
            // pass leaves them. Below one that starts being shown they have none
            // yet, and every style a pass computes differs from that.
            let shown = |st: &Style| st.display != Display::None && node.tag != atom::SVG;
            let hidden = shown(old) && !shown(&new);
            if tree {
                dirty.set(i, KID_TREES);
            } else if *old != new {
                dirty.set(i, KIDS);
            }
            match old.change(&new) {
                Change::None => {}
                Change::Paint => out.paint.push(i),
                Change::Layout => out.layout = true,
            }
            styles[i as usize] = new;
            if hidden {
                unstyle_below(dom, styles, i);
            }
        }
        if node.tag == atom::HTML {
            s.ctx.root_fs = styles[i as usize].font_size;
        }
        s.push(i);
        let st = &styles[i as usize];
        let descend = st.display != Display::None && node.tag != atom::SVG && dirty.get(i) & (BELOW | KIDS | KID_TREES) != 0;
        i = dfs_next(dom, i, 0, descend);
    }
    // A subtree the pass did not reach went where a full pass does not reach either:
    // out of the document, into an `<svg>` or below an element that is not shown.
    for k in 0..dirty.marked.len() {
        let n = dirty.marked[k];
        if dirty.get(n) & (TREE | SEEN) == TREE {
            styles[n as usize] = Style::initial();
            unstyle_below(dom, styles, n);
        }
    }
    dirty.clear();
    Some(out)
}

/// Gives the nodes below `n` the initial style that a pass leaves on nodes it does not reach.
fn unstyle_below(dom: &Dom, styles: &mut [Style], n: NodeId) {
    let mut c = dom.next_in(n, n);
    while c != NONE {
        styles[c as usize] = Style::initial();
        c = dom.next_in(c, n);
    }
}

/// The first node whose style in `got` differs from the one in `want`, which
/// `compute_styles` computed, described with the fields that differ. A node
/// `got` has no style for yet has the initial one.
pub fn first_difference(dom: &Dom, got: &[Style], want: &[Style]) -> Option<String> {
    let initial = Style::initial();
    for (i, b) in want.iter().enumerate() {
        let a = got.get(i).unwrap_or(&initial);
        if a == b {
            continue;
        }
        // NaN is not equal to itself; the printed forms tell real differences.
        let (x, y) = (format!("{a:?}"), format!("{b:?}"));
        if x != y {
            let fields: Vec<String> = x.split(", ").zip(y.split(", ")).filter(|(p, q)| p != q).map(|(p, q)| format!("{p} (full restyle: {q})")).collect();
            let classes: Vec<&str> = dom.classes_of(i as NodeId).iter().map(|&c| dom.atoms.name(c)).collect();
            let (tag, id) = (dom.nodes[i].tag, dom.nodes[i].id);
            return Some(format!("<{} id={:?} class={:?}> node {i}: {}", dom.atoms.name(tag), dom.atoms.name(id), classes.join(" "), fields.join(", ")));
        }
    }
    None
}

/// Elements a change can restyle, told by their own compound: those with one of
/// these names as tag, id or class, or with `all` every element.
#[derive(Default)]
pub struct Filter {
    all: bool,
    names: FxMap<Atom, ()>,
}

impl Filter {
    /// Adds the elements a compound can match: those with its id, else its first
    /// class, else its tag; every element when it names none of these.
    fn add(&mut self, c: &Compound) {
        let name = if c.id != 0 { c.id } else { c.classes.first().copied().unwrap_or(c.tag) };
        if name == 0 {
            self.all = true;
        } else {
            self.names.insert(name, ());
        }
    }

    fn any(&self) -> bool {
        self.all || !self.names.is_empty()
    }

    fn matches(&self, dom: &Dom, n: NodeId) -> bool {
        if self.all {
            return true;
        }
        let node = &dom.nodes[n as usize];
        let has = |a: Atom| self.names.contains_key(&a);
        has(node.tag) || (node.id != 0 && has(node.id)) || dom.classes_of(n).iter().any(|&c| has(c))
    }
}

/// Elements reached from one element: its descendants, its following siblings,
/// and the descendants of those, that pass each filter.
#[derive(Default)]
struct Reach {
    desc: Filter,
    sib: Filter,
    sib_desc: Filter,
}

impl Reach {
    fn add(&mut self, target: Target, subject: &Compound) {
        match target {
            Target::Desc => self.desc.add(subject),
            Target::Sib => self.sib.add(subject),
            Target::SibDesc => self.sib_desc.add(subject),
            Target::Own => {}
        }
    }

    fn any(&self) -> bool {
        self.desc.any() || self.sib.any() || self.sib_desc.any()
    }
}

/// What a change to one feature of an element can restyle.
#[derive(Default)]
pub struct Inv {
    /// The element itself; the filter applies to a change that leaves its names alone.
    own: Filter,
    /// What other compounds of selectors reach from it.
    reach: Reach,
    /// Anchors of a `:has()` above it: those in a subject compound, restyled
    /// themselves, and those in another compound, from which `far_reach` applies.
    up: Filter,
    far: Filter,
    far_reach: Reach,
    /// A `:has()` argument with a combinator matches through ancestors of any
    /// height, so anchors can be below the element too: its whole subtree is
    /// restyled, and with `far_tree` its following siblings' too.
    up_tree: bool,
    far_tree: bool,
    /// Only restyling every element is known to be enough.
    full: bool,
}

/// Where the elements that a feature in the `k`th compound of a selector (from the
/// right) can restyle are, relative to the element that changed.
#[derive(Clone, Copy)]
enum Target {
    Own,
    Desc,
    Sib,
    SibDesc,
}

impl Target {
    /// Where the elements are that going `self` from an element, then `next` from
    /// there, reaches.
    fn then(self, next: Target) -> Target {
        match (self, next) {
            (Target::Own, t) | (t, Target::Own) => t,
            (Target::Desc, _) => Target::Desc,
            (Target::Sib, Target::Sib) => Target::Sib,
            _ => Target::SibDesc,
        }
    }

    fn of(parts: &[Part], k: usize) -> Target {
        if k == 0 {
            return Target::Own;
        }
        match parts[k - 1].comb {
            Comb::Descendant | Comb::Child => Target::Desc,
            _ if parts[..k - 1].iter().any(|p| matches!(p.comb, Comb::Descendant | Comb::Child)) => Target::SibDesc,
            _ => Target::Sib,
        }
    }
}

impl Inv {
    fn add(&mut self, target: Target, subject: &Compound) {
        match target {
            Target::Own => self.own.add(subject),
            _ => self.reach.add(target, subject),
        }
    }

    fn any(&self) -> bool {
        self.full || self.own.any() || self.reach.any() || self.up.any() || self.far.any() || self.up_tree || self.far_tree
    }
}

/// What can change about an element while it stays where it is.
#[derive(Clone, Copy)]
enum Feature {
    Class(Atom),
    Id(Atom),
    Attr(Atom),
    Hover,
    /// Its position among element siblings, which structural pseudo-classes test.
    Position,
    /// Whether it has children, which `:empty` tests.
    Empty,
}

/// A compound and the compounds of the selectors inside its `:is()`, `:where()`
/// and `:not()`, each with where the elements are that a change to an element it
/// matches can restyle, `at` being that place for `c` itself.
fn each_compound<'c>(c: &'c Compound, at: Target, f: &mut impl FnMut(&'c Compound, Target)) {
    f(c, at);
    for p in &c.pseudos {
        if let Pseudo::Not(list) | Pseudo::Is(list) = p {
            for parts in list.iter() {
                for (k, part) in parts.iter().enumerate() {
                    each_compound(&part.c, Target::of(parts, k).then(at), f);
                }
            }
        }
    }
}

/// The features a compound tests itself; those of the selectors in its arguments
/// are reached through `each_compound`.
fn each_feature(c: &Compound, f: &mut impl FnMut(Feature)) {
    for &class in &c.classes {
        f(Feature::Class(class));
    }
    if c.id != 0 {
        f(Feature::Id(c.id));
    }
    for a in &c.attrs {
        f(Feature::Attr(a.name));
    }
    for p in &c.pseudos {
        match p {
            Pseudo::Hover => f(Feature::Hover),
            Pseudo::FirstChild
            | Pseudo::LastChild
            | Pseudo::OnlyChild
            | Pseudo::NthChild(..)
            | Pseudo::NthLastChild(..)
            | Pseudo::FirstOfType
            | Pseudo::LastOfType
            | Pseudo::NthOfType(..) => f(Feature::Position),
            Pseudo::Empty => f(Feature::Empty),
            Pseudo::Link => f(Feature::Attr(atom::HREF)),
            // A definition, or a dialog becoming modal, restyles everything.
            Pseudo::Has(_) | Pseudo::Not(_) | Pseudo::Is(_) | Pseudo::Root | Pseudo::Defined | Pseudo::Modal | Pseudo::Never => {}
        }
    }
}

/// The arguments of a compound's own `:has()`s.
fn each_has<'c>(c: &'c Compound, f: &mut impl FnMut(&'c [Part])) {
    for p in &c.pseudos {
        if let Pseudo::Has(args) = p {
            f(args);
        }
    }
}

/// False for a compound that never matches: one with an unsupported pseudo-class
/// or a pseudo-element, which match nothing, or a `:has()` whose argument has one.
fn can_match(c: &Compound) -> bool {
    c.pseudos.iter().all(|p| match p {
        Pseudo::Never => false,
        Pseudo::Has(args) => args.iter().all(|a| can_match(&a.c)),
        Pseudo::Is(list) => list.iter().any(|parts| parts.iter().all(|p| can_match(&p.c))),
        _ => true,
    })
}

/// Which elements' styles can change when something about one element changes,
/// worked out once per sheet from its selectors (Blink's invalidation sets): for a
/// class, id or attribute of an element, for it entering or leaving the hover
/// chain, and for children coming or going.
#[derive(Default)]
pub struct Invalidation {
    by_class: FxMap<Atom, Inv>,
    by_id: FxMap<Atom, Inv>,
    by_attr: FxMap<Atom, Inv>,
    hover: Inv,
    position: Inv,
    empty: Inv,
    /// For a child that comes or goes: its following siblings, and their
    /// descendants, that a sibling combinator reaches.
    sib: Filter,
    sib_desc: Filter,
    /// The `:has()` anchors above it, as in `Inv`; with `moves_full` a `:has()`
    /// nested in another, for which only a full restyle is known to be enough.
    up: Filter,
    far: Filter,
    far_reach: Reach,
    moves_full: bool,
}

/// Why an element is marked: its names changed, so `Inv::own` applies whatever
/// its filter says; or something else about it, so the filter applies too.
#[derive(Clone, Copy, PartialEq)]
enum Why {
    Names,
    State,
}

impl Invalidation {
    pub fn new(sheet: &Sheet) -> Invalidation {
        let mut inv = Invalidation::default();
        for sel in &sheet.sels {
            let parts = &sel.parts;
            if !parts.iter().all(|p| can_match(&p.c)) {
                continue;
            }
            let subject = &parts[0].c;
            for (k, part) in parts.iter().enumerate() {
                each_compound(&part.c, Target::of(parts, k), &mut |c, target| {
                    match target {
                        Target::Sib => inv.sib.add(subject),
                        Target::SibDesc => inv.sib_desc.add(subject),
                        Target::Own | Target::Desc => {}
                    }
                    each_feature(c, &mut |f| inv.entry(f).add(target, subject));
                    each_has(c, &mut |args| inv.add_has(target, (c, subject), args));
                });
            }
        }
        inv
    }

    fn entry(&mut self, f: Feature) -> &mut Inv {
        match f {
            Feature::Class(a) => self.by_class.entry(a).or_default(),
            Feature::Id(a) => self.by_id.entry(a).or_default(),
            Feature::Attr(a) => self.by_attr.entry(a).or_default(),
            Feature::Hover => &mut self.hover,
            Feature::Position => &mut self.position,
            Feature::Empty => &mut self.empty,
        }
    }

    /// A `:has(args)` in compound `anchor`, which reaches the elements `at` from its own
    /// element. The argument matches descendants of the anchor, so a change to an
    /// element changes which anchors above it match, and with a combinator in the
    /// argument, which matches through ancestors of any height, anchors below it too.
    fn add_has(&mut self, at: Target, (anchor, subject): (&Compound, &Compound), args: &[Part]) {
        let (mut tree, mut nested) = (args.len() > 1, false);
        let mut inner = Vec::new();
        for p in args {
            each_compound(&p.c, Target::Own, &mut |c, t| inner.push((c, t)));
        }
        for &(c, t) in &inner {
            tree |= !matches!(t, Target::Own);
            each_has(c, &mut |_| nested = true);
        }
        for (c, _) in inner {
            each_feature(c, &mut |f| {
                let e = self.entry(f);
                match at {
                    _ if nested => e.full = true,
                    Target::Own => {
                        e.up.add(anchor);
                        e.up_tree |= tree;
                    }
                    _ => {
                        e.far.add(anchor);
                        e.far_reach.add(at, subject);
                        e.far_tree |= tree;
                    }
                }
            });
        }
        match at {
            _ if nested => self.moves_full = true,
            Target::Own => self.up.add(anchor),
            _ => {
                self.far.add(anchor);
                self.far_reach.add(at, subject);
            }
        }
    }

    /// Marks what a change of element `n`'s attribute `name` can restyle; `old` are the
    /// classes or the id it had, for a change of those. False when only a full
    /// restyle is known to be enough.
    pub fn attr_changed(&self, dirty: &mut Dirty, dom: &Dom, n: NodeId, name: Atom, old: &[Atom]) -> bool {
        let mut ok = true;
        let mut changed = |inv: Option<&Inv>| {
            if let Some(inv) = inv {
                ok &= mark(dirty, dom, n, inv, Why::Names);
            }
        };
        match name {
            atom::CLASS => {
                let new = dom.classes_of(n);
                for c in old.iter().filter(|c| !new.contains(c)).chain(new.iter().filter(|c| !old.contains(c))) {
                    changed(self.by_class.get(c));
                }
            }
            atom::ID => {
                let new = dom.nodes[n as usize].id;
                for id in old.iter().copied().chain([new]).filter(|&id| id != 0) {
                    changed(self.by_id.get(&id));
                }
            }
            _ => {}
        }
        changed(self.by_attr.get(&name));
        ok
    }

    /// Marks what the hover moving from `old` to `new` can restyle.
    pub fn hover_changed(&self, dirty: &mut Dirty, dom: &Dom, old: NodeId, new: NodeId) -> bool {
        if !self.hover.any() {
            return true;
        }
        let (a, b) = (hover_chain(dom, old), hover_chain(dom, new));
        a.iter().filter(|e| !b.contains(e)).chain(b.iter().filter(|e| !a.contains(e))).all(|&e| mark(dirty, dom, e, &self.hover, Why::State))
    }

    /// Marks what `n` entering its parent, or being about to leave it, can restyle:
    /// all of `n`, siblings that sibling combinators or structural pseudo-classes
    /// tell from their neighbours, the parent for `:empty`, `:has()` anchors above
    /// it, and the ancestors that enter or leave the hover chain with it.
    pub fn child_moved(&self, dirty: &mut Dirty, dom: &Dom, n: NodeId, hover: NodeId) -> bool {
        let p = dom.nodes[n as usize].parent;
        if dom.is_element(n) {
            dirty.mark(dom, n, TREE);
            mark_following(dirty, dom, n, &self.sib, &self.sib_desc);
            if self.position.full {
                return false;
            }
            let mut c = dom.nodes[p as usize].first_child;
            while c != NONE {
                if dom.is_element(c) {
                    // The :has() anchors above are marked once, below.
                    if self.position.own.matches(dom, c) {
                        dirty.mark(dom, c, OWN);
                    }
                    mark_reach(dirty, dom, c, &self.position.reach);
                }
                c = dom.nodes[c as usize].next;
            }
        }
        if dom.is_element(p) && !mark(dirty, dom, p, &self.empty, Why::State) {
            return false;
        }
        if self.moves_full {
            return false;
        }
        mark_above(dirty, dom, p, (&self.up, &self.far), &self.far_reach);
        if dom.is_element(n) && self.far.matches(dom, n) {
            mark_reach(dirty, dom, n, &self.far_reach);
        }
        let mut h = hover;
        while h != NONE && h != n {
            h = dom.nodes[h as usize].parent;
        }
        if h == n {
            let mut a = p;
            while a != NONE && a != 0 {
                if !mark(dirty, dom, a, &self.hover, Why::State) {
                    return false;
                }
                a = dom.nodes[a as usize].parent;
            }
        }
        true
    }
}

/// Marks what a change described by `inv` to element `n` can restyle. False when
/// only a full restyle is known to be enough.
fn mark(dirty: &mut Dirty, dom: &Dom, n: NodeId, inv: &Inv, why: Why) -> bool {
    if inv.full {
        return false;
    }
    if inv.own.any() && (why == Why::Names || inv.own.matches(dom, n)) {
        dirty.mark(dom, n, OWN);
    }
    mark_reach(dirty, dom, n, &inv.reach);
    mark_above(dirty, dom, dom.nodes[n as usize].parent, (&inv.up, &inv.far), &inv.far_reach);
    if inv.up_tree || inv.far_tree {
        dirty.mark(dom, n, TREE);
    }
    if inv.far_tree {
        let every = Filter { all: true, ..Filter::default() };
        mark_following(dirty, dom, n, &Filter::default(), &every);
    }
    true
}

/// Marks what `reach` reaches from element `n`.
fn mark_reach(dirty: &mut Dirty, dom: &Dom, n: NodeId, reach: &Reach) {
    if reach.desc.all {
        dirty.mark(dom, n, TREE);
    } else if reach.desc.any() {
        mark_below(dirty, dom, n, &reach.desc);
    }
    if reach.sib.any() || reach.sib_desc.any() {
        mark_following(dirty, dom, n, &reach.sib, &reach.sib_desc);
    }
}

fn mark_below(dirty: &mut Dirty, dom: &Dom, n: NodeId, filter: &Filter) {
    let mut c = dom.next_in(n, n);
    while c != NONE {
        if dom.is_element(c) && filter.matches(dom, c) {
            dirty.mark(dom, c, OWN);
        }
        c = dom.next_in(c, n);
    }
}

fn mark_following(dirty: &mut Dirty, dom: &Dom, n: NodeId, sib: &Filter, sib_desc: &Filter) {
    let mut s = dom.nodes[n as usize].next;
    while s != NONE {
        if dom.is_element(s) {
            if sib.matches(dom, s) && sib.any() {
                dirty.mark(dom, s, OWN);
            }
            if sib_desc.all {
                dirty.mark(dom, s, TREE);
            } else if sib_desc.any() {
                mark_below(dirty, dom, s, sib_desc);
            }
        }
        s = dom.nodes[s as usize].next;
    }
}

/// Marks element `a` and its ancestors that pass `up`, and what `far_reach`
/// reaches from those that pass `far`.
fn mark_above(dirty: &mut Dirty, dom: &Dom, mut a: NodeId, (up, far): (&Filter, &Filter), far_reach: &Reach) {
    if !up.any() && !far.any() {
        return;
    }
    while a != NONE {
        if dom.is_element(a) {
            if up.matches(dom, a) {
                dirty.mark(dom, a, OWN);
            }
            if far.matches(dom, a) {
                mark_reach(dirty, dom, a, far_reach);
            }
        }
        a = dom.nodes[a as usize].parent;
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_styles, query, Generated, Piece, Style};
    use std::rc::Rc;
    use crate::css::{Display, Len};
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
    fn visibility_keywords_follow_the_cascade_and_the_parent() {
        let html = "<style>.h{visibility:hidden} .in{visibility:inherit} .un{visibility:unset} .init{visibility:initial} .bad{visibility:nonsense}</style>\
                    <div><p class='h in'></p><p class='h init'></p><p class='h bad'></p></div>\
                    <div class=h><p class=in></p><p class=un></p><p class=init></p></div>";
        assert_eq!(styles_of(html, "p").iter().map(|s| s.visible).collect::<Vec<_>>(), [true, true, false, false, false, true]);
    }

    #[test]
    fn clip_and_clip_path_that_leave_nothing_draw_nothing() {
        let html = "<style>p{position:absolute} .a{clip:rect(0 0 0 0)} .b{position:static;clip:rect(0 0 0 0)} .c{clip:rect(0,10px,10px,0)} .d{clip-path:inset(50%)} .e{clip-path:inset(0px 0px 99.9% 99.9%)}</style>\
                    <p class=a></p><p class=b></p><p class=c></p><p class=d></p><p class=e></p>";
        let s = styles_of(html, "p");
        assert_eq!(s.iter().map(|s| s.opacity).collect::<Vec<_>>(), [0.0, 1.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn box_shadow_keeps_the_outer_shadows_in_px() {
        let html = "<style>p{font-size:10px} .a{box-shadow:rgba(0,0,0,.2) 0 8px 23px 0, inset 0 0 0 4px red} .b{box-shadow:0 1em 2px currentcolor}</style><p class=a></p><p class=b></p>";
        let s = styles_of(html, "p");
        let px = |l: Len| if let Len::Px(v) = l { v } else { f32::NAN };
        let shadows = |s: &Style| s.rare().shadow.as_deref().unwrap_or(&[]).iter().map(|sh| (px(sh.x), px(sh.y), px(sh.blur), px(sh.spread), sh.color.map(|c| c.a()))).collect::<Vec<_>>();
        assert_eq!(shadows(&s[0]), [(0.0, 8.0, 23.0, 0.0, Some(51))]);
        assert_eq!(shadows(&s[1]), [(0.0, 10.0, 2.0, 0.0, None)]);
    }

    #[test]
    fn line_clamp_counts_only_on_a_webkit_box() {
        let html = "<style>p{display:-webkit-box;-webkit-line-clamp:2} p.open{display:block}</style><p></p><p class=open></p>";
        let s = styles_of(html, "p");
        assert_eq!(s.iter().map(|s| (s.webkit_box, s.rare().line_clamp, s.display)).collect::<Vec<_>>(), [(true, 2, Display::Block), (false, 2, Display::Block)]);
    }

    #[test]
    fn border_radius_gives_each_corner_its_value() {
        let html = "<style>.a{border-radius:0 40px 40px 0} .b{border-top-left-radius:30px} .c{border-radius:10px 30px 0} .d{border-radius:10px 20px / 5px}</style>\
                    <p class=a></p><p class=b></p><p class=c></p><p class=d></p>";
        let px = |s: &Style| s.radius.map(|l| if let Len::Px(v) = l { v } else { f32::NAN });
        let s = styles_of(html, "p");
        assert_eq!(s.iter().map(px).collect::<Vec<_>>(), [[0.0, 40.0, 40.0, 0.0], [30.0, 0.0, 0.0, 0.0], [10.0, 30.0, 0.0, 30.0], [10.0, 20.0, 10.0, 20.0]]);
    }

    #[test]
    fn all_unset_takes_inherited_or_initial_values() {
        let html = "<style>.u{all:unset} .after{color:#f00}</style>\
                    <div style=\"font-size:20px;color:#0f0\"><h1 class=u></h1><h1 class='u after'></h1><h1></h1></div>";
        let s = styles_of(html, "h1");
        assert_eq!(s.iter().map(|s| s.font_size).collect::<Vec<_>>(), [20.0, 20.0, 40.0]);
        assert_eq!(s.iter().map(|s| s.display).collect::<Vec<_>>(), [Display::Inline, Display::Inline, Display::Block]);
        assert_eq!(s.iter().map(|s| s.color.0 & 0xff_ffff).collect::<Vec<_>>(), [0x00ff00, 0xff0000, 0x00ff00]);
    }

    #[test]
    fn a_deferred_font_size_resolves_against_each_parent() {
        let html = "<style>.big{font-size:20px} .small{font-size:10px} .f{font-size:calc(1em + 1px)}</style>\
                    <div class=big><p class=f></p></div><div class=small><p class=f></p></div><div class=big><p class=f></p></div>";
        assert_eq!(styles_of(html, "p").iter().map(|s| s.font_size).collect::<Vec<_>>(), [21.0, 11.0, 21.0]);
    }
    #[test]
    fn is_where_and_not_take_selector_lists_with_selectors_4_specificity() {
        let css = ".a{width:2px} :where(#a){width:1px}
                   :is(#b, .zz){width:3px} p.b{width:4px}
                   .c:not(#zz){width:5px} p.c.c{width:6px}
                   .d:not(.x, .y){width:7px}
                   :is(div > .e){width:8px}
                   :is(.g, .h) + .f{width:9px}
                   .k:is(.zz, :focus-visible, .kk){width:10px}";
        let body = "<p id=a class=a></p><p id=b class=b></p><p class=c></p><p class='d x'></p><p class=d></p>\
                    <div><p class=e></p></div><section><p class=e></p></section><i class=h></i><p class=f></p><p class='k kk'></p>";
        let px = Len::Px;
        let want = [px(2.0), px(3.0), px(5.0), Len::Auto, px(7.0), px(8.0), Len::Auto, px(9.0), px(10.0)];
        assert_eq!(widths(&styles_of(&format!("<style>{css}</style>{body}"), "p")), want);
    }

    #[test]
    fn before_and_after_get_their_own_style_and_content() {
        let html = "<style>p{font-size:20px;color:#00f} .a::before{content:'\\201C' attr(data-x) '!';color:#f00}\
                    .a:after{content:url(i.png) 'x'} .b::before{content:none} .c::after{content:counter(n) ''}\
                    .d::before{display:none;content:''}</style>\
                    <p class=a data-x=hi></p><p class=b></p><p class=c></p><p class=d></p><p></p>";
        let s = styles_of(html, "p");
        let gen = |g: &Option<Rc<Generated>>| g.as_ref().map(|g| (g.style.color.0 & 0xff_ffff, g.style.font_size, g.content.to_vec()));
        assert_eq!(gen(&s[0].rare().before), Some((0xff_0000, 20.0, vec![Piece::Text("\u{201c}hi!".into())])));
        assert_eq!(gen(&s[0].rare().after), Some((0x00_00ff, 20.0, vec![Piece::Image("i.png".into()), Piece::Text("x".into())])));
        assert_eq!([gen(&s[1].rare().before), gen(&s[2].rare().after), gen(&s[3].rare().before), gen(&s[4].rare().before)], [None, Some((0x00_00ff, 20.0, vec![])), None, None]);
        let mut page = Page::from_html(html, None);
        assert!(query(&mut page.dom, 0, ".a::before, p::after", false).is_empty());
    }
}
