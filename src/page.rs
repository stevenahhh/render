//! A loaded document: DOM, stylesheet, computed styles and layout.

use crate::css::Sheet;
use crate::dom::{atom, parse_html, Dom, NodeId, NONE};
use crate::font::Fonts;
use crate::img::Images;
use crate::js::{Host, Js};
use crate::FxMap;
use crate::layout::{layout, Kind, Layout};
use crate::style::{compute_styles, Style};
use std::path::Path;

/// Built-in defaults, applied below every author rule.
const UA_CSS: &str = "
html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, dl, dt, dd, blockquote, pre, form,
fieldset, legend, table, thead, tbody, tfoot, caption, header, footer, main, nav, section,
article, aside, figure, figcaption, address, hr, details, summary, center { display: block }
head, script, style, title, meta, link, base, template, noscript, datalist, area, param,
source, track, [hidden] { display: none }
body { margin: 8px }
p, dl { margin: 1em 0 }
h1 { font-size: 2em; margin: 0.67em 0; font-weight: bold }
h2 { font-size: 1.5em; margin: 0.83em 0; font-weight: bold }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold }
h4 { margin: 1.33em 0; font-weight: bold }
h5 { font-size: 0.83em; margin: 1.67em 0; font-weight: bold }
h6 { font-size: 0.67em; margin: 2.33em 0; font-weight: bold }
ul, ol { margin: 1em 0; padding-left: 40px }
ul { list-style-type: disc }
ol { list-style-type: decimal }
ul ul, ol ul { list-style-type: circle }
ul ul, ol ul, ul ol, ol ol { margin: 0 }
li { display: list-item }
dd { margin-left: 40px }
blockquote, figure { margin: 1em 40px }
pre { white-space: pre; margin: 1em 0; font-family: monospace; font-size: 13px }
code, kbd, samp, tt { font-family: monospace; font-size: 13px }
b, strong, th { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
a[href] { color: #0000ee; text-decoration: underline; cursor: pointer }
small, sub, sup { font-size: smaller }
big { font-size: larger }
mark { background: yellow; color: black }
center { text-align: center }
hr { border-top: 1px solid #888; margin: 8px 0 }
tr { display: flex }
td, th { display: block; flex: 1 1 0; padding: 1px }
th { text-align: center }
img { display: inline-block; color: #666; font-size: 12px; overflow: hidden }
input, button, select, textarea { display: inline-block; box-sizing: border-box;
  font-family: sans-serif; font-size: 13.33px; font-weight: normal; font-style: normal;
  color: #000; border: 1px solid #767676; border-radius: 3px; line-height: normal;
  text-align: left; text-transform: none; letter-spacing: normal; cursor: default }
input, select, textarea { background: #fff; padding: 2px 4px; width: 150px }
button, input[type=submit], input[type=button], input[type=reset] { background: #efefef;
  padding: 2px 7px; width: auto; text-align: center; white-space: nowrap }
input[type=checkbox], input[type=radio] { width: 13px; height: 13px; padding: 0; margin: 3px }
input[type=radio] { border-radius: 50% }
input[type=hidden] { display: none }
textarea { white-space: pre; width: 180px; height: 40px; font-family: monospace }
select { width: auto }
option { display: none }
option:first-child { display: inline }
";

type Script = (String, String, Option<std::path::PathBuf>);

pub struct Page {
    pub dom: Dom,
    pub sheet: Sheet,
    pub styles: Vec<Style>,
    pub layout: Layout,
    pub images: Images,
    pub title: String,
    viewport_units: bool,
    media_sig: u64,
    vw: f32,
    vh: f32,
    hover: NodeId,
    /// Present once the page has run a script.
    js: Option<Js>,
    rects: FxMap<u32, [f32; 4]>,
    scroll: f32,
    dirty: bool,
    /// Scripts still to run, in document order: `(name, code, module path)`.
    deferred: Vec<Script>,
    /// Keep every element's box in the layout. Set before the first `update`.
    pub record_boxes: bool,
}

impl Page {
    /// Parses HTML and every stylesheet it references. `base` is the
    /// directory that relative `<link href>` paths resolve against.
    pub fn from_html(html: &str, base: Option<&Path>) -> Page {
        Page::parse(html, base, false)
    }

    /// With `defer`, every script is kept for `run_deferred`, so the time to the
    /// first frame does not depend on how much script a page carries. The scripts
    /// still run in document order.
    fn parse(html: &str, base: Option<&Path>, defer: bool) -> Page {
        let mut dom = parse_html(html);
        let mut sheet = Sheet::default();
        sheet.add(UA_CSS, &mut dom.atoms, 0);
        let mut viewport_units = html.contains("vw") || html.contains("vh");
        let mut title = String::new();
        let mut scripts: Vec<Script> = Vec::new();
        for n in 0..dom.nodes.len() as NodeId {
            match dom.nodes[n as usize].tag {
                atom::SCRIPT => {
                    let ty = dom.attr(n, atom::TYPE).unwrap_or("");
                    if !(ty.is_empty() || ty.contains("javascript") || ty == "module") {
                        continue;
                    }
                    let module = ty == "module";
                    match dom.attr(n, atom::SRC) {
                        Some(src) => {
                            let file = base.map(|b| b.join(src));
                            let code = file.as_ref().and_then(|f| std::fs::read(f).ok());
                            let module = file.filter(|_| module).and_then(|f| std::fs::canonicalize(f).ok());
                            match code {
                                Some(c) => scripts.push((src.to_string(), String::from_utf8_lossy(&c).into_owned(), module)),
                                None => eprintln!("[js] cannot read script {src}"),
                            }
                        }
                        None => {
                            let mut code = String::new();
                            dom.text_content(n, &mut code);
                            let _ = module;
                            scripts.push(("inline script".to_string(), code, None));
                        }
                    }
                }
                atom::STYLE => {
                    let mut css = String::new();
                    dom.text_content(n, &mut css);
                    sheet.add(&css, &mut dom.atoms, 1);
                }
                atom::LINK => {
                    let is_css = dom.attr(n, atom::REL).is_some_and(|r| r.eq_ignore_ascii_case("stylesheet"));
                    let path = dom.attr(n, atom::HREF).filter(|_| is_css).zip(base).map(|(h, b)| b.join(h));
                    if let Some(css) = path.and_then(|p| std::fs::read(p).ok()) {
                        let css = String::from_utf8_lossy(&css);
                        viewport_units |= css.contains("vw") || css.contains("vh");
                        sheet.add(&css, &mut dom.atoms, 1);
                    }
                }
                atom::TITLE if title.is_empty() => dom.text_content(n, &mut title),
                _ => {}
            }
        }
        sheet.finish();
        let mut page = Page {
            dom,
            sheet,
            styles: Vec::new(),
            layout: Layout::default(),
            images: Images::new(base.map(Path::to_path_buf)),
            title: title.trim().to_string(),
            viewport_units,
            media_sig: 0,
            vw: -1.0,
            vh: -1.0,
            hover: NONE,
            js: None,
            rects: FxMap::default(),
            scroll: 0.0,
            dirty: false,
            deferred: Vec::new(),
            record_boxes: false,
        };
        if !scripts.is_empty() {
            page.js = Some(Js::new(base));
            if defer {
                page.deferred = scripts;
            } else {
                page.run_scripts(&scripts, true);
            }
        }
        page
    }

    /// Runs scripts in order; `last` also fires the load event.
    fn run_scripts(&mut self, scripts: &[Script], last: bool) {
        self.with_js(|js| {
            for (name, code, module) in scripts {
                match module {
                    Some(path) => js.eval_module(code, name, path),
                    None => drop(js.eval(code, name)),
                }
            }
            if last {
                js.load();
            }
        });
    }

    /// Runs the next script `load_deferred` held back; the last one also fires the
    /// load event. Returns how many are left, or `None` when there was nothing to run.
    pub fn run_deferred(&mut self) -> Option<usize> {
        if self.deferred.is_empty() {
            return None;
        }
        let script = self.deferred.remove(0);
        let left = self.deferred.len();
        self.run_scripts(std::slice::from_ref(&script), left == 0);
        Some(left)
    }

    /// Runs `f` with the DOM lent to the script engine. A script that
    /// changes the document invalidates style and layout.
    fn with_js<R>(&mut self, f: impl FnOnce(&mut Js) -> R) -> Option<R> {
        let mut js = self.js.take()?;
        let dom = std::mem::replace(&mut self.dom, Dom::new(0));
        let view = if self.vw < 0.0 { [1024.0, 768.0, 0.0] } else { [self.vw, self.vh, self.scroll] };
        js.enter(Host { dom, dirty: false, rects: std::mem::take(&mut self.rects), view });
        let r = f(&mut js);
        let host = js.leave();
        self.dom = host.dom;
        self.rects = host.rects;
        if host.dirty {
            self.styles.clear();
            self.dirty = true;
        }
        self.js = Some(js);
        Some(r)
    }

    /// Border box `[x, y, w, h]` and computed style of the first element
    /// matching a selector. The box needs `record_boxes`.
    pub fn describe(&mut self, selector: &str) -> Option<(Option<[f32; 4]>, String)> {
        let n = *crate::style::query(&mut self.dom, 0, selector, true).first()?;
        let boxy = |k: Kind| matches!(k, Kind::Box | Kind::Area);
        let rect = self.layout.items.iter().find(|i| i.node == n && boxy(i.kind)).map(|i| [i.x, i.y, i.w, i.h]);
        Some((rect, self.styles.get(n as usize).map(|s| s.css()).unwrap_or_default()))
    }

    /// Describes the computed box of every element matching a selector.
    pub fn inspect(&mut self, selector: &str) -> Vec<String> {
        let found = crate::style::query(&mut self.dom, 0, selector, false);
        let describe = |n: NodeId| {
            let Some(s) = self.styles.get(n as usize) else { return String::new() };
            let rect = self.layout.items.iter().find(|i| i.node == n && i.kind != Kind::Text).map(|i| (i.x, i.y, i.w, i.h));
            format!(
                "<{}> display={:?} position={:?} width={:?} height={:?} flex_col={} opacity={} visible={} color={:08x} bg={:08x} font={} box={:?}",
                self.dom.atoms.name(self.dom.nodes[n as usize].tag),
                s.display, s.position, s.width, s.height, s.flex_col, s.opacity, s.visible, s.color.0, s.bg.0, s.font_size, rect
            )
        };
        found.into_iter().map(describe).collect()
    }

    /// Sends a click to the page's scripts; true when they cancelled it.
    pub fn click(&mut self, node: NodeId) -> bool {
        node != NONE && self.with_js(|js| js.dispatch(node, "click")).unwrap_or(false)
    }

    /// Runs due script timers; returns milliseconds until the next one.
    pub fn tick(&mut self) -> Option<f64> {
        if !self.deferred.is_empty() {
            return None;
        }
        self.with_js(|js| js.timers()).flatten()
    }

    /// Lays the page out and lets script timers run for up to `budget`, for
    /// one-shot output where no event loop keeps the page alive.
    pub fn settle(&mut self, fonts: &mut Fonts, vw: f32, vh: f32, budget: std::time::Duration) {
        let start = std::time::Instant::now();
        loop {
            self.update(fonts, vw, vh);
            let wait = match self.tick() {
                Some(ms) => std::time::Duration::from_secs_f64(ms.max(1.0) / 1e3),
                None => break,
            };
            if start.elapsed() + wait > budget {
                break;
            }
            std::thread::sleep(wait);
        }
        self.update(fonts, vw, vh);
    }

    /// True once after scripts changed the document.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn set_scroll(&mut self, y: f32) {
        self.scroll = y;
    }

    /// Like `load`, but holds deferred scripts back until `run_deferred`.
    pub fn load_deferred(path: &Path) -> std::io::Result<Page> {
        let bytes = std::fs::read(path)?;
        Ok(Page::parse(&String::from_utf8_lossy(&bytes), path.parent(), true))
    }

    pub fn load(path: &Path) -> std::io::Result<Page> {
        let bytes = std::fs::read(path)?;
        Ok(Page::from_html(&String::from_utf8_lossy(&bytes), path.parent()))
    }

    fn restyle(&mut self, vw: f32, vh: f32) {
        self.styles = compute_styles(&self.dom, &self.sheet, vw, vh, self.hover);
    }

    fn relayout(&mut self, fonts: &mut Fonts, vw: f32, vh: f32) {
        let mut old = std::mem::take(&mut self.layout);
        old.record_boxes = self.record_boxes || self.js.is_some();
        self.layout = layout(&self.dom, &self.styles, fonts, &mut self.images, vw, vh, old);
        if self.js.is_some() {
            self.rects.clear();
            for i in self.layout.items.iter().filter(|i| matches!(i.kind, Kind::Box | Kind::Area)) {
                self.rects.entry(i.node).or_insert([i.x, i.y, i.w, i.h]);
            }
        }
    }

    /// Brings style and layout up to date for a viewport in CSS pixels.
    /// Does nothing when the viewport is unchanged.
    pub fn update(&mut self, fonts: &mut Fonts, vw: f32, vh: f32) {
        let resized = vw != self.vw || vh != self.vh;
        let sig = self.sheet.media.iter().take(64).enumerate().fold(0u64, |s, (i, m)| s | (m.matches(vw, vh) as u64) << i);
        let restyle = self.styles.is_empty() || sig != self.media_sig || (self.viewport_units && resized);
        if restyle {
            self.restyle(vw, vh);
            self.media_sig = sig;
        }
        if restyle || resized {
            self.relayout(fonts, vw, vh);
        }
        self.vw = vw;
        self.vh = vh;
    }

    /// Records the hovered node; returns true when the page changed visually.
    pub fn set_hover(&mut self, fonts: &mut Fonts, node: NodeId) -> bool {
        if node == self.hover {
            return false;
        }
        self.hover = node;
        if !self.sheet.uses_hover || self.styles.is_empty() {
            return false;
        }
        self.restyle(self.vw, self.vh);
        self.relayout(fonts, self.vw, self.vh);
        true
    }

    /// Topmost element at a document position, or `NONE`.
    pub fn hit(&self, x: f32, y: f32) -> NodeId {
        self.layout
            .items
            .iter()
            .rev()
            .find(|i| matches!(i.kind, Kind::Box | Kind::Text | Kind::Image) && x >= i.x && x < i.x + i.w && y >= i.y && y < i.y + i.h)
            .map_or(NONE, |i| i.node)
    }

    /// `href` of the nearest enclosing link.
    pub fn link(&self, mut n: NodeId) -> Option<&str> {
        while n != NONE && n != 0 {
            if self.dom.nodes[n as usize].tag == atom::A {
                if let Some(h) = self.dom.attr(n, atom::HREF) {
                    return Some(h);
                }
            }
            n = self.dom.nodes[n as usize].parent;
        }
        None
    }

    pub fn pointer(&self, n: NodeId) -> bool {
        n != NONE && self.styles.get(n as usize).is_some_and(|s| s.pointer)
    }

    /// Document y of the element with this id, for `#fragment` links.
    pub fn anchor_y(&self, id: &str) -> Option<f32> {
        let target = (0..self.dom.nodes.len() as NodeId).find(|&n| self.dom.attr(n, atom::ID) == Some(id))?;
        let inside = |mut n: NodeId| {
            while n != NONE {
                if n == target {
                    return true;
                }
                n = self.dom.nodes[n as usize].parent;
            }
            false
        };
        self.layout.items.iter().find(|i| matches!(i.kind, Kind::Box | Kind::Text) && inside(i.node)).map(|i| i.y)
    }
}
