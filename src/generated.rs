//! Generated content drawn in place. While layout runs, an element's `::before`
//! and `::after` boxes are its first and last children, with the styles computed
//! for them along with the element's. Afterwards the tree and the styles are put
//! back, and the layout items of those boxes are credited to the element, which
//! hit testing and scripts see in their place.

use crate::css::Display;
use crate::dom::{atom, Dom, NodeId, NONE};
use crate::layout::Layout;
use crate::style::{Generated, Piece, Style};

/// What `add` changed, for `remove` to put back.
pub struct Added {
    nodes: usize,
    attrs: usize,
    text: usize,
    styles: usize,
    /// The generated boxes put into the tree.
    boxes: Vec<NodeId>,
    /// The element each node from `nodes` on was generated for.
    owners: Vec<NodeId>,
}

/// True for an element whose content is replaced, which has no generated content.
fn replaced(dom: &Dom, n: NodeId) -> bool {
    matches!(dom.nodes[n as usize].tag, atom::INPUT | atom::IMG | atom::SVG | atom::VIDEO | atom::BR) || dom.shows_image(n)
}

/// Puts the generated boxes of every element shown into the tree, their styles
/// appended to `styles`; `None` when there are none.
pub fn add(dom: &mut Dom, styles: &mut Vec<Style>) -> Option<Added> {
    let has = |s: &Style| s.display != Display::None && (s.rare().before.is_some() || s.rare().after.is_some());
    let owners: Vec<NodeId> = (0..styles.len().min(dom.nodes.len()) as NodeId).filter(|&n| has(&styles[n as usize]) && dom.is_element(n) && !replaced(dom, n)).collect();
    if owners.is_empty() {
        return None;
    }
    let mut added = Added { nodes: dom.nodes.len(), attrs: dom.attrs.len(), text: dom.text.len(), styles: styles.len(), boxes: Vec::new(), owners: Vec::new() };
    styles.resize(dom.nodes.len(), Style::initial());
    for n in owners {
        let generated = [(styles[n as usize].rare().before.clone(), true), (styles[n as usize].rare().after.clone(), false)];
        for (g, first) in generated {
            let Some(g) = g else { continue };
            let b = dom.create_element(if first { "::before" } else { "::after" });
            let at = if first { dom.nodes[n as usize].first_child } else { NONE };
            dom.insert_before(n, b, at);
            styles.push(g.style.clone());
            added.boxes.push(b);
            added.owners.push(n);
            add_content(dom, styles, &g, b);
            added.owners.resize(dom.nodes.len() - added.nodes, n);
        }
    }
    Some(added)
}

/// The text and images of generated box `b`, as its children.
fn add_content(dom: &mut Dom, styles: &mut Vec<Style>, g: &Generated, b: NodeId) {
    for piece in g.content.iter() {
        let c = match piece {
            Piece::Text(t) => {
                styles.push(Style::initial());
                dom.create_text(t)
            }
            Piece::Image(url) => {
                styles.push(Generated::image_style(&g.style));
                let c = dom.create_element("img");
                dom.set_attr(c, "src", Some(url));
                c
            }
        };
        dom.insert_before(b, c, NONE);
    }
}

/// Takes the generated boxes out of the tree again and credits their items in
/// `layout` to the elements they were generated for.
pub fn remove(dom: &mut Dom, styles: &mut Vec<Style>, added: Added, layout: &mut Layout) {
    for it in &mut layout.items {
        if let Some(&owner) = (it.node as usize).checked_sub(added.nodes).and_then(|k| added.owners.get(k)) {
            it.node = owner;
        }
    }
    for &b in added.boxes.iter().rev() {
        dom.detach(b);
    }
    dom.nodes.truncate(added.nodes);
    dom.attrs.truncate(added.attrs);
    dom.text.truncate(added.text);
    styles.truncate(added.styles);
}
