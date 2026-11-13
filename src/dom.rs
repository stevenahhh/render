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

/// What `Dom::kind_of` reports about a node: a `<script>`, an element that may
/// be a custom element, a `<style>` or stylesheet `<link>`.
pub const HAS_SCRIPT: u32 = 1;
pub const HAS_CUSTOM: u32 = 2;
pub const HAS_SHEET: u32 = 4;

/// True for the `rel` of a link to a stylesheet the page applies: a list of
/// tokens, compared ignoring ASCII case, with `stylesheet` and without
/// `alternate`, which names a sheet applied only when the user picks it.
pub fn is_stylesheet(rel: &str) -> bool {
    let has = |token: &str| rel.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case(token));
    has("stylesheet") && !has("alternate")
}

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
    COMMENT = "#comment", FRAGMENT = "#document-fragment", TEMPLATE = "template", SHADOW = "#shadow-root",
    SLOT = "slot", IFRAME = "iframe", XMP = "xmp", NOEMBED = "noembed", NOFRAMES = "noframes",
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

    /// The atom of `s` if it was ever interned, without interning it.
    pub fn get(&self, s: &str) -> Option<Atom> {
        self.map.get(s).copied()
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
    /// Comments are text nodes without text of their own, so style and layout
    /// pass over them; their data is kept here, as (start, len) in `text`.
    pub comments: FxMap<NodeId, (u32, u32)>,
    /// The content of each `<template>`: a fragment outside the tree.
    pub templates: FxMap<NodeId, NodeId>,
    /// The template each content fragment belongs to, and the host of each shadow root.
    pub hosts: FxMap<NodeId, NodeId>,
    /// The shadow root of each shadow host.
    pub shadows: FxMap<NodeId, NodeId>,
    /// The names of the custom elements scripts defined.
    pub defined: Vec<Atom>,
    /// The dialogs `showModal()` opened, which `:modal` matches.
    pub modal: Vec<NodeId>,
    /// The CSS of the style sheets each shadow root adopted.
    pub adopted: FxMap<NodeId, String>,
    /// Declarations finished animations hold on each element they fill forwards.
    pub animated: FxMap<NodeId, String>,
    /// True once a `<base>` element was created; nodes are never taken back.
    pub has_base: bool,
}

impl Dom {
    pub fn new(cap: usize) -> Dom {
        let mut d = Dom {
            nodes: Vec::with_capacity(cap / 24 + 8),
            attrs: Vec::new(),
            classes: Vec::new(),
            text: String::with_capacity(cap),
            atoms: Interner::new(),
            comments: FxMap::default(),
            templates: FxMap::default(),
            hosts: FxMap::default(),
            shadows: FxMap::default(),
            defined: Vec::new(),
            modal: Vec::new(),
            adopted: FxMap::default(),
            animated: FxMap::default(),
            has_base: false,
        };
        d.new_node(atom::DOCUMENT);
        d
    }

    /// A document without even a root, standing in for one that is lent out.
    pub fn empty() -> Dom {
        let atoms = Interner { map: FxMap::default(), names: Vec::new(), scratch: String::new() };
        Dom { nodes: Vec::new(), attrs: Vec::new(), classes: Vec::new(), text: String::new(), atoms, comments: FxMap::default(), templates: FxMap::default(), hosts: FxMap::default(), shadows: FxMap::default(), defined: Vec::new(), modal: Vec::new(), adopted: FxMap::default(), animated: FxMap::default(), has_base: false }
    }

    fn new_node(&mut self, tag: Atom) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.has_base |= tag == atom::BASE;
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
        t != atom::TEXT && t != atom::DOCUMENT && t != atom::FRAGMENT && t != atom::SHADOW
    }

    pub fn is_comment(&self, n: NodeId) -> bool {
        self.nodes[n as usize].tag == atom::TEXT && self.comments.contains_key(&n)
    }

    /// The data of a text or comment node.
    pub fn data_of(&self, n: NodeId) -> &str {
        match self.comments.get(&n) {
            Some(&(s, l)) => &self.text[s as usize..(s + l) as usize],
            None => self.text_of(n),
        }
    }

    pub fn set_data(&mut self, n: NodeId, s: &str) {
        if self.is_comment(n) {
            let start = self.text.len() as u32;
            self.text.push_str(s);
            self.comments.insert(n, (start, s.len() as u32));
        } else {
            self.set_text(n, s);
        }
    }

    pub fn create_comment(&mut self, s: &str) -> NodeId {
        let n = self.new_node(atom::TEXT);
        let start = self.text.len() as u32;
        self.text.push_str(s);
        self.comments.insert(n, (start, s.len() as u32));
        n
    }

    /// The content fragment of a `<template>`, made on first use for one a script created.
    pub fn template_content(&mut self, n: NodeId) -> NodeId {
        if let Some(&c) = self.templates.get(&n) {
            return c;
        }
        let c = self.new_node(atom::FRAGMENT);
        self.templates.insert(n, c);
        self.hosts.insert(c, n);
        c
    }

    /// The node after `n` in a depth-first walk of the subtree of `root`.
    pub fn next_in(&self, n: NodeId, root: NodeId) -> NodeId {
        let node = &self.nodes[n as usize];
        if node.first_child != NONE {
            return node.first_child;
        }
        let mut c = n;
        while c != root && c != NONE {
            let next = self.nodes[c as usize].next;
            if next != NONE {
                return next;
            }
            c = self.nodes[c as usize].parent;
        }
        NONE
    }

    /// The first element below `root` whose id is `id`, in tree order.
    pub fn by_id(&self, root: NodeId, id: &str) -> NodeId {
        let Some(want) = self.atoms.get(id).filter(|&a| a != 0) else { return NONE };
        let mut n = self.next_in(root, root);
        while n != NONE && !(self.nodes[n as usize].id == want && self.is_element(n)) {
            n = self.next_in(n, root);
        }
        n
    }

    pub fn kind_of(&self, n: NodeId) -> u32 {
        match self.nodes[n as usize].tag {
            atom::SCRIPT => HAS_SCRIPT,
            atom::STYLE => HAS_SHEET,
            atom::LINK if self.attr(n, atom::REL).is_some_and(is_stylesheet) => HAS_SHEET,
            t if self.is_element(n) && self.atoms.name(t).contains('-') => HAS_CUSTOM,
            _ => 0,
        }
    }

    /// The kinds of all nodes in the subtree of `n`, `n` and shadow trees included.
    pub fn kinds_in(&self, n: NodeId) -> u32 {
        let mut kinds = 0;
        self.each_including(n, &mut |c| kinds |= self.kind_of(c));
        kinds
    }

    /// Calls `f` on `n` and its descendants in shadow-including tree order: a
    /// host's shadow tree comes right after the host, before its children.
    pub fn each_including(&self, n: NodeId, f: &mut impl FnMut(NodeId)) {
        let (mut c, hosts) = (n, !self.shadows.is_empty());
        while c != NONE {
            f(c);
            if let Some(&root) = self.shadows.get(&c).filter(|_| hosts) {
                let mut k = self.nodes[root as usize].first_child;
                while k != NONE {
                    self.each_including(k, f);
                    k = self.nodes[k as usize].next;
                }
            }
            c = self.next_in(c, n);
        }
    }

    /// A shadow root for `host`, outside the tree like a template's content.
    pub fn attach_shadow(&mut self, host: NodeId) -> NodeId {
        let root = self.new_node(atom::SHADOW);
        self.shadows.insert(host, root);
        self.hosts.insert(root, host);
        root
    }

    /// True when `n` is in the document, counting shadow trees of hosts that are.
    pub fn connected(&self, n: NodeId) -> bool {
        let mut root = self.root_of(n);
        while self.nodes[root as usize].tag == atom::SHADOW {
            match self.hosts.get(&root) {
                Some(&host) => root = self.root_of(host),
                None => return false,
            }
        }
        root == 0
    }

    /// The root of the tree `n` is in: the document when it is connected.
    pub fn root_of(&self, mut n: NodeId) -> NodeId {
        while self.nodes[n as usize].parent != NONE {
            n = self.nodes[n as usize].parent;
        }
        n
    }

    /// A copy of `n` with its attributes, and with `deep` its descendants and template content.
    pub fn clone_node(&mut self, n: NodeId, deep: bool) -> NodeId {
        let (tag, id, text, classes, (s, c)) = {
            let src = &self.nodes[n as usize];
            (src.tag, src.id, src.text, src.classes, src.attrs)
        };
        let m = self.new_node(tag);
        // Attribute values change in place, so the copy gets entries of its own.
        let start = self.attrs.len() as u32;
        for i in s..s + c {
            let a = Attr { name: self.attrs[i as usize].name, value: self.attrs[i as usize].value };
            self.attrs.push(a);
        }
        let node = &mut self.nodes[m as usize];
        (node.id, node.text, node.classes, node.attrs) = (id, text, classes, (start, c));
        if let Some(&d) = self.comments.get(&n) {
            self.comments.insert(m, d);
        }
        if deep {
            if let Some(&content) = self.templates.get(&n) {
                let copy = self.clone_node(content, true);
                self.templates.insert(m, copy);
                self.hosts.insert(copy, m);
            }
            let mut k = self.nodes[n as usize].first_child;
            while k != NONE {
                let copy = self.clone_node(k, true);
                self.append(m, copy);
                k = self.nodes[k as usize].next;
            }
        }
        m
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
        self.parse_into(n, html, Parse::Fragment);
    }

    /// Replaces the children of `n` with parsed HTML: a fragment as `innerHTML`
    /// parses one, one that may declare shadow roots as `setHTMLUnsafe` does, or
    /// with `n` a document, a whole document. Returns false when the markup left
    /// an element open, as one `document.write` of several can.
    pub fn parse_into(&mut self, n: NodeId, html: &str, how: Parse) -> bool {
        while self.nodes[n as usize].first_child != NONE {
            let c = self.nodes[n as usize].first_child;
            self.detach(c);
        }
        let (fragment, declarative) = match how {
            Parse::Fragment => (true, false),
            Parse::Unsafe => (true, true),
            Parse::Document => (false, true),
        };
        let (stack, html_, body) = if fragment { (vec![n, n, n], n, n) } else { (vec![n], NONE, NONE) };
        let base = stack.len();
        let mut parser = Parser { src: html, b: html.as_bytes(), pos: 0, dom: self, stack, html: html_, head: NONE, body, fragment, templates: 0, declarative };
        parser.run();
        parser.stack.len() <= base
    }

    /// Makes the content of a `<template shadowrootmode>` that just closed the
    /// shadow root of its parent, keeping the template's attributes on the root
    /// for scripts to read. A parent that cannot host one, or already does,
    /// keeps the template as it is.
    fn declare_shadow(&mut self, template: NodeId) {
        let host = self.nodes[template as usize].parent;
        let mode = self.atoms.get("shadowrootmode").and_then(|a| self.attr(template, a));
        if !matches!(mode, Some(m) if m.eq_ignore_ascii_case("open") || m.eq_ignore_ascii_case("closed")) || host == NONE || self.shadows.contains_key(&host) {
            return;
        }
        let tag = self.atoms.name(self.nodes[host as usize].tag);
        let valid = matches!(tag, "article" | "aside" | "blockquote" | "body" | "div" | "footer" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header" | "main" | "nav" | "p" | "section" | "span")
            || (tag.contains('-') && self.is_element(host));
        if !valid {
            return;
        }
        let Some(content) = self.templates.remove(&template) else { return };
        self.hosts.remove(&content);
        let root = self.attach_shadow(host);
        self.nodes[root as usize].attrs = self.nodes[template as usize].attrs;
        while self.nodes[content as usize].first_child != NONE {
            let c = self.nodes[content as usize].first_child;
            self.detach(c);
            self.append(root, c);
        }
        self.detach(template);
    }

    /// Serializes a node (`outer`) or only its children back to HTML.
    pub fn serialize(&self, n: NodeId, outer: bool, out: &mut String) {
        let mut p = self.nodes[n as usize].parent;
        while p != NONE && self.nodes[p as usize].tag != atom::SVG && self.atoms.name(self.nodes[p as usize].tag) != "foreignobject" {
            p = self.nodes[p as usize].parent;
        }
        let svg = p != NONE && self.nodes[p as usize].tag == atom::SVG;
        self.serialize_in(n, outer, svg, out);
    }

    /// `serialize` with `svg` true inside an `<svg>`, where the names the parser
    /// lowercased take back their SVG case (`clipPath`, `viewBox`).
    fn serialize_in(&self, n: NodeId, outer: bool, svg: bool, out: &mut String) {
        let node = &self.nodes[n as usize];
        if let Some(&(s, l)) = self.comments.get(&n) {
            out.push_str("<!--");
            out.push_str(&self.text[s as usize..(s + l) as usize]);
            out.push_str("-->");
            return;
        }
        if node.tag == atom::TEXT {
            let raw = self.nodes.get(node.parent as usize).is_some_and(|p| is_raw_text(p.tag));
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
        let svg = svg || node.tag == atom::SVG;
        let name = self.atoms.name(node.tag);
        let name = if svg { svg_case(name, &SVG_TAGS) } else { name };
        let outer = outer && !matches!(node.tag, atom::FRAGMENT | atom::DOCUMENT);
        if outer {
            out.push('<');
            out.push_str(name);
            let (s, c) = node.attrs;
            for a in &self.attrs[s as usize..(s + c) as usize] {
                out.push(' ');
                let attr = self.atoms.name(a.name);
                out.push_str(if svg { svg_case(attr, &SVG_ATTRS) } else { attr });
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
        let mut c = match self.templates.get(&n) {
            Some(&content) => self.nodes[content as usize].first_child,
            None => node.first_child,
        };
        // The content of a <foreignObject> is HTML again.
        let svg = svg && name != "foreignObject";
        while c != NONE {
            self.serialize_in(c, true, svg, out);
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

/// SVG element names with capitals, which the HTML parser lowercases.
const SVG_TAGS: [&str; 37] = [
    "altGlyph", "altGlyphDef", "altGlyphItem", "animateColor", "animateMotion", "animateTransform", "clipPath",
    "feBlend", "feColorMatrix", "feComponentTransfer", "feComposite", "feConvolveMatrix", "feDiffuseLighting",
    "feDisplacementMap", "feDistantLight", "feDropShadow", "feFlood", "feFuncA", "feFuncB", "feFuncG", "feFuncR",
    "feGaussianBlur", "feImage", "feMerge", "feMergeNode", "feMorphology", "feOffset", "fePointLight",
    "feSpecularLighting", "feSpotLight", "feTile", "feTurbulence", "foreignObject", "glyphRef", "linearGradient",
    "radialGradient", "textPath",
];

/// SVG attribute names with capitals, which the HTML parser lowercases.
const SVG_ATTRS: [&str; 58] = [
    "attributeName", "attributeType", "baseFrequency", "baseProfile", "calcMode", "clipPathUnits", "diffuseConstant",
    "edgeMode", "filterUnits", "glyphRef", "gradientTransform", "gradientUnits", "kernelMatrix", "kernelUnitLength",
    "keyPoints", "keySplines", "keyTimes", "lengthAdjust", "limitingConeAngle", "markerHeight", "markerUnits",
    "markerWidth", "maskContentUnits", "maskUnits", "numOctaves", "pathLength", "patternContentUnits",
    "patternTransform", "patternUnits", "pointsAtX", "pointsAtY", "pointsAtZ", "preserveAlpha", "preserveAspectRatio",
    "primitiveUnits", "refX", "refY", "repeatCount", "repeatDur", "requiredExtensions", "requiredFeatures",
    "specularConstant", "specularExponent", "spreadMethod", "startOffset", "stdDeviation", "stitchTiles",
    "surfaceScale", "systemLanguage", "tableValues", "targetX", "targetY", "textLength", "viewBox", "viewTarget",
    "xChannelSelector", "yChannelSelector", "zoomAndPan",
];

/// The SVG spelling of a lowercased name from `table`, or the name itself.
fn svg_case<'a>(name: &'a str, table: &[&'static str]) -> &'a str {
    if !name.bytes().any(|b| b.is_ascii_uppercase()) {
        if let Some(t) = table.iter().find(|t| t.eq_ignore_ascii_case(name)) {
            return t;
        }
    }
    name
}

/// How `Dom::parse_into` reads its markup.
#[derive(Clone, Copy)]
pub enum Parse {
    Fragment,
    Unsafe,
    Document,
}

pub fn parse_html(src: &str) -> Dom {
    let mut dom = Dom::new(src.len());
    Parser { src, b: src.as_bytes(), pos: 0, dom: &mut dom, stack: vec![0], html: NONE, head: NONE, body: NONE, fragment: false, templates: 0, declarative: true }.run();
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
    /// Template contents open on the stack. Inside one there is no implied
    /// `<head>` or `<body>`, and end tags do not reach past it.
    templates: u32,
    /// A `<template shadowrootmode>` becomes the shadow root of its parent.
    declarative: bool,
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
    matches!(t, TITLE | META | LINK | STYLE | SCRIPT | BASE | TEMPLATE | NOSCRIPT)
}

/// Elements whose content is text up to their end tag, with no character
/// references: `<noscript>` among them, since scripting is always enabled.
fn is_raw_text(t: Atom) -> bool {
    use atom::*;
    matches!(t, SCRIPT | STYLE | NOSCRIPT | IFRAME | XMP | NOEMBED | NOFRAMES)
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
                if b[lt..].starts_with(b"<!--") {
                    let body = lt + 4;
                    // `<!-->` and `<!--->` are empty comments.
                    let (end, after) = match &b[body..] {
                        [b'>', ..] => (body, body + 1),
                        [b'-', b'>', ..] => (body, body + 2),
                        rest => memmem::find(rest, b"-->").map_or((len, len), |o| (body + o, body + o + 3)),
                    };
                    let src = self.src;
                    self.comment(&src[body..end]);
                    self.pos = after;
                } else {
                    let end = memchr(b'>', &b[lt..]).map_or(len, |o| lt + o);
                    // `<!DOCTYPE>` is no node; any other `<!...>` is a bogus comment.
                    if !b[lt + 2..end].get(..7).is_some_and(|d| d.eq_ignore_ascii_case(b"doctype")) {
                        let src = self.src;
                        self.comment(&src[lt + 2..end]);
                    }
                    self.pos = (end + 1).min(len);
                }
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
                // A bogus comment whose data keeps the `?`, as Lit's `<?lit$...$>` markers rely on.
                let end = memchr(b'>', &b[lt..]).map_or(len, |o| lt + o);
                let src = self.src;
                self.comment(&src[lt + 1..end]);
                self.pos = (end + 1).min(len);
            } else {
                self.text("<");
                self.pos = lt + 1;
            }
        }
    }

    fn ensure_html(&mut self) {
        if self.html == NONE {
            self.html = self.dom.new_node(atom::HTML);
            self.dom.append(self.stack[0], self.html);
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

    fn comment(&mut self, data: &str) {
        let parent = *self.stack.last().unwrap();
        // Layout finds <html> as the document's first child, so comments around it are left out.
        if self.stack.len() == 1 && !self.fragment {
            return;
        }
        let n = self.dom.create_comment(data);
        self.dom.append(parent, n);
    }

    fn text(&mut self, t: &str) {
        let ws = t.bytes().all(|c| c.is_ascii_whitespace());
        if self.templates == 0 && (self.stack.len() <= 2 || self.body == NONE) {
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
            atom::HTML | atom::HEAD | atom::BODY if self.fragment || self.templates > 0 => return,
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
        if self.templates > 0 {
            // Inside template content: no implied head or body.
        } else if self.body == NONE && is_head_tag(tag) {
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
        if tag == atom::TEMPLATE {
            let content = self.dom.template_content(n);
            self.stack.push(n);
            self.stack.push(content);
            self.templates += 1;
            return;
        }

        let in_svg = || tag == atom::SVG || self.stack.iter().any(|&s| self.dom.nodes[s as usize].tag == atom::SVG);
        if is_void(tag) || (self_closing && in_svg()) {
            return;
        }
        let raw = is_raw_text(tag);
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
            if bounds.contains(&t) || t == atom::FRAGMENT {
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
                if self.templates == 0 && self.body == NONE && self.stack.len() > 2 {
                    self.stack.truncate(2);
                }
                return;
            }
            _ => {}
        }
        for i in (3..self.stack.len()).rev() {
            let t = self.dom.nodes[self.stack[i] as usize].tag;
            if t == tag {
                self.templates -= (tag == atom::TEMPLATE) as u32;
                let template = self.stack[i];
                self.stack.truncate(i);
                // In a fragment the context node is no parent the markup declares.
                if tag == atom::TEMPLATE && self.declarative && !(self.fragment && self.stack[i - 1] == self.stack[0]) {
                    self.dom.declare_shadow(template);
                }
                return;
            }
            if t == atom::FRAGMENT && tag != atom::TEMPLATE {
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

#[cfg(test)]
mod tests {
    use super::{atom, parse_html, NodeId, HAS_CUSTOM, HAS_SCRIPT, HAS_SHEET, NONE};

    fn first(dom: &super::Dom, tag: u32) -> NodeId {
        (0..dom.nodes.len() as NodeId).find(|&n| dom.nodes[n as usize].tag == tag).unwrap()
    }

    #[test]
    fn comments_are_kept_but_hold_no_text() {
        let dom = parse_html("<!-- top --><html><body><div><!--hi-->a<!---->b</div></body></html>");
        let div = first(&dom, atom::DIV);
        let c = dom.nodes[div as usize].first_child;
        assert!(dom.is_comment(c));
        assert_eq!((dom.data_of(c), dom.text_of(c)), ("hi", ""));
        let mut text = String::new();
        dom.text_content(div, &mut text);
        assert_eq!(text, "ab");
        let mut html = String::new();
        dom.serialize(div, false, &mut html);
        assert_eq!(html, "<!--hi-->a<!---->b");
        assert!(!dom.is_comment(dom.nodes[0].first_child), "a comment before <html> is left out");
    }

    #[test]
    fn noscript_and_iframe_hold_raw_text_and_noscript_stays_in_head() {
        let dom = parse_html("<head><noscript><img src=p.gif></noscript><link rel=stylesheet href=a.css></head>\
            <body><noscript><iframe src=x></iframe></noscript><iframe>a<b>c</b>&amp;</iframe><p>after</p></body>");
        let head = first(&dom, atom::HEAD);
        let (noscript, link) = (first(&dom, atom::NOSCRIPT), first(&dom, atom::LINK));
        assert_eq!((dom.nodes[noscript as usize].parent, dom.nodes[link as usize].parent), (head, head));
        assert!(!(0..dom.nodes.len()).any(|n| dom.nodes[n].tag == atom::IMG), "no element inside noscript");
        let iframe = first(&dom, atom::IFRAME);
        let mut html = String::new();
        dom.serialize(iframe, true, &mut html);
        assert_eq!(html, "<iframe>a<b>c</b>&amp;</iframe>");
        let mut inner = String::new();
        dom.serialize(first(&dom, atom::BODY), false, &mut inner);
        assert_eq!(inner, "<noscript><iframe src=x></iframe></noscript><iframe>a<b>c</b>&amp;</iframe><p>after</p>");
    }

    #[test]
    fn svg_serializes_with_its_case_sensitive_names() {
        let dom = parse_html("<body><svg viewBox='0 0 1 1'><defs><clipPath id=c clipPathUnits=objectBoundingBox></clipPath>\
            <filter id=f><feGaussianBlur stdDeviation=2></feGaussianBlur></filter></defs>\
            <foreignObject><div viewBox=x></div></foreignObject></svg><div viewbox=y></div></body>");
        let mut html = String::new();
        dom.serialize(first(&dom, atom::BODY), false, &mut html);
        assert_eq!(html, "<svg viewBox=\"0 0 1 1\"><defs><clipPath id=\"c\" clipPathUnits=\"objectBoundingBox\"></clipPath>\
            <filter id=\"f\"><feGaussianBlur stdDeviation=\"2\"></feGaussianBlur></filter></defs>\
            <foreignObject><div viewbox=\"x\"></div></foreignObject></svg><div viewbox=\"y\"></div>");
        let mut inner = String::new();
        dom.serialize(first(&dom, atom::DIV), true, &mut inner);
        assert_eq!(inner, "<div viewbox=\"x\"></div>", "HTML inside foreignObject keeps lowercase names");
    }

    #[test]
    fn processing_instructions_and_bogus_declarations_become_comments() {
        let dom = parse_html("<!DOCTYPE html><body><div><?lit$123$>a<!x y>b</div></body>");
        let div = first(&dom, atom::DIV);
        let mut html = String::new();
        dom.serialize(div, false, &mut html);
        assert_eq!(html, "<!--?lit$123$-->a<!--x y-->b");
    }

    #[test]
    fn template_content_is_outside_the_tree() {
        let dom = parse_html("<body><template id=t><li>A</li><li>B</li></template><p>after</p></body>");
        let t = first(&dom, atom::TEMPLATE);
        assert_eq!(dom.nodes[t as usize].first_child, NONE);
        let content = dom.templates[&t];
        assert_eq!((dom.nodes[content as usize].tag, dom.hosts[&content]), (atom::FRAGMENT, t));
        assert_eq!(dom.kinds_in(content), 0);
        let p = first(&dom, atom::P);
        assert_eq!(dom.nodes[p as usize].parent, dom.nodes[t as usize].parent, "the template closes at its end tag");
        let mut html = String::new();
        dom.serialize(t, true, &mut html);
        assert_eq!(html, "<template id=\"t\"><li>A</li><li>B</li></template>");
    }

    #[test]
    fn declarative_shadow_roots_attach_to_their_parent() {
        let src = "<body><x-a><template shadowrootmode=open shadowrootdelegatesfocus><style>p{}</style><p>in</p><slot></slot></template><b>light</b></x-a>\
            <x-b><template shadowrootmode=closed></template><template shadowrootmode=open><i>second</i></template></x-b>\
            <ul><template shadowrootmode=open><li>no host</li></template></ul><div><template shadowrootmode=bogus></template></div></body>";
        let mut dom = parse_html(src);
        let tags: Vec<NodeId> = (0..dom.nodes.len() as NodeId).filter(|&n| dom.is_element(n) && dom.atoms.name(dom.nodes[n as usize].tag).starts_with("x-")).collect();
        let (a, b) = (tags[0], tags[1]);
        let root = dom.shadows[&a];
        let mut html = String::new();
        dom.serialize(root, false, &mut html);
        assert_eq!(html, "<style>p{}</style><p>in</p><slot></slot>");
        let mut light = String::new();
        dom.serialize(a, false, &mut light);
        assert_eq!(light, "<b>light</b>", "the template leaves the light tree");
        let delegates = dom.atoms.get("shadowrootdelegatesfocus").unwrap();
        assert_eq!(dom.attr(root, delegates), Some(""));
        let rb = dom.shadows[&b];
        assert_eq!(dom.nodes[rb as usize].first_child, NONE, "the first declared root wins");
        let mut rest = String::new();
        dom.serialize(b, false, &mut rest);
        assert_eq!(rest, "<template shadowrootmode=\"open\"><i>second</i></template>");
        assert_eq!(dom.shadows.len(), 2, "a <ul> or a bad mode declares nothing");
        let div = first(&dom, atom::DIV);
        dom.set_inner_html(div, "<x-c><template shadowrootmode=open><i></i></template></x-c>");
        assert_eq!(dom.shadows.len(), 2, "innerHTML keeps templates");
        dom.parse_into(div, "<template shadowrootmode=open></template><x-c><template shadowrootmode=open><i></i></template></x-c>", super::Parse::Unsafe);
        assert_eq!(dom.shadows.len(), 3, "setHTMLUnsafe declares, but not on its own context");
        let doc = dom.create_element("#document");
        dom.parse_into(doc, "<title>t</title><p><template shadowrootmode=open>x</template></p>", super::Parse::Document);
        let mut whole = String::new();
        dom.serialize(doc, false, &mut whole);
        assert_eq!(whole, "<html><head><title>t</title></head><body><p></p></body></html>");
        assert_eq!(dom.shadows.len(), 4);
    }

    #[test]
    fn fragments_report_elements_left_open() {
        let mut dom = parse_html("<body><div></div></body>");
        let div = first(&dom, atom::DIV);
        let how = super::Parse::Fragment;
        assert!(dom.parse_into(div, "<p>a</p>text<br><img>", how));
        assert!(!dom.parse_into(div, "<p>a</p><div id=w>", how));
        assert!(!dom.parse_into(div, "<template><b></b>", how));
        assert!(dom.parse_into(div, "<div id=w>two</div>\n", how));
    }

    #[test]
    fn clones_copy_attributes_and_template_content() {
        let mut dom = parse_html("<body><template class=x><b>x</b></template></body>");
        let t = first(&dom, atom::TEMPLATE);
        let copy = dom.clone_node(t, true);
        dom.set_attr(t, "class", Some("changed"));
        assert_eq!(dom.attr(copy, atom::CLASS), Some("x"));
        let content = dom.templates[&copy];
        assert_ne!(content, dom.templates[&t]);
        let mut html = String::new();
        dom.serialize(content, false, &mut html);
        assert_eq!(html, "<b>x</b>");
        assert_eq!(dom.nodes[copy as usize].parent, NONE);
    }

    #[test]
    fn links_are_stylesheets_by_a_token_of_their_rel() {
        let dom = parse_html("<link rel=stylesheet><link rel='preload stylesheet'><link rel=' Stylesheet  icon '><link rel='alternate stylesheet'><link rel=preload><link rel=stylesheets>");
        let links: Vec<NodeId> = (0..dom.nodes.len() as NodeId).filter(|&n| dom.nodes[n as usize].tag == atom::LINK).collect();
        let kinds: Vec<u32> = links.iter().map(|&n| dom.kind_of(n)).collect();
        assert_eq!(kinds, [HAS_SHEET, HAS_SHEET, HAS_SHEET, 0, 0, 0]);
    }

    #[test]
    fn finds_ids_kinds_and_connected_nodes() {
        let mut dom = parse_html("<body><div id=a><x-el></x-el><script></script><style></style></div></body>");
        let div = first(&dom, atom::DIV);
        assert_eq!(dom.by_id(0, "a"), div);
        assert_eq!(dom.by_id(0, "missing"), NONE);
        assert_eq!(dom.kinds_in(div), HAS_CUSTOM | HAS_SCRIPT | HAS_SHEET);
        let root = dom.attach_shadow(div);
        let inner = dom.create_element("span");
        dom.insert_before(root, inner, NONE);
        assert!(dom.connected(inner));
        assert_eq!(dom.root_of(inner), root);
        dom.detach(div);
        assert!(!dom.connected(inner));
    }
}
