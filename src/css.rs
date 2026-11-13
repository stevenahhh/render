//! CSS values, declaration parsing, selectors and the rule index.

use crate::dom::{Atom, Interner};
pub use crate::media::{media_matches, Media};

pub mod drops;
mod grid;
pub use grid::{Areas, Breadth, GridPos, LineName, Track, TrackList};
use crate::FxMap;
use std::borrow::Cow;
use std::rc::Rc;

/// 0xAARRGGBB, not premultiplied.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Color(pub u32);

impl Color {
    pub const TRANSPARENT: Color = Color(0);
    pub const BLACK: Color = Color(0xFF00_0000);
    pub const WHITE: Color = Color(0xFFFF_FFFF);

    pub fn rgba(r: u32, g: u32, b: u32, a: u32) -> Color {
        Color(a << 24 | r << 16 | g << 8 | b)
    }
    #[inline]
    pub fn a(self) -> u32 {
        self.0 >> 24
    }
    pub fn with_opacity(self, o: f32) -> Color {
        if o >= 1.0 {
            return self;
        }
        let a = (self.a() as f32 * o.max(0.0) + 0.5) as u32;
        Color(self.0 & 0x00FF_FFFF | a << 24)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Len {
    Auto,
    Px(f32),
    Pct(f32),
    Em(f32),
    Rem(f32),
    Vw(f32),
    Vh(f32),
    /// percent + px
    Calc(f32, f32),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    ListItem,
    /// The three below are replaced by one of the above during styling.
    Grid,
    Contents,
    Inherit,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WhiteSpace {
    Normal,
    NoWrap,
    Pre,
    /// Spaces and line feeds kept, as with `Pre`, and lines wrapped after spaces.
    PreWrap,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TextTransform {
    None,
    Upper,
    Lower,
    Capitalize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ListStyle {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Justify {
    Start,
    Center,
    End,
    Between,
    Around,
    Evenly,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Align {
    Auto,
    Stretch,
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Family {
    Serif = 0,
    Sans = 1,
    Mono = 2,
}

/// A `font-weight` value; the relative keywords depend on the parent's weight.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FontWeight {
    Abs(u16),
    Bolder,
    Lighter,
}

/// An `@font-face` rule: a downloadable face of a family.
#[derive(Debug)]
pub struct FontFace {
    /// Family name, lower case.
    pub family: Box<str>,
    /// `src` entries in order of preference: URL (resolved like other `url()`s)
    /// and the `format()` hint when given. `local()` entries are left out.
    pub srcs: Vec<(Box<str>, Option<Box<str>>)>,
    /// The weights the face covers, `(400, 400)` when not given.
    pub weight: (u16, u16),
    pub italic: bool,
    /// The code points the face is for, from `unicode-range`; empty for all of them.
    pub ranges: Vec<(u32, u32)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LineHeight {
    Normal,
    Number(f32),
    Len(Len),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FontSize {
    Len(Len),
    Larger,
    Smaller,
}

/// One outer `box-shadow`. `color` is `None` for currentColor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub x: Len,
    pub y: Len,
    pub blur: Len,
    pub spread: Len,
    pub color: Option<Color>,
}

/// `box-shadow`: its outer shadows, `Some(None)` for none. Inset shadows, which are
/// not drawn, are left out.
fn box_shadow(v: &str) -> Option<Option<Rc<[Shadow]>>> {
    if v == "none" {
        return Some(None);
    }
    let mut out = Vec::new();
    for part in SplitTop::new(v, b',') {
        let (mut lens, mut color, mut inset) = (Vec::new(), None, false);
        for t in tokens(part) {
            if t == "inset" {
                inset = true;
            } else if let Some(l) = parse_len(t) {
                lens.push(l);
            } else {
                color = border_color(t)?;
            }
        }
        if !(2..=4).contains(&lens.len()) {
            return None;
        }
        if !inset {
            let zero = Len::Px(0.0);
            out.push(Shadow { x: lens[0], y: lens[1], blur: lens.get(2).copied().unwrap_or(zero), spread: lens.get(3).copied().unwrap_or(zero), color });
        }
    }
    Some((!out.is_empty()).then(|| Rc::from(out)))
}

#[derive(Debug, PartialEq)]
pub struct Gradient {
    /// CSS angle in degrees: 0 points up, 90 points right.
    pub angle: f32,
    pub stops: Vec<(f32, Color)>,
}

/// The lines of a `grid-template-rows` track list: each line name with its
/// 1-based line number, and how many lines there are.
#[derive(Debug, PartialEq)]
pub struct RowLines {
    pub names: Vec<(Box<str>, u16)>,
    pub count: u16,
}

/// The start line an item names in `grid-row`.
#[derive(Clone, Debug)]
pub enum GridLine {
    /// A line name, or an area name that matches its `-start` line.
    Name(Box<str>),
    /// A line number; negative numbers count from the last line.
    Num(i16),
}

/// Named lines of a track list such as `[image] auto [headline-start] 1fr`.
/// A `repeat()` counts as one track.
fn row_lines(v: &str) -> RowLines {
    let mut names = Vec::new();
    let mut line = 1u16;
    let mut rest = v.trim();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('[') {
            let end = r.find(']').unwrap_or(r.len());
            names.extend(r[..end].split_ascii_whitespace().map(|n| (n.into(), line)));
            rest = r.get(end + 1..).unwrap_or("");
        } else {
            let mut depth = 0u32;
            let end = rest
                .char_indices()
                .find(|&(_, c)| {
                    match c {
                        '(' => depth += 1,
                        ')' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    depth == 0 && (c.is_ascii_whitespace() || c == '[')
                })
                .map_or(rest.len(), |(i, _)| i);
            line += 1;
            rest = &rest[end..];
        }
        rest = rest.trim_start();
    }
    RowLines { names, count: line }
}

/// Start line of a `grid-row`, `grid-row-start` or `grid-area` value.
fn grid_line(v: &str) -> Option<GridLine> {
    let start = v.split('/').next()?.trim();
    let mut words = start.split_ascii_whitespace();
    let first = words.next()?;
    if first == "auto" || first == "span" {
        return None;
    }
    match first.parse::<i16>() {
        Ok(n) if n != 0 => Some(GridLine::Num(n)),
        Ok(_) => None,
        Err(_) => Some(GridLine::Name(first.into())),
    }
}

/// One parsed declaration. Shorthands are expanded at parse time; sides are
/// indexed top, right, bottom, left.
#[derive(Clone, Debug)]
pub enum Decl {
    Display(Display),
    Position(Position),
    Width(Len),
    Height(Len),
    MinWidth(Len),
    MaxWidth(Len),
    MinHeight(Len),
    MaxHeight(Len),
    Margin(u8, Len),
    Padding(u8, Len),
    Inset(u8, Len),
    BorderWidth(u8, Len),
    BorderStyle(u8, bool),
    /// `None` is currentColor.
    BorderColor(u8, Option<Color>),
    /// One corner's radius: 0 top-left, 1 top-right, 2 bottom-right, 3 bottom-left.
    Radius(u8, Len),
    BoxSizing(bool),
    Background(Color),
    Gradient(Option<Rc<Gradient>>),
    BoxShadow(Option<Rc<[Shadow]>>),
    /// `background-image: url()`, the URL resolved against its stylesheet.
    BgImage(Option<Rc<str>>),
    BgSize(BgSize),
    BgPos(Len, Len),
    /// `background-clip: text`.
    ClipText(bool),
    /// `None` is `inherit`.
    Color(Option<Color>),
    FontSize(FontSize),
    Weight(FontWeight),
    Italic(bool),
    /// The generic family the list falls back to, and the list as written.
    Family(Family, Rc<str>),
    LineHeight(LineHeight),
    TextAlign(TextAlign),
    TextDeco(u8),
    WhiteSpace(WhiteSpace),
    /// `-webkit-line-clamp`, 0 for none.
    LineClamp(u16),
    /// Whether `display` is the old `-webkit-box`, which `-webkit-line-clamp` needs.
    WebkitBox(bool),
    TextTransform(TextTransform),
    LetterSpacing(Len),
    ListStyle(ListStyle),
    FlexDir(bool),
    FlexWrap(bool),
    Justify(Justify),
    AlignItems(Align),
    AlignSelf(Align),
    Grow(f32),
    Shrink(f32),
    Basis(Len),
    RowGap(Len),
    ColGap(Len),
    Overflow(bool),
    /// `None` is the parent's.
    Visible(Option<bool>),
    /// `all: unset`: every property but the custom ones takes its inherited or
    /// initial value again, as if no earlier declaration had set it.
    AllUnset,
    /// `all: revert`: every property but the custom ones takes the value the
    /// built-in style sheet gives it.
    AllRevert,
    /// `inherit` (and `unset` of an inherited property): the property, by name,
    /// takes its parent's value.
    Inherit(Box<str>),
    /// `revert` and `revert-layer`: the property, by name, takes the value the
    /// built-in style sheet gives it.
    Revert(Box<str>),
    Opacity(f32),
    Pointer(bool),
    Translate(Len, Len),
    /// A `transform` that scales to nothing on some axis, so that nothing is drawn.
    ScaledAway(bool),
    /// `clip` (0) or `clip-path` (1) cuts the element to nothing.
    CutAway(u8, bool),
    /// `None` is auto.
    ZIndex(Option<i32>),
    /// Column count, and whether the tracks share the width evenly.
    GridCols(u8, bool),
    GridFlowCol(bool),
    /// `grid-template-rows`: its named lines, or `None` for `subgrid`,
    /// which takes the parent grid's lines.
    GridRows(Option<Rc<RowLines>>),
    /// Start line of `grid-row`; `None` is automatic placement.
    GridRow(Option<GridLine>),
    /// `order` of a flex or grid item.
    Order(i32),
    /// SVG fill; `None` is currentColor.
    Fill(Option<Color>),
    /// Custom property definition.
    Var(Box<str>, Box<str>),
    /// A value containing `var()`, or lengths only known while styling, parsed
    /// once the variables and sizes are known.
    Deferred(Box<str>, Box<str>),
    /// `content`; `None` is `none` or `normal`, which generate no box.
    Content(Option<Rc<[Content]>>),
    /// `float`: 0, `FLOAT_LEFT` or `FLOAT_RIGHT`.
    Float(u8),
    /// `clear`: `CLEAR_LEFT` and `CLEAR_RIGHT` bits.
    Clear(u8),
    /// The box establishes a block formatting context of its own, as
    /// `display: flow-root` and tables do; comes after the `Display` it goes with.
    FlowRoot(bool),
    /// The track list of a grid's rows (0) or columns (1); `None` is `none`.
    GridTracks(u8, Option<Rc<TrackList>>),
    GridAreas(Option<Rc<Areas>>),
    /// Sizes of a grid's implicit rows (0) or columns (1).
    GridAuto(u8, Option<Rc<[Track]>>),
    GridDense(bool),
    /// Rows (0) or columns (1) taken from the parent grid.
    Subgrid(u8, bool),
    /// One end of a grid item's place: row start, column start, row end or column end (0 to 3).
    GridPlace(u8, GridPos),
    JustifyItems(Align),
    JustifySelf(Align),
}

pub const FLOAT_LEFT: u8 = 1;
pub const FLOAT_RIGHT: u8 = 2;
pub const CLEAR_LEFT: u8 = 1;
pub const CLEAR_RIGHT: u8 = 2;

/// One piece of a `content` value.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(Box<str>),
    /// The value of the element's attribute of this name.
    Attr(Box<str>),
    /// An image, its URL resolved like other `url()`s.
    Url(Rc<str>),
}

/// Reads a `content` value. Counters show nothing; text after a `/` is
/// alternative text, which is not drawn.
fn parse_content(v: &str) -> Option<Rc<[Content]>> {
    let mut parts = Vec::new();
    for t in tokens(v) {
        let lower = t.to_ascii_lowercase();
        match lower.as_str() {
            "none" | "normal" => return None,
            "/" => break,
            "open-quote" => parts.push(Content::Text("\u{201C}".into())),
            "close-quote" => parts.push(Content::Text("\u{201D}".into())),
            _ if t.starts_with(['"', '\'']) => parts.push(Content::Text(unquote(t).into())),
            _ if lower.starts_with("url(") => parts.extend(css_url(t).map(|u| Content::Url(Rc::from(u.trim())))),
            _ if lower.starts_with("attr(") => parts.push(Content::Attr(lower[5..].trim_end_matches(')').trim().into())),
            _ => {}
        }
    }
    Some(parts.into())
}

/// `background-size`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BgSize {
    Auto,
    Cover,
    Contain,
    /// Width and height; `Auto` keeps the image's aspect ratio.
    Size(Len, Len),
}

pub const DECO_UNDERLINE: u8 = 1;
pub const DECO_LINE_THROUGH: u8 = 2;

/// Splits on a separator byte outside parentheses, quotes and escapes. A space
/// separator matches any ASCII whitespace.
pub struct SplitTop<'a> {
    s: &'a str,
    pos: usize,
    sep: u8,
    done: bool,
}

impl<'a> SplitTop<'a> {
    pub fn new(s: &'a str, sep: u8) -> Self {
        SplitTop { s, pos: 0, sep, done: false }
    }
}

impl<'a> Iterator for SplitTop<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        if self.done {
            return None;
        }
        let b = self.s.as_bytes();
        let start = self.pos;
        let mut i = start;
        let mut depth = 0i32;
        while i < b.len() {
            let c = b[i];
            if c == b'(' {
                depth += 1;
            } else if c == b')' {
                depth -= 1;
            } else if c == b'\\' {
                i += 1;
            } else if c == b'"' || c == b'\'' {
                i += 1;
                while i < b.len() && b[i] != c {
                    i += 1 + (b[i] == b'\\') as usize;
                }
            } else if depth <= 0 && (c == self.sep || (self.sep == b' ' && c.is_ascii_whitespace())) {
                break;
            }
            i += 1;
        }
        let i = i.min(b.len());
        if i >= b.len() {
            self.done = true;
        }
        self.pos = i + 1;
        Some(&self.s[start..i])
    }
}

fn tokens(v: &str) -> impl Iterator<Item = &str> {
    SplitTop::new(v, b' ').filter(|t| !t.is_empty())
}

fn num(s: &str) -> Option<f32> {
    let c = *s.as_bytes().first()?;
    if c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+') {
        s.parse().ok()
    } else {
        None
    }
}

/// Evaluates a `calc()` body with + - * / over px and percentages.
/// Terms are (percent, px, is plain number).
fn calc(inner: &str) -> Option<Len> {
    let (mut pct, mut px, mut sign, mut op) = (0.0, 0.0, 1.0, b'+');
    let mut term: Option<(f32, f32, bool)> = None;
    let mut pieces = Vec::new();
    for t in tokens(inner) {
        split_products(t, &mut pieces);
    }
    for t in pieces {
        match t {
            "+" | "-" => {
                if let Some((p, v, _)) = term.take() {
                    pct += sign * p;
                    px += sign * v;
                }
                sign = if t == "+" { 1.0 } else { -1.0 };
            }
            "*" | "/" => op = t.as_bytes()[0],
            _ => {
                let v = if let Ok(n) = t.parse::<f32>() {
                    (0.0, n, true)
                } else {
                    let nested;
                    let t = if t.starts_with('(') {
                        nested = format!("calc{t}");
                        &nested
                    } else {
                        t
                    };
                    match parse_len(t)? {
                        Len::Px(v) => (0.0, v, false),
                        Len::Pct(p) => (p, 0.0, false),
                        Len::Calc(p, v) => (p, v, false),
                        _ => return None,
                    }
                };
                term = Some(match (term, op) {
                    (Some(a), b'*') if v.2 => (a.0 * v.1, a.1 * v.1, a.2),
                    (Some(a), b'*') => (v.0 * a.1, v.1 * a.1, false),
                    (Some(a), b'/') if v.1 != 0.0 => (a.0 / v.1, a.1 / v.1, a.2),
                    _ => v,
                });
                op = b'+';
            }
        }
    }
    if let Some((p, v, _)) = term {
        pct += sign * p;
        px += sign * v;
    }
    Some(if pct == 0.0 { Len::Px(px) } else { Len::Calc(pct, px) })
}

/// Splits a `calc()` token at `*` and `/` outside parentheses, which CSS
/// allows without surrounding spaces (`calc(var(--x)*-1)`), keeping each
/// operator as its own piece.
fn split_products<'a>(t: &'a str, out: &mut Vec<&'a str>) {
    let (mut depth, mut start) = (0u32, 0);
    for (i, c) in t.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '*' | '/' if depth == 0 => {
                if i > start {
                    out.push(&t[start..i]);
                }
                out.push(&t[i..i + 1]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < t.len() {
        out.push(&t[start..]);
    }
}

/// Whether `clip: rect(top, right, bottom, left)` leaves nothing of the box; an `auto`
/// side is the box's own.
fn clip_is_empty(v: &str) -> bool {
    let Some(args) = v.strip_prefix("rect(").and_then(|r| r.strip_suffix(')')) else { return false };
    let px: Vec<Option<f32>> = args
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(|t| if let Some(Len::Px(v)) = parse_len(t) { Some(v) } else { None })
        .collect();
    let [t, r, b, l] = px[..] else { return false };
    matches!((t, b), (Some(t), Some(b)) if b <= t) || matches!((l, r), (Some(l), Some(r)) if r <= l)
}

/// Whether `clip-path: inset()` leaves nothing of the box, from its percentages; other
/// lengths but 0 depend on the box and count as leaving some.
fn inset_is_empty(v: &str) -> bool {
    let Some(args) = v.strip_prefix("inset(").and_then(|r| r.strip_suffix(')')) else { return false };
    let pct: Option<Vec<f32>> = args
        .split_whitespace()
        .take_while(|t| *t != "round")
        .map(|t| match parse_len(t) {
            Some(Len::Pct(p)) => Some(p),
            Some(Len::Px(v)) if v == 0.0 => Some(0.0),
            _ => None,
        })
        .collect();
    let [t, r, b, l] = match pct.as_deref() {
        Some(&[a]) => [a; 4],
        Some(&[a, b]) => [a, b, a, b],
        Some(&[a, b, c]) => [a, b, c, b],
        Some(&[a, b, c, d]) => [a, b, c, d],
        _ => return false,
    };
    t + b >= 100.0 || l + r >= 100.0
}

/// Whether a `transform` value scales to nothing on some axis. Other scale factors are
/// ignored, as are the functions other than translations.
fn scaled_away(v: &str) -> bool {
    tokens(v).any(|f| {
        let Some((name, args)) = f.strip_suffix(')').and_then(|r| r.split_once('(')) else { return false };
        let mut a = SplitTop::new(args, b',').map(|a| a.trim().parse::<f32>().ok());
        match name {
            "scale" | "scale3d" => {
                let sx = a.next().flatten();
                let sy = a.next().flatten().or(if name == "scale" { sx } else { None });
                sx == Some(0.0) || sy == Some(0.0)
            }
            "scalex" | "scaley" => a.next().flatten() == Some(0.0),
            _ => false,
        }
    })
}

/// `translate` components of a `transform` value; other functions are ignored.
fn parse_translate(v: &str) -> (Len, Len) {
    let (mut x, mut y) = (Len::Px(0.0), Len::Px(0.0));
    for f in tokens(v) {
        let Some((name, args)) = f.strip_suffix(')').and_then(|r| r.split_once('(')) else { continue };
        let mut a = SplitTop::new(args, b',').map(|a| parse_len(a.trim()).filter(|l| *l != Len::Auto));
        match name {
            "translate" | "translate3d" => {
                x = a.next().flatten().unwrap_or(x);
                y = a.next().flatten().unwrap_or(y);
            }
            "translatex" => x = a.next().flatten().unwrap_or(x),
            "translatey" => y = a.next().flatten().unwrap_or(y),
            _ => {}
        }
    }
    (x, y)
}

pub fn parse_len(s: &str) -> Option<Len> {
    use Len::*;
    if s == "auto" {
        return Some(Auto);
    }
    if let Some(inner) = s.strip_prefix("calc(").and_then(|r| r.strip_suffix(')')) {
        return calc(inner);
    }
    if let Some((func, args)) = s.strip_suffix(')').and_then(|r| r.split_once('(')) {
        let pick: fn(f32, f32) -> f32 = match func {
            // Safe-area insets and similar environment values are zero on a desktop window.
            "env" => return SplitTop::new(args, b',').nth(1).map_or(Some(Px(0.0)), |a| calc(a.trim())),
            "min" => f32::min,
            "max" => f32::max,
            "clamp" => return SplitTop::new(args, b',').nth(1).and_then(|a| calc(a.trim())),
            _ => return None,
        };
        let vals: Vec<Len> = SplitTop::new(args, b',').map(|a| calc(a.trim())).collect::<Option<_>>()?;
        let px = vals.iter().filter_map(|l| if let Px(v) = l { Some(*v) } else { None });
        return match px.clone().count() {
            0 => vals.first().copied(),
            n if n == vals.len() => px.reduce(pick).map(Px),
            _ => vals.into_iter().find(|l| !matches!(l, Px(_))),
        };
    }
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i].is_ascii_digit() || matches!(b[i], b'.' | b'-' | b'+')) {
        i += 1;
    }
    let n = num(&s[..i])?;
    Some(match viewport_unit(&s[i..]) {
        "px" => Px(n),
        "" if n == 0.0 => Px(0.0),
        "%" => Pct(n),
        "em" => Em(n),
        "rem" => Rem(n),
        "pt" => Px(n * 4.0 / 3.0),
        "vw" | "vmin" => Vw(n),
        "vh" | "vmax" => Vh(n),
        "ex" | "ch" => Em(n * 0.5),
        "in" => Px(n * 96.0),
        "cm" => Px(n * 37.8),
        "mm" => Px(n * 3.78),
        "pc" => Px(n * 16.0),
        "q" => Px(n * 3.78 / 4.0),
        // A line of normal height.
        "lh" | "rlh" => Em(n * 1.2),
        _ => return None,
    })
}

/// The viewport unit a dynamic, small, large or container-relative unit
/// measures here: the window does not resize its viewport as a phone's toolbars
/// do, and a container query unit outside any container measures the viewport.
pub fn viewport_unit(unit: &str) -> &str {
    match unit {
        "dvw" | "svw" | "lvw" | "cqw" | "cqi" => "vw",
        "dvh" | "svh" | "lvh" | "cqh" | "cqb" => "vh",
        "dvmin" | "svmin" | "lvmin" | "cqmin" => "vmin",
        "dvmax" | "svmax" | "lvmax" | "cqmax" => "vmax",
        u => u,
    }
}

fn hex(h: &str) -> Option<Color> {
    if !h.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    let n = |shift: u32| (v >> shift & 0xF) * 17;
    Some(match h.len() {
        3 => Color::rgba(n(8), n(4), n(0), 255),
        4 => Color::rgba(n(12), n(8), n(4), n(0)),
        6 => Color(0xFF00_0000 | v),
        8 => Color(v >> 8 | (v & 0xFF) << 24),
        _ => return None,
    })
}

fn hue(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 0.5 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

pub fn parse_color(s: &str) -> Option<Color> {
    if let Some(h) = s.strip_prefix('#') {
        return hex(h);
    }
    if let Some(p) = s.find('(') {
        let inner = s[p + 1..].strip_suffix(')')?;
        let a: Vec<&str> = inner
            .split(|c: char| c == ',' || c == '/' || c.is_ascii_whitespace())
            .filter(|t| !t.is_empty())
            .collect();
        if a.len() < 3 {
            return None;
        }
        let frac = |t: &str| match t.strip_suffix('%') {
            Some(p) => num(p).map(|v| v / 100.0),
            None => num(t),
        };
        let alpha = match a.get(3) {
            Some(t) => frac(t)?.clamp(0.0, 1.0),
            None => 1.0,
        };
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        let (r, g, b) = match &s[..p] {
            "rgb" | "rgba" => {
                let ch = |t: &str| match t.strip_suffix('%') {
                    Some(p) => num(p).map(|v| v / 100.0),
                    None => num(t).map(|v| v / 255.0),
                };
                (ch(a[0])?, ch(a[1])?, ch(a[2])?)
            }
            "hsl" | "hsla" => {
                let h = num(a[0].trim_end_matches("deg"))?.rem_euclid(360.0) / 360.0;
                let (sat, l) = (frac(a[1])?.clamp(0.0, 1.0), frac(a[2])?.clamp(0.0, 1.0));
                let q = if l < 0.5 { l * (1.0 + sat) } else { l + sat - l * sat };
                let pp = 2.0 * l - q;
                (hue(pp, q, h + 1.0 / 3.0), hue(pp, q, h), hue(pp, q, h - 1.0 / 3.0))
            }
            _ => return None,
        };
        return Some(Color::rgba(byte(r), byte(g), byte(b), byte(alpha)));
    }
    named(s).map(|v| Color(0xFF00_0000 | v))
}

fn named(s: &str) -> Option<u32> {
    Some(match s {
        "aliceblue" => 0xf0f8ff,
        "antiquewhite" => 0xfaebd7,
        "aqua" | "cyan" => 0x00ffff,
        "aquamarine" => 0x7fffd4,
        "azure" => 0xf0ffff,
        "beige" => 0xf5f5dc,
        "bisque" => 0xffe4c4,
        "black" => 0x000000,
        "blanchedalmond" => 0xffebcd,
        "blue" => 0x0000ff,
        "blueviolet" => 0x8a2be2,
        "brown" => 0xa52a2a,
        "burlywood" => 0xdeb887,
        "cadetblue" => 0x5f9ea0,
        "chartreuse" => 0x7fff00,
        "chocolate" => 0xd2691e,
        "coral" => 0xff7f50,
        "cornflowerblue" => 0x6495ed,
        "cornsilk" => 0xfff8dc,
        "crimson" => 0xdc143c,
        "darkblue" => 0x00008b,
        "darkcyan" => 0x008b8b,
        "darkgoldenrod" => 0xb8860b,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "darkgreen" => 0x006400,
        "darkkhaki" => 0xbdb76b,
        "darkmagenta" => 0x8b008b,
        "darkolivegreen" => 0x556b2f,
        "darkorange" => 0xff8c00,
        "darkorchid" => 0x9932cc,
        "darkred" => 0x8b0000,
        "darksalmon" => 0xe9967a,
        "darkseagreen" => 0x8fbc8f,
        "darkslateblue" => 0x483d8b,
        "darkslategray" | "darkslategrey" => 0x2f4f4f,
        "darkturquoise" => 0x00ced1,
        "darkviolet" => 0x9400d3,
        "deeppink" => 0xff1493,
        "deepskyblue" => 0x00bfff,
        "dimgray" | "dimgrey" => 0x696969,
        "dodgerblue" => 0x1e90ff,
        "firebrick" => 0xb22222,
        "floralwhite" => 0xfffaf0,
        "forestgreen" => 0x228b22,
        "fuchsia" | "magenta" => 0xff00ff,
        "gainsboro" => 0xdcdcdc,
        "ghostwhite" => 0xf8f8ff,
        "gold" => 0xffd700,
        "goldenrod" => 0xdaa520,
        "gray" | "grey" => 0x808080,
        "green" => 0x008000,
        "greenyellow" => 0xadff2f,
        "honeydew" => 0xf0fff0,
        "hotpink" => 0xff69b4,
        "indianred" => 0xcd5c5c,
        "indigo" => 0x4b0082,
        "ivory" => 0xfffff0,
        "khaki" => 0xf0e68c,
        "lavender" => 0xe6e6fa,
        "lavenderblush" => 0xfff0f5,
        "lawngreen" => 0x7cfc00,
        "lemonchiffon" => 0xfffacd,
        "lightblue" => 0xadd8e6,
        "lightcoral" => 0xf08080,
        "lightcyan" => 0xe0ffff,
        "lightgoldenrodyellow" => 0xfafad2,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "lightgreen" => 0x90ee90,
        "lightpink" => 0xffb6c1,
        "lightsalmon" => 0xffa07a,
        "lightseagreen" => 0x20b2aa,
        "lightskyblue" => 0x87cefa,
        "lightslategray" | "lightslategrey" => 0x778899,
        "lightsteelblue" => 0xb0c4de,
        "lightyellow" => 0xffffe0,
        "lime" => 0x00ff00,
        "limegreen" => 0x32cd32,
        "linen" => 0xfaf0e6,
        "maroon" => 0x800000,
        "mediumaquamarine" => 0x66cdaa,
        "mediumblue" => 0x0000cd,
        "mediumorchid" => 0xba55d3,
        "mediumpurple" => 0x9370db,
        "mediumseagreen" => 0x3cb371,
        "mediumslateblue" => 0x7b68ee,
        "mediumspringgreen" => 0x00fa9a,
        "mediumturquoise" => 0x48d1cc,
        "mediumvioletred" => 0xc71585,
        "midnightblue" => 0x191970,
        "mintcream" => 0xf5fffa,
        "mistyrose" => 0xffe4e1,
        "moccasin" => 0xffe4b5,
        "navajowhite" => 0xffdead,
        "navy" => 0x000080,
        "oldlace" => 0xfdf5e6,
        "olive" => 0x808000,
        "olivedrab" => 0x6b8e23,
        "orange" => 0xffa500,
        "orangered" => 0xff4500,
        "orchid" => 0xda70d6,
        "palegoldenrod" => 0xeee8aa,
        "palegreen" => 0x98fb98,
        "paleturquoise" => 0xafeeee,
        "palevioletred" => 0xdb7093,
        "papayawhip" => 0xffefd5,
        "peachpuff" => 0xffdab9,
        "peru" => 0xcd853f,
        "pink" => 0xffc0cb,
        "plum" => 0xdda0dd,
        "powderblue" => 0xb0e0e6,
        "purple" => 0x800080,
        "rebeccapurple" => 0x663399,
        "red" => 0xff0000,
        "rosybrown" => 0xbc8f8f,
        "royalblue" => 0x4169e1,
        "saddlebrown" => 0x8b4513,
        "salmon" => 0xfa8072,
        "sandybrown" => 0xf4a460,
        "seagreen" => 0x2e8b57,
        "seashell" => 0xfff5ee,
        "sienna" => 0xa0522d,
        "silver" => 0xc0c0c0,
        "skyblue" => 0x87ceeb,
        "slateblue" => 0x6a5acd,
        "slategray" | "slategrey" => 0x708090,
        "snow" => 0xfffafa,
        "springgreen" => 0x00ff7f,
        "steelblue" => 0x4682b4,
        "tan" => 0xd2b48c,
        "teal" => 0x008080,
        "thistle" => 0xd8bfd8,
        "tomato" => 0xff6347,
        "turquoise" => 0x40e0d0,
        "violet" => 0xee82ee,
        "wheat" => 0xf5deb3,
        "white" => 0xffffff,
        "whitesmoke" => 0xf5f5f5,
        "yellow" => 0xffff00,
        "yellowgreen" => 0x9acd32,
        _ => return None,
    })
}

/// Colour token including `transparent`.
/// A grid track list without its `[line names]`, which are not tracks.
fn without_line_names(v: &str) -> String {
    let mut depth = 0u32;
    v.chars()
        .filter(|&c| {
            match c {
                '[' => depth += 1,
                ']' => depth = depth.saturating_sub(1),
                _ => return depth == 0,
            }
            false
        })
        .collect()
}

/// The address in a `url()` token, without quotes and with its escapes decoded.
fn css_url(t: &str) -> Option<Cow<'_, str>> {
    let inner = t.strip_prefix("url(")?.strip_suffix(')')?.trim();
    Some(unquote(inner)).filter(|u| !u.trim().is_empty())
}

fn is_position(t: &str) -> bool {
    matches!(t, "left" | "right" | "top" | "bottom" | "center") || matches!(parse_len(t), Some(Len::Px(_) | Len::Pct(_)))
}

/// `background-position` from its keywords and lengths; a missing axis is centred.
fn bg_position(parts: &[&str]) -> Option<(Len, Len)> {
    let (mut x, mut y, mut rest) = (None, None, Vec::new());
    for &p in parts {
        match p {
            "left" => x = Some(Len::Pct(0.0)),
            "right" => x = Some(Len::Pct(100.0)),
            "top" => y = Some(Len::Pct(0.0)),
            "bottom" => y = Some(Len::Pct(100.0)),
            "center" => rest.push(Len::Pct(50.0)),
            _ => rest.push(parse_len(p).filter(|l| *l != Len::Auto)?),
        }
    }
    let mut rest = rest.into_iter();
    let x = x.or_else(|| rest.next()).unwrap_or(Len::Pct(50.0));
    let y = y.or_else(|| rest.next()).unwrap_or(Len::Pct(50.0));
    Some((x, y))
}

fn bg_size(v: &str) -> Option<BgSize> {
    match v.trim() {
        "" => None,
        "cover" => Some(BgSize::Cover),
        "contain" => Some(BgSize::Contain),
        v => {
            let mut t = tokens(v).map(parse_len);
            let w = t.next()??;
            Some(BgSize::Size(w, t.next().flatten().unwrap_or(Len::Auto)))
        }
    }
}

/// `css` with every relative `url()` rewritten to resolve against `base`, the
/// location of the stylesheet it came from: a URL, or a path relative to the page.
fn rebase_urls<'a>(css: &'a str, base: &str) -> Cow<'a, str> {
    if base.is_empty() || !css.contains("url(") {
        return Cow::Borrowed(css);
    }
    let web = crate::net::Url::parse(base);
    let dir = &base[..base.rfind('/').map_or(0, |i| i + 1)];
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(p) = rest.find("url(") {
        let Some(len) = rest[p..].find(')') else { break };
        let token = &rest[p..p + len + 1];
        out.push_str(&rest[..p]);
        let absolute = |u: &str| u.starts_with(['/', '#']) || u.starts_with("data:") || u.contains("://");
        match css_url(token).filter(|u| !absolute(u.trim())) {
            Some(u) => {
                let u = u.trim();
                let joined = match &web {
                    Some(b) => b.join(u).map(|j| j.to_string()),
                    None => Some(format!("{dir}{u}")),
                };
                // Written back as a string, so it is read the same way again.
                let escaped = joined.as_deref().unwrap_or(u).replace('\\', "\\\\").replace('"', "\\\"");
                out.push_str(&format!("url(\"{escaped}\")"));
            }
            None => out.push_str(token),
        }
        rest = &rest[p + len + 1..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn color_tok(t: &str) -> Option<Color> {
    if t == "transparent" {
        Some(Color::TRANSPARENT)
    } else {
        parse_color(t)
    }
}

fn parse_gradient(v: &str) -> Option<Gradient> {
    let inner = v.strip_prefix("linear-gradient(")?.strip_suffix(')')?;
    let args: Vec<&str> = SplitTop::new(inner, b',').map(str::trim).collect();
    let mut angle = 180.0;
    let mut first = 0;
    let head = *args.first()?;
    if let Some(dir) = head.strip_prefix("to ") {
        let (mut x, mut y) = (0.0f32, 0.0f32);
        for w in dir.split_ascii_whitespace() {
            match w {
                "top" => y = 1.0,
                "bottom" => y = -1.0,
                "right" => x = 1.0,
                "left" => x = -1.0,
                _ => {}
            }
        }
        angle = x.atan2(y).to_degrees();
        first = 1;
    } else if let Some(n) = head.strip_suffix("deg").and_then(num) {
        angle = n;
        first = 1;
    } else if let Some(n) = head.strip_suffix("turn").and_then(num) {
        angle = n * 360.0;
        first = 1;
    }
    let mut stops: Vec<(Option<f32>, Color)> = Vec::new();
    for a in &args[first..] {
        let mut tk = tokens(a);
        let c = color_tok(tk.next()?)?;
        let pos = tk.next().and_then(|p| p.strip_suffix('%')).and_then(num).map(|p| p / 100.0);
        stops.push((pos, c));
    }
    if stops.len() < 2 {
        return None;
    }
    let last = stops.len() - 1;
    stops[0].0.get_or_insert(0.0);
    stops[last].0.get_or_insert(1.0);
    let mut i = 1;
    while i < last {
        if stops[i].0.is_none() {
            let mut j = i;
            while stops[j].0.is_none() {
                j += 1;
            }
            let (a, b) = (stops[i - 1].0.unwrap(), stops[j].0.unwrap());
            for k in i..j {
                stops[k].0 = Some(a + (b - a) * (k - i + 1) as f32 / (j - i + 1) as f32);
            }
            i = j;
        }
        i += 1;
    }
    Some(Gradient { angle, stops: stops.into_iter().map(|(p, c)| (p.unwrap(), c)).collect() })
}

fn four<T: Copy>(v: &str, parse: impl Fn(&str) -> Option<T>) -> Option<[T; 4]> {
    let t: Vec<T> = tokens(v).map(parse).collect::<Option<Vec<_>>>()?;
    Some(match t.len() {
        1 => [t[0]; 4],
        2 => [t[0], t[1], t[0], t[1]],
        3 => [t[0], t[1], t[2], t[1]],
        4 => [t[0], t[1], t[2], t[3]],
        _ => return None,
    })
}

fn size(v: &str) -> Option<Len> {
    match v {
        "none" | "max-content" | "min-content" | "fit-content" => Some(Len::Auto),
        _ => parse_len(v),
    }
}

fn border_width(t: &str) -> Option<Len> {
    match t {
        "thin" => Some(Len::Px(1.0)),
        "medium" => Some(Len::Px(3.0)),
        "thick" => Some(Len::Px(5.0)),
        _ => parse_len(t).filter(|l| *l != Len::Auto),
    }
}

fn border_style(t: &str) -> Option<bool> {
    match t {
        "none" | "hidden" => Some(false),
        "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset" => Some(true),
        _ => None,
    }
}

fn border_color(t: &str) -> Option<Option<Color>> {
    if t == "currentcolor" {
        Some(None)
    } else {
        color_tok(t).map(Some)
    }
}

fn border_shorthand(v: &str, sides: std::ops::Range<u8>, out: &mut Vec<Decl>) {
    let (mut w, mut st, mut c) = (Len::Px(3.0), false, None);
    for t in tokens(v) {
        if let Some(s) = border_style(t) {
            st = s;
        } else if let Some(l) = border_width(t) {
            w = l;
        } else if let Some(col) = border_color(t) {
            c = col;
        }
    }
    for i in sides {
        out.push(Decl::BorderWidth(i, w));
        out.push(Decl::BorderStyle(i, st));
        out.push(Decl::BorderColor(i, c));
    }
}

fn font_size(v: &str) -> Option<FontSize> {
    let px = |n: f32| Some(FontSize::Len(Len::Px(n)));
    match v {
        "xx-small" => px(9.0),
        "x-small" => px(10.0),
        "small" => px(13.0),
        "medium" => px(16.0),
        "large" => px(18.0),
        "x-large" => px(24.0),
        "xx-large" => px(32.0),
        "larger" => Some(FontSize::Larger),
        "smaller" => Some(FontSize::Smaller),
        _ => parse_len(v).filter(|l| *l != Len::Auto).map(FontSize::Len),
    }
}

fn font_weight(v: &str) -> Option<FontWeight> {
    match v {
        "bold" => Some(FontWeight::Abs(700)),
        "normal" => Some(FontWeight::Abs(400)),
        "bolder" => Some(FontWeight::Bolder),
        "lighter" => Some(FontWeight::Lighter),
        _ => num(v).filter(|n| (1.0..=1000.0).contains(n)).map(|n| FontWeight::Abs(n as u16)),
    }
}

fn line_height(v: &str) -> Option<LineHeight> {
    if v == "normal" {
        return Some(LineHeight::Normal);
    }
    if let Ok(n) = v.parse::<f32>() {
        return Some(LineHeight::Number(n));
    }
    parse_len(v).filter(|l| *l != Len::Auto).map(LineHeight::Len)
}

fn font_family(v: &str) -> Family {
    family_names(v).find_map(|n| builtin_family(&n)).unwrap_or(Family::Sans)
}

/// The names of a `font-family` list, unquoted and in lower case.
pub fn family_names(v: &str) -> impl Iterator<Item = String> + '_ {
    SplitTop::new(v, b',').map(|name| unquote(name.trim()).to_ascii_lowercase())
}

/// The built-in family that stands in for a family name, if any does.
pub fn builtin_family(n: &str) -> Option<Family> {
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&["mono", "courier", "consol", "menlo", "code"]) {
        return Some(Family::Mono);
    }
    if has(&["sans", "arial", "helvetica", "system-ui", "-apple-system", "segoe", "roboto", "inter", "verdana", "tahoma", "gothic", "pretendard"]) {
        return Some(Family::Sans);
    }
    if has(&["serif", "times", "georgia", "garamond", "palatino", "myeongjo", "batang"]) {
        return Some(Family::Serif);
    }
    None
}

/// Reads the body of an `@font-face` rule; `None` without a family or a usable source.
fn font_face(body: &str) -> Option<FontFace> {
    let mut face = FontFace { family: "".into(), srcs: Vec::new(), weight: (400, 400), italic: false, ranges: Vec::new() };
    for decl in SplitTop::new(body, b';') {
        let Some((name, value)) = decl.split_once(':') else { continue };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "font-family" => face.family = family_names(value).next()?.into(),
            "font-style" => face.italic = value.starts_with("italic") || value.starts_with("oblique"),
            "font-weight" => {
                let mut w = tokens(value).filter_map(|t| match font_weight(t) {
                    Some(FontWeight::Abs(n)) => Some(n),
                    _ => None,
                });
                if let Some(lo) = w.next() {
                    face.weight = (lo, w.next().unwrap_or(lo).max(lo));
                }
            }
            "src" => {
                for entry in SplitTop::new(value, b',') {
                    let mut parts = tokens(entry.trim());
                    let Some(url) = parts.next().and_then(css_url) else { continue };
                    let format = parts.find_map(|p| p.strip_prefix("format(")?.strip_suffix(')'));
                    let format = format.map(|f| unquote(f.trim()).to_ascii_lowercase().into());
                    face.srcs.push((url.trim().into(), format));
                }
            }
            "unicode-range" => face.ranges = unicode_ranges(value).unwrap_or_default(),
            _ => {}
        }
    }
    (!face.family.is_empty() && !face.srcs.is_empty()).then_some(face)
}

/// The code point ranges of a `unicode-range` value: `U+26`, `U+0-7F` or `U+4??`,
/// separated by commas. `None` when one is invalid, which makes the whole value invalid.
fn unicode_ranges(v: &str) -> Option<Vec<(u32, u32)>> {
    let hex = |s: &str| (!s.is_empty() && s.len() <= 6).then(|| u32::from_str_radix(s, 16).ok()).flatten();
    let range = |r: &str| {
        let r = r.trim().to_ascii_uppercase();
        let r = r.strip_prefix("U+")?;
        let (lo, hi) = match r.split_once('-') {
            Some((a, b)) => (hex(a)?, hex(b)?),
            None if r.contains('?') => (hex(&r.replace('?', "0"))?, hex(&r.replace('?', "F"))?),
            None => (hex(r)?, hex(r)?),
        };
        (lo <= hi && hi <= 0x10ffff).then_some((lo, hi))
    };
    v.split(',').map(range).collect()
}

fn align(v: &str) -> Option<Align> {
    Some(match v {
        "stretch" | "normal" => Align::Stretch,
        "flex-start" | "start" | "baseline" => Align::Start,
        "center" => Align::Center,
        "flex-end" | "end" => Align::End,
        "auto" => Align::Auto,
        _ => return None,
    })
}

const SIDE_NAMES: [&str; 4] = ["top", "right", "bottom", "left"];

fn side_of(name: &str) -> Option<u8> {
    SIDE_NAMES.iter().position(|s| *s == name).map(|i| i as u8)
}

/// Initial values of properties that are not inherited, which `initial`, `unset`
/// and `revert` (with no author origin below to fall back to) set them to.
const INITIAL: &[(&str, &str)] = &[
    ("display", "inline"),
    ("position", "static"),
    ("float", "none"),
    ("clear", "none"),
    ("top", "auto"),
    ("right", "auto"),
    ("bottom", "auto"),
    ("left", "auto"),
    ("inset", "auto"),
    ("width", "auto"),
    ("height", "auto"),
    ("min-width", "auto"),
    ("min-height", "auto"),
    ("max-width", "none"),
    ("max-height", "none"),
    ("margin", "0"),
    ("margin-top", "0"),
    ("margin-right", "0"),
    ("margin-bottom", "0"),
    ("margin-left", "0"),
    ("padding", "0"),
    ("padding-top", "0"),
    ("padding-right", "0"),
    ("padding-bottom", "0"),
    ("padding-left", "0"),
    ("box-sizing", "content-box"),
    ("flex", "0 1 auto"),
    ("flex-grow", "0"),
    ("flex-shrink", "1"),
    ("flex-basis", "auto"),
    ("flex-direction", "row"),
    ("flex-wrap", "nowrap"),
    ("order", "0"),
    ("align-self", "auto"),
    ("justify-self", "auto"),
    ("overflow", "visible"),
    ("overflow-x", "visible"),
    ("overflow-y", "visible"),
    ("z-index", "auto"),
    ("opacity", "1"),
    ("transform", "none"),
    ("background", "none"),
    ("background-color", "transparent"),
    ("background-image", "none"),
    ("clip-path", "none"),
    ("vertical-align", "baseline"),
    ("grid", "none"),
    ("grid-template", "none"),
    ("grid-template-columns", "none"),
    ("grid-template-rows", "none"),
    ("grid-template-areas", "none"),
    ("grid-auto-columns", "auto"),
    ("grid-auto-rows", "auto"),
    ("grid-auto-flow", "row"),
    ("grid-area", "auto"),
    ("grid-row", "auto"),
    ("grid-column", "auto"),
    ("grid-row-start", "auto"),
    ("grid-row-end", "auto"),
    ("grid-column-start", "auto"),
    ("grid-column-end", "auto"),
];

/// Parses one declaration and appends the longhands it expands to.
/// Unknown properties and invalid values are dropped, as CSS requires.
pub fn parse_decl(name: &str, value: &str, out: &mut Vec<Decl>) {
    use Decl as D;
    // Its strings keep their case.
    if name == "content" {
        out.push(D::Content(parse_content(value.trim())));
        return;
    }
    let lower;
    let v = value.trim();
    let v = if v.bytes().any(|b| b.is_ascii_uppercase()) {
        lower = v.to_ascii_lowercase();
        &lower[..]
    } else {
        v
    };
    let name = match name {
        "margin-inline-start" => "margin-left",
        "margin-inline-end" => "margin-right",
        "margin-block-start" => "margin-top",
        "margin-block-end" => "margin-bottom",
        "padding-inline-start" => "padding-left",
        "padding-inline-end" => "padding-right",
        "padding-block-start" => "padding-top",
        "padding-block-end" => "padding-bottom",
        "inset-inline-start" => "left",
        "inset-inline-end" => "right",
        "inset-block-start" => "top",
        "inset-block-end" => "bottom",
        "border-inline-start" => "border-left",
        "border-inline-end" => "border-right",
        "border-block-start" => "border-top",
        "border-block-end" => "border-bottom",
        n => n,
    };
    // CSS-wide keywords: `inherit` and `revert` copy the property from the parent or
    // from the built-in style; `unset` inherits an inherited property; `initial`, and
    // `unset` otherwise, reset a property to its initial value.
    if matches!(v, "inherit" | "unset" | "revert" | "revert-layer") && crate::style::copies(name) {
        let inherited = crate::style::inherited(name);
        match v {
            "inherit" => return out.push(D::Inherit(name.into())),
            "unset" if inherited => return out.push(D::Inherit(name.into())),
            "revert" | "revert-layer" => return out.push(D::Revert(name.into())),
            _ => {}
        }
    }
    if matches!(v, "initial" | "unset" | "revert" | "revert-layer") {
        if v.starts_with("revert") && name != "all" {
            drops::note("keyword-as-initial", v);
        }
        if let Some(&(_, initial)) = INITIAL.iter().find(|(n, _)| *n == name) {
            return parse_decl(name, initial, out);
        }
    }
    let mut push = |d: Option<Decl>| {
        if let Some(d) = d {
            out.push(d)
        }
    };
    match name {
        "display" if matches!(v, "flow-root" | "table" | "table-cell" | "table-caption") => {
            out.push(D::Display(Display::Block));
            out.push(D::FlowRoot(true));
        }
        // The old flexbox, laid out as a block: `-webkit-line-clamp` counts only on one.
        "display" if matches!(v, "-webkit-box" | "-webkit-inline-box") => {
            out.push(D::Display(if v == "-webkit-box" { Display::Block } else { Display::InlineBlock }));
            out.push(D::WebkitBox(true));
        }
        "float" => push(Some(D::Float(match v {
            "left" | "inline-start" => FLOAT_LEFT,
            "right" | "inline-end" => FLOAT_RIGHT,
            _ => 0,
        }))),
        "clear" => push(Some(D::Clear(match v {
            "left" | "inline-start" => CLEAR_LEFT,
            "right" | "inline-end" => CLEAR_RIGHT,
            "both" => CLEAR_LEFT | CLEAR_RIGHT,
            _ => 0,
        }))),
        "display" => push(match v {
            "none" => Some(Display::None),
            "inline" => Some(Display::Inline),
            "inline-block" | "inline-table" => Some(Display::InlineBlock),
            "flex" | "table-row" => Some(Display::Flex),
            "inline-flex" => Some(Display::InlineFlex),
            "list-item" => Some(Display::ListItem),
            "grid" | "inline-grid" => Some(Display::Grid),
            "contents" => Some(Display::Contents),
            "inherit" => Some(Display::Inherit),
            "block" | "flow-root" | "table" | "table-cell" | "table-row-group" | "table-header-group" | "table-footer-group" | "table-caption" => Some(Display::Block),
            _ => None,
        }
        .map(D::Display)),
        "-webkit-line-clamp" => push(if v == "none" { Some(D::LineClamp(0)) } else { v.parse().ok().map(D::LineClamp) }),
        "z-index" => {
            if v == "auto" {
                out.push(D::ZIndex(None));
            } else {
                push(v.parse().ok().map(|z| D::ZIndex(Some(z))));
            }
        }
        "grid-template-columns" => {
            let cols = match v.strip_prefix("repeat(") {
                Some(r) => r.split(',').next().and_then(|n| n.trim().parse().ok()).unwrap_or(3),
                None if v == "none" => 1,
                None => tokens(&without_line_names(v)).count().clamp(1, 12),
            };
            let even = v.contains("fr") || v.contains('%') || v.contains("repeat(");
            out.push(D::GridCols(cols as u8, even));
            grid::parse_grid(name, v, out);
        }
        "grid-auto-flow" => {
            out.push(D::GridFlowCol(v.starts_with("column")));
            out.push(D::GridDense(v.contains("dense")));
        }
        "grid-template-rows" => {
            out.push(D::GridRows((v != "subgrid").then(|| Rc::new(row_lines(v)))));
            grid::parse_grid(name, v, out);
        }
        "grid-row" | "grid-row-start" | "grid-area" => {
            out.push(D::GridRow(grid_line(v)));
            grid::parse_grid(name, v, out);
        }
        "grid-template-areas" | "grid-template" | "grid" | "grid-auto-rows" | "grid-auto-columns" | "grid-column" | "grid-row-end"
        | "grid-column-start" | "grid-column-end" | "justify-items" | "justify-self" | "place-items" | "place-self" => grid::parse_grid(name, v, out),
        "order" => push(v.parse().ok().map(D::Order)),
        "position" => push(Some(D::Position(match v {
            "relative" => Position::Relative,
            "absolute" => Position::Absolute,
            "fixed" => Position::Fixed,
            _ => Position::Static,
        }))),
        "width" => push(size(v).map(D::Width)),
        "height" => push(size(v).map(D::Height)),
        "min-width" => push(size(v).map(D::MinWidth)),
        "max-width" => push(size(v).map(D::MaxWidth)),
        "min-height" => push(size(v).map(D::MinHeight)),
        "max-height" => push(size(v).map(D::MaxHeight)),
        "margin" | "padding" | "inset" => {
            if let Some(a) = four(v, parse_len) {
                for (i, l) in a.into_iter().enumerate() {
                    let i = i as u8;
                    out.push(match name {
                        "margin" => D::Margin(i, l),
                        "padding" => D::Padding(i, l),
                        _ => D::Inset(i, l),
                    });
                }
            }
        }
        "top" | "right" | "bottom" | "left" => {
            push(parse_len(v).map(|l| D::Inset(side_of(name).unwrap(), l)))
        }
        "margin-inline" | "margin-block" | "padding-inline" | "padding-block" => {
            let t: Vec<Len> = tokens(v).filter_map(parse_len).collect();
            if let Some(&a) = t.first() {
                let b = *t.get(1).unwrap_or(&a);
                let (s0, s1) = if name.ends_with("inline") { (3, 1) } else { (0, 2) };
                if name.starts_with("margin") {
                    out.push(D::Margin(s0, a));
                    out.push(D::Margin(s1, b));
                } else {
                    out.push(D::Padding(s0, a));
                    out.push(D::Padding(s1, b));
                }
            }
        }
        "border" => border_shorthand(v, 0..4, out),
        "border-width" => {
            if let Some(a) = four(v, border_width) {
                out.extend(a.into_iter().enumerate().map(|(i, l)| D::BorderWidth(i as u8, l)));
            }
        }
        "border-style" => {
            if let Some(a) = four(v, border_style) {
                out.extend(a.into_iter().enumerate().map(|(i, s)| D::BorderStyle(i as u8, s)));
            }
        }
        "border-color" => {
            if let Some(a) = four(v, border_color) {
                out.extend(a.into_iter().enumerate().map(|(i, c)| D::BorderColor(i as u8, c)));
            }
        }
        "border-radius" => {
            // Vertical radii follow a `/`: the horizontal ones stand for both.
            if let Some(a) = four(v.split('/').next().unwrap_or(v), parse_len) {
                out.extend(a.into_iter().enumerate().map(|(i, l)| D::Radius(i as u8, l)));
            }
        }
        "border-top-left-radius" => push(tokens(v).next().and_then(parse_len).map(|l| D::Radius(0, l))),
        "border-top-right-radius" => push(tokens(v).next().and_then(parse_len).map(|l| D::Radius(1, l))),
        "border-bottom-right-radius" => push(tokens(v).next().and_then(parse_len).map(|l| D::Radius(2, l))),
        "border-bottom-left-radius" => push(tokens(v).next().and_then(parse_len).map(|l| D::Radius(3, l))),
        "box-sizing" => push(Some(D::BoxSizing(v == "border-box"))),
        "box-shadow" => push(box_shadow(v).map(D::BoxShadow)),
        "background" | "background-color" | "background-image" => {
            let (mut color, mut grad, mut none, mut url) = (None, None, false, None);
            let (mut pos, mut size, mut after_slash) = (Vec::new(), None, false);
            for t in tokens(v) {
                if t.starts_with("linear-gradient(") {
                    // Of several layers the last, painted under the others, stands for them all.
                    grad = SplitTop::new(t, b',').filter_map(|l| parse_gradient(l.trim())).last().map(Rc::new);
                } else if let Some(u) = css_url(t.trim_end_matches(',')) {
                    url = url.or(Some(Rc::from(u.trim())));
                } else if t == "none" {
                    none = true;
                } else if let Some(c) = color_tok(t) {
                    color = Some(c);
                } else if name == "background" {
                    // Position, then `/ size`, written with or without spaces around the slash.
                    let (p, s) = t.split_once('/').unwrap_or(if after_slash { ("", t) } else { (t, "") });
                    after_slash = t.ends_with('/');
                    pos.extend((!p.is_empty() && is_position(p)).then_some(p));
                    size = size.or_else(|| bg_size(s));
                }
            }
            if name != "background-image" && (color.is_some() || name == "background") {
                out.push(D::Background(color.unwrap_or(Color::TRANSPARENT)));
            }
            if name != "background-color" && (grad.is_some() || none || name == "background") {
                out.push(D::Gradient(grad));
            }
            if name != "background-color" && (url.is_some() || none || name == "background") {
                out.push(D::BgImage(url));
            }
            if name == "background" {
                let (x, y) = if pos.is_empty() { (Len::Pct(0.0), Len::Pct(0.0)) } else { bg_position(&pos).unwrap_or((Len::Pct(0.0), Len::Pct(0.0))) };
                out.push(D::BgPos(x, y));
                out.push(D::BgSize(size.unwrap_or(BgSize::Auto)));
            }
        }
        "background-size" => push(bg_size(v).map(D::BgSize)),
        "background-clip" | "-webkit-background-clip" => push(Some(D::ClipText(SplitTop::new(v, b',').any(|l| l.trim() == "text")))),
        "background-position" => push(bg_position(&tokens(v).collect::<Vec<_>>()).map(|(x, y)| D::BgPos(x, y))),
        "color" => {
            if v == "inherit" {
                out.push(D::Color(None));
            } else {
                push(color_tok(v).map(|c| D::Color(Some(c))));
            }
        }
        "font-size" => push(font_size(v).map(D::FontSize)),
        "font-weight" => push(font_weight(v).map(D::Weight)),
        "font-style" => push(Some(D::Italic(v == "italic" || v == "oblique"))),
        "font-family" => push(Some(D::Family(font_family(v), v.into()))),
        "line-height" => push(line_height(v).map(D::LineHeight)),
        "font" => {
            let toks: Vec<&str> = tokens(v).collect();
            let (mut weight, mut italic) = (FontWeight::Abs(400), false);
            for (i, t) in toks.iter().enumerate() {
                match *t {
                    "italic" | "oblique" => italic = true,
                    "normal" | "small-caps" => {}
                    _ => {
                        if let Some(w) = font_weight(t) {
                            weight = w;
                            continue;
                        }
                        let (sz, lh) = t.split_once('/').unwrap_or((t, ""));
                        let Some(fs) = font_size(sz) else { return };
                        out.push(D::Italic(italic));
                        out.push(D::Weight(weight));
                        out.push(D::FontSize(fs));
                        out.push(D::LineHeight(line_height(lh).unwrap_or(LineHeight::Normal)));
                        if i + 1 < toks.len() {
                            let list = toks[i + 1..].join(" ");
                            out.push(D::Family(font_family(&list), list.into()));
                        }
                        return;
                    }
                }
            }
        }
        "text-align" => push(Some(D::TextAlign(match v {
            "center" => TextAlign::Center,
            "right" | "end" => TextAlign::Right,
            _ => TextAlign::Left,
        }))),
        "text-decoration" | "text-decoration-line" => {
            let mut d = 0;
            for t in tokens(v) {
                match t {
                    "underline" => d |= DECO_UNDERLINE,
                    "line-through" => d |= DECO_LINE_THROUGH,
                    _ => {}
                }
            }
            out.push(D::TextDeco(d));
        }
        "white-space" => push(Some(D::WhiteSpace(match v {
            "nowrap" => WhiteSpace::NoWrap,
            "pre" => WhiteSpace::Pre,
            "pre-wrap" | "break-spaces" => WhiteSpace::PreWrap,
            _ => WhiteSpace::Normal,
        }))),
        "text-transform" => push(Some(D::TextTransform(match v {
            "uppercase" => TextTransform::Upper,
            "lowercase" => TextTransform::Lower,
            "capitalize" => TextTransform::Capitalize,
            _ => TextTransform::None,
        }))),
        "letter-spacing" => {
            push(if v == "normal" { Some(Len::Px(0.0)) } else { parse_len(v) }.map(D::LetterSpacing))
        }
        "list-style" | "list-style-type" => {
            for t in tokens(v) {
                let ls = match t {
                    "none" => ListStyle::None,
                    "disc" => ListStyle::Disc,
                    "circle" => ListStyle::Circle,
                    "square" => ListStyle::Square,
                    "decimal" | "decimal-leading-zero" | "lower-alpha" | "upper-alpha" | "lower-roman" | "upper-roman" | "lower-latin" | "upper-latin" => ListStyle::Decimal,
                    _ => continue,
                };
                out.push(D::ListStyle(ls));
            }
        }
        "flex-direction" => push(Some(D::FlexDir(v.starts_with("column")))),
        "flex-wrap" => push(Some(D::FlexWrap(v.starts_with("wrap")))),
        "flex-flow" => {
            for t in tokens(v) {
                match t {
                    "row" | "row-reverse" => out.push(D::FlexDir(false)),
                    "column" | "column-reverse" => out.push(D::FlexDir(true)),
                    "wrap" | "wrap-reverse" => out.push(D::FlexWrap(true)),
                    "nowrap" => out.push(D::FlexWrap(false)),
                    _ => {}
                }
            }
        }
        "justify-content" => push(Some(D::Justify(match v {
            "center" => Justify::Center,
            "flex-end" | "end" | "right" => Justify::End,
            "space-between" => Justify::Between,
            "space-around" => Justify::Around,
            "space-evenly" => Justify::Evenly,
            _ => Justify::Start,
        }))),
        "align-items" => push(align(v).map(D::AlignItems)),
        "align-self" => push(align(v).map(D::AlignSelf)),
        "flex-grow" => push(num(v).map(D::Grow)),
        "flex-shrink" => push(num(v).map(D::Shrink)),
        "flex-basis" => push(size(v).map(D::Basis)),
        "flex" => {
            let (mut g, mut s, mut b) = (0.0, 1.0, Len::Auto);
            match v {
                "none" => s = 0.0,
                "auto" => g = 1.0,
                "initial" => {}
                _ => {
                    let mut nums = 0;
                    b = Len::Px(0.0);
                    for t in tokens(v) {
                        if let Ok(n) = t.parse::<f32>() {
                            if nums == 0 {
                                g = n;
                            } else {
                                s = n;
                            }
                            nums += 1;
                        } else if let Some(l) = size(t) {
                            b = l;
                        }
                    }
                }
            }
            out.push(D::Grow(g));
            out.push(D::Shrink(s));
            out.push(D::Basis(b));
        }
        "gap" | "grid-gap" => {
            let t: Vec<Len> = tokens(v).filter_map(parse_len).collect();
            if let Some(&r) = t.first() {
                out.push(D::RowGap(r));
                out.push(D::ColGap(*t.get(1).unwrap_or(&r)));
            }
        }
        "row-gap" => push(parse_len(v).map(D::RowGap)),
        "column-gap" => push(parse_len(v).map(D::ColGap)),
        "overflow" | "overflow-x" | "overflow-y" => {
            push(Some(D::Overflow(tokens(v).any(|t| t != "visible"))))
        }
        "all" => push(match v {
            "unset" => Some(D::AllUnset),
            "revert" | "revert-layer" => Some(D::AllRevert),
            _ => None,
        }),
        "visibility" => push(match v {
            "visible" | "initial" => Some(D::Visible(Some(true))),
            "hidden" | "collapse" => Some(D::Visible(Some(false))),
            // An inherited property: unset and revert take the parent's value too.
            "inherit" | "unset" | "revert" | "revert-layer" => Some(D::Visible(None)),
            _ => None,
        }),
        "opacity" => push(
            match v.strip_suffix('%') {
                Some(p) => num(p).map(|n| n / 100.0),
                None => num(v),
            }
            .map(|o| D::Opacity(o.clamp(0.0, 1.0))),
        ),
        "cursor" => push(Some(D::Pointer(v == "pointer"))),
        "transform" => {
            let (x, y) = parse_translate(v);
            out.push(D::Translate(x, y));
            out.push(D::ScaledAway(scaled_away(v)));
        }
        "clip" => out.push(D::CutAway(0, clip_is_empty(v))),
        "clip-path" | "-webkit-clip-path" => out.push(D::CutAway(1, inset_is_empty(v))),
        "fill" => match v {
            "none" => out.push(D::Fill(Some(Color::TRANSPARENT))),
            "currentcolor" => out.push(D::Fill(None)),
            _ => push(color_tok(v).map(|c| D::Fill(Some(c)))),
        },
        _ => {
            let Some(rest) = name.strip_prefix("margin-") else {
                if let Some(rest) = name.strip_prefix("padding-") {
                    if let (Some(i), Some(l)) = (side_of(rest), parse_len(v)) {
                        out.push(D::Padding(i, l));
                    }
                } else if let Some(rest) = name.strip_prefix("border-") {
                    if let Some(i) = side_of(rest) {
                        border_shorthand(v, i..i + 1, out);
                    } else if let Some((side, prop)) = rest.split_once('-') {
                        let Some(i) = side_of(side) else { return };
                        match prop {
                            "width" => push(border_width(v).map(|l| D::BorderWidth(i, l))),
                            "style" => push(border_style(v).map(|s| D::BorderStyle(i, s))),
                            "color" => push(border_color(v).map(|c| D::BorderColor(i, c))),
                            _ => {}
                        }
                    }
                }
                return;
            };
            if let (Some(i), Some(l)) = (side_of(rest), parse_len(v)) {
                out.push(D::Margin(i, l));
            }
        }
    }
}

/// Units whose size is known only while styling an element.
const CONTEXT_UNITS: [&str; 6] = ["vw", "vh", "vmin", "vmax", "rem", "em"];

/// True when a function such as `calc()` or `min()` combines lengths in units
/// known only while styling, which it cannot add up until then.
fn needs_context(val: &str) -> bool {
    val.contains('(') && absolute_lengths(val, |u| CONTEXT_UNITS.contains(&viewport_unit(u)).then_some(1.0)).is_some()
}

/// `v` with every length whose unit `px_per` gives a size for rewritten in px,
/// or `None` when there is no such length.
pub fn absolute_lengths(v: &str, px_per: impl Fn(&str) -> Option<f32>) -> Option<String> {
    let b = v.as_bytes();
    let word = |i: usize| b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'#' | b'.');
    let (mut out, mut copied, mut i) = (String::new(), 0, 0);
    while i < b.len() {
        // A number starts after a separator or a minus sign that is not part of a name.
        let after_minus = i > 0 && b[i - 1] == b'-' && (i < 2 || !(word(i - 2) || b[i - 2] == b'-'));
        let starts = (b[i].is_ascii_digit() || b[i] == b'.') && (i == 0 || !(word(i - 1) || b[i - 1] == b'-') || after_minus);
        if !starts {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
            j += 1;
        }
        let mut k = j;
        while k < b.len() && b[k].is_ascii_alphabetic() {
            k += 1;
        }
        match (v[i..j].parse::<f32>(), px_per(&v[j..k])) {
            (Ok(n), Some(f)) if k == b.len() || !(b[k].is_ascii_digit() || b[k] == b'%') => {
                out.push_str(&v[copied..i]);
                out.push_str(&format!("{}px", n * f));
                copied = k;
            }
            _ => {}
        }
        i = k.max(i + 1);
    }
    (copied > 0).then(|| out + &v[copied..])
}

/// Parses a declaration block into (normal, !important, uses custom properties).
pub fn parse_declarations(body: &str) -> (Vec<Decl>, Vec<Decl>, bool) {
    let (mut normal, mut imp, mut dynamic) = (Vec::new(), Vec::new(), false);
    for piece in SplitTop::new(body, b';') {
        let Some(colon) = piece.find(':') else { continue };
        let name = piece[..colon].trim();
        let mut val = piece[colon + 1..].trim();
        if name.is_empty() || val.is_empty() {
            continue;
        }
        let mut important = false;
        if let Some(bang) = val.rfind('!') {
            if val[bang + 1..].trim().eq_ignore_ascii_case("important") {
                important = true;
                val = val[..bang].trim_end();
            }
        }
        let out = if important { &mut imp } else { &mut normal };
        if name.starts_with("--") {
            out.push(Decl::Var(name.into(), val.into()));
            dynamic = true;
            continue;
        }
        let lname: Cow<str> = if name.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(name.to_ascii_lowercase())
        } else {
            Cow::Borrowed(name)
        };
        if val.contains("var(") || needs_context(val) {
            out.push(Decl::Deferred(lname.into(), val.into()));
            dynamic = true;
        } else {
            let before = out.len();
            parse_decl(&lname, val, out);
            if out.len() == before {
                drops::note("decl", &lname);
            }
        }
    }
    (normal, imp, dynamic)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Comb {
    None,
    Descendant,
    Child,
    Adjacent,
    Sibling,
}

#[derive(Debug)]
pub enum AttrOp {
    Exists,
    Eq,
    Includes,
    Dash,
    Prefix,
    Suffix,
    Substr,
}

#[derive(Debug)]
pub struct AttrSel {
    pub name: Atom,
    pub op: AttrOp,
    pub value: Box<str>,
}

#[derive(Debug)]
pub enum Pseudo {
    Root,
    FirstChild,
    LastChild,
    OnlyChild,
    NthChild(i32, i32),
    NthLastChild(i32, i32),
    FirstOfType,
    LastOfType,
    NthOfType(i32, i32),
    /// `:not()`: matches when none of the selectors matches the element.
    Not(Box<[Vec<Part>]>),
    /// `:is()` or `:where()`: matches when one of the selectors matches the element.
    Is(Box<[Vec<Part>]>),
    /// Matches when some descendant matches the selector.
    Has(Vec<Part>),
    Hover,
    Link,
    Empty,
    /// `:defined`: not a custom element, or one whose name is defined.
    Defined,
    /// `:modal`: a dialog `showModal()` opened.
    Modal,
    /// Unsupported pseudo-class or pseudo-element: never matches.
    Never,
}

#[derive(Default, Debug)]
pub struct Compound {
    /// 0 matches any tag.
    pub tag: Atom,
    /// 0 means no id constraint.
    pub id: Atom,
    pub classes: Vec<Atom>,
    pub attrs: Vec<AttrSel>,
    pub pseudos: Vec<Pseudo>,
    /// The pseudo-element a subject compound selects: 0, `BEFORE` or `AFTER`.
    pub elem: u8,
}

/// `::before`, the box generated as an element's first child.
pub const BEFORE: u8 = 1;
/// `::after`, the box generated as an element's last child.
pub const AFTER: u8 = 2;

/// One compound of a complex selector plus the combinator joining it to the
/// compound on its left.
#[derive(Debug)]
pub struct Part {
    pub c: Compound,
    pub comb: Comb,
}

/// The character a CSS escape stands for (CSS Syntax 4.3.7), its backslash just
/// before `s[i..]`, and where the text after it starts. One to six hex digits name
/// a code point, and one whitespace after them belongs to the escape; any other
/// character stands for itself.
fn escape(s: &str, i: usize) -> (char, usize) {
    let b = s.as_bytes();
    let hex = b[i..].iter().take(6).take_while(|c| c.is_ascii_hexdigit()).count();
    if hex == 0 {
        return s[i..].chars().next().map_or(('\u{FFFD}', i), |c| (c, i + c.len_utf8()));
    }
    let mut end = i + hex;
    if b[end..].starts_with(b"\r\n") {
        end += 2;
    } else if end < b.len() && matches!(b[end], b' ' | b'\t' | b'\n' | b'\r' | 0x0c) {
        end += 1;
    }
    let code = u32::from_str_radix(&s[i..i + hex], 16).unwrap_or(0);
    (char::from_u32(code).filter(|&c| c != '\0').unwrap_or('\u{FFFD}'), end)
}

/// `s` with its CSS escapes decoded; a backslash before a newline, which
/// continues a string on the next line, is dropped with the newline.
fn unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('\\') {
        return Cow::Borrowed(s);
    }
    let b = s.as_bytes();
    let (mut out, mut i) = (String::with_capacity(s.len()), 0);
    while let Some(p) = memchr::memchr(b'\\', &b[i..]) {
        out.push_str(&s[i..i + p]);
        i += p + 1;
        match b.get(i) {
            None => {}
            Some(b'\r') if b.get(i + 1) == Some(&b'\n') => i += 2,
            Some(b'\n' | b'\r' | 0x0c) => i += 1,
            Some(_) => {
                let (c, end) = escape(s, i);
                out.push(c);
                i = end;
            }
        }
    }
    out.push_str(&s[i..]);
    Cow::Owned(out)
}

/// The value of a CSS string such as `"a\"b"`, or of other text, with its
/// escapes decoded.
fn unquote(t: &str) -> Cow<'_, str> {
    match t.as_bytes().first() {
        Some(&q @ (b'"' | b'\'')) => {
            let body = &t[1..];
            let end = string_end(body.as_bytes(), 0, q);
            unescape(&body[..end.min(body.len())])
        }
        _ => unescape(t),
    }
}

/// Where the string closed by the quote `q` ends in `b`, reading from `i`: the
/// index of that quote, or `b.len()` when it is missing. Escaped quotes are skipped.
fn string_end(b: &[u8], mut i: usize, q: u8) -> usize {
    while i < b.len() && b[i] != q {
        i += 1 + (b[i] == b'\\') as usize;
    }
    i.min(b.len())
}

/// Reads a name at `*i`, the CSS escapes in it decoded, so `lg\:flex` reads as
/// `lg:flex`.
fn ident<'s>(s: &'s str, i: &mut usize) -> Cow<'s, str> {
    let b = s.as_bytes();
    let start = *i;
    let (mut out, mut copied) = (String::new(), start);
    while *i < b.len() {
        match b[*i] {
            c if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80 => *i += 1,
            b'\\' if b.get(*i + 1).is_some_and(|&c| !matches!(c, b'\n' | b'\r' | 0x0c)) => {
                out.push_str(&s[copied..*i]);
                let (c, end) = escape(s, *i + 1);
                out.push(c);
                (*i, copied) = (end, end);
            }
            _ => break,
        }
    }
    if copied == start {
        return Cow::Borrowed(&s[start..*i]);
    }
    out.push_str(&s[copied..*i]);
    Cow::Owned(out)
}

/// Where the bracket or parenthesis opened at `b[open]` closes, past nested
/// ones, strings and escapes: the index of the closing byte.
fn block_end(b: &[u8], open: usize) -> Option<usize> {
    let (o, c) = (b[open], if b[open] == b'[' { b']' } else { b')' });
    let (mut depth, mut i) = (0u32, open);
    while i < b.len() {
        match b[i] {
            x if x == o => depth += 1,
            x if x == c => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'\\' => i += 1,
            q @ (b'"' | b'\'') => i = string_end(b, i + 1, q),
            _ => {}
        }
        i += 1;
    }
    None
}

fn parse_nth(arg: &str) -> Option<(i32, i32)> {
    let a: String = arg.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    match a.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    match a.split_once('n') {
        Some((x, y)) => {
            let x = match x {
                "" | "+" => 1,
                "-" => -1,
                _ => x.parse().ok()?,
            };
            let y = if y.is_empty() { 0 } else { y.parse().ok()? };
            Some((x, y))
        }
        None => Some((0, a.parse().ok()?)),
    }
}

/// An attribute selector from the text between its brackets: `name`, or
/// `name op value` where the value is a name or a string, then a case flag.
fn parse_attr_sel(inner: &str, atoms: &mut Interner) -> Option<AttrSel> {
    let b = inner.as_bytes();
    let skip_ws = |i: &mut usize| {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
    };
    let mut i = 0;
    skip_ws(&mut i);
    let name = ident(inner, &mut i);
    if name.is_empty() {
        return None;
    }
    let name = atoms.intern_lower(&name);
    skip_ws(&mut i);
    if i == b.len() {
        return Some(AttrSel { name, op: AttrOp::Exists, value: "".into() });
    }
    let op = match (b[i], b.get(i + 1)) {
        (b'=', _) => AttrOp::Eq,
        (b'~', Some(b'=')) => AttrOp::Includes,
        (b'|', Some(b'=')) => AttrOp::Dash,
        (b'^', Some(b'=')) => AttrOp::Prefix,
        (b'$', Some(b'=')) => AttrOp::Suffix,
        (b'*', Some(b'=')) => AttrOp::Substr,
        _ => return None,
    };
    i += if matches!(op, AttrOp::Eq) { 1 } else { 2 };
    skip_ws(&mut i);
    let value = match b.get(i) {
        Some(&q @ (b'"' | b'\'')) => {
            let end = string_end(b, i + 1, q);
            let v = unescape(&inner[i + 1..end]);
            i = (end + 1).min(b.len());
            v
        }
        _ => ident(inner, &mut i),
    };
    skip_ws(&mut i);
    if i < b.len() && matches!(b[i] | 0x20, b'i' | b's') {
        i += 1;
        skip_ws(&mut i);
    }
    (i == b.len()).then(|| AttrSel { name, op, value: value.into() })
}

fn parse_compound(s: &str, i: &mut usize, atoms: &mut Interner, hover: &mut bool, spec: &mut u32) -> Option<Compound> {
    let b = s.as_bytes();
    let mut c = Compound::default();
    let start = *i;
    while *i < b.len() {
        match b[*i] {
            b'*' => *i += 1,
            b'#' => {
                *i += 1;
                let name = ident(s, i);
                if name.is_empty() {
                    return None;
                }
                c.id = atoms.intern(&name);
                *spec += 0x10000;
            }
            b'.' => {
                *i += 1;
                let name = ident(s, i);
                if name.is_empty() {
                    return None;
                }
                c.classes.push(atoms.intern(&name));
                *spec += 0x100;
            }
            b'[' => {
                let end = block_end(b, *i)?;
                c.attrs.push(parse_attr_sel(&s[*i + 1..end], atoms)?);
                *i = end + 1;
                *spec += 0x100;
            }
            b':' => {
                *i += 1;
                let element = *i < b.len() && b[*i] == b':';
                *i += element as usize;
                let name = ident(s, i).to_ascii_lowercase();
                // CSS 2 wrote the oldest pseudo-elements with one colon.
                if element || matches!(name.as_str(), "before" | "after" | "first-line" | "first-letter") {
                    match name.as_str() {
                        "before" => c.elem = BEFORE,
                        "after" => c.elem = AFTER,
                        _ => {
                            drops::note("pseudo-element", &name);
                            c.pseudos.push(Pseudo::Never)
                        }
                    }
                    if *i < b.len() && b[*i] == b'(' {
                        *i = block_end(b, *i)? + 1;
                    }
                    *spec += 1;
                    continue;
                }
                let mut arg = "";
                if *i < b.len() && b[*i] == b'(' {
                    let close = block_end(b, *i)?;
                    arg = &s[*i + 1..close];
                    *i = close + 1;
                }
                // A pseudo-class taking selectors counts as its most specific one; `:where()` counts nothing.
                if !matches!(name.as_str(), "is" | "matches" | "-webkit-any" | "where" | "not") {
                    *spec += 0x100;
                }
                c.pseudos.push(match name.as_str() {
                    "root" => Pseudo::Root,
                    "first-child" => Pseudo::FirstChild,
                    "last-child" => Pseudo::LastChild,
                    "only-child" => Pseudo::OnlyChild,
                    "first-of-type" => Pseudo::FirstOfType,
                    "last-of-type" => Pseudo::LastOfType,
                    "nth-child" => parse_nth(arg).map_or(Pseudo::Never, |(a, b)| Pseudo::NthChild(a, b)),
                    "nth-last-child" => parse_nth(arg).map_or(Pseudo::Never, |(a, b)| Pseudo::NthLastChild(a, b)),
                    "nth-of-type" => parse_nth(arg).map_or(Pseudo::Never, |(a, b)| Pseudo::NthOfType(a, b)),
                    "is" | "matches" | "-webkit-any" | "where" => match selector_args(arg, atoms, hover, true) {
                        Some((list, most)) => {
                            *spec += if name == "where" { 0 } else { most };
                            Pseudo::Is(list)
                        }
                        None => Pseudo::Never,
                    },
                    "not" => match selector_args(arg, atoms, hover, false) {
                        Some((list, most)) => {
                            *spec += most;
                            Pseudo::Not(list)
                        }
                        None => Pseudo::Never,
                    },
                    "has" => parse_selector(arg.trim(), atoms, hover).filter(|(parts, _)| parts[0].c.elem == 0).map_or(Pseudo::Never, |(parts, _)| Pseudo::Has(parts)),
                    "hover" => {
                        *hover = true;
                        Pseudo::Hover
                    }
                    "link" | "any-link" => Pseudo::Link,
                    "empty" => Pseudo::Empty,
                    "defined" => Pseudo::Defined,
                    "modal" => Pseudo::Modal,
                    _ => {
                        drops::note("pseudo-class", &name);
                        Pseudo::Never
                    }
                });
            }
            ch if ch.is_ascii_alphabetic() || matches!(ch, b'_' | b'-' | b'\\') || ch >= 0x80 => {
                if *i != start {
                    return None;
                }
                let name = ident(s, i);
                if name.is_empty() {
                    return None;
                }
                c.tag = atoms.intern_lower(&name);
                *spec += 1;
            }
            _ => break,
        }
    }
    if *i == start {
        None
    } else {
        Some(c)
    }
}

/// The selectors in the argument of `:is()`, `:where()` or `:not()`, and the
/// specificity of the most specific one. A selector that cannot be read makes the
/// whole argument invalid, unless the list is `forgiving` and drops it instead.
fn selector_args(arg: &str, atoms: &mut Interner, hover: &mut bool, forgiving: bool) -> Option<(Box<[Vec<Part>]>, u32)> {
    let (mut list, mut most) = (Vec::new(), 0);
    for sel in SplitTop::new(arg, b',') {
        match parse_selector(sel.trim(), atoms, hover) {
            Some((parts, spec)) if parts[0].c.elem == 0 => {
                most = most.max(spec);
                list.push(parts);
            }
            _ if forgiving => {}
            _ => return None,
        }
    }
    (!list.is_empty()).then(|| (list.into_boxed_slice(), most))
}

/// Returns the compounds right to left, so `parts[0]` is the subject.
fn parse_selector(s: &str, atoms: &mut Interner, hover: &mut bool) -> Option<(Vec<Part>, u32)> {
    let b = s.as_bytes();
    let (mut i, mut spec, mut comb) = (0, 0, Comb::None);
    let mut parts: Vec<Part> = Vec::new();
    loop {
        let mut ws = false;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
            ws = true;
        }
        if i >= b.len() {
            break;
        }
        let explicit = match b[i] {
            b'>' => Some(Comb::Child),
            b'+' => Some(Comb::Adjacent),
            b'~' => Some(Comb::Sibling),
            _ => None,
        };
        if let Some(c) = explicit {
            comb = c;
            i += 1;
            continue;
        }
        if !parts.is_empty() && comb == Comb::None {
            if !ws {
                return None;
            }
            comb = Comb::Descendant;
        }
        let c = parse_compound(s, &mut i, atoms, hover, &mut spec)?;
        parts.push(Part { c, comb });
        comb = Comb::None;
    }
    if parts.is_empty() || comb != Comb::None {
        return None;
    }
    parts.reverse();
    // A pseudo-element ends a selector.
    if parts[1..].iter().any(|p| p.c.elem != 0) {
        return None;
    }
    Some((parts, spec))
}

/// Parses a comma-separated selector list for the DOM, dropping invalid
/// selectors and those of pseudo-elements, which select no element.
pub fn parse_selector_list(s: &str, atoms: &mut Interner) -> Vec<Vec<Part>> {
    let mut hover = false;
    let parsed = SplitTop::new(s, b',').filter_map(|sel| parse_selector(sel.trim(), atoms, &mut hover));
    parsed.filter(|(parts, _)| parts[0].c.elem == 0).map(|(parts, _)| parts).collect()
}

/// False for a compound with a pseudo-class or pseudo-element this engine does
/// not know, at any depth.
fn supported(c: &Compound) -> bool {
    c.pseudos.iter().all(|p| match p {
        Pseudo::Never => false,
        Pseudo::Not(list) | Pseudo::Is(list) => list.iter().all(|parts| parts.iter().all(|p| supported(&p.c))),
        Pseudo::Has(parts) => parts.iter().all(|p| supported(&p.c)),
        _ => true,
    })
}

/// Whether an `@supports` condition holds for this engine: a declaration when it
/// is parsed into something, as custom properties and `var()` values always are,
/// and `selector()` when its selector is read and supported.
fn supports(cond: &str, atoms: &mut Interner) -> bool {
    let mut leaf = |t: &str| {
        let holds = if let Some(sel) = t.strip_prefix("selector(").and_then(|r| r.strip_suffix(')')) {
            let mut hover = false;
            parse_selector(sel.trim(), atoms, &mut hover).is_some_and(|(parts, _)| parts.iter().all(|p| supported(&p.c)))
        } else if let Some((name, value)) = t.split_once(':') {
            let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
            let mut out = Vec::new();
            if !name.starts_with("--") && !value.contains("var(") {
                parse_decl(&name, value, &mut out);
            }
            name.starts_with("--") || value.contains("var(") || !out.is_empty()
        } else {
            false
        };
        crate::media::Cond::Is(holds)
    };
    crate::media::condition(cond, &mut leaf, true).is_some_and(|c| c.eval(0.0, 0.0))
}

pub struct Rule {
    pub decls: Vec<Decl>,
    pub important: Vec<Decl>,
    /// Index into `Sheet::media`, or `u32::MAX` when unconditional.
    pub media: u32,
    pub dynamic: bool,
}

pub struct SelEntry {
    pub parts: Vec<Part>,
    pub spec: u32,
    pub rule: u32,
    /// 0 for the built-in sheet, 1 for author sheets.
    pub level: u8,
    /// Bloom slots of names that must appear on some ancestor of the subject.
    pub anc: [u16; 4],
    pub anc_n: u8,
    /// The pseudo-element of the subject: 0, `BEFORE` or `AFTER`.
    pub elem: u8,
}

/// Slot of an atom in the 4096-entry ancestor filter.
#[inline]
pub fn bloom_slot(a: Atom) -> u16 {
    (a.wrapping_mul(0x9E37_79B1) >> 20) as u16
}

/// Names required on ancestors: every compound reached from its right-hand
/// neighbour through a child or descendant combinator.
fn ancestor_slots(parts: &[Part]) -> ([u16; 4], u8) {
    let (mut anc, mut n) = ([0u16; 4], 0usize);
    for k in 1..parts.len() {
        if !matches!(parts[k - 1].comb, Comb::Child | Comb::Descendant) {
            continue;
        }
        let c = &parts[k].c;
        for a in [c.id].into_iter().chain(c.classes.iter().copied()).chain([c.tag]) {
            if a != 0 && n < anc.len() {
                anc[n] = bloom_slot(a);
                n += 1;
            }
        }
    }
    (anc, n as u8)
}

/// All rules of a page, with selectors bucketed by the id, class or tag of
/// their subject compound so an element only tests plausible candidates.
#[derive(Default)]
pub struct Sheet {
    pub rules: Vec<Rule>,
    pub sels: Vec<SelEntry>,
    pub media: Vec<Media>,
    pub by_id: FxMap<Atom, Vec<u32>>,
    pub by_class: FxMap<Atom, Vec<u32>>,
    pub by_tag: FxMap<Atom, Vec<u32>>,
    pub universal: Vec<u32>,
    pub uses_hover: bool,
    /// Some selector is of `::before` or `::after`.
    pub generates: bool,
    /// `@font-face` rules in document order.
    pub fonts: Vec<FontFace>,
}

fn skip_block(b: &[u8], mut i: usize) -> usize {
    let mut depth = 1;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            q @ (b'"' | b'\'') => i = string_end(b, i + 1, q),
            b'\\' => i += 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

fn strip_comments(css: &str) -> Cow<'_, str> {
    if !css.contains("/*") {
        return Cow::Borrowed(css);
    }
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(p) = rest.find("/*") {
        out.push_str(&rest[..p]);
        out.push(' ');
        rest = match rest[p + 2..].find("*/") {
            Some(e) => &rest[p + 2 + e + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    Cow::Owned(out)
}

impl Sheet {
    /// Adds the rules of a stylesheet. `base` is where it was loaded from, which
    /// its relative `url()`s resolve against; empty when that is the page itself.
    pub fn add(&mut self, css: &str, atoms: &mut Interner, level: u8, base: &str) {
        let css = strip_comments(css);
        let css = rebase_urls(&css, base);
        let mut i = 0;
        self.rules_in(&css, &mut i, u32::MAX, level, atoms, false);
    }

    fn rules_in(&mut self, s: &str, i: &mut usize, media: u32, level: u8, atoms: &mut Interner, nested: bool) {
        let b = s.as_bytes();
        loop {
            while *i < b.len() && b[*i].is_ascii_whitespace() {
                *i += 1;
            }
            if *i >= b.len() {
                return;
            }
            if b[*i] == b'}' {
                *i += 1;
                if nested {
                    return;
                }
                continue;
            }
            let start = *i;
            if b[*i] == b'@' {
                while *i < b.len() && b[*i] != b'{' && b[*i] != b';' {
                    *i += 1;
                }
                if *i >= b.len() {
                    return;
                }
                let prelude = &s[start..*i];
                *i += 1;
                if b[*i - 1] == b';' {
                    continue;
                }
                if let Some(q) = prelude.strip_prefix("@media") {
                    let own = Media::parse(q);
                    let m = match self.media.get(media as usize) {
                        Some(outer) => own.within(outer),
                        None => own,
                    };
                    self.media.push(m);
                    let idx = self.media.len() as u32 - 1;
                    self.rules_in(s, i, idx, level, atoms, true);
                } else if let Some(cond) = prelude.strip_prefix("@supports") {
                    if supports(cond, atoms) {
                        self.rules_in(s, i, media, level, atoms, true);
                    } else {
                        *i = skip_block(b, *i);
                    }
                } else if prelude.starts_with("@layer") {
                    self.rules_in(s, i, media, level, atoms, true);
                } else if prelude.trim_end().eq_ignore_ascii_case("@font-face") {
                    let open = *i;
                    *i = skip_block(b, open);
                    self.fonts.extend(font_face(&s[open..(*i - 1).max(open)]));
                } else {
                    drops::note("at-rule", prelude.split(|c: char| c.is_ascii_whitespace() || c == '(').next().unwrap_or(prelude));
                    *i = skip_block(b, *i);
                }
                continue;
            }
            let Some(open) = memchr::memchr(b'{', &b[*i..]) else {
                *i = b.len();
                return;
            };
            let open = *i + open;
            let end = skip_block(b, open + 1);
            *i = end;
            let body = &s[open + 1..(end - 1).max(open + 1)];
            let (decls, important, dynamic) = parse_declarations(body);
            if decls.is_empty() && important.is_empty() {
                continue;
            }
            let rule = self.rules.len() as u32;
            let before = self.sels.len();
            for sel in SplitTop::new(&s[start..open], b',') {
                if let Some((parts, spec)) = parse_selector(sel.trim(), atoms, &mut self.uses_hover) {
                    let (anc, anc_n) = ancestor_slots(&parts);
                    let elem = parts[0].c.elem;
                    self.generates |= elem != 0;
                    self.sels.push(SelEntry { parts, spec, rule, level, anc, anc_n, elem });
                }
            }
            if self.sels.len() > before {
                self.rules.push(Rule { decls, important, media, dynamic });
            }
        }
    }

    /// Builds the selector buckets. Call once after the last `add`.
    pub fn finish(&mut self) {
        for (i, sel) in self.sels.iter().enumerate() {
            let c = &sel.parts[0].c;
            let i = i as u32;
            if c.id != 0 {
                self.by_id.entry(c.id).or_default().push(i);
            } else if let Some(&cl) = c.classes.first() {
                self.by_class.entry(cl).or_default().push(i);
            } else if c.tag != 0 {
                self.by_tag.entry(c.tag).or_default().push(i);
            } else {
                self.universal.push(i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FontWeight, Sheet};
    use crate::dom::Interner;

    #[test]
    fn font_face_rules_are_collected_with_resolved_sources() {
        let css = "@font-face{font-family:'SF Pro Text';font-style:normal;font-weight:100 900;\
                   src:local('SF Pro'),url('/wss/a.woff2') format('woff2'),url(\"a.ttf\") format(\"truetype\");unicode-range:U+0-FF}\
                   @media(min-width:1px){@font-face{font-family:Fira;font-style:italic;font-weight:bold;src:url(f.woff)}}\
                   @font-face{font-family:NoSource}\
                   @font-face{font-family:Sub;src:url(s.woff2);unicode-range:U+0400-045F, u+4??,U+20AC}\
                   @font-face{font-family:Bad;src:url(b.woff2);unicode-range:U+41,U+5A-41}\
                   p{font-weight:600}";
        let mut sheet = Sheet::default();
        sheet.add(css, &mut Interner::new(), 1, "https://example.com/css/site.css");
        let faces: Vec<_> = sheet.fonts.iter().map(|f| (&*f.family, f.weight, f.italic, f.srcs.len())).collect();
        assert_eq!(faces, [("sf pro text", (100, 900), false, 2), ("fira", (700, 700), true, 1), ("sub", (400, 400), false, 1), ("bad", (400, 400), false, 1)]);
        let ranges: Vec<_> = sheet.fonts.iter().map(|f| &f.ranges[..]).collect();
        assert_eq!(ranges, [&[(0, 0xff)][..], &[], &[(0x400, 0x45f), (0x400, 0x4ff), (0x20ac, 0x20ac)], &[]]);
        let srcs: Vec<_> = sheet.fonts[0].srcs.iter().map(|(u, f)| (&**u, f.as_deref())).collect();
        assert_eq!(srcs, [("/wss/a.woff2", Some("woff2")), ("https://example.com/css/a.ttf", Some("truetype"))]);
        assert_eq!(&*sheet.fonts[1].srcs[0].0, "https://example.com/css/f.woff");
        assert_eq!(sheet.rules.len(), 1);
    }

    #[test]
    fn selectors_and_strings_decode_css_escapes() {
        let css = r#".lg\:flex,.md\:w-1\/2,.\32xl\:p-4,.\32 xl\:m-1,.top-\[3px\],.w-0\.5,.a\,b,.\e9 t\E9,.\!w,.\0,.\D800{width:1px}
                     #a\.b,[data-x="a\"b"],[data-y='\31 0' i],d\69v{width:2px}
                     .x\
                     y{width:3px}
                     @font-face{font-family:"\5FAE\8F6F";src:url("a\"b.woff")}"#;
        let mut atoms = Interner::new();
        let mut sheet = Sheet::default();
        sheet.add(css, &mut atoms, 1, "");
        let names: Vec<String> = sheet
            .sels
            .iter()
            .map(|s| {
                let c = &s.parts[0].c;
                let mut n: Vec<&str> = c.classes.iter().map(|&a| atoms.name(a)).collect();
                n.extend((c.id != 0).then(|| atoms.name(c.id)));
                n.extend((c.tag != 0).then(|| atoms.name(c.tag)));
                n.extend(c.attrs.iter().map(|a| &*a.value));
                n.join(" ")
            })
            .collect();
        let want = ["lg:flex", "md:w-1/2", "2xl:p-4", "2xl:m-1", "top-[3px]", "w-0.5", "a,b", "\u{e9}t\u{e9}", "!w", "\u{fffd}", "\u{fffd}", "a.b", "a\"b", "10", "div"];
        assert_eq!(names, want);
        assert_eq!(sheet.rules.len(), 2, "an escaped newline is no escape in a name, so the third rule is dropped");
        assert_eq!((&*sheet.fonts[0].family, &*sheet.fonts[0].srcs[0].0), ("\u{5fae}\u{8f6f}", "a\"b.woff"));
    }

    #[test]
    fn conditional_rules_nest_and_supports_asks_the_parser() {
        let css = "@supports (display: grid) { @media (min-width: 1000px) { .a{width:1px} } }
                   @supports (backdrop-filter: blur(2px)) { .b{width:2px} }
                   @supports not (backdrop-filter: blur(2px)) { .c{width:3px} }
                   @media (max-width: 100px), (min-width: 1000px) { @supports selector(:has(a)) or (foo: bar) { .d{width:4px} } }
                   @supports selector(:focus-visible) { .e{width:5px} }
                   @supports (--x: 1) and (color: var(--y)) { .f{width:6px} }
                   @media (width <= 370px) { .g{width:7px} }";
        let mut atoms = Interner::new();
        let mut sheet = Sheet::default();
        sheet.add(css, &mut atoms, 1, "");
        let applied = |vw: f32| -> Vec<&str> {
            let shown = |s: &&super::SelEntry| {
                let m = sheet.rules[s.rule as usize].media;
                m == u32::MAX || sheet.media[m as usize].matches(vw, 800.0)
            };
            sheet.sels.iter().filter(shown).map(|s| atoms.name(s.parts[0].c.classes[0])).collect()
        };
        assert_eq!(applied(1280.0), ["a", "c", "d", "f"]);
        assert_eq!(applied(800.0), ["c", "f"]);
        assert_eq!(applied(300.0), ["c", "f", "g"]);
    }

    #[test]
    fn font_weight_values() {
        let weight = |v: &str| super::font_weight(v);
        assert_eq!(weight("bold"), Some(FontWeight::Abs(700)));
        assert_eq!(weight("normal"), Some(FontWeight::Abs(400)));
        assert_eq!(weight("550"), Some(FontWeight::Abs(550)));
        assert_eq!(weight("bolder"), Some(FontWeight::Bolder));
        assert_eq!(weight("1001"), None);
    }
}
