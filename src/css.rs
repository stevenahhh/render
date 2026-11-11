//! CSS values, declaration parsing, selectors and the rule index.

use crate::dom::{Atom, Interner};
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

#[derive(Debug)]
pub struct Gradient {
    /// CSS angle in degrees: 0 points up, 90 points right.
    pub angle: f32,
    pub stops: Vec<(f32, Color)>,
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
    Radius(Len),
    BoxSizing(bool),
    Background(Color),
    Gradient(Option<Rc<Gradient>>),
    /// `None` is `inherit`.
    Color(Option<Color>),
    FontSize(FontSize),
    Bold(bool),
    Italic(bool),
    Family(Family),
    LineHeight(LineHeight),
    TextAlign(TextAlign),
    TextDeco(u8),
    WhiteSpace(WhiteSpace),
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
    Visible(bool),
    Opacity(f32),
    Pointer(bool),
    Translate(Len, Len),
    /// `None` is auto.
    ZIndex(Option<i32>),
    /// Column count, and whether the tracks share the width evenly.
    GridCols(u8, bool),
    GridFlowCol(bool),
    /// SVG fill; `None` is currentColor.
    Fill(Option<Color>),
    /// Custom property definition.
    Var(Box<str>, Box<str>),
    /// A value containing `var()`, parsed once the variables are known.
    Deferred(Box<str>, Box<str>),
}

pub const DECO_UNDERLINE: u8 = 1;
pub const DECO_LINE_THROUGH: u8 = 2;

/// Splits on a separator byte outside parentheses and quotes. A space
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
            } else if c == b'"' || c == b'\'' {
                i += 1;
                while i < b.len() && b[i] != c {
                    i += 1;
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
    for t in tokens(inner) {
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
    Some(match &s[i..] {
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
        _ => return None,
    })
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

fn font_weight(v: &str) -> Option<bool> {
    match v {
        "bold" | "bolder" => Some(true),
        "normal" | "lighter" => Some(false),
        _ => num(v).map(|n| n >= 600.0),
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
    for name in v.split(',') {
        let n = name.trim().trim_matches(|c| c == '"' || c == '\'');
        let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
        if has(&["mono", "courier", "consol", "menlo", "code"]) {
            return Family::Mono;
        }
        if has(&["sans", "arial", "helvetica", "system-ui", "-apple-system", "segoe", "roboto", "inter", "verdana", "tahoma", "gothic", "pretendard"]) {
            return Family::Sans;
        }
        if has(&["serif", "times", "georgia", "garamond", "palatino", "myeongjo", "batang"]) {
            return Family::Serif;
        }
    }
    Family::Sans
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

/// Parses one declaration and appends the longhands it expands to.
/// Unknown properties and invalid values are dropped, as CSS requires.
pub fn parse_decl(name: &str, value: &str, out: &mut Vec<Decl>) {
    use Decl as D;
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
    let mut push = |d: Option<Decl>| {
        if let Some(d) = d {
            out.push(d)
        }
    };
    match name {
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
                None => tokens(v).count().clamp(1, 12),
            };
            let even = v.contains("fr") || v.contains('%') || v.contains("repeat(");
            out.push(D::GridCols(cols as u8, even));
        }
        "grid-auto-flow" => push(Some(D::GridFlowCol(v.starts_with("column")))),
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
        "border-radius" => push(tokens(v).next().and_then(parse_len).map(D::Radius)),
        "box-sizing" => push(Some(D::BoxSizing(v == "border-box"))),
        "background" | "background-color" | "background-image" => {
            let (mut color, mut grad, mut none) = (None, None, false);
            for t in tokens(v) {
                if t.starts_with("linear-gradient(") {
                    grad = parse_gradient(t).map(Rc::new);
                } else if t == "none" {
                    none = true;
                } else if let Some(c) = color_tok(t) {
                    color = Some(c);
                }
            }
            if name != "background-image" && (color.is_some() || name == "background") {
                out.push(D::Background(color.unwrap_or(Color::TRANSPARENT)));
            }
            if name != "background-color" && (grad.is_some() || none || name == "background") {
                out.push(D::Gradient(grad));
            }
        }
        "color" => {
            if v == "inherit" {
                out.push(D::Color(None));
            } else {
                push(color_tok(v).map(|c| D::Color(Some(c))));
            }
        }
        "font-size" => push(font_size(v).map(D::FontSize)),
        "font-weight" => push(font_weight(v).map(D::Bold)),
        "font-style" => push(Some(D::Italic(v == "italic" || v == "oblique"))),
        "font-family" => push(Some(D::Family(font_family(v)))),
        "line-height" => push(line_height(v).map(D::LineHeight)),
        "font" => {
            let toks: Vec<&str> = tokens(v).collect();
            let (mut bold, mut italic) = (false, false);
            for (i, t) in toks.iter().enumerate() {
                match *t {
                    "italic" | "oblique" => italic = true,
                    "bold" | "bolder" => bold = true,
                    "normal" | "small-caps" => {}
                    _ => {
                        if let Ok(w) = t.parse::<f32>() {
                            bold = w >= 600.0;
                            continue;
                        }
                        let (sz, lh) = t.split_once('/').unwrap_or((t, ""));
                        let Some(fs) = font_size(sz) else { return };
                        out.push(D::Italic(italic));
                        out.push(D::Bold(bold));
                        out.push(D::FontSize(fs));
                        out.push(D::LineHeight(line_height(lh).unwrap_or(LineHeight::Normal)));
                        if i + 1 < toks.len() {
                            out.push(D::Family(font_family(&toks[i + 1..].join(" "))));
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
            "pre" | "pre-wrap" | "break-spaces" => WhiteSpace::Pre,
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
        "visibility" => push(Some(D::Visible(v == "visible"))),
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
        }
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
        if val.contains("var(") {
            out.push(Decl::Deferred(lname.into(), val.into()));
            dynamic = true;
        } else {
            parse_decl(&lname, val, out);
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
    Not(Box<Compound>),
    /// Matches when some descendant matches the selector.
    Has(Vec<Part>),
    Hover,
    Link,
    Empty,
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
}

/// One compound of a complex selector plus the combinator joining it to the
/// compound on its left.
#[derive(Debug)]
pub struct Part {
    pub c: Compound,
    pub comb: Comb,
}

fn ident(b: &[u8], i: &mut usize) -> std::ops::Range<usize> {
    let s = *i;
    while *i < b.len() && (b[*i].is_ascii_alphanumeric() || b[*i] == b'-' || b[*i] == b'_' || b[*i] >= 0x80) {
        *i += 1;
    }
    s..*i
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

fn parse_attr_sel(inner: &str, atoms: &mut Interner) -> Option<AttrSel> {
    let inner = inner.trim();
    let Some(eq) = inner.find('=') else {
        return Some(AttrSel { name: atoms.intern_lower(inner), op: AttrOp::Exists, value: "".into() });
    };
    let (mut name, op) = (&inner[..eq], AttrOp::Eq);
    let op = match name.as_bytes().last() {
        Some(b'~') => AttrOp::Includes,
        Some(b'|') => AttrOp::Dash,
        Some(b'^') => AttrOp::Prefix,
        Some(b'$') => AttrOp::Suffix,
        Some(b'*') => AttrOp::Substr,
        _ => op,
    };
    if !matches!(op, AttrOp::Eq) {
        name = &name[..name.len() - 1];
    }
    let mut value = inner[eq + 1..].trim();
    if let Some(v) = value.strip_suffix(" i").or_else(|| value.strip_suffix(" s")) {
        value = v.trim();
    }
    let value = value.trim_matches(|c| c == '"' || c == '\'');
    Some(AttrSel { name: atoms.intern_lower(name.trim()), op, value: value.into() })
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
                let r = ident(b, i);
                if r.is_empty() {
                    return None;
                }
                c.id = atoms.intern(&s[r]);
                *spec += 0x10000;
            }
            b'.' => {
                *i += 1;
                let r = ident(b, i);
                if r.is_empty() {
                    return None;
                }
                c.classes.push(atoms.intern(&s[r]));
                *spec += 0x100;
            }
            b'[' => {
                let end = *i + s[*i..].find(']')?;
                c.attrs.push(parse_attr_sel(&s[*i + 1..end], atoms)?);
                *i = end + 1;
                *spec += 0x100;
            }
            b':' => {
                *i += 1;
                if *i < b.len() && b[*i] == b':' {
                    *i += 1;
                    ident(b, i);
                    c.pseudos.push(Pseudo::Never);
                    continue;
                }
                let name = s[ident(b, i)].to_ascii_lowercase();
                let mut arg = "";
                if *i < b.len() && b[*i] == b'(' {
                    let mut depth = 0;
                    let open = *i;
                    while *i < b.len() {
                        if b[*i] == b'(' {
                            depth += 1;
                        } else if b[*i] == b')' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        *i += 1;
                    }
                    arg = s.get(open + 1..*i)?;
                    *i += 1;
                }
                *spec += 0x100;
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
                    "not" => {
                        let arg = arg.trim();
                        let (mut j, mut sp) = (0, 0);
                        match parse_compound(arg, &mut j, atoms, hover, &mut sp) {
                            Some(inner) if j == arg.len() => Pseudo::Not(Box::new(inner)),
                            _ => Pseudo::Never,
                        }
                    }
                    "has" => parse_selector(arg.trim(), atoms, hover).map_or(Pseudo::Never, |(parts, _)| Pseudo::Has(parts)),
                    "hover" => {
                        *hover = true;
                        Pseudo::Hover
                    }
                    "link" | "any-link" => Pseudo::Link,
                    "empty" => Pseudo::Empty,
                    _ => Pseudo::Never,
                });
            }
            ch if ch.is_ascii_alphabetic() || ch == b'_' || ch == b'-' || ch >= 0x80 => {
                if *i != start {
                    return None;
                }
                c.tag = atoms.intern_lower(&s[ident(b, i)]);
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
    Some((parts, spec))
}

/// Parses a comma-separated selector list, dropping invalid selectors.
pub fn parse_selector_list(s: &str, atoms: &mut Interner) -> Vec<Vec<Part>> {
    let mut hover = false;
    SplitTop::new(s, b',').filter_map(|sel| parse_selector(sel.trim(), atoms, &mut hover)).map(|(parts, _)| parts).collect()
}

#[derive(Clone, Copy, Debug)]
pub struct Media {
    pub min_w: f32,
    pub max_w: f32,
    pub min_h: f32,
    pub max_h: f32,
    pub on: bool,
}

impl Media {
    pub fn matches(&self, vw: f32, vh: f32) -> bool {
        self.on && vw >= self.min_w && vw <= self.max_w && vh >= self.min_h && vh <= self.max_h
    }
}

/// Evaluates a media query such as a `<source media>` attribute.
pub fn media_matches(q: &str, vw: f32, vh: f32) -> bool {
    parse_media(q, None).matches(vw, vh)
}

fn parse_media(q: &str, outer: Option<Media>) -> Media {
    let q = q.to_ascii_lowercase();
    let q = q.split(',').next().unwrap_or("").trim();
    let mut m = outer.unwrap_or(Media { min_w: 0.0, max_w: f32::INFINITY, min_h: 0.0, max_h: f32::INFINITY, on: true });
    if q.starts_with("not") || (q.contains("print") && !q.contains("screen")) {
        m.on = false;
    }
    let mut rest = q;
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')') else { break };
        let feature = &rest[open + 1..open + close];
        rest = &rest[open + close + 1..];
        let Some((name, val)) = feature.split_once(':') else { continue };
        let val = val.trim();
        let px = match parse_len(val) {
            Some(Len::Px(v)) => v,
            Some(Len::Em(v)) | Some(Len::Rem(v)) => v * 16.0,
            _ => f32::NAN,
        };
        match name.trim() {
            "min-width" if !px.is_nan() => m.min_w = m.min_w.max(px),
            "max-width" if !px.is_nan() => m.max_w = m.max_w.min(px),
            "min-height" if !px.is_nan() => m.min_h = m.min_h.max(px),
            "max-height" if !px.is_nan() => m.max_h = m.max_h.min(px),
            "prefers-color-scheme" => m.on &= val == "light",
            "prefers-reduced-motion" => m.on &= val == "no-preference",
            _ => {}
        }
    }
    m
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
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
            }
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
    pub fn add(&mut self, css: &str, atoms: &mut Interner, level: u8) {
        let css = strip_comments(css);
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
                    let outer = self.media.get(media as usize).copied();
                    self.media.push(parse_media(q, outer));
                    let idx = self.media.len() as u32 - 1;
                    self.rules_in(s, i, idx, level, atoms, true);
                } else if prelude.starts_with("@supports") || prelude.starts_with("@layer") {
                    self.rules_in(s, i, media, level, atoms, true);
                } else {
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
                    self.sels.push(SelEntry { parts, spec, rule, level, anc, anc_n });
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
