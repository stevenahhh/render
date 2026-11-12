//! Conditions of conditional rules: media queries (Media Queries 4) and the
//! `not`/`and`/`or` grammar `@supports` shares with them.
//!
//! A query is read once, with its sheet. Everything about the environment but
//! the viewport is fixed: a desktop browser window with a mouse, light colours,
//! no reduced motion, one device pixel per CSS pixel. So a feature is decided
//! when it is read, and what is left compares the viewport's width, height or
//! aspect ratio.

use crate::css::{parse_len, Len};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Axis {
    Width,
    Height,
    Ratio,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Op {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

impl Op {
    fn holds(self, a: f32, b: f32) -> bool {
        let near = (a - b).abs() <= 1e-4 * b.abs().max(1.0);
        match self {
            Op::Lt => a < b && !near,
            Op::Le => a <= b || near,
            Op::Eq => near,
            Op::Ge => a >= b || near,
            Op::Gt => a > b && !near,
        }
    }

    /// The operator with its sides swapped: `a < b` is `b > a`.
    fn flip(self) -> Op {
        match self {
            Op::Lt => Op::Gt,
            Op::Le => Op::Ge,
            Op::Eq => Op::Eq,
            Op::Ge => Op::Le,
            Op::Gt => Op::Lt,
        }
    }
}

/// A condition, decided as far as it can be without the viewport.
#[derive(Clone, Debug)]
pub(crate) enum Cond {
    Is(bool),
    /// The viewport's width, height or aspect ratio against a value.
    Cmp(Axis, Op, f32),
    Not(Rc<Cond>),
    All(Rc<[Cond]>),
    Any(Rc<[Cond]>),
}

impl Cond {
    pub(crate) fn eval(&self, vw: f32, vh: f32) -> bool {
        match self {
            Cond::Is(b) => *b,
            Cond::Cmp(axis, op, v) => {
                let x = match axis {
                    Axis::Width => vw,
                    Axis::Height => vh,
                    Axis::Ratio => vw / vh.max(1e-6),
                };
                op.holds(x, *v)
            }
            Cond::Not(c) => !c.eval(vw, vh),
            Cond::All(cs) => cs.iter().all(|c| c.eval(vw, vh)),
            Cond::Any(cs) => cs.iter().any(|c| c.eval(vw, vh)),
        }
    }
}

/// The media query list of an `@media` rule, a `<source media>` or `matchMedia`.
#[derive(Clone, Debug)]
pub struct Media(Cond);

impl Media {
    /// Reads a media query list: it matches when any of its queries does, and a
    /// query that cannot be read matches nothing. An empty list matches always.
    pub fn parse(q: &str) -> Media {
        let q = q.trim().to_ascii_lowercase();
        if q.is_empty() {
            return Media(Cond::Is(true));
        }
        let queries: Vec<Cond> = split_top(&q, b',').into_iter().map(|one| query(one.trim()).unwrap_or(Cond::Is(false))).collect();
        Media(if queries.len() == 1 { queries.into_iter().next().unwrap_or(Cond::Is(false)) } else { Cond::Any(queries.into()) })
    }

    /// This list inside the rules of `outer`: both must match.
    pub fn within(self, outer: &Media) -> Media {
        Media(Cond::All(Rc::from([outer.0.clone(), self.0])))
    }

    pub fn matches(&self, vw: f32, vh: f32) -> bool {
        self.0.eval(vw, vh)
    }
}

/// Evaluates a media query list such as a `<source media>` attribute.
pub fn media_matches(q: &str, vw: f32, vh: f32) -> bool {
    Media::parse(q).matches(vw, vh)
}

/// `s` split at `sep` outside parentheses.
fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0);
    for (i, &c) in s.as_bytes().iter().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// One query: a condition, or a media type with an optional `not` or `only`
/// before it and `and` conditions after it.
fn query(q: &str) -> Option<Cond> {
    if q.starts_with('(') || (q.starts_with("not") && q[3..].trim_start().starts_with('(')) {
        return condition(q, &mut feature, true);
    }
    let (negate, rest) = match q.split_once(char::is_whitespace) {
        Some(("not", rest)) => (true, rest.trim_start()),
        Some(("only", rest)) => (false, rest.trim_start()),
        _ => (false, q),
    };
    let (ty, conds) = match rest.find(|c: char| c.is_whitespace() || c == '(') {
        Some(end) => (&rest[..end], rest[end..].trim_start()),
        None => (rest, ""),
    };
    if matches!(ty, "" | "not" | "only" | "and" | "or" | "layer") {
        return None;
    }
    let mut c = Cond::Is(matches!(ty, "all" | "screen"));
    if !conds.is_empty() {
        let after = conds.strip_prefix("and")?;
        if !after.starts_with(|ch: char| ch.is_whitespace() || ch == '(') {
            return None;
        }
        c = Cond::All(Rc::from([c, condition(after.trim_start(), &mut feature, false)?]));
    }
    Some(if negate { Cond::Not(Rc::new(c)) } else { c })
}

/// A condition: `not (x)`, or `(x)` followed by any number of `and (y)` or, with
/// `or_ok`, of `or (y)`, never both. `leaf` decides what is between a pair of
/// parentheses that is not itself a condition, and a function such as `selector(...)`.
pub(crate) fn condition(s: &str, leaf: &mut impl FnMut(&str) -> Cond, or_ok: bool) -> Option<Cond> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("not") {
        let rest = rest.trim_start();
        if s.len() > 3 && (rest.starts_with('(') || s.as_bytes()[3].is_ascii_whitespace()) {
            let (inner, after) = group(rest, leaf)?;
            return after.trim().is_empty().then(|| Cond::Not(Rc::new(inner)));
        }
    }
    let (first, mut rest) = group(s, leaf)?;
    let (mut list, mut joint) = (vec![first], "");
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let word = if rest.starts_with("and") {
            "and"
        } else if rest.starts_with("or") && or_ok {
            "or"
        } else {
            return None;
        };
        if !joint.is_empty() && joint != word {
            return None;
        }
        joint = word;
        let after = &rest[word.len()..];
        if !after.starts_with(|c: char| c.is_whitespace() || c == '(') {
            return None;
        }
        let (next, r) = group(after.trim_start(), leaf)?;
        list.push(next);
        rest = r;
    }
    Some(match (list.len(), joint) {
        (1, _) => list.pop().unwrap_or(Cond::Is(false)),
        (_, "and") => Cond::All(list.into()),
        _ => Cond::Any(list.into()),
    })
}

/// The parenthesized group or function at the start of `s`, and the text after it.
fn group<'a>(s: &'a str, leaf: &mut impl FnMut(&str) -> Cond) -> Option<(Cond, &'a str)> {
    let open = s.find('(')?;
    let name = &s[..open];
    if !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
        return None;
    }
    let mut depth = 0;
    let close = s.bytes().enumerate().skip(open).find_map(|(i, c)| {
        match c {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        (depth == 0).then_some(i)
    })?;
    let inner = &s[open + 1..close];
    let rest = &s[close + 1..];
    if !name.is_empty() {
        return Some((leaf(&s[..=close]), rest));
    }
    let t = inner.trim();
    let nested = t.starts_with('(') || (t.starts_with("not") && t[3..].trim_start().starts_with('('));
    let c = if nested { condition(t, leaf, true).unwrap_or(Cond::Is(false)) } else { leaf(t) };
    Some((c, rest))
}

/// A media feature between parentheses: `name: value`, `name`, or a range such
/// as `width <= 370px` or `400px <= width < 800px`. One that cannot be read, or
/// that is unknown, matches nothing.
fn feature(t: &str) -> Cond {
    if t.contains('(') {
        // A function or general enclosed text.
        return Cond::Is(false);
    }
    if let Some((name, value)) = t.split_once(':') {
        let (name, value) = (name.trim(), value.trim());
        let (op, base) = match name {
            "-webkit-min-device-pixel-ratio" | "min--moz-device-pixel-ratio" => (Op::Ge, "-webkit-device-pixel-ratio"),
            "-webkit-max-device-pixel-ratio" | "max--moz-device-pixel-ratio" => (Op::Le, "-webkit-device-pixel-ratio"),
            _ => match (name.strip_prefix("min-"), name.strip_prefix("max-")) {
                (Some(b), _) => (Op::Ge, b),
                (_, Some(b)) => (Op::Le, b),
                _ => (Op::Eq, name),
            },
        };
        return match range(base, op, value) {
            Some(c) => c,
            None if op == Op::Eq => discrete(name, value),
            None => Cond::Is(false),
        };
    }
    let parts = split_ops(t);
    match parts.as_slice() {
        [name] => boolean(name),
        [a, op, b] => match (op_of(op), is_name(a), is_name(b)) {
            (Some(op), true, false) => range(a, op, b).unwrap_or(Cond::Is(false)),
            (Some(op), false, true) => range(b, op.flip(), a).unwrap_or(Cond::Is(false)),
            _ => Cond::Is(false),
        },
        [lo, op1, name, op2, hi] => {
            let (Some(op1), Some(op2)) = (op_of(op1), op_of(op2)) else { return Cond::Is(false) };
            let same_way = matches!((op1, op2), (Op::Lt | Op::Le, Op::Lt | Op::Le) | (Op::Gt | Op::Ge, Op::Gt | Op::Ge));
            match (same_way && is_name(name), range(name, op1.flip(), lo), range(name, op2, hi)) {
                (true, Some(a), Some(b)) => Cond::All(Rc::from([a, b])),
                _ => Cond::Is(false),
            }
        }
        _ => Cond::Is(false),
    }
}

/// The words and comparison operators of a range feature.
fn split_ops(t: &str) -> Vec<&str> {
    let b = t.as_bytes();
    let (mut out, mut start, mut i) = (Vec::new(), 0, 0);
    while i < b.len() {
        if matches!(b[i], b'<' | b'>' | b'=') {
            out.push(t[start..i].trim());
            let len = if b[i] != b'=' && b.get(i + 1) == Some(&b'=') { 2 } else { 1 };
            out.push(&t[i..i + len]);
            i += len;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(t[start..].trim());
    out
}

fn op_of(s: &str) -> Option<Op> {
    Some(match s {
        "<" => Op::Lt,
        "<=" => Op::Le,
        "=" => Op::Eq,
        ">=" => Op::Ge,
        ">" => Op::Gt,
        _ => return None,
    })
}

fn is_name(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic()) || (s.starts_with('-') && s[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '-'))
}

/// A length in a media query, in px: `em` and `rem` are the initial 16px.
fn length(v: &str) -> Option<f32> {
    match parse_len(v)? {
        Len::Px(px) => Some(px),
        Len::Em(n) | Len::Rem(n) => Some(n * 16.0),
        _ => None,
    }
}

/// `16/9`, `16 / 9` or a plain number such as `1.5`.
fn ratio(v: &str) -> Option<f32> {
    let (a, b) = v.split_once('/').unwrap_or((v, "1"));
    let (a, b) = (a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?);
    (a > 0.0 && b > 0.0).then(|| a / b)
}

/// A resolution in dots per CSS pixel.
fn dppx(v: &str) -> Option<f32> {
    let split = v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len());
    let n = v[..split].parse::<f32>().ok()?;
    match &v[split..] {
        "dppx" | "x" => Some(n),
        "dpi" => Some(n / 96.0),
        "dpcm" => Some(n * 2.54 / 96.0),
        _ => None,
    }
}

/// A feature of the range type compared with `value`; `None` for any other
/// feature, or a value it cannot take.
fn range(name: &str, op: Op, value: &str) -> Option<Cond> {
    let value = value.trim();
    let fixed = |have: f32, v: Option<f32>| v.map(|v| Cond::Is(op.holds(have, v)));
    match name {
        "width" | "device-width" => length(value).map(|v| Cond::Cmp(Axis::Width, op, v)),
        "height" | "device-height" => length(value).map(|v| Cond::Cmp(Axis::Height, op, v)),
        "aspect-ratio" | "device-aspect-ratio" => ratio(value).map(|v| Cond::Cmp(Axis::Ratio, op, v)),
        "resolution" => fixed(1.0, dppx(value)),
        "-webkit-device-pixel-ratio" => fixed(1.0, value.parse().ok()),
        "color" => fixed(8.0, value.parse().ok()),
        "color-index" | "monochrome" => fixed(0.0, value.parse().ok()),
        _ => None,
    }
}

/// A feature with keyword values, as this window has it.
fn discrete(name: &str, value: &str) -> Cond {
    Cond::Is(match name {
        "orientation" => {
            return match value {
                "portrait" => Cond::Cmp(Axis::Ratio, Op::Le, 1.0),
                "landscape" => Cond::Cmp(Axis::Ratio, Op::Gt, 1.0),
                _ => Cond::Is(false),
            }
        }
        "prefers-color-scheme" => value == "light",
        "prefers-reduced-motion" | "prefers-contrast" | "prefers-reduced-transparency" | "prefers-reduced-data" => value == "no-preference",
        "forced-colors" | "inverted-colors" => value == "none",
        "hover" | "any-hover" => value == "hover",
        "pointer" | "any-pointer" => value == "fine",
        "display-mode" => value == "browser",
        "scripting" => value == "enabled",
        "update" => value == "fast",
        "overflow-block" | "overflow-inline" => value == "scroll",
        "color-gamut" => value == "srgb",
        "dynamic-range" | "video-dynamic-range" => value == "standard",
        "grid" => value == "0",
        _ => false,
    })
}

/// A feature named on its own, which matches when its value is not zero or `none`.
fn boolean(name: &str) -> Cond {
    Cond::Is(match name {
        "width" | "height" | "aspect-ratio" | "device-width" | "device-height" | "device-aspect-ratio" | "orientation" => true,
        "resolution" | "-webkit-device-pixel-ratio" | "color" | "hover" | "any-hover" | "pointer" | "any-pointer" => true,
        "prefers-color-scheme" | "display-mode" | "scripting" | "update" | "overflow-block" | "overflow-inline" => true,
        "color-gamut" | "dynamic-range" | "video-dynamic-range" | "-webkit-transform-3d" => true,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::media_matches;

    #[test]
    fn media_queries_follow_media_queries_4_at_1280_by_800() {
        let cases: &[(&str, bool)] = &[
            ("(min-width: 1024px)", true),
            ("(min-width: 64rem)", true),
            ("(max-width: 1068px)", false),
            ("only screen and (max-width: 1068px)", false),
            ("not all and (min-width: 1024px)", false),
            ("not all and (max-width: 1024px)", true),
            ("print", false),
            ("print, (max-width: 100px)", false),
            ("(max-width: 100px), screen", true),
            ("(width<=370px)", false),
            ("(width <= 1280px)", true),
            ("(400px < width)", true),
            ("(width>=2000px)", false),
            ("(400px <= width < 1300px)", true),
            ("(400px <= width < 1280px)", false),
            ("(1300px > width >= 400px)", true),
            ("(min-width:1000px) and (max-width:1100px)", false),
            ("(orientation: portrait)", false),
            ("(orientation: landscape)", true),
            ("(min-aspect-ratio: 16/9)", false),
            ("(min-aspect-ratio: 16 / 10)", true),
            ("(min-resolution: 2dppx)", false),
            ("(-webkit-min-device-pixel-ratio: 1)", true),
            ("(prefers-color-scheme: dark)", false),
            ("(prefers-reduced-motion: no-preference)", true),
            ("(hover: hover) and (pointer: fine)", true),
            ("(display-mode: standalone)", false),
            ("(inverted-colors)", false),
            ("(transform-3d), (-webkit-transform-3d)", true),
            ("(unknown-feature: 1)", false),
            ("(min-width: 640)", false),
            ("screen and (min-width: 1px) or (max-width: 1px)", false),
            ("(not (max-width: 100px)) and ((min-width: 1px) or (color))", true),
            ("", true),
        ];
        for &(q, want) in cases {
            assert_eq!(media_matches(q, 1280.0, 800.0), want, "{q}");
        }
    }
}
