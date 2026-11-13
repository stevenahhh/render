//! Prints the element outline of parsed HTML files, one "<depth> <tag>" line per element,
//! in the format of WebKit's outline for diffing: `domdump <in.html> <out.txt>...` in pairs.
use render::dom::{parse_html, Dom, NodeId, NONE};

fn walk(dom: &Dom, n: NodeId, depth: usize, out: &mut String) {
    let mut c = dom.nodes[n as usize].first_child;
    while c != NONE {
        if dom.is_element(c) {
            out.push_str(&format!("{depth} {}\n", dom.atoms.name(dom.nodes[c as usize].tag)));
            walk(dom, c, depth + 1, out);
        } else if dom.nodes[c as usize].tag == render::dom::atom::TEXT {
            let t = dom.text_of(c).split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                out.push_str(&format!("{depth} \"{}\"\n", t.chars().take(40).collect::<String>()));
            }
        }
        c = dom.nodes[c as usize].next;
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for pair in args.chunks(2) {
        let bytes = std::fs::read(&pair[0]).expect("read input");
        let dom = parse_html(&String::from_utf8_lossy(&bytes));
        let mut out = String::new();
        walk(&dom, 0, 0, &mut out);
        std::fs::write(&pair[1], out).expect("write output");
    }
}
