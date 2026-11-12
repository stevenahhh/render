//! Shadow trees drawn in place. While style and layout run, a shadow host's
//! children are those of its shadow root, and a slot's children are the host
//! children assigned to it, or its own fallback content when none are. The
//! rules of a shadow tree's `<style>` elements match only inside that tree,
//! and the document's class and id rules do not reach into it.

use crate::css::{Compound, Pseudo, Sheet};
use crate::dom::{atom, Atom, Dom, NodeId, NONE};
use crate::FxMap;

/// What `compose` changed, for `restore` to put back.
pub struct Composed {
    /// Original `[parent, first child, last child, previous, next]` of each relinked node.
    links: FxMap<NodeId, [u32; 5]>,
    /// Original id and class range of each element of a shadow tree.
    names: Vec<(NodeId, Atom, (u32, u32))>,
    classes: usize,
}

fn links(dom: &Dom, n: NodeId) -> [u32; 5] {
    let node = &dom.nodes[n as usize];
    [node.parent, node.first_child, node.last_child, node.prev, node.next]
}

fn children(dom: &Dom, n: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut c = dom.nodes[n as usize].first_child;
    while c != NONE {
        out.push(c);
        c = dom.nodes[c as usize].next;
    }
    out
}

/// The name of the slot a host child asks for: its `slot` attribute, empty for
/// the default slot; `None` for a comment, which no slot takes.
fn slot_of(dom: &Dom, n: NodeId) -> Option<String> {
    if dom.is_comment(n) {
        return None;
    }
    if !dom.is_element(n) {
        return Some(String::new());
    }
    let name = dom.atoms.get("slot").and_then(|a| dom.attr(n, a));
    Some(name.unwrap_or("").to_string())
}

/// `name` as written inside the shadow tree `root`.
fn scoped(dom: &mut Dom, name: Atom, root: NodeId) -> Atom {
    let s = format!("{}\u{e000}{root}", dom.atoms.name(name));
    dom.atoms.intern(&s)
}

/// The class every element of the shadow tree `root` carries while it is drawn.
fn scope_class(dom: &mut Dom, root: NodeId) -> Atom {
    dom.atoms.intern(&format!("\u{e001}{root}"))
}

/// The shadow roots of connected hosts, in a fixed order.
fn roots(dom: &Dom) -> Vec<(NodeId, NodeId)> {
    let mut list: Vec<(NodeId, NodeId)> = dom.shadows.iter().map(|(&h, &r)| (h, r)).filter(|&(h, _)| dom.connected(h)).collect();
    list.sort_unstable();
    list
}

/// Relinks the tree into its composed form; `None` when there is no shadow tree.
pub fn compose(dom: &mut Dom) -> Option<Composed> {
    let roots = roots(dom);
    if roots.is_empty() {
        return None;
    }
    let mut lists: Vec<(NodeId, Vec<NodeId>)> = Vec::new();
    for &(host, root) in &roots {
        lists.push((host, children(dom, root)));
        let light: Vec<(NodeId, Option<String>)> = children(dom, host).into_iter().map(|c| (c, slot_of(dom, c))).collect();
        let mut taken: Vec<String> = Vec::new();
        let mut n = dom.next_in(root, root);
        while n != NONE {
            if dom.nodes[n as usize].tag == atom::SLOT {
                let name = dom.atoms.get("name").and_then(|a| dom.attr(n, a)).unwrap_or("").to_string();
                if !taken.contains(&name) {
                    let assigned: Vec<NodeId> = light.iter().filter(|(_, s)| s.as_deref() == Some(&*name)).map(|&(c, _)| c).collect();
                    if !assigned.is_empty() {
                        lists.push((n, assigned));
                    }
                    taken.push(name);
                }
            }
            n = dom.next_in(n, root);
        }
    }
    let mut names = Vec::new();
    let classes = dom.classes.len();
    for &(_, root) in &roots {
        let scope = scope_class(dom, root);
        let mut n = dom.next_in(root, root);
        while n != NONE {
            if dom.is_element(n) {
                let (id, range) = (dom.nodes[n as usize].id, dom.nodes[n as usize].classes);
                names.push((n, id, range));
                let own: Vec<Atom> = dom.classes_of(n).to_vec();
                let start = dom.classes.len() as u32;
                for c in own {
                    let s = scoped(dom, c, root);
                    dom.classes.push(s);
                }
                dom.classes.push(scope);
                dom.nodes[n as usize].classes = (start, dom.classes.len() as u32 - start);
                if id != 0 {
                    dom.nodes[n as usize].id = scoped(dom, id, root);
                }
            }
            n = dom.next_in(n, root);
        }
    }
    let mut saved: FxMap<NodeId, [u32; 5]> = FxMap::default();
    for (parent, kids) in lists {
        saved.entry(parent).or_insert_with(|| links(dom, parent));
        for &k in &kids {
            saved.entry(k).or_insert_with(|| links(dom, k));
        }
        let node = &mut dom.nodes[parent as usize];
        node.first_child = kids.first().copied().unwrap_or(NONE);
        node.last_child = kids.last().copied().unwrap_or(NONE);
        for (i, &k) in kids.iter().enumerate() {
            let node = &mut dom.nodes[k as usize];
            node.parent = parent;
            node.prev = if i == 0 { NONE } else { kids[i - 1] };
            node.next = kids.get(i + 1).copied().unwrap_or(NONE);
        }
    }
    Some(Composed { links: saved, names, classes })
}

/// Puts the tree back as scripts see it.
pub fn restore(dom: &mut Dom, composed: Composed) {
    for (n, [parent, first, last, prev, next]) in composed.links {
        let node = &mut dom.nodes[n as usize];
        (node.parent, node.first_child, node.last_child, node.prev, node.next) = (parent, first, last, prev, next);
    }
    for (n, id, range) in composed.names {
        let node = &mut dom.nodes[n as usize];
        (node.id, node.classes) = (id, range);
    }
    dom.classes.truncate(composed.classes);
}

/// Makes a compound name the classes and ids of the shadow tree `root`, in the
/// selectors of its `:is()`, `:where()`, `:not()` and `:has()` too.
fn scope_compound(dom: &mut Dom, c: &mut Compound, root: NodeId) {
    for class in &mut c.classes {
        *class = scoped(dom, *class, root);
    }
    if c.id != 0 {
        c.id = scoped(dom, c.id, root);
    }
    for p in &mut c.pseudos {
        match p {
            Pseudo::Is(list) | Pseudo::Not(list) => {
                for part in list.iter_mut().flatten() {
                    scope_compound(dom, &mut part.c, root);
                }
            }
            Pseudo::Has(parts) => {
                for part in parts {
                    scope_compound(dom, &mut part.c, root);
                }
            }
            _ => {}
        }
    }
}

/// Adds the `<style>` rules of every shadow tree to `sheet`, each made to match
/// only the elements of its own tree.
pub fn add_scoped_sheets(dom: &mut Dom, sheet: &mut Sheet) {
    for (_, root) in roots(dom) {
        let first = sheet.sels.len();
        let mut n = dom.next_in(root, root);
        while n != NONE {
            if dom.nodes[n as usize].tag == atom::STYLE {
                let mut css = String::new();
                dom.text_content(n, &mut css);
                sheet.add(&css, &mut dom.atoms, 1, "");
            }
            n = dom.next_in(n, root);
        }
        let scope = scope_class(dom, root);
        for sel in &mut sheet.sels[first..] {
            for part in &mut sel.parts {
                scope_compound(dom, &mut part.c, root);
            }
            sel.parts[0].c.classes.push(scope);
            // The ancestor filter knows the names as written in the document.
            sel.anc_n = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{compose, restore};
    use crate::dom::{atom, parse_html, NodeId, NONE};

    fn kids(dom: &crate::dom::Dom, n: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut c = dom.nodes[n as usize].first_child;
        while c != NONE {
            out.push(c);
            c = dom.nodes[c as usize].next;
        }
        out
    }

    #[test]
    fn hosts_show_their_shadow_tree_with_children_in_slots() {
        let mut dom = parse_html("<body><div id=h><b>light</b><i slot=t>titled</i><u slot=none>lost</u></div></body>");
        let host = dom.by_id(0, "h");
        let light = kids(&dom, host);
        let root = dom.attach_shadow(host);
        dom.set_inner_html(root, "<p><slot name=t>fallback</slot></p><slot><em>fallback</em></slot><slot name=missing><s>shown</s></slot>");
        let before: Vec<_> = (0..dom.nodes.len() as NodeId).map(|n| super::links(&dom, n)).collect();
        let composed = compose(&mut dom).unwrap();
        let shown = kids(&dom, host);
        assert_eq!(shown, kids(&dom, root));
        let [p, default, missing] = [shown[0], shown[1], shown[2]];
        let named = dom.nodes[p as usize].first_child;
        assert_eq!(kids(&dom, named), vec![light[1]]);
        assert_eq!(kids(&dom, default), vec![light[0]]);
        assert_eq!(dom.nodes[light[0] as usize].parent, default);
        assert_eq!(dom.nodes[dom.nodes[missing as usize].first_child as usize].tag, dom.atoms.get("s").unwrap());
        assert!(dom.classes_of(p).len() == 1 && dom.nodes[p as usize].tag == atom::P);
        restore(&mut dom, composed);
        let after: Vec<_> = (0..dom.nodes.len() as NodeId).map(|n| super::links(&dom, n)).collect();
        assert_eq!(before, after);
        assert!(dom.classes_of(p).is_empty());
    }
}
