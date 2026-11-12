//! Arena DOM and a forgiving, single-pass HTML parser.
//!
//! Nodes live in one `Vec` and link to each other by index. Tag names, ids,
//! classes and attribute names are interned, so selector matching compares
//! integers. All text and attribute values share one string buffer.

use crate::FxMap;
use memchr::{memchr, memmem};

pub type Atom = u32;
pub type NodeId = u32;
pub const NONE: u32 = u32::MAX;

macro_rules! atoms {
    ($($name:ident = $s:expr),* $(,)?) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms, dead_code)]
        #[repr(u32)]
        enum AtomIdx { EMPTY_ = 0, $($name),* }
        /// Atoms that are interned up front, in this order.
        pub mod atom {
            $(pub const $name: u32 = super::AtomIdx::$name as u32;)*
        }
        const PREDEFINED: &[&str] = &["", $($s),*];
    };
}

atoms! {
    DOCUMENT = "#document", TEXT = "#text",
    HTML = "html", HEAD = "head", BODY = "body", TITLE = "title", STYLE = "style",
    SCRIPT = "script", LINK = "link", META = "meta", BASE = "base",
    P = "p", DIV = "div", SPAN = "span", A = "a", BR = "br", HR = "hr", IMG = "img",
    INPUT = "input", LI = "li", UL = "ul", OL = "ol", DL = "dl", DT = "dt", DD = "dd",
    TABLE = "table", THEAD = "thead", TBODY = "tbody", TFOOT = "tfoot", TR = "tr",
    TD = "td", TH = "th", CAPTION = "caption", BUTTON = "button", OPTION = "option",
    SELECT = "select", TEXTAREA = "textarea", PRE = "pre",
    H1 = "h1", H2 = "h2", H3 = "h3", H4 = "h4", H5 = "h5", H6 = "h6",
    ADDRESS = "address", ARTICLE = "article", ASIDE = "aside", BLOCKQUOTE = "blockquote",
    DETAILS = "details", FIELDSET = "fieldset", FIGCAPTION = "figcaption",
    FIGURE = "figure", FOOTER = "footer", FORM = "form", HEADER = "header",
    MAIN = "main", NAV = "nav", SECTION = "section", SUMMARY = "summary",
    AREA = "area", COL = "col", EMBED = "embed", SOURCE = "source", TRACK = "track",
    WBR = "wbr",
    ID = "id", CLASS = "class", HREF = "href", REL = "rel", TYPE = "type",
    VALUE = "value", PLACEHOLDER = "placeholder", ALT = "alt", WIDTH = "width",
    HEIGHT = "height", START = "start", SRC = "src", SRCSET = "srcset", MEDIA = "media",
    PICTURE = "picture", NOSCRIPT = "noscript", SVG = "svg", VIDEO = "video", POSTER = "poster",
}

pub struct Interner {
    map: FxMap<Box<str>, Atom>,
    names: Vec<Box<str>>,
    scratch: String,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub fn new() -> Self {
        let mut i = Interner { map: FxMap::default(), names: Vec::new(), scratch: String::new() };
        for s in PREDEFINED {
            i.intern(s);
        }
        i
    }

    pub fn intern(&mut self, s: &str) -> Atom {
        if let Some(&a) = self.map.get(s) {
            return a;
        }
        let a = self.names.len() as Atom;
        self.names.push(s.into());
        self.map.insert(s.into(), a);
        a
    }

    /// Interns the ASCII-lowercased form without allocating in the common case.
    pub fn intern_lower(&mut self, s: &str) -> Atom {
        if !s.bytes().any(|b| b.is_ascii_uppercase()) {
            return self.intern(s);
        }
        let mut t = std::mem::take(&mut self.scratch);
        t.clear();
        t.push_str(s);
        t.make_ascii_lowercase();
        let a = self.intern(&t);
        self.scratch = t;
        a
    }

    pub fn name(&self, a: Atom) -> &str {
        &self.names[a as usize]
    }
}

pub struct Node {
    pub parent: u32,
    pub first_child: u32,
    pub last_child: u32,
    pub prev: u32,
    pub next: u32,
    /// `atom::TEXT` for text nodes, `atom::DOCUMENT` for the root.
    pub tag: Atom,
    /// Interned `id` attribute, 0 when absent.
    pub id: Atom,
    /// Text nodes: (start, len) in `Dom::text`.
    pub text: (u32, u32),
    /// (start, count) in `Dom::attrs`.
    pub attrs: (u32, u32),
    /// (start, count) in `Dom::classes`.
    pub classes: (u32, u32),
}

pub struct Attr {
    pub name: Atom,
    pub value: (u32, u32),
}

pub struct Dom {
    pub nodes: Vec<Node>,
    pub attrs: Vec<Attr>,
    pub classes: Vec<Atom>,
    pub text: String,
    pub atoms: Interner,
}

impl Dom {
    pub fn new(cap: usize) -> Dom {
        let mut d = Dom {
            nodes: Vec::with_capacity(cap / 24 + 8),
            attrs: Vec::new(),
            classes: Vec::new(),
            text: String::with_capacity(cap),
            atoms: Interner::new(),
        };
        d.new_node(atom::DOCUMENT);
        d
    }

    fn new_node(&mut self, tag: Atom) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(Node {
            parent: NONE,
            first_child: NONE,
            last_child: NONE,
            prev: NONE,
            next: NONE,
            tag,
            id: 0,
            text: (0, 0),
            attrs: (0, 0),
            classes: (0, 0),
        });
        id
    }

    fn append(&mut self, parent: NodeId, child: NodeId) {
        let last = self.nodes[parent as usize].last_child;
        {
            let c = &mut self.nodes[child as usize];
            c.parent = parent;
            c.prev = last;
        }
        if last == NONE {
            self.nodes[parent as usize].first_child = child;
        } else {
            self.nodes[last as usize].next = child;
        }
        self.nodes[parent as usize].last_child = child;
    }

    #[inline]
    pub fn is_element(&self, n: NodeId) -> bool {
        let t = self.nodes[n as usize].tag;
        t != atom::TEXT && t != atom::DOCUMENT
    }

    /// True for an element whose content is an image: `<img>`, inline `<svg>`,
    /// and a `<video>` with a poster, which stands still on its poster frame.
    pub fn shows_image(&self, n: NodeId) -> bool {
        match self.nodes[n as usize].tag {
            atom::IMG | atom::SVG => true,
            atom::VIDEO => self.attr(n, atom::POSTER).is_some_and(|p| !p.is_empty()),
            _ => false,
        }
    }

    #[inline]
    pub fn text_of(&self, n: NodeId) -> &str {
        let (s, l) = self.nodes[n as usize].text;
        &self.text[s as usize..(s + l) as usize]
    }

    pub fn attr(&self, n: NodeId, name: Atom) -> Option<&str> {
        let (s, c) = self.nodes[n as usize].attrs;
        self.attrs[s as usize..(s + c) as usize].iter().find(|a| a.name == name).map(|a| {
            let (vs, vl) = a.value;
            &self.text[vs as usize..(vs + vl) as usize]
        })
    }

    #[inline]
    pub fn classes_of(&self, n: NodeId) -> &[Atom] {
        let (s, c) = self.nodes[n as usize].classes;
        &self.classes[s as usize..(s + c) as usize]
    }

    pub fn create_element(&mut self, tag: &str) -> NodeId {
        let a = self.atoms.intern_lower(tag);
        self.new_node(a)
    }

    pub fn create_text(&mut self, s: &str) -> NodeId {
        let n = self.new_node(atom::TEXT);
        self.set_text(n, s);
        n
    }

    pub fn set_text(&mut self, n: NodeId, s: &str) {
        let start = self.text.len() as u32;
        self.text.push_str(s);
        self.nodes[n as usize].text = (start, s.len() as u32);
    }

    /// Unlinks a node from its parent. The node stays in the arena.
    pub fn detach(&mut self, n: NodeId) {
        let (p, prev, next) = {
            let node = &mut self.nodes[n as usize];
            let old = (node.parent, node.prev, node.next);
            (node.parent, node.prev, node.next) = (NONE, NONE, NONE);
            old
        };
        if p == NONE {
            return;
        }
        if prev == NONE {
            self.nodes[p as usize].first_child = next;
        } else {
            self.nodes[prev as usize].next = next;
        }
        if next == NONE {
            self.nodes[p as usize].last_child = prev;
        } else {
            self.nodes[next as usize].prev = prev;
        }
    }

    /// Moves `child` under `parent` before `before` (or last when `NONE`).
    /// Refuses to create a cycle.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: NodeId) -> bool {
        let mut a = parent;
        while a != NONE {
            if a == child {
                return false;
            }
            a = self.nodes[a as usize].parent;
        }
        self.detach(child);
        if before == NONE || self.nodes[before as usize].parent != parent {
            self.append(parent, child);
            return true;
        }
        let prev = self.nodes[before as usize].prev;
        {
            let c = &mut self.nodes[child as usize];
            (c.parent, c.prev, c.next) = (parent, prev, before);
        }
        self.nodes[before as usize].prev = child;
        if prev == NONE {
            self.nodes[parent as usize].first_child = child;
        } else {
            self.nodes[prev as usize].next = child;
        }
        true
    }

    /// Keeps the interned id and class list in step with their attributes.
    fn sync_names(&mut self, n: NodeId, name: Atom, value: &str) {
        if name == atom::ID {
            self.nodes[n as usize].id = if value.is_empty() { 0 } else { self.atoms.intern(value) };
        } else if name == atom::CLASS {
            let start = self.classes.len() as u32;
            for c in value.split_ascii_whitespace() {
                let a = self.atoms.intern(c);
                self.classes.push(a);
            }
            self.nodes[n as usize].classes = (start, self.classes.len() as u32 - start);
        }
    }

    /// Sets or, with `None`, removes an attribute. Attribute storage is
    /// append-only, so a changed set is copied to the end of the buffer.
    pub fn set_attr(&mut self, n: NodeId, name: &str, value: Option<&str>) {
        let name = self.atoms.intern_lower(name);
        let (s, c) = self.nodes[n as usize].attrs;
        let range = s as usize..(s + c) as usize;
        let found = self.attrs[range.clone()].iter().position(|a| a.name == name);
        let stored = value.map(|v| {
            let start = self.text.len() as u32;
            self.text.push_str(v);
            (start, v.len() as u32)
        });
        match (found, stored) {
            (Some(i), Some(v)) => self.attrs[range.start + i].value = v,
            (None, None) => return,
            (found, stored) => {
                let start = self.attrs.len() as u32;
                for i in range.clone() {
                    if Some(i - range.start) != found {
                        let a = Attr { name: self.attrs[i].name, value: self.attrs[i].value };
                        self.attrs.push(a);
                    }
                }
                if let Some(value) = stored {
                    self.attrs.push(Attr { name, value });
                }
                self.nodes[n as usize].attrs = (start, self.attrs.len() as u32 - start);
            }
        }
        self.sync_names(n, name, value.unwrap_or(""));
    }

    /// Replaces the children of `n` with the parsed fragment.
    pub fn set_inner_html(&mut self, n: NodeId, html: &str) {
        while self.nodes[n as usize].first_child != NONE {
            let c = self.nodes[n as usize].first_child;
            self.detach(c);
        }
        Parser { src: html, b: html.as_bytes(), pos: 0, dom: self, stack: vec![n, n, n], html: n, head: NONE, body: n, fragment: true }.run();
    }

    /// Serializes a node (`outer`) or only its children back to HTML.
    pub fn serialize(&self, n: NodeId, outer: bool, out: &mut String) {
        let node = &self.nodes[n as usize];
        if node.tag == atom::TEXT {
            let raw = matches!(self.nodes.get(node.parent as usize).map(|p| p.tag), Some(atom::SCRIPT | atom::STYLE));
            for c in self.text_of(n).chars() {
                match c {
                    '&' if !raw => out.push_str("&amp;"),
                    '<' if !raw => out.push_str("&lt;"),
                    '>' if !raw => out.push_str("&gt;"),
                    c => out.push(c),
                }
            }
            return;
        }
        let name = self.atoms.name(node.tag);
        if outer {
            out.push('<');
            out.push_str(name);
            let (s, c) = node.attrs;
            for a in &self.attrs[s as usize..(s + c) as usize] {
                out.push(' ');
                out.push_str(self.atoms.name(a.name));
                out.push_str("=\"");
                let (vs, vl) = a.value;
                out.push_str(&self.text[vs as usize..(vs + vl) as usize].replace('&', "&amp;").replace('"', "&quot;"));
                out.push('"');
            }
            out.push('>');
            if is_void(node.tag) {
                return;
            }
        }
        let mut c = node.first_child;
        while c != NONE {
            self.serialize(c, true, out);
            c = self.nodes[c as usize].next;
        }
        if outer {
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
    }

    /// Concatenated text of all descendants.
    pub fn text_content(&self, n: NodeId, out: &mut String) {
        let mut c = self.nodes[n as usize].first_child;
        while c != NONE {
            if self.nodes[c as usize].tag == atom::TEXT {
                out.push_str(self.text_of(c));
            } else {
                self.text_content(c, out);
            }
            c = self.nodes[c as usize].next;
        }
    }
}

pub fn parse_html(src: &str) -> Dom {
    let mut dom = Dom::new(src.len());
    Parser { src, b: src.as_bytes(), pos: 0, dom: &mut dom, stack: vec![0], html: NONE, head: NONE, body: NONE, fragment: false }.run();
    dom
}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    dom: &'a mut Dom,
    /// Parsing `innerHTML`: the context node fills the three protected stack slots.
    fragment: bool,
    /// Open elements: [document, html, head|body, ...].
    stack: Vec<NodeId>,
    html: NodeId,
    head: NodeId,
    body: NodeId,
}

#[inline]
fn is_name(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b':'
}

fn is_void(t: Atom) -> bool {
    use atom::*;
    matches!(t, AREA | BASE | BR | COL | EMBED | HR | IMG | INPUT | LINK | META | SOURCE | TRACK | WBR)
}

fn is_head_tag(t: Atom) -> bool {
    use atom::*;
    matches!(t, TITLE | META | LINK | STYLE | SCRIPT | BASE)
}

fn closes_p(t: Atom) -> bool {
    use atom::*;
    matches!(
        t,
        ADDRESS | ARTICLE | ASIDE | BLOCKQUOTE | DETAILS | DIV | DL | FIELDSET | FIGCAPTION
            | FIGURE | FOOTER | FORM | H1 | H2 | H3 | H4 | H5 | H6 | HEADER | HR | MAIN | NAV
            | OL | P | PRE | SECTION | TABLE | UL | LI | DD | DT | SUMMARY
    )
}

impl<'a> Parser<'a> {
    fn run(&mut self) {
        let b = self.b;
        let len = b.len();
        while self.pos < len {
            let lt = match memchr(b'<', &b[self.pos..]) {
                Some(o) => self.pos + o,
                None => len,
            };
            if lt > self.pos {
                let src = self.src;
                self.text(&src[self.pos..lt]);
            }
            self.pos = lt;
            if lt >= len {
                break;
            }
            let nx = if lt + 1 < len { b[lt + 1] } else { 0 };
            if nx == b'!' {
                self.pos = if b[lt..].starts_with(b"<!--") {
                    memmem::find(&b[lt + 4..], b"-->").map_or(len, |o| lt + 4 + o + 3)
                } else {
                    memchr(b'>', &b[lt..]).map_or(len, |o| lt + o + 1)
                };
            } else if nx == b'/' {
                let ns = lt + 2;
                let mut e = ns;
                while e < len && is_name(b[e]) {
                    e += 1;
                }
                self.pos = memchr(b'>', &b[e..]).map_or(len, |o| e + o + 1);
                if e > ns {
                    let tag = self.dom.atoms.intern_lower(&self.src[ns..e]);
                    self.end_tag(tag);
                }
            } else if nx.is_ascii_alphabetic() {
                self.start_tag();
            } else if nx == b'?' {
                self.pos = memchr(b'>', &b[lt..]).map_or(len, |o| lt + o + 1);
            } else {
                self.text("<");
                self.pos = lt + 1;
            }
        }
    }

    fn ensure_html(&mut self) {
        if self.html == NONE {
            self.html = self.dom.new_node(atom::HTML);
            self.dom.append(0, self.html);
            self.stack.truncate(1);
            self.stack.push(self.html);
        }
    }

    fn ensure_head_open(&mut self) {
        self.ensure_html();
        if self.head == NONE {
            self.head = self.dom.new_node(atom::HEAD);
            self.dom.append(self.html, self.head);
            self.stack.truncate(2);
            self.stack.push(self.head);
        } else if !self.stack.contains(&self.head) {
            self.stack.truncate(2);
            self.stack.push(self.head);
        }
    }

    fn ensure_body_open(&mut self) {
        self.ensure_html();
        if self.body == NONE {
            self.body = self.dom.new_node(atom::BODY);
            self.dom.append(self.html, self.body);
            self.stack.truncate(2);
            self.stack.push(self.body);
        } else if self.stack.len() < 3 || self.stack[2] != self.body {
            self.stack.truncate(2);
            self.stack.push(self.body);
        }
    }

    fn text(&mut self, t: &str) {
        let ws = t.bytes().all(|c| c.is_ascii_whitespace());
        if self.stack.len() <= 2 || self.body == NONE {
            // Outside <body>: whitespace is insignificant, real text opens the body.
            if ws {
                return;
            }
            self.ensure_body_open();
        }
        let start = self.dom.text.len() as u32;
        push_decoded(&mut self.dom.text, t);
        let n = self.dom.new_node(atom::TEXT);
        self.dom.nodes[n as usize].text = (start, self.dom.text.len() as u32 - start);
        let parent = *self.stack.last().unwrap();
        self.dom.append(parent, n);
    }

    fn set_attrs(&mut self, n: NodeId, attrs: (u32, u32), classes: (u32, u32), id: Atom) {
        let node = &mut self.dom.nodes[n as usize];
        if node.attrs.1 == 0 {
            node.attrs = attrs;
            node.classes = classes;
            node.id = id;
        }
    }

    fn start_tag(&mut self) {
        let b = self.b;
        let src = self.src;
        let len = b.len();
        let mut i = self.pos + 1;
        let ns = i;
        while i < len && is_name(b[i]) {
            i += 1;
        }
        let tag = self.dom.atoms.intern_lower(&src[ns..i]);
        let attr_start = self.dom.attrs.len() as u32;
        let class_start = self.dom.classes.len() as u32;
        let mut id = 0;
        let mut self_closing = false;
        loop {
            while i < len && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= len {
                break;
            }
            if b[i] == b'>' {
                self_closing = b[i - 1] == b'/';
                i += 1;
                break;
            }
            let an = i;
            while i < len && !matches!(b[i], b'=' | b'>' | b'/') && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i == an {
                i += 1; // stray '/' or '='
                continue;
            }
            let name = self.dom.atoms.intern_lower(&src[an..i]);
            while i < len && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let mut val = "";
            if i < len && b[i] == b'=' {
                i += 1;
                while i < len && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i < len && (b[i] == b'"' || b[i] == b'\'') {
                    let q = b[i];
                    i += 1;
                    let vs = i;
                    i = memchr(q, &b[i..]).map_or(len, |o| i + o);
                    val = &src[vs..i];
                    if i < len {
                        i += 1;
                    }
                } else {
                    let vs = i;
                    while i < len && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    val = &src[vs..i];
                }
            }
            let start = self.dom.text.len();
            push_decoded(&mut self.dom.text, val);
            let vl = (self.dom.text.len() - start) as u32;
            self.dom.attrs.push(Attr { name, value: (start as u32, vl) });
            if name == atom::ID {
                id = self.dom.atoms.intern(&self.dom.text[start..]);
            } else if name == atom::CLASS {
                for c in self.dom.text[start..].split_ascii_whitespace() {
                    let a = self.dom.atoms.intern(c);
                    self.dom.classes.push(a);
                }
            }
        }
        self.pos = i;
        let attrs = (attr_start, self.dom.attrs.len() as u32 - attr_start);
        let classes = (class_start, self.dom.classes.len() as u32 - class_start);

        match tag {
            atom::HTML | atom::HEAD | atom::BODY if self.fragment => return,
            atom::HTML => {
                self.ensure_html();
                self.set_attrs(self.html, attrs, classes, id);
                return;
            }
            atom::HEAD => {
                if self.body == NONE {
                    self.ensure_head_open();
                }
                return;
            }
            atom::BODY => {
                self.ensure_body_open();
                self.set_attrs(self.body, attrs, classes, id);
                return;
            }
            _ => {}
        }
        if self.body == NONE && is_head_tag(tag) {
            self.ensure_head_open();
        } else {
            self.ensure_body_open();
        }
        self.auto_close(tag);

        let n = self.dom.new_node(tag);
        {
            let node = &mut self.dom.nodes[n as usize];
            node.attrs = attrs;
            node.classes = classes;
            node.id = id;
        }
        let parent = *self.stack.last().unwrap();
        self.dom.append(parent, n);

        let in_svg = || tag == atom::SVG || self.stack.iter().any(|&s| self.dom.nodes[s as usize].tag == atom::SVG);
        if is_void(tag) || (self_closing && in_svg()) {
            return;
        }
        let raw = matches!(tag, atom::SCRIPT | atom::STYLE);
        if raw || matches!(tag, atom::TITLE | atom::TEXTAREA) {
            let name = self.dom.atoms.name(tag).to_string();
            let (end, after) = self.find_close(&name);
            if end > self.pos {
                let start = self.dom.text.len() as u32;
                if raw {
                    self.dom.text.push_str(&src[self.pos..end]);
                } else {
                    push_decoded(&mut self.dom.text, &src[self.pos..end]);
                }
                let t = self.dom.new_node(atom::TEXT);
                self.dom.nodes[t as usize].text = (start, self.dom.text.len() as u32 - start);
                self.dom.append(n, t);
            }
            self.pos = after;
            return;
        }
        self.stack.push(n);
        if tag == atom::PRE && self.pos < len && b[self.pos] == b'\n' {
            self.pos += 1;
        }
    }

    /// Finds `</name` from the current position: (content end, position after the tag).
    fn find_close(&self, name: &str) -> (usize, usize) {
        let b = self.b;
        let len = b.len();
        let nb = name.as_bytes();
        let mut i = self.pos;
        while let Some(o) = memchr(b'<', &b[i..]) {
            let lt = i + o;
            let ne = lt + 2 + nb.len();
            if lt + 1 < len
                && b[lt + 1] == b'/'
                && ne <= len
                && b[lt + 2..ne].eq_ignore_ascii_case(nb)
                && (ne == len || !is_name(b[ne]))
            {
                return (lt, memchr(b'>', &b[ne..]).map_or(len, |o| ne + o + 1));
            }
            i = lt + 1;
        }
        (len, len)
    }

    /// Pops through the nearest open `tag`, stopping at any boundary tag.
    fn close_if_open(&mut self, tag: Atom, bounds: &[Atom]) {
        for i in (3..self.stack.len()).rev() {
            let t = self.dom.nodes[self.stack[i] as usize].tag;
            if t == tag {
                self.stack.truncate(i);
                return;
            }
            if bounds.contains(&t) {
                return;
            }
        }
    }

    fn auto_close(&mut self, tag: Atom) {
        use atom::*;
        if closes_p(tag) {
            self.close_if_open(P, &[TABLE, TD, TH, CAPTION, BUTTON]);
        }
        match tag {
            LI => self.close_if_open(LI, &[UL, OL, TABLE, TD, TH]),
            DT | DD => {
                self.close_if_open(DT, &[DL]);
                self.close_if_open(DD, &[DL]);
            }
            TR => self.close_if_open(TR, &[TABLE]),
            TD | TH => {
                self.close_if_open(TD, &[TR, TABLE]);
                self.close_if_open(TH, &[TR, TABLE]);
            }
            THEAD | TBODY | TFOOT => {
                self.close_if_open(THEAD, &[TABLE]);
                self.close_if_open(TBODY, &[TABLE]);
                self.close_if_open(TFOOT, &[TABLE]);
            }
            OPTION => self.close_if_open(OPTION, &[SELECT]),
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: Atom) {
        match tag {
            atom::HTML | atom::BODY => return,
            atom::HEAD => {
                if self.body == NONE && self.stack.len() > 2 {
                    self.stack.truncate(2);
                }
                return;
            }
            _ => {}
        }
        for i in (3..self.stack.len()).rev() {
            if self.dom.nodes[self.stack[i] as usize].tag == tag {
                self.stack.truncate(i);
                return;
            }
        }
    }
}

/// Appends `s` to `out`, decoding character references.
pub fn push_decoded(out: &mut String, s: &str) {
    let b = s.as_bytes();
    let mut i = 0;
    while let Some(off) = memchr(b'&', &b[i..]) {
        let amp = i + off;
        out.push_str(&s[i..amp]);
        let end = (amp + 12).min(b.len());
        let mut done = false;
        if let Some(semi) = b[amp + 1..end].iter().position(|&c| c == b';') {
            if let Some(c) = entity(&b[amp + 1..amp + 1 + semi]) {
                out.push(c);
                i = amp + semi + 2;
                done = true;
            }
        }
        if !done {
            out.push('&');
            i = amp + 1;
        }
    }
    out.push_str(&s[i..]);
}

fn entity(name: &[u8]) -> Option<char> {
    if let Some(num) = name.strip_prefix(b"#") {
        let s = std::str::from_utf8(num).ok()?;
        let code = match s.strip_prefix(['x', 'X']) {
            Some(h) => u32::from_str_radix(h, 16).ok()?,
            None => s.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        b"amp" => '&',
        b"lt" => '<',
        b"gt" => '>',
        b"quot" => '"',
        b"apos" => '\'',
        b"nbsp" => '\u{a0}',
        b"copy" => '\u{a9}',
        b"reg" => '\u{ae}',
        b"trade" => '\u{2122}',
        b"hellip" => '\u{2026}',
        b"mdash" => '\u{2014}',
        b"ndash" => '\u{2013}',
        b"lsquo" => '\u{2018}',
        b"rsquo" => '\u{2019}',
        b"ldquo" => '\u{201c}',
        b"rdquo" => '\u{201d}',
        b"laquo" => '\u{ab}',
        b"raquo" => '\u{bb}',
        b"middot" => '\u{b7}',
        b"bull" => '\u{2022}',
        b"times" => '\u{d7}',
        b"divide" => '\u{f7}',
        b"deg" => '\u{b0}',
        b"larr" => '\u{2190}',
        b"uarr" => '\u{2191}',
        b"rarr" => '\u{2192}',
        b"darr" => '\u{2193}',
        b"check" => '\u{2713}',
        b"euro" => '\u{20ac}',
        b"pound" => '\u{a3}',
        b"yen" => '\u{a5}',
        b"cent" => '\u{a2}',
        b"sect" => '\u{a7}',
        b"para" => '\u{b6}',
        _ => return None,
    })
}
