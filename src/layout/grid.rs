//! Grid layout (CSS Grid Layout 1): items are placed on the lines of the explicit
//! grid and of the implicit tracks added for them, then the tracks are sized by
//! their sizing functions and the items they hold.
//!
//! Items contribute their max-content size to intrinsic tracks; where those
//! overflow the grid they shrink in proportion, in place of the min-content sizes
//! a browser would fall back to.

use super::{clamp_w, definite, hextra, is_abs, is_ws, sides, spec_w, Engine, Sides, FIT, NAN};
use crate::css::{Align, Areas, Breadth, GridPos, Justify, Len, Track, TrackList};
use crate::dom::{NodeId, NONE};

/// A grid item: an element, or a run of text in an anonymous item.
struct GItem {
    node: NodeId,
    text: bool,
    /// Row start, column start, row end and column end lines.
    area: [usize; 4],
    sd: Sides,
    first: u32,
    end: u32,
    item: u32,
    h: f32,
}

/// One axis of the explicit grid.
struct Axis {
    tracks: Vec<Track>,
    names: Vec<(Box<str>, usize)>,
    /// Tracks of an `auto-fit` repetition, which collapse when no item is in them.
    fit: std::ops::Range<usize>,
}

/// A track list made for `avail` space: the auto repetition goes in as many
/// times as it fits (CSS Grid 7.2.3.2), at least once.
fn expand(list: Option<&TrackList>, avail: f32, gap: f32) -> Axis {
    let Some(list) = list else { return Axis { tracks: Vec::new(), names: Vec::new(), fit: 0..0 } };
    let mut tracks = list.tracks.clone();
    let (mut added, mut fit) = (0, 0..0);
    if let Some((at, rep, auto_fit)) = &list.auto {
        let fixed = |t: &Track| {
            let size = |b: Breadth| if let Breadth::Len(l) = b { definite(l, avail) } else { None };
            size(t.max).or(size(t.min))
        };
        let mut count = 1;
        if let Some(each) = rep.iter().map(fixed).sum::<Option<f32>>().filter(|s| *s > 0.0 && !avail.is_nan()) {
            let others: f32 = list.tracks.iter().map(|t| fixed(t).unwrap_or(0.0)).sum();
            let room = avail - others - gap * list.tracks.len() as f32 + gap;
            count = ((room / (each + gap * rep.len() as f32)).floor().max(1.0)) as usize;
        }
        let at = (*at as usize).min(tracks.len());
        added = count * rep.len();
        let after = tracks.split_off(at);
        tracks.extend(rep.iter().cycle().take(added).copied());
        tracks.extend(after);
        if *auto_fit {
            fit = at..at + added;
        }
    }
    let names = list.names.iter().map(|(n, line, after)| (n.clone(), *line as usize + if *after { added } else { 0 })).collect();
    Axis { tracks, names, fit }
}

/// The line `pos` names on an axis (0 for rows, 1 for columns), as the start or
/// `end` of a placement: a number, a named line, or an area's edge.
fn line(axis: &Axis, explicit: usize, areas: Option<&Areas>, k: usize, pos: &GridPos, end: bool) -> Option<isize> {
    match pos {
        GridPos::Line(n) if *n > 0 => Some(*n as isize - 1),
        GridPos::Line(n) => Some(explicit as isize + 1 + *n as isize),
        GridPos::Name(name) => {
            let find = |want: &str| axis.names.iter().find(|(n, _)| &**n == want).map(|&(_, l)| l as isize);
            let edge = format!("{name}{}", if end { "-end" } else { "-start" });
            find(&edge).or_else(|| find(name)).or_else(|| {
                let (_, a) = areas?.areas.iter().find(|(n, _)| n == name)?;
                Some(a[k + if end { 2 } else { 0 }] as isize)
            })
        }
        GridPos::Auto | GridPos::Span(_) => None,
    }
}

/// An item's place on one axis: its start line when it is definite, and how
/// many tracks it spans.
fn axis_place(start: Option<isize>, end: Option<isize>, (sp, ep): (&GridPos, &GridPos)) -> (Option<usize>, usize) {
    let span = |p: &GridPos| if let GridPos::Span(n) = p { *n as usize } else { 1 };
    let (start, n) = match (start, end) {
        (Some(s), Some(e)) if e != s => (Some(s.min(e)), (s - e).unsigned_abs()),
        (Some(s), _) => (Some(s), span(ep)),
        (None, Some(e)) => (Some(e - span(sp) as isize), span(sp)),
        (None, None) => (None, span(sp).max(span(ep))),
    };
    (start.map(|s| s.max(0) as usize), n.max(1))
}

/// Cells taken on the grid, by major (flow) track then minor track.
#[derive(Default)]
struct Cells(Vec<Vec<bool>>);

impl Cells {
    fn free(&self, major: usize, minor: usize, (span_major, span_minor): (usize, usize)) -> bool {
        (major..major + span_major).all(|r| (minor..minor + span_minor).all(|c| !self.0.get(r).is_some_and(|row| row.get(c).copied().unwrap_or(false))))
    }

    fn take(&mut self, major: usize, minor: usize, (span_major, span_minor): (usize, usize)) {
        for r in major..major + span_major {
            if self.0.len() <= r {
                self.0.resize(r + 1, Vec::new());
            }
            let row = &mut self.0[r];
            if row.len() < minor + span_minor {
                row.resize(minor + span_minor, false);
            }
            row[minor..minor + span_minor].fill(true);
        }
    }
}

/// Places items whose positions are `[major, minor]` (start, span) pairs in the
/// flow direction (CSS Grid 8.5, sparse unless `dense`). Returns `[major start,
/// minor start]` for each, and the number of minor tracks.
fn auto_place(places: &[[(Option<usize>, usize); 2]], minor_explicit: usize, dense: bool) -> (Vec<[usize; 2]>, usize) {
    let mut out = vec![[0usize; 2]; places.len()];
    let mut cells = Cells::default();
    let mut minors = minor_explicit.max(1);
    for p in places {
        minors = minors.max(p[1].0.map_or(p[1].1, |s| s + p[1].1));
    }
    let mut done = vec![false; places.len()];
    for (i, p) in places.iter().enumerate() {
        if let [(Some(maj), sm), (Some(min), sn)] = *p {
            cells.take(maj, min, (sm, sn));
            out[i] = [maj, min];
            done[i] = true;
        }
    }
    // Items locked to a major track go in it at the earliest minor track free.
    let mut row_cursor: Vec<usize> = Vec::new();
    for (i, p) in places.iter().enumerate() {
        if let [(Some(maj), sm), (None, sn)] = *p {
            if row_cursor.len() <= maj {
                row_cursor.resize(maj + 1, 0);
            }
            let mut min = if dense { 0 } else { row_cursor[maj] };
            while !cells.free(maj, min, (sm, sn)) {
                min += 1;
            }
            cells.take(maj, min, (sm, sn));
            row_cursor[maj] = min + sn;
            minors = minors.max(min + sn);
            out[i] = [maj, min];
            done[i] = true;
        }
    }
    let (mut maj, mut min) = (0, 0);
    for (i, p) in places.iter().enumerate() {
        if done[i] {
            continue;
        }
        let [(_, sm), (start, sn)] = *p;
        if dense {
            (maj, min) = (0, 0);
        }
        match start {
            Some(s) => {
                if s < min {
                    maj += 1;
                }
                min = s;
                while !cells.free(maj, min, (sm, sn)) {
                    maj += 1;
                }
            }
            None => loop {
                if min + sn > minors {
                    (maj, min) = (maj + 1, 0);
                    continue;
                }
                if cells.free(maj, min, (sm, sn)) {
                    break;
                }
                min += 1;
            },
        }
        cells.take(maj, min, (sm, sn));
        out[i] = [maj, min];
        min += sn;
    }
    (out, minors)
}

/// Sizes the tracks of an axis in `avail` space (NaN when indefinite) from the
/// items' `(start, span, size, least)` contributions (CSS Grid 11.3 to 11.8, with
/// max-content sizes for intrinsic ones, and `least` as the minimum an `auto`
/// minimum of a flexible track keeps). `stretch` grows `auto` tracks into
/// leftover space; `collapsed` tracks are 0.
fn size_tracks(tracks: &[Track], avail: f32, gap: f32, items: &[(usize, usize, f32, f32)], collapsed: &[bool], stretch: bool) -> Vec<f32> {
    let n = tracks.len();
    let fixed = |b: Breadth| match b {
        Breadth::Len(l) => definite(l, avail),
        _ => None,
    };
    let flex = |t: &Track| if let Breadth::Fr(f) = t.max { Some(f) } else { None };
    let intrinsic = |b: Breadth| matches!(b, Breadth::Auto | Breadth::MinContent | Breadth::MaxContent | Breadth::Fit(_)) || (matches!(b, Breadth::Len(Len::Pct(_) | Len::Calc(..))) && avail.is_nan());
    let mut base = vec![0.0f32; n];
    let mut limit = vec![f32::INFINITY; n];
    let mut most = vec![0.0f32; n];
    let mut least = vec![0.0f32; n];
    for &(s, span, size, min) in items {
        if span == 1 && s < n {
            most[s] = most[s].max(size);
            least[s] = least[s].max(min);
        }
    }
    for (i, t) in tracks.iter().enumerate() {
        if collapsed.get(i).copied().unwrap_or(false) {
            limit[i] = 0.0;
            continue;
        }
        base[i] = match t.min {
            _ if flex(t).is_some() => if t.min == Breadth::Auto { least[i] } else { 0.0 },
            b => fixed(b).unwrap_or(if intrinsic(b) { most[i] } else { 0.0 }),
        };
        limit[i] = match t.max {
            Breadth::Fr(_) => f32::INFINITY,
            Breadth::Fit(l) => most[i].min(definite(l, avail).unwrap_or(most[i])).max(base[i]),
            b => fixed(b).unwrap_or(most[i]).max(base[i]),
        };
    }
    // Items spanning several tracks add what they need beyond them to the
    // intrinsic tracks they span, when none of those is flexible.
    for &(s, span, size, _) in items.iter().filter(|it| it.1 > 1) {
        let range = s.min(n)..(s + span).min(n);
        if range.is_empty() || tracks[range.clone()].iter().any(|t| flex(t).is_some()) {
            continue;
        }
        let grow: Vec<usize> = range.clone().filter(|&i| intrinsic(tracks[i].max) && !collapsed.get(i).copied().unwrap_or(false)).collect();
        let have: f32 = base[range.clone()].iter().sum::<f32>() + gap * (range.len() - 1) as f32;
        if size > have && !grow.is_empty() {
            let add = (size - have) / grow.len() as f32;
            for i in grow {
                base[i] += add;
                limit[i] = limit[i].max(base[i]);
            }
        }
    }
    let gaps = gap * n.saturating_sub(1 + collapsed.iter().filter(|&&c| c).count()) as f32;
    let mut size = base.clone();
    if !avail.is_nan() {
        // Intrinsic tracks too wide for the grid shrink in proportion.
        let room = avail - gaps - size.iter().enumerate().filter(|&(i, _)| !intrinsic(tracks[i].min)).map(|(_, s)| s).sum::<f32>();
        let soft: f32 = size.iter().enumerate().filter(|&(i, _)| intrinsic(tracks[i].min)).map(|(_, s)| s).sum();
        if soft > room + FIT && soft > 0.0 {
            let k = (room / soft).max(0.0);
            for i in (0..n).filter(|&i| intrinsic(tracks[i].min)) {
                size[i] *= k;
                limit[i] = size[i];
            }
        }
        // Growing to their limits shares the free space evenly.
        let mut free = avail - gaps - size.iter().sum::<f32>();
        while free > FIT {
            let open: Vec<usize> = (0..n).filter(|&i| size[i] + FIT < limit[i] && limit[i].is_finite() && flex(&tracks[i]).is_none()).collect();
            if open.is_empty() {
                break;
            }
            let share = free / open.len() as f32;
            for i in open {
                let add = share.min(limit[i] - size[i]);
                size[i] += add;
                free -= add;
            }
        }
    } else {
        for i in 0..n {
            if limit[i].is_finite() {
                size[i] = size[i].max(limit[i]);
            }
        }
    }
    let flexible: Vec<usize> = (0..n).filter(|&i| flex(&tracks[i]).is_some() && !collapsed.get(i).copied().unwrap_or(false)).collect();
    if !flexible.is_empty() {
        let fr_size = if avail.is_nan() {
            flexible.iter().map(|&i| most[i].max(base[i]) / flex(&tracks[i]).unwrap_or(1.0).max(1e-6)).fold(0.0, f32::max)
        } else {
            // A track whose minimum is more than its share is left out and the rest share again.
            let mut set = flexible.clone();
            loop {
                let rest: f32 = (0..n).filter(|i| !set.contains(i)).map(|i| size[i]).sum();
                let factors: f32 = set.iter().map(|&i| flex(&tracks[i]).unwrap_or(0.0)).sum();
                let fr = ((avail - gaps - rest) / factors.max(1.0)).max(0.0);
                let before = set.len();
                set.retain(|&i| flex(&tracks[i]).unwrap_or(0.0) * fr >= base[i]);
                if set.len() == before {
                    break fr;
                }
            }
        };
        for &i in &flexible {
            size[i] = (flex(&tracks[i]).unwrap_or(0.0) * fr_size).max(base[i]);
        }
    } else if stretch && !avail.is_nan() {
        let free = avail - gaps - size.iter().sum::<f32>();
        let auto: Vec<usize> = (0..n).filter(|&i| tracks[i].max == Breadth::Auto && !collapsed.get(i).copied().unwrap_or(false)).collect();
        if free > FIT && !auto.is_empty() {
            for &i in &auto {
                size[i] += free / auto.len() as f32;
            }
        }
    }
    size
}

/// Start positions of tracks of `sizes` from `origin`, with `gap` between those
/// not collapsed and free space distributed by `justify`; and the end.
fn positions(sizes: &[f32], collapsed: &[bool], gap: f32, origin: f32, room: f32, justify: Justify) -> (Vec<f32>, f32) {
    let open = (0..sizes.len()).filter(|&i| !collapsed.get(i).copied().unwrap_or(false)).count();
    let used = sizes.iter().sum::<f32>() + gap * open.saturating_sub(1) as f32;
    let free = if room.is_nan() { 0.0 } else { (room - used).max(0.0) };
    let k = open as f32;
    let (lead, between) = match justify {
        _ if open == 0 => (0.0, 0.0),
        Justify::Start => (0.0, 0.0),
        Justify::Center => (free / 2.0, 0.0),
        Justify::End => (free, 0.0),
        Justify::Between if k > 1.0 => (0.0, free / (k - 1.0)),
        Justify::Between => (0.0, 0.0),
        Justify::Around => (free / k / 2.0, free / k),
        Justify::Evenly => (free / (k + 1.0), free / (k + 1.0)),
    };
    let (mut out, mut at, mut first) = (Vec::with_capacity(sizes.len() + 1), origin + lead, true);
    for (i, s) in sizes.iter().enumerate() {
        if !collapsed.get(i).copied().unwrap_or(false) {
            if !first {
                at += gap + between;
            }
            first = false;
        }
        out.push(at);
        at += s;
    }
    out.push(at);
    (out, at)
}

impl Engine<'_> {
    /// The items of grid `n`, in `order`, placed on its lines, and its row and
    /// column tracks; `cw` is the width an `auto-fill` repetition fills.
    fn grid_items(&mut self, n: NodeId, cw: f32, inner_h: f32, y: f32, cx: f32) -> (Vec<GItem>, [Vec<Track>; 2], [Vec<bool>; 2]) {
        let st = self.st;
        let s = &st[n as usize];
        let mut kids = Vec::new();
        self.flex_children(n, &mut kids);
        let mut items: Vec<GItem> = Vec::new();
        for &c in &kids {
            let text = self.dom.nodes[c as usize].tag == crate::dom::atom::TEXT;
            if text {
                if is_ws(self.dom.text_of(c)) {
                    continue;
                }
            } else {
                let cs = &st[c as usize];
                if cs.display == crate::css::Display::None {
                    continue;
                }
                if is_abs(cs) {
                    self.abs.push((c, cx, y));
                    continue;
                }
            }
            let sd = if text { Sides::default() } else { sides(&st[c as usize], cw) };
            items.push(GItem { node: c, text, area: [0; 4], sd, first: 0, end: 0, item: NONE, h: 0.0 });
        }
        items.sort_by_key(|it| if it.text { 0 } else { st[it.node as usize].rare().order });
        let rare = s.rare();
        let axes = [expand(rare.tracks[0].as_deref(), inner_h, s.gap_row), expand(rare.tracks[1].as_deref(), cw, s.gap_col)];
        let areas = rare.areas.as_deref();
        let explicit = [axes[0].tracks.len().max(areas.map_or(0, |a| a.rows as usize)), axes[1].tracks.len().max(areas.map_or(0, |a| a.cols as usize))];
        let auto = GridPos::Auto;
        let places: Vec<[(Option<usize>, usize); 2]> = items
            .iter()
            .map(|it| {
                let place = if it.text { None } else { st[it.node as usize].rare().place.as_deref() };
                let pos = |k: usize| place.map_or(&auto, |p| &p[k]);
                [0, 1].map(|k| {
                    let start = line(&axes[k], explicit[k], areas, k, pos(k), false);
                    let end = line(&axes[k], explicit[k], areas, k, pos(k + 2), true);
                    axis_place(start, end, (pos(k), pos(k + 2)))
                })
            })
            .collect();
        // Placement runs in the flow direction: rows, or columns with `grid-auto-flow: column`.
        let (major, minor) = if s.grid_flow_col { (1, 0) } else { (0, 1) };
        let flow: Vec<[(Option<usize>, usize); 2]> = places.iter().map(|p| [p[major], p[minor]]).collect();
        let (starts, _) = auto_place(&flow, explicit[minor], rare.grid_dense);
        let mut count = explicit;
        for ((it, p), at) in items.iter_mut().zip(&places).zip(&starts) {
            let mut start = [0; 2];
            (start[major], start[minor]) = (at[0], at[1]);
            it.area = [start[0], start[1], start[0] + p[0].1, start[1] + p[1].1];
            count = [count[0].max(it.area[2]), count[1].max(it.area[3])];
        }
        let tracks = [0, 1].map(|k| {
            let mut t = axes[k].tracks.clone();
            let implicit = rare.auto_tracks[k].as_deref().filter(|a| !a.is_empty());
            let fill = Track { min: Breadth::Auto, max: Breadth::Auto };
            while t.len() < count[k] {
                let j = t.len() - axes[k].tracks.len();
                t.push(implicit.map_or(fill, |a| a[j % a.len()]));
            }
            t
        });
        let collapsed = [0, 1].map(|k| {
            let mut used = vec![false; tracks[k].len()];
            for it in &items {
                used[it.area[k]..it.area[k + 2]].fill(true);
            }
            (0..tracks[k].len()).map(|i| axes[k].fit.contains(&i) && !used[i]).collect::<Vec<bool>>()
        });
        (items, tracks, collapsed)
    }

    /// Lays out grid item `it` of grid `n` in its columns at `y`, given the height of
    /// its area (NaN when not known yet) and the height it is stretched to (NaN for none).
    fn grid_item(&mut self, n: NodeId, it: &mut GItem, (col_x, col_w): (&[f32], &[f32]), y: f32, (area_h, forced): (f32, f32)) {
        let st = self.st;
        let s = &st[n as usize];
        let x0 = col_x[it.area[1]];
        let area_w = (col_x[it.area[3] - 1] + col_w[it.area[3] - 1] - x0).max(0.0);
        it.first = self.mark();
        if it.text {
            let next = self.dom.nodes[it.node as usize].next;
            it.h = self.layout_inline(n, it.node, next, x0, area_w, y);
        } else {
            let cs = &st[it.node as usize];
            let sd = it.sd;
            let justify = if cs.justify_self == Align::Auto { s.justify_items } else { cs.justify_self };
            let stretch = justify == Align::Stretch && !sd.auto_l && !sd.auto_r;
            let room = (area_w - sd.ml - sd.mr).max(0.0);
            let w = spec_w(cs, area_w, &sd).unwrap_or_else(|| clamp_w(cs, if stretch { room } else { (self.intrinsic(it.node) - sd.ml - sd.mr).min(room) }, area_w, &sd));
            let free = (room - w).max(0.0);
            let off = match (sd.auto_l, sd.auto_r, justify) {
                (true, true, _) | (false, false, Align::Center) => free / 2.0,
                (true, false, _) | (false, false, Align::End) => free,
                _ => 0.0,
            };
            let r = self.layout_box(it.node, x0 + sd.ml + off, y + sd.mt, w, area_w, area_h, forced, true);
            (it.h, it.item) = (r.h, r.item);
        }
        it.end = self.mark();
    }

    /// Column contributions of grid items: where they start, how many columns they
    /// span, their max-content width, and the width they cannot go below (a fixed
    /// `width` or `min-width`, standing in for the min-content size).
    fn col_contrib(&mut self, items: &[GItem]) -> Vec<(usize, usize, f32, f32)> {
        let st = self.st;
        items
            .iter()
            .map(|it| {
                let (size, least) = if it.text {
                    (self.text_intrinsic(it.node), 0.0)
                } else {
                    let cs = &st[it.node as usize];
                    let px = |l: Len| if let Len::Px(v) = l { Some(v) } else { None };
                    let extra = if cs.border_box { 0.0 } else { hextra(cs, &it.sd) };
                    let least = px(cs.width).or(px(cs.rare().min_w)).map_or(0.0, |v| v + extra + it.sd.ml + it.sd.mr);
                    (self.intrinsic(it.node), least)
                };
                (it.area[1], it.area[3] - it.area[1], size, least)
            })
            .collect()
    }

    /// Max-content width of grid `n`'s content: its columns at their max-content sizes.
    pub(super) fn grid_intrinsic(&mut self, n: NodeId) -> f32 {
        let s = &self.st[n as usize];
        let gap = s.gap_col;
        let mark = self.abs.len();
        let (items, [_, cols], [_, collapsed]) = self.grid_items(n, NAN, NAN, 0.0, 0.0);
        self.abs.truncate(mark);
        let contrib = self.col_contrib(&items);
        let sizes = size_tracks(&cols, NAN, gap, &contrib, &collapsed, false);
        positions(&sizes, &collapsed, gap, 0.0, NAN, Justify::Start).1
    }

    /// Lays out grid container `n`'s items in its content box at (`cx`, `y`),
    /// `cw` wide and `inner_h` high when that is definite. Returns the content height.
    pub(super) fn layout_grid(&mut self, n: NodeId, cx: f32, cw: f32, y: f32, inner_h: f32) -> f32 {
        let st = self.st;
        let s = &st[n as usize];
        let (mut items, [rows, cols], [row_gone, col_gone]) = self.grid_items(n, cw, inner_h, y, cx);
        let col_contrib = self.col_contrib(&items);
        let col_w = size_tracks(&cols, cw, s.gap_col, &col_contrib, &col_gone, matches!(s.justify, Justify::Start));
        let (col_x, _) = positions(&col_w, &col_gone, s.gap_col, cx, cw, s.justify);
        // Rows of a fixed size give the items in them a definite height to resolve percentages against.
        let fixed_row = |t: &Track| match (t.min, t.max) {
            (Breadth::Len(a), Breadth::Len(b)) if a == b => definite(a, inner_h),
            _ => None,
        };
        let fixed_h: Vec<f32> = items
            .iter()
            .map(|it| rows[it.area[0]..it.area[2]].iter().map(fixed_row).sum::<Option<f32>>().map_or(NAN, |h| h + s.gap_row * (it.area[2] - it.area[0] - 1) as f32))
            .collect();
        let snap = self.snapshot();
        for (it, &area_h) in items.iter_mut().zip(&fixed_h) {
            self.grid_item(n, it, (&col_x[..], &col_w[..]), y, (area_h, NAN));
        }
        let row_contrib: Vec<(usize, usize, f32, f32)> = items.iter().map(|it| (it.area[0], it.area[2] - it.area[0], it.h + it.sd.mt + it.sd.mb, 0.0)).collect();
        let row_h = size_tracks(&rows, inner_h, s.gap_row, &row_contrib, &row_gone, true);
        let (row_y, bottom) = positions(&row_h, &row_gone, s.gap_row, y, inner_h, Justify::Start);
        let area_h = |it: &GItem| (row_y[it.area[2] - 1] + row_h[it.area[2] - 1] - row_y[it.area[0]]).max(0.0);
        // An item whose height depends on its grid area's (a percentage, or stretching a
        // container that places its own content by its height) is laid out again in it.
        let again = items.iter().zip(&fixed_h).any(|(it, fh)| {
            let cs = &st[it.node as usize];
            let pct = |l: Len| matches!(l, Len::Pct(_) | Len::Calc(..));
            let short = it.h + it.sd.mt + it.sd.mb + FIT < area_h(it);
            let container = cs.height == Len::Auto && short && matches!(cs.display, crate::css::Display::Flex | crate::css::Display::InlineFlex);
            !it.text && fh.is_nan() && (pct(cs.height) || pct(cs.rare().min_h) || container)
        });
        if again {
            self.rewind(snap);
            for it in items.iter_mut() {
                let cs = &st[it.node as usize];
                let align = if cs.align_self == Align::Auto { s.align_items } else { cs.align_self };
                let (h, outer_m) = (area_h(it), it.sd.mt + it.sd.mb);
                let stretch = !it.text && matches!(align, Align::Stretch | Align::Auto) && cs.height == Len::Auto;
                let forced = if stretch { (h - outer_m).max(0.0) } else { NAN };
                self.grid_item(n, it, (&col_x[..], &col_w[..]), row_y[it.area[0]], (h, forced));
            }
        }
        for it in &items {
            if again {
                let outer = it.h + it.sd.mt + it.sd.mb;
                let cs = &st[it.node as usize];
                let align = if cs.align_self == Align::Auto { s.align_items } else { cs.align_self };
                let dy = match align {
                    _ if it.text => 0.0,
                    Align::Center => (area_h(it) - outer) / 2.0,
                    Align::End => area_h(it) - outer,
                    Align::Start | Align::Stretch | Align::Auto => 0.0,
                };
                self.shift(it.first, it.end, 0.0, dy);
                continue;
            }
            let y0 = row_y[it.area[0]];
            let area_h = (row_y[it.area[2] - 1] + row_h[it.area[2] - 1] - y0).max(0.0);
            let outer = it.h + it.sd.mt + it.sd.mb;
            let mut dy = y0 - y;
            if !it.text {
                let cs = &st[it.node as usize];
                let align = if cs.align_self == Align::Auto { s.align_items } else { cs.align_self };
                match align {
                    Align::Center => dy += (area_h - outer) / 2.0,
                    Align::End => dy += area_h - outer,
                    Align::Start => {}
                    Align::Stretch | Align::Auto => {
                        if cs.height == Len::Auto && it.item != NONE && outer < area_h {
                            self.out.items[it.item as usize].h = area_h - it.sd.mt - it.sd.mb;
                        }
                    }
                }
            }
            self.shift(it.first, it.end, 0.0, dy);
        }
        bottom - y
    }
}
