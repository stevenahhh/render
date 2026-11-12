//! A loaded document: DOM, stylesheet, computed styles and layout.

use crate::css::{FontFace, Sheet};
use crate::dom::{atom, is_stylesheet, parse_html, Atom, Dom, NodeId, HAS_SHEET, NONE};
use crate::font::Fonts;
use crate::url::Url;
use crate::img::Images;
use crate::js::{Host, Js};
use crate::FxMap;
use crate::layout::{layout, Kind, Layout};
use crate::style::{compute_styles, Dirty, Invalidation, Style, OWN};
use crate::net::{Site, Wake};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Set `RENDER_CHECK_RESTYLE` to have every incremental style update checked
/// against styling and laying out the whole page again: the program stops with
/// the first element or layout item that differs.
fn check_restyles() -> bool {
    static CHECK: OnceLock<bool> = OnceLock::new();
    *CHECK.get_or_init(|| std::env::var_os("RENDER_CHECK_RESTYLE").is_some())
}

/// How long a page shown progressively waits for its linked stylesheets after the
/// HTML has arrived; later ones restyle the page when they come. The preload
/// scanner requests them as soon as their tag arrives, over a connection opened
/// while the HTML was requested, so one usually lands one round trip after the
/// HTML: a few hundred milliseconds even on a 170 ms RTT server. One second is
/// twice that, while keeping a page whose HTML arrives within half a second
/// well inside a two-second first frame.
const CSS_BUDGET: Duration = Duration::from_millis(1000);

/// Most wall-clock time `settle` gives a page after its scripts ran. An app that
/// starts from timers (YouTube's needs several seconds of script in this engine)
/// finishes well within it; a page that keeps working without end is stopped.
const SETTLE_CAP: Duration = Duration::from_secs(15);

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

/// A script of the page, waiting for its turn.
struct Script {
    node: NodeId,
    /// `src` as written, empty for code in the page.
    src: String,
    /// The code of a script in the page.
    code: String,
    module: bool,
}

fn has_attr(dom: &Dom, n: NodeId, name: &str) -> bool {
    dom.atoms.get(name).is_some_and(|a| dom.attr(n, a).is_some())
}

/// The page's scripts in the order a browser runs them: those that block the
/// parser in document order, then deferred, async and module scripts, which run
/// after the document is parsed. `nomodule` scripts are for browsers without modules.
fn scripts_of(dom: &Dom) -> Vec<Script> {
    let (mut now, mut later) = (Vec::new(), Vec::new());
    let mut n = 0;
    while n != NONE {
        if dom.nodes[n as usize].tag == atom::SCRIPT {
            let ty = dom.attr(n, atom::TYPE).unwrap_or("").trim().to_ascii_lowercase();
            let module = ty == "module";
            let classic = ty.is_empty() || ty.contains("javascript");
            if module || (classic && !has_attr(dom, n, "nomodule")) {
                let src = dom.attr(n, atom::SRC);
                let mut code = String::new();
                if src.is_none() {
                    dom.text_content(n, &mut code);
                }
                let deferred = module || (src.is_some() && (has_attr(dom, n, "defer") || has_attr(dom, n, "async")));
                let script = Script { node: n, src: src.unwrap_or("").to_string(), code, module };
                if deferred { later.push(script) } else { now.push(script) }
            }
        }
        n = dom.next_in(n, 0);
    }
    now.extend(later);
    now
}

/// The stylesheet references in a document, then its image and (when asked)
/// script references, so they can be downloaded together before the page is built.
fn resource_srcs(html: &str, scripts: bool) -> (Vec<String>, Vec<String>) {
    let dom = parse_html(html);
    let first = |set: &str| set.split(',').next().and_then(|c| c.split_ascii_whitespace().next()).map(str::to_string);
    let (mut css, mut out) = (Vec::new(), Vec::new());
    for n in 0..dom.nodes.len() as NodeId {
        match dom.nodes[n as usize].tag {
            atom::LINK if dom.attr(n, atom::REL).is_some_and(is_stylesheet) => {
                css.extend(dom.attr(n, atom::HREF).map(str::to_string));
            }
            atom::SCRIPT if scripts => out.extend(dom.attr(n, atom::SRC).map(str::to_string)),
            atom::IMG => {
                out.extend(dom.attr(n, atom::SRC).map(str::to_string));
                out.extend(dom.attr(n, atom::SRCSET).and_then(first));
            }
            atom::VIDEO => out.extend(dom.attr(n, atom::POSTER).map(str::to_string)),
            // A <source> names an image with srcset; src is for video and audio.
            atom::SOURCE => out.extend(dom.attr(n, atom::SRCSET).and_then(first)),
            _ => {}
        }
    }
    (css, out)
}

/// The page's style sheet: built-in defaults, then every `<style>` and linked
/// stylesheet in the document, in document order. With `wait` a linked one is
/// downloaded first; without, one still downloading is left out. Returns the
/// sheet, whether its CSS uses viewport units, and the stylesheets left out.
fn style_sheet(dom: &mut Dom, base: Option<&Path>, site: Option<&Site>, wait: bool, adopted: &str) -> (Sheet, bool, Vec<String>) {
    let mut sheet = Sheet::default();
    sheet.add(UA_CSS, &mut dom.atoms, 0, "");
    let (mut viewport_units, mut left_out) = (false, Vec::new());
    let mut owners = Vec::new();
    let mut n = 0;
    while n != NONE {
        if dom.kind_of(n) == HAS_SHEET {
            owners.push(n);
        }
        n = dom.next_in(n, 0);
    }
    for n in owners {
        match dom.nodes[n as usize].tag {
            atom::STYLE => {
                let mut css = String::new();
                dom.text_content(n, &mut css);
                sheet.add(&css, &mut dom.atoms, 1, "");
            }
            atom::LINK => {
                let is_css = dom.attr(n, atom::REL).is_some_and(is_stylesheet);
                let href = dom.attr(n, atom::HREF).filter(|_| is_css);
                let path = match (site, href) {
                    (Some(site), Some(h)) if !wait => site.file_if_ready(h).unwrap_or_else(|| {
                        left_out.push(h.to_string());
                        None
                    }),
                    (Some(site), h) => h.and_then(|h| site.file(h)),
                    (None, h) => h.zip(base).map(|(h, b)| b.join(h)),
                };
                if let Some(css) = path.and_then(|p| std::fs::read(p).ok()) {
                    let css = String::from_utf8_lossy(&css);
                    viewport_units |= css.contains("vw") || css.contains("vh");
                    // Where the sheet came from: its URL, or its path relative to the page.
                    let at = match site {
                        Some(site) => href.and_then(|h| site.url.join(h)).map(|u| u.to_string()).unwrap_or_default(),
                        None => href.unwrap_or_default().to_string(),
                    };
                    sheet.add(&css, &mut dom.atoms, 1, &at);
                }
            }
            _ => {}
        }
    }
    sheet.add(adopted, &mut dom.atoms, 1, "");
    crate::shadow::add_scoped_sheets(dom, &mut sheet);
    sheet.finish();
    (sheet, viewport_units, left_out)
}

/// A page's HTML and the stylesheets that arrived in time, downloaded on a worker
/// thread for `Page::from_download` to build on the main one.
pub struct Download {
    pub site: Arc<Site>,
    html: String,
    /// How many linked stylesheets arrived before the page was handed over, of how many.
    pub stylesheets: (usize, usize),
}

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
    /// Present for a page with scripts, except while it is lent to them.
    js: Option<Js>,
    /// The page has scripts, which read the boxes of the layout.
    scripted: bool,
    rects: FxMap<u32, [f32; 4]>,
    scroll: f32,
    dirty: bool,
    /// A script changed a `<style>` or stylesheet `<link>`.
    sheet_dirty: bool,
    /// The viewport scripts see, until a frame of another size is laid out.
    script_size: (f32, f32),
    /// Scripts still to run, in the order they run.
    pending: Vec<Script>,
    /// The page's address: its URL, or the `file:` URL of a local page.
    url: Url,
    /// CSS of the sheets the document adopted.
    adopted: String,
    /// Elements to style again since the last update.
    marks: Dirty,
    /// What a change can restyle, worked out from `sheet` when first needed.
    inval: Option<Invalidation>,
    /// Every element is styled again at the next update: the sheet changed, or
    /// no narrower set of elements is known to be enough.
    restyle_all: bool,
    /// Set for a page opened from the web: its URL and downloaded resources.
    site: Option<Arc<Site>>,
    /// Keep every element's box in the layout. Set before the first `update`.
    pub record_boxes: bool,
    /// For a page shown before its resources arrived: linked stylesheets left out of
    /// `sheet` so far, the site's arrival count last looked at, and whether arrived
    /// images call for a new layout.
    css_left_out: Vec<String>,
    arrivals: usize,
    stale: bool,
    /// The sheet's `@font-face` rules.
    web_faces: Rc<[FontFace]>,
    /// Web fonts asked for that are still downloading, for a page that does not wait.
    fonts_pending: Vec<Box<str>>,
    /// Downloads are waited for (one-shot output) rather than taken in later.
    wait: bool,
    /// Directory a local page's relative URLs resolve against.
    base: Option<PathBuf>,
}

/// The address of a page from the web, as scripts see it.
fn web_url(site: &Site) -> Url {
    Url::parse(&site.url.to_string(), None).unwrap_or_default()
}

/// Moves a sheet's `@font-face` rules out, to be shared with the font matcher.
fn take_faces(sheet: &mut Sheet) -> Rc<[FontFace]> {
    Rc::from(std::mem::take(&mut sheet.fonts))
}

impl Page {
    /// Parses HTML and every stylesheet it references, and runs its scripts.
    /// `base` is the directory that relative paths resolve against.
    pub fn from_html(html: &str, base: Option<&Path>) -> Page {
        let url = base.map_or_else(|| Url::parse("about:blank", None).unwrap_or_default(), |b| Url::from_path(&b.join("index.html")));
        let mut page = Page::parse(html, url, base, false, None, true);
        page.run_pending(&mut Fonts::new(), 1024.0, 768.0);
        page
    }

    /// Scripts are kept for `run_scripts` or, one at a time, for `run_deferred`,
    /// so the time to the first frame does not depend on how much script a page
    /// carries. A page shown in the window (`defer`) rolls its own dice; one-shot
    /// output is repeatable.
    ///
    /// With `site`, stylesheets, scripts and images are downloaded from the web.
    ///
    /// Without `wait` (for a page from the web), nothing waits for a download: a
    /// stylesheet or image that has not arrived is left out until it does.
    fn parse(html: &str, url: Url, base: Option<&Path>, defer: bool, site: Option<Arc<Site>>, wait: bool) -> Page {
        let mut dom = parse_html(html);
        let arrivals = site.as_ref().map_or(0, |s| s.arrivals());
        let (mut sheet, css_units, css_left_out) = style_sheet(&mut dom, base, site.as_deref(), wait, "");
        let web_faces = take_faces(&mut sheet);
        let viewport_units = css_units || html.contains("vw") || html.contains("vh");
        let mut title = String::new();
        if let Some(t) = (0..dom.nodes.len() as NodeId).find(|&n| dom.nodes[n as usize].tag == atom::TITLE) {
            dom.text_content(t, &mut title);
        }
        let scripts = scripts_of(&dom);
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
            scripted: !scripts.is_empty(),
            rects: FxMap::default(),
            scroll: 0.0,
            dirty: false,
            sheet_dirty: false,
            script_size: (1024.0, 768.0),
            pending: Vec::new(),
            url,
            adopted: String::new(),
            marks: Dirty::default(),
            inval: None,
            restyle_all: false,
            site: site.clone(),
            record_boxes: false,
            css_left_out,
            arrivals,
            stale: false,
            web_faces,
            fonts_pending: Vec::new(),
            wait,
            base: base.map(Path::to_path_buf),
        };
        if let Some(site) = site {
            if defer {
                let later = scripts.iter().filter(|s| !s.src.is_empty()).map(|s| s.src.clone());
                site.prefetch_later(later.collect());
            }
            page.images.fetch_from(site, wait);
        }
        if !scripts.is_empty() {
            let js = Js::new(&page.url, page.site.clone());
            page.js = Some(if defer { js } else { js.repeatable() });
            page.pending = scripts;
        }
        page
    }

    /// A page with nothing in it, standing in for one that is lent to its scripts.
    fn placeholder() -> Page {
        Page {
            dom: Dom::empty(),
            sheet: Sheet::default(),
            styles: Vec::new(),
            layout: Layout::default(),
            images: Images::default(),
            title: String::new(),
            viewport_units: false,
            media_sig: 0,
            vw: -1.0,
            vh: -1.0,
            hover: NONE,
            js: None,
            scripted: false,
            rects: FxMap::default(),
            scroll: 0.0,
            dirty: false,
            sheet_dirty: false,
            script_size: (0.0, 0.0),
            pending: Vec::new(),
            url: Url::default(),
            adopted: String::new(),
            marks: Dirty::default(),
            inval: None,
            restyle_all: false,
            site: None,
            record_boxes: false,
            css_left_out: Vec::new(),
            arrivals: 0,
            stale: false,
            web_faces: Rc::from([]),
            fonts_pending: Vec::new(),
            wait: false,
            base: None,
        }
    }

    /// The code of an external classic script, read from disk or the web.
    fn script_code(&self, src: &str) -> Option<(String, String)> {
        let url = Url::parse(src, Some(&self.url))?;
        let file = match &self.site {
            Some(site) => site.file(src),
            None => url.to_path(),
        };
        let code = std::fs::read(file?).ok()?;
        Some((String::from_utf8_lossy(&code).into_owned(), url.href()))
    }

    /// Runs the first `count` waiting scripts in order; with `last` the load event follows.
    fn run_scripts(&mut self, fonts: &mut Fonts, count: usize, last: bool) {
        let scripts: Vec<Script> = self.pending.drain(..count.min(self.pending.len())).collect();
        let page_url = self.url.href();
        let mut runs = Vec::with_capacity(scripts.len());
        for s in scripts {
            match (s.module, s.src.is_empty()) {
                (true, true) => runs.push((s.node, "inline script".to_string(), Some(s.code), page_url.clone(), true)),
                (true, false) => match Url::parse(&s.src, Some(&self.url)) {
                    Some(url) => runs.push((s.node, s.src, None, url.href(), true)),
                    None => eprintln!("[js] cannot read script {}", s.src),
                },
                (false, true) => runs.push((s.node, "inline script".to_string(), Some(s.code), page_url.clone(), false)),
                (false, false) => match self.script_code(&s.src) {
                    Some((code, url)) => runs.push((s.node, s.src, Some(code), url, false)),
                    None => eprintln!("[js] cannot read script {}", s.src),
                },
            }
        }
        self.with_js(fonts, |js| {
            for (node, name, code, url, module) in runs {
                if module {
                    js.run_module(code.as_deref(), &name, &url);
                } else {
                    js.run_script(node, code.as_deref().unwrap_or(""), &name, &url);
                }
            }
            if last {
                js.load();
            }
        });
    }

    /// Runs every script still waiting, then fires the load event, scripts seeing
    /// a viewport of `vw` by `vh`.
    pub fn run_pending(&mut self, fonts: &mut Fonts, vw: f32, vh: f32) {
        if self.vw < 0.0 {
            self.script_size = (vw, vh);
        }
        if !self.pending.is_empty() {
            self.run_scripts(fonts, self.pending.len(), true);
        }
    }

    /// Runs the next script `load_deferred` held back; the last one also fires the
    /// load event. Returns how many are left, or `None` when there was nothing to run.
    pub fn run_deferred(&mut self, fonts: &mut Fonts) -> Option<usize> {
        if self.pending.is_empty() {
            return None;
        }
        let left = self.pending.len() - 1;
        self.run_scripts(fonts, 1, left == 0);
        Some(left)
    }

    /// Runs `f` with the page lent to the script engine, together with the
    /// fonts a layout that scripts ask for needs.
    fn with_js<R>(&mut self, fonts: &mut Fonts, f: impl FnOnce(&mut Js) -> R) -> Option<R> {
        let mut js = self.js.take()?;
        let page = std::mem::replace(self, Page::placeholder());
        js.enter(Host { page, fonts: std::mem::take(fonts) });
        let r = f(&mut js);
        let host = js.leave();
        *self = host.page;
        *fonts = host.fonts;
        self.js = Some(js);
        Some(r)
    }

    fn in_style(&self, n: NodeId) -> bool {
        let tag = |n: NodeId| self.dom.nodes[n as usize].tag;
        let parent = self.dom.nodes[n as usize].parent;
        tag(n) == atom::STYLE || (parent != NONE && tag(parent) == atom::STYLE)
    }

    /// True for a node of an inline `<svg>`, which is drawn from its markup as a whole.
    fn in_svg(&self, mut n: NodeId) -> bool {
        while n != NONE {
            if self.dom.nodes[n as usize].tag == atom::SVG {
                return true;
            }
            n = self.dom.nodes[n as usize].parent;
        }
        false
    }

    /// Marks the elements a change can restyle, as `mark` finds them with the
    /// sheet's invalidation sets; everything when it finds no narrower set.
    fn invalidate(&mut self, mark: impl FnOnce(&Invalidation, &mut Dirty, &Dom) -> bool) {
        if self.restyle_all {
            return;
        }
        let inval = self.inval.get_or_insert_with(|| Invalidation::new(&self.sheet));
        if !mark(inval, &mut self.marks, &self.dom) {
            self.restyle_all = true;
        }
    }

    /// Notes a change to what the style sheet is built from: the text of the
    /// document's `<style>` elements and the `rel` and `href` of its links.
    fn sheet_changed(&mut self) {
        self.sheet_dirty = true;
        self.restyle_all = true;
    }

    /// Notes a change of the attribute `name`, which had the value `old`: the
    /// elements whose selectors it can change are styled again, and the layout,
    /// which reads `src`, `width` and the like, is redone for those it reads.
    pub(crate) fn attr_changed(&mut self, n: NodeId, name: Atom, old: Option<&str>) {
        self.dirty = true;
        if !self.dom.connected(n) {
            return;
        }
        if self.dom.nodes[n as usize].tag == atom::LINK && matches!(name, atom::REL | atom::HREF) {
            let was_sheet = name == atom::REL && old.is_some_and(is_stylesheet);
            if was_sheet || self.dom.kind_of(n) == HAS_SHEET {
                self.sheet_changed();
                return;
            }
        }
        if crate::layout::READ_ATTRS.contains(&name) || self.in_svg(n) {
            self.stale = true;
        }
        let atoms = &self.dom.atoms;
        let old: Vec<Atom> = match name {
            atom::CLASS => old.map(|v| v.split_ascii_whitespace().filter_map(|c| atoms.get(c)).collect()).unwrap_or_default(),
            atom::ID => old.and_then(|v| atoms.get(v)).into_iter().collect(),
            _ => Vec::new(),
        };
        // Inline style, and a poster that makes a `<video>` show an image, are read
        // when styling the element itself.
        let own = name == atom::STYLE || (name == atom::POSTER && self.dom.nodes[n as usize].tag == atom::VIDEO);
        self.invalidate(|inval, marks, dom| {
            if own {
                marks.mark(dom, n, OWN);
            }
            inval.attr_changed(marks, dom, n, name, &old)
        });
    }

    /// Notes new data in a text or comment node: a new layout, and for the text
    /// of a `<style>` a new style sheet.
    pub(crate) fn text_changed(&mut self, n: NodeId) {
        self.dirty = true;
        if !self.in_style(n) {
            self.stale = true;
        } else if self.dom.connected(n) {
            self.sheet_changed();
        }
    }

    /// Notes that `n` is about to leave its parent, or has just been inserted.
    /// Returns the kinds of node (`dom::HAS_*`) that this takes out of or brings
    /// into the document.
    pub(crate) fn moved(&mut self, n: NodeId) -> u32 {
        self.dirty = true;
        if !self.dom.connected(n) {
            return 0;
        }
        // An `<html>` anywhere but at the root changes the size `rem` lengths
        // resolve against for the elements after it.
        let (mut kinds, mut root, mut c) = (0, false, n);
        while c != NONE {
            kinds |= self.dom.kind_of(c);
            root |= self.dom.nodes[c as usize].tag == atom::HTML;
            c = self.dom.next_in(c, n);
        }
        self.stale = true;
        if kinds & HAS_SHEET != 0 || self.in_style(n) {
            self.sheet_changed();
        } else if root {
            self.restyle_all = true;
        } else {
            let hover = self.hover;
            self.invalidate(|inval, marks, dom| inval.child_moved(marks, dom, n, hover));
        }
        kinds
    }

    pub(crate) fn adopt(&mut self, css: String) {
        self.adopted = css;
        self.sheet_changed();
        self.dirty = true;
    }

    /// A node's border box `[x, y, w, h]` from a layout brought up to date first.
    pub(crate) fn fresh_rect(&mut self, fonts: &mut Fonts, n: NodeId) -> [f32; 4] {
        let (vw, vh) = if self.vw < 0.0 { self.script_size } else { (self.vw, self.vh) };
        self.update(fonts, vw, vh);
        self.rects.get(&n).copied().unwrap_or([0.0; 4])
    }

    /// Viewport width, height and scroll offset in CSS pixels, as scripts see them.
    pub(crate) fn script_view(&self) -> [f32; 3] {
        if self.vw < 0.0 {
            [self.script_size.0, self.script_size.1, 0.0]
        } else {
            [self.vw, self.vh, self.scroll]
        }
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
    pub fn click(&mut self, fonts: &mut Fonts, node: NodeId) -> bool {
        node != NONE && self.with_js(fonts, |js| js.dispatch(node, "click")).unwrap_or(false)
    }

    /// From now on animation frame callbacks wait for `frame`, which the window calls
    /// once per display refresh. A page whose scripts have not run yet only records it.
    pub fn frames_from_host(&mut self, fonts: &mut Fonts) {
        if self.js.as_ref().is_some_and(Js::booted) {
            self.with_js(fonts, Js::frames_from_host);
        } else if let Some(js) = &mut self.js {
            js.frames_from_host();
        }
    }

    /// Runs the animation frame callbacks queued before this frame; true when more
    /// wait for the next one.
    pub fn frame(&mut self, fonts: &mut Fonts) -> bool {
        self.with_js(fonts, Js::frame).unwrap_or(false)
    }

    /// True when animation frame callbacks wait for a frame.
    pub fn wants_frame(&mut self, fonts: &mut Fonts) -> bool {
        self.js.as_ref().is_some_and(Js::booted) && self.with_js(fonts, Js::frames_waiting).unwrap_or(false)
    }

    /// Tells the page's scripts whether it can be seen (`document.visibilityState`).
    pub fn set_hidden(&mut self, fonts: &mut Fonts, hidden: bool) {
        if self.js.as_ref().is_some_and(Js::booted) {
            self.with_js(fonts, |js| js.set_hidden(hidden));
        } else if let Some(js) = &mut self.js {
            js.set_hidden(hidden);
        }
    }

    /// Runs due script timers; returns milliseconds until the next one.
    pub fn tick(&mut self, fonts: &mut Fonts) -> Option<f64> {
        if !self.pending.is_empty() {
            return None;
        }
        self.with_js(fonts, |js| js.timers()).flatten()
    }

    /// Runs the page's scripts, lays it out, and lets its timers run until the
    /// page has waited `budget` with nothing to do, for one-shot output where no
    /// event loop keeps the page alive. Nothing is painted until the end, so those
    /// waits are skipped rather than slept. The time scripts spend working does
    /// not count, as an app may need seconds of script to start; `SETTLE_CAP` of
    /// wall-clock time ends a page that never stops. Answers to requests the page
    /// made take real time and are waited for.
    pub fn settle(&mut self, fonts: &mut Fonts, vw: f32, vh: f32, budget: Duration) {
        self.run_pending(fonts, vw, vh);
        let start = Instant::now();
        let mut idle = Duration::ZERO;
        loop {
            self.update(fonts, vw, vh);
            let next = self.tick(fonts);
            if start.elapsed() > SETTLE_CAP {
                break;
            }
            if self.js.as_ref().is_some_and(Js::waiting) {
                let wait = next.map_or(2.0, |ms| ms.clamp(0.1, 2.0));
                std::thread::sleep(Duration::from_secs_f64(wait / 1e3));
                continue;
            }
            let Some(ms) = next else { break };
            let wait = Duration::from_secs_f64(ms.max(1.0) / 1e3);
            if idle + wait > budget {
                break;
            }
            idle += wait;
            self.with_js(fonts, |js| js.skip(wait.as_secs_f64() * 1e3));
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

    /// Scrolls to `y` and dispatches `scroll`, so scripts see the position.
    pub fn scroll_to(&mut self, fonts: &mut Fonts, y: f32) {
        self.scroll = y;
        self.with_js(fonts, |js| js.dispatch(0, "scroll"));
    }

    /// Like `load`, but for the window, which runs the scripts with `run_deferred`.
    pub fn load_deferred(path: &Path) -> std::io::Result<Page> {
        if let Some(page) = Page::from_web(path, true) {
            return page;
        }
        let bytes = std::fs::read(path)?;
        Ok(Page::parse(&String::from_utf8_lossy(&bytes), Url::from_path(path), path.parent(), true, None, true))
    }

    /// The page behind an `http://` or `https://` location, or `None` for a file path.
    fn from_web(path: &Path, defer: bool) -> Option<std::io::Result<Page>> {
        let location = path.to_str().filter(|l| crate::net::is_url(l))?;
        Some(Page::load_url(location, defer).map_err(std::io::Error::other))
    }

    /// Downloads a page and the stylesheets, scripts and images it names, the
    /// resources in parallel. With `defer` scripts are fetched in the background.
    pub fn load_url(location: &str, defer: bool) -> Result<Page, String> {
        let (site, html) = Site::open(location)?;
        let (mut srcs, rest) = resource_srcs(&html, !defer);
        srcs.extend(rest);
        site.prefetch(&srcs);
        Ok(Page::parse(&html, web_url(&site), None, defer, Some(site), true))
    }

    /// Downloads a page's HTML and its linked stylesheets, waiting for those at most
    /// `CSS_BUDGET` past the HTML; then starts its images downloading in the
    /// background unless `current` says a newer navigation has replaced this one.
    /// Runs on any thread: the page is built from the result by `from_download`.
    pub fn download(location: &str, current: &dyn Fn() -> bool) -> Result<Download, String> {
        let (site, html) = Site::open(location)?;
        let (css, rest) = resource_srcs(&html, false);
        site.prefetch_later(css.clone());
        site.preconnect(&rest);
        let stylesheets = site.wait_for(&css, Instant::now() + CSS_BUDGET);
        if current() {
            site.prefetch_later(rest);
        }
        Ok(Download { site, html, stylesheets })
    }

    /// Builds a page from a `download` without waiting for anything still on its
    /// way: stylesheets and images that arrive later are taken in by `take_arrivals`.
    /// Scripts are held back for `run_deferred`.
    pub fn from_download(download: Download) -> Page {
        let url = web_url(&download.site);
        Page::parse(&download.html, url, None, true, Some(download.site), false)
    }

    /// Calls `wake` from a downloading thread whenever a resource of this page arrives.
    pub fn on_arrival(&self, wake: Wake) {
        if let Some(js) = &self.js {
            js.on_answer(wake.clone());
        }
        if let Some(site) = &self.site {
            site.on_arrival(wake);
        }
    }

    /// Takes in the stylesheets, images and web fonts that arrived since the last
    /// call, so the next `update` restyles or lays the page out again. Returns
    /// how many stylesheets, images and fonts arrived.
    pub fn take_arrivals(&mut self, fonts: &mut Fonts) -> (usize, usize, usize) {
        let Some(site) = self.site.clone() else { return (0, 0, 0) };
        let arrivals = site.arrivals();
        if arrivals == self.arrivals {
            return (0, 0, 0);
        }
        self.arrivals = arrivals;
        let mut css = 0;
        if self.css_left_out.iter().any(|href| site.arrived(href)) {
            let (mut sheet, units, left_out) = style_sheet(&mut self.dom, None, Some(&site), false, &self.adopted);
            css = self.css_left_out.len().saturating_sub(left_out.len());
            self.web_faces = take_faces(&mut sheet);
            self.sheet = sheet;
            self.inval = None;
            self.viewport_units |= units;
            self.styles.clear();
            self.css_left_out = left_out;
        }
        let images = self.images.arrived();
        let pending = std::mem::take(&mut self.fonts_pending);
        let (arrived, waiting): (Vec<_>, Vec<_>) = pending.into_iter().partition(|url| site.arrived(url));
        self.fonts_pending = waiting;
        for url in &arrived {
            fonts.provide(url, site.file_if_ready(url).flatten().and_then(|p| std::fs::read(p).ok()));
        }
        self.stale |= images + arrived.len() > 0;
        (css, images, arrived.len())
    }

    /// Fetches the web fonts the last layout asked for: waiting for them when
    /// the page waits for downloads, else taking in those already here and
    /// leaving the rest to `take_arrivals`. True when a font was taken in.
    fn fetch_fonts(&mut self, fonts: &mut Fonts) -> bool {
        let wanted = fonts.take_wanted();
        if wanted.is_empty() {
            return false;
        }
        let read = |p: PathBuf| std::fs::read(p).ok();
        let mut taken = false;
        match &self.site {
            Some(site) if self.wait => {
                site.prefetch(&wanted.iter().map(|u| u.to_string()).collect::<Vec<_>>());
                for url in &wanted {
                    fonts.provide(url, site.file(url).and_then(read));
                }
                taken = true;
            }
            Some(site) => {
                for url in wanted {
                    match site.file_if_ready(&url) {
                        Some(path) => {
                            fonts.provide(&url, path.and_then(read));
                            taken = true;
                        }
                        None => self.fonts_pending.push(url),
                    }
                }
            }
            None => {
                for url in &wanted {
                    fonts.provide(url, self.base.as_ref().and_then(|b| read(b.join(&**url))));
                }
                taken = true;
            }
        }
        taken
    }

    /// The URL of a page opened from the web.
    pub fn url(&self) -> Option<&crate::net::Url> {
        self.site.as_ref().map(|s| &s.url)
    }

    /// Reads or downloads a page for one-shot output. Its scripts wait for
    /// `run_pending` or `settle`.
    pub fn load(path: &Path) -> std::io::Result<Page> {
        if let Some(page) = Page::from_web(path, false) {
            return page;
        }
        let bytes = std::fs::read(path)?;
        Ok(Page::parse(&String::from_utf8_lossy(&bytes), Url::from_path(path), path.parent(), false, None, true))
    }

    /// Builds the style sheet again after scripts changed a `<style>` or `<link>`.
    fn refresh_sheet(&mut self) {
        if std::mem::take(&mut self.sheet_dirty) {
            let (mut sheet, units, left_out) = style_sheet(&mut self.dom, self.base.as_deref(), self.site.as_deref(), self.wait, &self.adopted);
            self.web_faces = take_faces(&mut sheet);
            self.sheet = sheet;
            self.inval = None;
            self.viewport_units |= units;
            self.css_left_out = left_out;
        }
    }

    /// Styles (with `restyle`) and lays out the page, its shadow trees drawn in place.
    fn style_and_layout(&mut self, fonts: &mut Fonts, vw: f32, vh: f32, restyle: bool) {
        if restyle {
            self.refresh_sheet();
        }
        let composed = crate::shadow::compose(&mut self.dom);
        if restyle {
            self.styles = compute_styles(&self.dom, &self.sheet, vw, vh, self.hover);
            self.marks.clear();
            self.restyle_all = false;
        }
        self.relayout(fonts, vw, vh);
        if let Some(c) = composed {
            crate::shadow::restore(&mut self.dom, c);
        }
    }

    fn relayout(&mut self, fonts: &mut Fonts, vw: f32, vh: f32) {
        self.stale = false;
        fonts.set_web_faces(&self.web_faces);
        let generated = if self.sheet.generates { crate::generated::add(&mut self.dom, &mut self.styles) } else { None };
        loop {
            let mut old = std::mem::take(&mut self.layout);
            old.record_boxes = self.record_boxes || self.scripted;
            self.layout = layout(&self.dom, &self.styles, fonts, &mut self.images, vw, vh, old);
            // Text measured with a stand-in face is measured again with the web font.
            if !self.fetch_fonts(fonts) {
                break;
            }
        }
        if let Some(g) = generated {
            crate::generated::remove(&mut self.dom, &mut self.styles, g, &mut self.layout);
        }
        if self.scripted {
            self.rects.clear();
            for i in self.layout.items.iter().filter(|i| matches!(i.kind, Kind::Box | Kind::Area)) {
                self.rects.entry(i.node).or_insert([i.x, i.y, i.w, i.h]);
            }
            // Inline elements draw no box of their own: theirs is the union of their content.
            let mut inline: FxMap<u32, [f32; 4]> = FxMap::default();
            for i in self.layout.items.iter().filter(|i| matches!(i.kind, Kind::Text | Kind::Image)) {
                let mut n = i.node;
                while n != NONE && n != 0 && !self.rects.contains_key(&n) {
                    let r = inline.entry(n).or_insert([i.x, i.y, i.w, i.h]);
                    let (x1, y1) = ((r[0] + r[2]).max(i.x + i.w), (r[1] + r[3]).max(i.y + i.h));
                    (r[0], r[1]) = (r[0].min(i.x), r[1].min(i.y));
                    (r[2], r[3]) = (x1 - r[0], y1 - r[1]);
                    n = self.dom.nodes[n as usize].parent;
                }
            }
            self.rects.extend(inline);
        }
    }

    /// Brings style and layout up to date for a viewport in CSS pixels.
    /// Does nothing when the viewport is unchanged and nothing changed.
    pub fn update(&mut self, fonts: &mut Fonts, vw: f32, vh: f32) {
        self.refresh(fonts, vw, vh);
    }

    /// `update`; true when styles or the layout changed.
    fn refresh(&mut self, fonts: &mut Fonts, vw: f32, vh: f32) -> bool {
        let resized = vw != self.vw || vh != self.vh;
        let mut restyle = self.styles.is_empty() || self.restyle_all || (self.viewport_units && resized);
        // Which media queries match changes only with the viewport or the sheet, and
        // a new sheet comes with a restyle: it is built first, to be the one matched.
        if restyle || resized {
            if restyle {
                self.refresh_sheet();
            }
            let sig = self.sheet.media.iter().take(64).enumerate().fold(0u64, |s, (i, m)| s | (m.matches(vw, vh) as u64) << i);
            restyle |= sig != self.media_sig;
            self.media_sig = sig;
        }
        let changed = if restyle {
            self.style_and_layout(fonts, vw, vh, true);
            true
        } else if !self.marks.is_empty() {
            self.restyle_marked(fonts, vw, vh, resized)
        } else if resized || self.stale {
            self.style_and_layout(fonts, vw, vh, false);
            true
        } else {
            false
        };
        self.vw = vw;
        self.vh = vh;
        changed
    }

    /// Styles again the elements marked since the last update, then lays the page
    /// out again when a style changed in what layout reads, or repaints the boxes
    /// whose colours alone changed. With shadow trees, whose composed form the
    /// marks do not follow, every element is styled again instead.
    fn restyle_marked(&mut self, fonts: &mut Fonts, vw: f32, vh: f32, resized: bool) -> bool {
        let restyled = match crate::shadow::compose(&mut self.dom) {
            Some(composed) => {
                crate::shadow::restore(&mut self.dom, composed);
                None
            }
            None => crate::style::restyle(&self.dom, &self.sheet, vw, vh, self.hover, &mut self.styles, &mut self.marks),
        };
        let Some(r) = restyled else {
            self.style_and_layout(fonts, vw, vh, true);
            return true;
        };
        let relayout = resized || self.stale || r.layout || (!r.paint.is_empty() && !self.layout.recolour(&self.dom, &self.styles, &r.paint));
        if relayout {
            self.style_and_layout(fonts, vw, vh, false);
        }
        if check_restyles() {
            self.check(fonts, vw, vh);
            eprintln!("[restyle check] incremental update matches a full one ({} recoloured, relayout {relayout})", r.paint.len());
        }
        relayout || !r.paint.is_empty()
    }

    /// Stops the program when the styles or the layout differ from styling and
    /// laying out the whole page again (see `check_restyles`).
    fn check(&mut self, fonts: &mut Fonts, vw: f32, vh: f32) {
        let mut full = compute_styles(&self.dom, &self.sheet, vw, vh, self.hover);
        if let Some(d) = crate::style::first_difference(&self.dom, &self.styles, &full) {
            panic!("incremental restyle differs from a full one at {d}");
        }
        let mut fresh = Layout { record_boxes: self.record_boxes || self.scripted, ..Layout::default() };
        let generated = if self.sheet.generates { crate::generated::add(&mut self.dom, &mut full) } else { None };
        fresh = layout(&self.dom, &full, fonts, &mut self.images, vw, vh, fresh);
        if let Some(g) = generated {
            crate::generated::remove(&mut self.dom, &mut full, g, &mut fresh);
        }
        if let Some(d) = self.layout.difference(&fresh) {
            panic!("layout after an incremental restyle differs from a full one: {d}");
        }
    }

    /// Records the hovered node; returns true when the page changed visually.
    pub fn set_hover(&mut self, fonts: &mut Fonts, node: NodeId) -> bool {
        if node == self.hover {
            return false;
        }
        let old = std::mem::replace(&mut self.hover, node);
        if !self.sheet.uses_hover || self.styles.is_empty() {
            return false;
        }
        self.invalidate(|inval, marks, dom| inval.hover_changed(marks, dom, old, node));
        self.refresh(fonts, self.vw, self.vh)
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

#[cfg(test)]
mod tests {
    use super::Page;
    use crate::css::Len;
    use crate::dom::{NodeId, NONE};
    use crate::font::Fonts;
    use crate::layout::{layout, Layout};
    use crate::style::{compute_styles, first_difference, query};

    /// Checks that the page's styles and layout are those of styling and laying out
    /// the whole page again at 1024 by 768, on the composed tree, as updates do.
    fn assert_matches_full(page: &mut Page, fonts: &mut Fonts) {
        let composed = crate::shadow::compose(&mut page.dom);
        let mut full = compute_styles(&page.dom, &page.sheet, 1024.0, 768.0, page.hover);
        let styles = first_difference(&page.dom, &page.styles, &full);
        let generated = crate::generated::add(&mut page.dom, &mut full);
        let mut fresh = layout(&page.dom, &full, fonts, &mut page.images, 1024.0, 768.0, Layout { record_boxes: true, ..Layout::default() });
        if let Some(g) = generated {
            crate::generated::remove(&mut page.dom, &mut full, g, &mut fresh);
        }
        if let Some(c) = composed {
            crate::shadow::restore(&mut page.dom, c);
        }
        assert_eq!(styles, None);
        assert_eq!(page.layout.difference(&fresh), None);
    }

    /// Loads a page whose script changes it after it was styled once, reading
    /// layout where the script has `;;` so that the update there is incremental,
    /// and checks the result against a full restyle. `$(id)` finds an element.
    fn after_script(css: &str, body: &str, script: &str) -> (Page, Fonts) {
        let read = "document.body.offsetHeight;";
        let script = script.replace(";;", &format!(";{read}"));
        let html = format!("<html><head><style>{css}</style></head><body>{body}<script>const $=(i)=>document.getElementById(i);{read}{script}{read}</script></body></html>");
        let mut page = Page::from_html(&html, None);
        let mut fonts = Fonts::new();
        page.update(&mut fonts, 1024.0, 768.0);
        assert_matches_full(&mut page, &mut fonts);
        (page, fonts)
    }

    /// Text colours, as hex, of the elements matching `sel` below `root`.
    fn colours(page: &mut Page, root: NodeId, sel: &str) -> Vec<String> {
        let found = query(&mut page.dom, root, sel, false);
        found.into_iter().map(|n| format!("{:06x}", page.styles[n as usize].color.0 & 0xff_ffff)).collect()
    }

    #[test]
    fn script_changes_restyle_what_their_selectors_reach() {
        // (rule, css, body, script, selector, colours after it)
        let cases: [(&str, &str, &str, &str, &str, &[&str]); 25] = [
            ("descendant", ".on .x{color:#f00}", "<div id=a><p class=x></p><p></p></div><p class=x></p>", "$('a').className='on';", "p", &["ff0000", "000000", "000000"]),
            ("child", ".on>.x{color:#f00} b{color:#00f}", "<div id=a><p class=x><b class=x></b></p></div>", "$('a').classList.add('on');", ".x", &["ff0000", "0000ff"]),
            ("adjacent", ".a+.b{color:#f00}", "<i id=s></i><b class=b></b><b class=b></b>", "$('s').className='a';", "b", &["ff0000", "000000"]),
            ("adjacent after insertion", ".a+.b{color:#f00}", "<i class=a id=s></i><b class=b></b>", "$('s').after(document.createElement('u'));", "b", &["000000"]),
            ("general sibling", ".a~.b{color:#f00}", "<b class=b></b><i id=s></i><b class=b></b><p><b class=b></b></p><b class=b></b>", "$('s').className='a';", "b", &["000000", "ff0000", "000000", "ff0000"]),
            ("sibling after removal", ".a~.b{color:#f00}", "<i class=a id=s></i><b class=b></b>", "$('s').remove();", "b", &["000000"]),
            (
                "structural",
                "li:first-child{color:#f00} li:last-child{color:#00f} li:nth-child(2){color:#0f0}",
                "<ul id=u><li id=x></li><li></li><li id=y></li></ul>",
                "$('u').insertBefore(document.createElement('li'),$('u').firstChild);;$('y').remove();",
                "li",
                &["ff0000", "00ff00", "0000ff"],
            ),
            (
                "attribute",
                "[data-open] .p{color:#f00} [aria-hidden=true]{color:#00f}",
                "<div id=d><p class=p></p></div><i id=s></i>",
                "$('d').setAttribute('data-open','');$('s').setAttribute('aria-hidden','true');;$('d').removeAttribute('data-open');",
                "p, i",
                &["000000", "0000ff"],
            ),
            ("is with a combinator", ":is(.on .a) .x{color:#f00}", "<div id=d><div class=a><p class=x></p></div></div>", "$('d').className='on';", "p", &["ff0000"]),
            ("is with a sibling", ":is(.a + .b) .x{color:#f00}", "<i id=s></i><div class=b><p class=x></p></div>", "$('s').className='a';", "p", &["ff0000"]),
            ("where list", ":where(.on, .open) > .x{color:#f00}", "<div id=d><p class=x></p></div>", "$('d').className='open';", "p", &["ff0000"]),
            ("not list", ".x:not(.a, .b){color:#f00}", "<p class=x id=p></p><p class=x></p>", "$('p').classList.add('b');", "p", &["000000", "ff0000"]),
            ("not with a combinator", ".x:not(.off *){color:#f00}", "<div id=d><p class=x></p></div><p class=x></p>", "$('d').className='off';", "p", &["000000", "ff0000"]),
            ("generated content", ".on::after{content:'';display:block;height:5px} .on .x{color:#f00}", "<div id=d><p class=x></p></div>", "$('d').className='on';", "p", &["ff0000"]),
            ("not", ".c:not(.on){color:#f00}", "<p class=c id=c1></p><p class=c></p>", "$('c1').classList.add('on');", "p", &["000000", "ff0000"]),
            (
                "has",
                ".l:has(.sel){color:#f00}",
                "<div class=l><p id=i></p></div><div class=l id=m></div>",
                "$('i').className='sel';;const q=document.createElement('b');q.className='sel';$('m').appendChild(q);",
                "div",
                &["ff0000", "ff0000"],
            ),
            (
                "has outside the subject",
                ".c:has(.t) p{color:#f00}",
                "<div class=c><i id=x></i><p></p></div><div class=c id=d><p></p></div>",
                "$('x').className='t';;const t=document.createElement('i');t.className='t';$('d').appendChild(t);",
                "p",
                &["ff0000", "ff0000"],
            ),
            (
                "link attributes",
                ".on{color:#f00}",
                "<p id=p></p>",
                "const l=document.createElement('link');l.rel='preload';document.head.appendChild(l);l.setAttribute('as','style');;$('p').className='on';",
                "p",
                &["ff0000"],
            ),
            ("inherited", ".t{color:#f00}", "<div id=t><section><p></p></section></div>", "$('t').className='t';;$('t').style.color='#00f';", "p", &["0000ff"]),
            ("custom property", ".t{--c:#0f0} p{color:var(--c,#000)}", "<div id=t><section><p></p></section></div><p></p>", "$('t').className='t';", "p", &["00ff00", "000000"]),
            ("shown again", ".h{display:none} .x{color:#f00}", "<div id=h class=h><span><p class=x></p></span></div>", "$('h').className='';", "p", &["ff0000"]),
            ("hidden", ".h{display:none} .x{color:#f00}", "<div id=h><p class=x></p></div>", "$('h').className='h';", "p", &["000000"]),
            ("moved", ".a .x{color:#f00}", "<div class=a id=a></div><div><p class=x id=p></p></div>", "$('a').appendChild($('p'));", "p", &["ff0000"]),
            ("moved into svg", ".x{color:#f00}", "<svg id=s></svg><p class=x id=p></p>", "$('s').appendChild($('p'));", "p", &["000000"]),
            (
                "stylesheet",
                "",
                "<p id=p></p>",
                "const s=document.createElement('style');s.textContent='p{color:#f00}';document.head.appendChild(s);;$('p').className='x';;s.remove();",
                "p",
                &["000000"],
            ),
        ];
        for (rule, css, body, script, sel, want) in cases {
            let (mut page, _) = after_script(css, body, script);
            assert_eq!(colours(&mut page, 0, sel), want, "{rule}");
        }
    }

    #[test]
    fn colour_changes_repaint_the_laid_out_boxes() {
        let css = "div{border:1px solid #000} .on{background:#f00;color:#0f0;opacity:.5}";
        let toggle = "$('a').classList.toggle('on');";
        let (mut page, _) = after_script(css, "<div id=a>text <span>more</span></div>", &[toggle; 3].join(";"));
        assert_eq!(colours(&mut page, 0, "div, span"), ["00ff00", "00ff00"]);
    }

    #[test]
    fn font_relative_lengths_follow_font_size_changes() {
        let css = ".big{font-size:20px} .x{width:2em;height:2rem}";
        let (mut page, _) = after_script(css, "<div id=a><p class=x></p></div>", "$('a').className='big';;document.documentElement.className='big';");
        let p = query(&mut page.dom, 0, "p", true)[0];
        let s = &page.styles[p as usize];
        assert_eq!((s.width, s.height), (Len::Px(40.0), Len::Px(40.0)));
    }

    #[test]
    fn media_queries_and_hover_restyle_on_viewport_and_pointer_changes() {
        let css = "@media (max-width:600px){.x{color:#f00}} .h:hover .x{color:#00f}";
        let (mut page, mut fonts) = after_script(css, "<div class=h id=h><p class=x></p></div>", "");
        page.update(&mut fonts, 500.0, 768.0);
        assert_eq!(colours(&mut page, 0, "p"), ["ff0000"]);
        page.update(&mut fonts, 1024.0, 768.0);
        let h = query(&mut page.dom, 0, "#h", true)[0];
        assert!(page.set_hover(&mut fonts, h));
        assert_eq!(colours(&mut page, 0, "p"), ["0000ff"]);
        assert_matches_full(&mut page, &mut fonts);
        assert!(page.set_hover(&mut fonts, NONE));
        assert_eq!(colours(&mut page, 0, "p"), ["000000"]);
        assert_matches_full(&mut page, &mut fonts);
    }

    #[test]
    fn shadow_trees_restyle_their_scoped_rules_and_slots() {
        let script = "const r=$('h').attachShadow({mode:'open'});\
                      r.innerHTML='<style>.in{color:#f00} .on .in{color:#0f0}</style><div><p class=in><slot></slot></p></div>';;\
                      r.lastChild.className='on';;$('h').appendChild(document.createElement('b'));";
        let (mut page, _) = after_script(".in{color:#00f} b{color:#f0f}", "<div id=h><b></b></div>", script);
        let host = query(&mut page.dom, 0, "#h", true)[0];
        let root = page.dom.shadows[&host];
        assert_eq!(colours(&mut page, root, ".in"), ["00ff00"]);
        assert_eq!(colours(&mut page, 0, "b"), ["ff00ff", "ff00ff"]);
    }
    #[test]
    fn generated_boxes_are_laid_out_in_their_element_and_left_out_of_the_document() {
        let css = "body{margin:0;font:16px/20px sans-serif} .m{display:inline-block;width:10px;height:10px}\
                   #b::after{content:'';display:block;height:7px} #i::before{content:'';display:inline-block;width:30px;height:5px}\
                   #f{display:flex} #f::before{content:'';width:12px;height:3px}";
        let body = "<div id=b>t</div><div id=i><i class=m id=im></i></div><div id=f><i class=m id=fm></i></div>";
        let (mut page, _) = after_script(css, body, "$('b').title='x';");
        let nodes = page.dom.nodes.len();
        let mut rect = |sel: &str| page.describe(sel).and_then(|d| d.0);
        assert_eq!(rect("#b").map(|r| r[3]), Some(27.0));
        assert_eq!(rect("#im").map(|r| r[0]), Some(30.0));
        assert_eq!(rect("#fm").map(|r| r[0]), Some(12.0));
        assert_eq!(page.dom.nodes.len(), nodes);
        assert!(query(&mut page.dom, 0, "#b::after", false).is_empty());
    }
}
