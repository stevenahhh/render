//! Resolved values for `getComputedStyle`: the computed style, with sizes,
//! margins and offsets of a laid-out box taken from the layout, written the way
//! browsers serialize them.

use crate::css::{Color, Display, GridPos, Len, Position, CLEAR_LEFT, CLEAR_RIGHT, FLOAT_LEFT, FLOAT_RIGHT};
use crate::style::Style;

/// What a resolved value is read from: the element's style and border box, and
/// those of its parent, whose content box percentages refer to.
pub struct Element<'a> {
    pub style: &'a Style,
    /// The border box, `None` when the element has none.
    pub rect: Option<[f32; 4]>,
    pub parent: Option<(&'a Style, [f32; 4])>,
}

/// A number as CSS serializes it: no trailing zeros, at most four decimals.
fn num(v: f32) -> String {
    let v = (f64::from(v) * 10_000.0).round() / 10_000.0;
    if v == 0.0 { "0".to_string() } else { v.to_string() }
}

fn px(v: f32) -> String {
    format!("{}px", num(v))
}

fn grid_pos(p: &GridPos) -> String {
    match p {
        GridPos::Auto => "auto".to_string(),
        GridPos::Line(n) => n.to_string(),
        GridPos::Span(n) => format!("span {n}"),
        GridPos::Name(name) => name.to_string(),
    }
}

fn color(c: Color) -> String {
    let (r, g, b, a) = (c.0 >> 16 & 255, c.0 >> 8 & 255, c.0 & 255, c.a());
    if a == 255 {
        return format!("rgb({r}, {g}, {b})");
    }
    // The shortest alpha that stands for the same byte.
    let alpha = (1..=3).map(|d| ((f64::from(a) / 255.0) * 10f64.powi(d)).round() / 10f64.powi(d)).find(|x| (x * 255.0).round() as u32 == a).unwrap_or(f64::from(a) / 255.0);
    format!("rgba({r}, {g}, {b}, {alpha})")
}

impl Element<'_> {
    /// The width of the parent's content box, which percentages of widths, margins and padding refer to.
    fn base(&self) -> f32 {
        self.parent.map_or(0.0, |(s, r)| r[2] - s.bw[1] - s.bw[3] - self.resolve(s.padding[1], 0.0) - self.resolve(s.padding[3], 0.0))
    }

    fn resolve(&self, l: Len, base: f32) -> f32 {
        match l {
            Len::Px(v) => v,
            Len::Pct(p) => p * base / 100.0,
            Len::Calc(p, v) => p * base / 100.0 + v,
            _ => 0.0,
        }
    }

    fn computed(&self, l: Len) -> String {
        match l {
            Len::Auto => "auto".to_string(),
            Len::Pct(p) => format!("{}%", num(p)),
            l => px(self.resolve(l, self.base())),
        }
    }

    /// `width` (`horizontal`) or `height` of the box: the content box, or with
    /// `box-sizing: border-box` the border box, of a laid-out box that sizes.
    fn size(&self, horizontal: bool) -> String {
        let s = self.style;
        let specified = if horizontal { s.width } else { s.height };
        let Some(rect) = self.rect.filter(|_| !matches!(s.display, Display::Inline | Display::None)) else { return self.computed(specified) };
        let base = self.base();
        let (lo, hi) = if horizontal { (3, 1) } else { (0, 2) };
        let extra = if s.border_box { 0.0 } else { s.bw[lo] + s.bw[hi] + self.resolve(s.padding[lo], base) + self.resolve(s.padding[hi], base) };
        px((if horizontal { rect[2] } else { rect[3] }) - extra)
    }

    /// A margin: from the layout when `auto` places a block, else as computed.
    fn margin(&self, side: usize) -> String {
        let s = self.style;
        match (s.margin[side], self.rect, self.parent) {
            (Len::Auto, Some(r), Some((ps, pr))) if side % 2 == 1 && !matches!(s.display, Display::Inline) => {
                let left = pr[0] + ps.bw[3] + self.resolve(ps.padding[3], 0.0);
                let right = left + self.base();
                px(if side == 3 { r[0] - left } else { right - r[0] - r[2] })
            }
            (Len::Auto, ..) => "0px".to_string(),
            (l, ..) => px(self.resolve(l, self.base())),
        }
    }

    /// The resolved value of `name`; `None` for a property this does not know.
    pub fn value(&self, name: &str) -> Option<String> {
        let s = self.style;
        const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];
        let side = |prefix: &str| SIDES.iter().position(|side| name.strip_prefix(prefix) == Some(side));
        if let Some(i) = side("margin-") {
            return Some(self.margin(i));
        }
        if let Some(i) = side("padding-") {
            return Some(px(self.resolve(s.padding[i], self.base())));
        }
        if let Some(i) = SIDES.iter().position(|side| name == *side) {
            return Some(self.computed(s.inset[i]));
        }
        if let Some(i) = SIDES.iter().position(|side| name.strip_prefix("border-").and_then(|r| r.strip_suffix("-width")) == Some(side)) {
            return Some(px(s.bw[i]));
        }
        // Row start, column start, row end, column end.
        const PLACES: [&str; 4] = ["grid-row-start", "grid-column-start", "grid-row-end", "grid-column-end"];
        if let Some(i) = PLACES.iter().position(|p| *p == name) {
            return Some(s.rare().place.as_ref().map_or("auto".to_string(), |place| grid_pos(&place[i])));
        }
        if let Some(axis) = ["grid-row", "grid-column"].iter().position(|p| *p == name) {
            let [start, end] = [PLACES[axis], PLACES[axis + 2]].map(|p| self.value(p).unwrap_or_default());
            return Some(format!("{start} / {end}"));
        }
        Some(match name {
            "float" => match s.float {
                FLOAT_LEFT => "left",
                FLOAT_RIGHT => "right",
                _ => "none",
            }
            .to_string(),
            "clear" => match s.clear & (CLEAR_LEFT | CLEAR_RIGHT) {
                0 => "none",
                CLEAR_LEFT => "left",
                CLEAR_RIGHT => "right",
                _ => "both",
            }
            .to_string(),
            // A grid keeps its own flag; the display it is laid out with says flex.
            "display" if s.grid => (if matches!(s.display, Display::InlineFlex) { "inline-grid" } else { "grid" }).to_string(),
            "display" => match s.display {
                Display::None => "none",
                Display::Block => "block",
                Display::Inline => "inline",
                Display::InlineBlock => "inline-block",
                Display::Flex => "flex",
                Display::InlineFlex => "inline-flex",
                Display::ListItem => "list-item",
                Display::Grid => "grid",
                Display::Contents => "contents",
                Display::Inherit => "inline",
            }
            .to_string(),
            "position" => match s.position {
                Position::Static => "static",
                Position::Relative => "relative",
                Position::Absolute => "absolute",
                Position::Fixed => "fixed",
            }
            .to_string(),
            "visibility" => (if s.visible { "visible" } else { "hidden" }).to_string(),
            "box-sizing" => (if s.border_box { "border-box" } else { "content-box" }).to_string(),
            "width" => self.size(true),
            "height" => self.size(false),
            "font-size" => px(s.font_size),
            "font-weight" => s.weight.to_string(),
            "color" => color(s.color),
            "background-color" => color(s.bg),
            // A transform, clip or clip-path that leaves nothing of the element draws it with
            // opacity 0, which is not its own.
            "opacity" if s.scaled_away || s.cut_away != 0 => num(s.own_opacity),
            "opacity" => num(self.parent.map_or(s.opacity, |(p, _)| if p.opacity > 0.0 { s.opacity / p.opacity } else { 1.0 })),
            "overflow" | "overflow-x" | "overflow-y" => (if s.overflow_hidden { "hidden" } else { "visible" }).to_string(),
            "transform" => {
                let [w, h] = self.rect.map_or([0.0; 2], |r| [r[2], r[3]]);
                let [x, y] = [self.resolve(s.translate[0], w), self.resolve(s.translate[1], h)];
                if x == 0.0 && y == 0.0 { "none".to_string() } else { format!("matrix(1, 0, 0, 1, {}, {})", num(x), num(y)) }
            }
            _ if name.starts_with("--") => s.vars.as_ref().and_then(|v| v.get(name)).map_or(String::new(), |v| v.trim().to_string()),
            _ => return None,
        })
    }
}

/// The properties `getComputedStyle` lists, custom properties left out.
pub const NAMES: &[&str] = &[
    "display", "position", "visibility", "box-sizing", "width", "height", "margin-top", "margin-right", "margin-bottom", "margin-left",
    "padding-top", "padding-right", "padding-bottom", "padding-left", "border-top-width", "border-right-width", "border-bottom-width",
    "border-left-width", "top", "right", "bottom", "left", "font-size", "font-weight", "color", "background-color", "opacity", "overflow",
    "overflow-x", "overflow-y", "transform", "float", "clear", "grid-row-start", "grid-column-start", "grid-row-end", "grid-column-end",
];

#[cfg(test)]
mod tests {
    use super::{color, num};
    use crate::css::Color;

    #[test]
    fn colours_and_numbers_serialize_like_browsers() {
        assert_eq!(color(Color::rgba(51, 102, 153, 255)), "rgb(51, 102, 153)");
        assert_eq!(color(Color::rgba(255, 0, 0, 128)), "rgba(255, 0, 0, 0.5)");
        assert_eq!(color(Color::TRANSPARENT), "rgba(0, 0, 0, 0)");
        assert_eq!((num(48.0), num(0.5), num(1.0 / 3.0), num(-0.0)), ("48".into(), "0.5".into(), "0.3333".into(), "0".into()));
    }
}
