//! Grid values: track lists, `grid-template-areas`, where items go on the
//! grid's lines, and the shorthands that set them.

use super::{num, parse_len, unquote, Align, Decl, Len, SplitTop};
use std::rc::Rc;

/// The breadth of a track sizing function.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Breadth {
    Len(Len),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    /// `fit-content(limit)`.
    Fit(Len),
}

/// A track's sizing function: `minmax(min, max)`, or one breadth for both.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Track {
    pub min: Breadth,
    pub max: Breadth,
}

/// A line name: the name, the index of its line, and whether it comes after
/// the auto repetition, which its index counts as no tracks.
pub type LineName = (Box<str>, u16, bool);

/// `grid-template-columns` or `grid-template-rows`: the tracks, the names of the
/// lines around them, and at most one `repeat(auto-fill | auto-fit, ...)`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct TrackList {
    pub tracks: Vec<Track>,
    pub names: Vec<LineName>,
    /// Tracks repeated as often as they fit, put in before track `.0`; with
    /// `.2` (`auto-fit`) the empty ones collapse.
    pub auto: Option<(u16, Vec<Track>, bool)>,
}

/// `grid-template-areas`: each named area's lines, as 0-based row start, column
/// start, row end and column end.
#[derive(Clone, PartialEq, Debug)]
pub struct Areas {
    pub areas: Vec<(Box<str>, [u16; 4])>,
    pub rows: u16,
    pub cols: u16,
}

/// One end of a grid item's place on an axis.
#[derive(Clone, PartialEq, Debug, Default)]
pub enum GridPos {
    #[default]
    Auto,
    /// A line number; negative ones count back from the last explicit line.
    Line(i16),
    Span(u16),
    /// A named line, or the start or end line of the area of that name.
    Name(Box<str>),
}

fn breadth(t: &str) -> Option<Breadth> {
    Some(match t {
        "auto" => Breadth::Auto,
        "min-content" => Breadth::MinContent,
        "max-content" => Breadth::MaxContent,
        _ => match t.strip_suffix("fr").and_then(num) {
            Some(fr) => Breadth::Fr(fr.max(0.0)),
            None => Breadth::Len(parse_len(t).filter(|l| *l != Len::Auto)?),
        },
    })
}

fn track(t: &str) -> Option<Track> {
    if let Some(args) = t.strip_prefix("minmax(").and_then(|a| a.strip_suffix(')')) {
        let mut parts = SplitTop::new(args, b',');
        let (min, max) = (breadth(parts.next()?.trim())?, breadth(parts.next()?.trim())?);
        return (!matches!(min, Breadth::Fr(_))).then_some(Track { min, max });
    }
    if let Some(arg) = t.strip_prefix("fit-content(").and_then(|a| a.strip_suffix(')')) {
        let limit = parse_len(arg.trim()).filter(|l| *l != Len::Auto)?;
        return Some(Track { min: Breadth::Auto, max: Breadth::Fit(limit) });
    }
    Some(match breadth(t)? {
        fr @ Breadth::Fr(_) => Track { min: Breadth::Auto, max: fr },
        b => Track { min: b, max: b },
    })
}

/// A piece of a track list: line names, a string of `grid-template`, or a token.
enum Piece<'a> {
    Names(&'a str),
    Str(&'a str),
    Token(&'a str),
}

/// The pieces of a track list, in order.
fn pieces(v: &str) -> Vec<Piece<'_>> {
    let (b, mut out, mut i) = (v.as_bytes(), Vec::new(), 0);
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_whitespace() => i += 1,
            b'[' => {
                let end = v[i..].find(']').map_or(v.len(), |e| i + e);
                out.push(Piece::Names(&v[i + 1..end]));
                i = end + 1;
            }
            q @ (b'"' | b'\'') => {
                let end = v[i + 1..].find(q as char).map_or(v.len(), |e| i + 1 + e);
                out.push(Piece::Str(&v[i..(end + 1).min(v.len())]));
                i = end + 1;
            }
            _ => {
                let (start, mut depth) = (i, 0u32);
                while i < b.len() && (depth > 0 || !(b[i].is_ascii_whitespace() || b[i] == b'[')) {
                    match b[i] {
                        b'(' => depth += 1,
                        b')' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    i += 1;
                }
                out.push(Piece::Token(&v[start..i]));
            }
        }
    }
    out
}

/// Reads a track list such as `[a] 100px repeat(2, 1fr) [b]`.
pub fn track_list(v: &str) -> Option<TrackList> {
    let mut list = TrackList::default();
    for piece in pieces(v) {
        let line = list.tracks.len() as u16;
        let after = list.auto.is_some();
        match piece {
            Piece::Names(names) => list.names.extend(names.split_ascii_whitespace().map(|n| (n.into(), line, after))),
            Piece::Str(_) => return None,
            Piece::Token(t) => match t.strip_prefix("repeat(").and_then(|a| a.strip_suffix(')')) {
                Some(args) => {
                    let (count, body) = args.split_once(',')?;
                    let inner = track_list(body)?;
                    match count.trim() {
                        c @ ("auto-fill" | "auto-fit") if !after && inner.auto.is_none() => list.auto = Some((line, inner.tracks, c == "auto-fit")),
                        c => {
                            let times: u16 = c.parse().ok().filter(|n| (1..=1000).contains(n))?;
                            for _ in 0..times {
                                let base = list.tracks.len() as u16;
                                list.names.extend(inner.names.iter().map(|(n, i, _)| (n.clone(), base + i, after)));
                                list.tracks.extend(&inner.tracks);
                            }
                        }
                    }
                }
                None => list.tracks.push(track(t)?),
            },
        }
    }
    Some(list)
}

/// The cells of the rows of `grid-template-areas`, as an `Areas`; `None` when
/// the rows have different lengths.
fn areas(rows: &[Vec<&str>]) -> Option<Areas> {
    let cols = rows.first()?.len();
    if cols == 0 || rows.iter().any(|r| r.len() != cols) {
        return None;
    }
    let mut out: Vec<(Box<str>, [u16; 4])> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, &name) in row.iter().enumerate() {
            if name.bytes().all(|b| b == b'.') {
                continue;
            }
            let (r, c) = (r as u16, c as u16);
            match out.iter_mut().find(|a| &*a.0 == name) {
                Some((_, a)) => *a = [a[0].min(r), a[1].min(c), a[2].max(r + 1), a[3].max(c + 1)],
                None => out.push((name.into(), [r, c, r + 1, c + 1])),
            }
        }
    }
    Some(Areas { areas: out, rows: rows.len() as u16, cols: cols as u16 })
}

fn cells(s: &str) -> Vec<&str> {
    s.split_ascii_whitespace().collect()
}

/// Splits `v` at its `/` outside parentheses and strings.
fn halves(v: &str) -> (&str, Option<&str>) {
    let mut it = SplitTop::new(v, b'/');
    let a = it.next().unwrap_or("").trim();
    (a, it.next().map(str::trim))
}

/// `grid-template`: `none`, `rows / columns`, or rows with area strings.
fn template(v: &str, out: &mut Vec<Decl>) {
    if v == "none" {
        out.extend([Decl::GridTracks(0, None), Decl::GridTracks(1, None), Decl::GridAreas(None)]);
        return;
    }
    let (rows, cols) = halves(v);
    let cols = cols.map(|c| track_list(c).map(Rc::new));
    if cols == Some(None) {
        return;
    }
    let cols = cols.flatten();
    if !rows.contains(['"', '\'']) {
        if let Some(rows) = track_list(rows) {
            out.extend([Decl::GridTracks(0, Some(Rc::new(rows))), Decl::GridTracks(1, cols), Decl::GridAreas(None)]);
        }
        return;
    }
    let (mut list, mut strings) = (TrackList::default(), Vec::new());
    for piece in pieces(rows) {
        let line = list.tracks.len() as u16;
        match piece {
            Piece::Names(names) => list.names.extend(names.split_ascii_whitespace().map(|n| (n.into(), line, false))),
            Piece::Str(s) => {
                strings.push(unquote(s).into_owned());
                list.tracks.push(Track { min: Breadth::Auto, max: Breadth::Auto });
            }
            Piece::Token(t) => match (list.tracks.last_mut(), track(t)) {
                (Some(last), Some(size)) => *last = size,
                _ => return,
            },
        }
    }
    let rows: Vec<Vec<&str>> = strings.iter().map(|s| cells(s)).collect();
    let Some(areas) = areas(&rows) else { return };
    out.extend([Decl::GridTracks(0, Some(Rc::new(list))), Decl::GridTracks(1, cols), Decl::GridAreas(Some(Rc::new(areas)))]);
}

/// The sizes of `grid-auto-rows` or `grid-auto-columns`.
fn sizes(v: &str) -> Option<Rc<[Track]>> {
    pieces(v).into_iter().map(|p| if let Piece::Token(t) = p { track(t) } else { None }).collect::<Option<Vec<_>>>().map(Into::into)
}

/// `grid`: a `grid-template`, or one axis auto-flowing in implicit tracks.
fn grid(v: &str, out: &mut Vec<Decl>) {
    let (rows, cols) = halves(v);
    let flow = |part: &str| part.split_ascii_whitespace().any(|w| w == "auto-flow");
    let auto = |part: &str| {
        let rest: Vec<&str> = part.split_ascii_whitespace().filter(|w| !matches!(*w, "auto-flow" | "dense")).collect();
        if rest.is_empty() { None } else { sizes(&rest.join(" ")) }
    };
    let dense = v.split_ascii_whitespace().any(|w| w == "dense");
    match cols {
        Some(c) if flow(rows) => {
            let Some(c) = track_list(c) else { return };
            out.extend([Decl::GridTracks(0, None), Decl::GridTracks(1, Some(Rc::new(c))), Decl::GridAuto(0, auto(rows)), Decl::GridAuto(1, None)]);
            out.extend([Decl::GridAreas(None), Decl::GridFlowCol(false), Decl::GridDense(dense)]);
        }
        Some(c) if flow(c) => {
            let Some(r) = track_list(rows) else { return };
            out.extend([Decl::GridTracks(0, Some(Rc::new(r))), Decl::GridTracks(1, None), Decl::GridAuto(0, None), Decl::GridAuto(1, auto(c))]);
            out.extend([Decl::GridAreas(None), Decl::GridFlowCol(true), Decl::GridDense(dense)]);
        }
        _ => {
            template(v, out);
            out.extend([Decl::GridAuto(0, None), Decl::GridAuto(1, None), Decl::GridFlowCol(false), Decl::GridDense(false)]);
        }
    }
}

/// One end of a placement: `auto`, a line number, `span n`, or a line or area name.
fn grid_pos(v: &str) -> Option<GridPos> {
    let words: Vec<&str> = v.split_ascii_whitespace().collect();
    let number = |w: &str| w.parse::<i16>().ok();
    Some(match words.as_slice() {
        ["auto"] => GridPos::Auto,
        ["span", rest @ ..] if !rest.is_empty() => GridPos::Span(rest.iter().find_map(|w| number(w)).map_or(1, |n| n.max(1) as u16)),
        [w] => match number(w) {
            Some(0) => return None,
            Some(n) => GridPos::Line(n),
            None => GridPos::Name((*w).into()),
        },
        [a, b] => match (number(a), number(b)) {
            (Some(_), None) => GridPos::Name((*b).into()),
            (None, Some(_)) => GridPos::Name((*a).into()),
            _ => return None,
        },
        _ => return None,
    })
}

/// The end an omitted part of a placement shorthand takes: the same name as
/// `start`, or `auto`.
fn same_name(start: &GridPos) -> GridPos {
    match start {
        GridPos::Name(n) => GridPos::Name(n.clone()),
        _ => GridPos::Auto,
    }
}

fn justify(v: &str) -> Option<Align> {
    Some(match v {
        "stretch" | "normal" | "legacy" => Align::Stretch,
        "start" | "flex-start" | "self-start" | "left" | "baseline" => Align::Start,
        "center" => Align::Center,
        "end" | "flex-end" | "self-end" | "right" => Align::End,
        "auto" => Align::Auto,
        _ => return None,
    })
}

/// Appends the declarations a grid property or shorthand expands to.
pub fn parse_grid(name: &str, v: &str, out: &mut Vec<Decl>) {
    let axis = |n: &str| (n.contains("column")) as u8;
    match name {
        "grid-template-rows" | "grid-template-columns" => {
            let a = axis(name);
            if v.starts_with("subgrid") {
                out.extend([Decl::Subgrid(a, true), Decl::GridTracks(a, None)]);
            } else if v == "none" {
                out.extend([Decl::Subgrid(a, false), Decl::GridTracks(a, None)]);
            } else if let Some(list) = track_list(v) {
                out.extend([Decl::Subgrid(a, false), Decl::GridTracks(a, Some(Rc::new(list)))]);
            }
        }
        "grid-template-areas" => {
            if v == "none" {
                out.push(Decl::GridAreas(None));
            } else {
                let strings: Vec<String> = super::tokens(v).map(|t| unquote(t).into_owned()).collect();
                let rows: Vec<Vec<&str>> = strings.iter().map(|s| cells(s)).collect();
                if let Some(a) = areas(&rows) {
                    out.push(Decl::GridAreas(Some(Rc::new(a))));
                }
            }
        }
        "grid-template" => template(v, out),
        "grid" => grid(v, out),
        "grid-auto-rows" | "grid-auto-columns" => {
            if let Some(s) = sizes(v) {
                out.push(Decl::GridAuto(axis(name), Some(s)));
            }
        }
        "grid-row" | "grid-column" => {
            let (a, b) = halves(v);
            let Some(start) = grid_pos(a) else { return };
            let end = match b {
                Some(b) => match grid_pos(b) {
                    Some(e) => e,
                    None => return,
                },
                None => same_name(&start),
            };
            let k = axis(name);
            out.extend([Decl::GridPlace(k, start), Decl::GridPlace(k + 2, end)]);
        }
        "grid-area" => {
            let parts: Vec<GridPos> = match SplitTop::new(v, b'/').map(|p| grid_pos(p.trim())).collect::<Option<Vec<_>>>() {
                Some(p) if (1..=4).contains(&p.len()) => p,
                _ => return,
            };
            let rs = parts[0].clone();
            let cs = parts.get(1).cloned().unwrap_or_else(|| same_name(&rs));
            let re = parts.get(2).cloned().unwrap_or_else(|| same_name(&rs));
            let ce = parts.get(3).cloned().unwrap_or_else(|| same_name(&cs));
            out.extend([Decl::GridPlace(0, rs), Decl::GridPlace(1, cs), Decl::GridPlace(2, re), Decl::GridPlace(3, ce)]);
        }
        "grid-row-start" | "grid-column-start" | "grid-row-end" | "grid-column-end" => {
            let k = axis(name) + if name.ends_with("end") { 2 } else { 0 };
            out.extend(grid_pos(v).map(|p| Decl::GridPlace(k, p)));
        }
        "justify-items" => out.extend(justify(v).map(Decl::JustifyItems)),
        "justify-self" => out.extend(justify(v).map(Decl::JustifySelf)),
        "place-items" | "place-self" => {
            let mut words = v.split_ascii_whitespace();
            let (Some(a), b) = (words.next(), words.next()) else { return };
            let (Some(al), Some(ju)) = (super::align(a), justify(b.unwrap_or(a))) else { return };
            if name == "place-items" {
                out.extend([Decl::AlignItems(al), Decl::JustifyItems(ju)]);
            } else {
                out.extend([Decl::AlignSelf(al), Decl::JustifySelf(ju)]);
            }
        }
        _ => {}
    }
}
