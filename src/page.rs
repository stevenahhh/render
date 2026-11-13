//! A loaded document: DOM, stylesheet, computed styles and layout.

use crate::css::{FontFace, Sheet};
use crate::dom::{atom, is_stylesheet, parse_html, parse_xhtml, Atom, Dom, NodeId, HAS_SHEET, NONE};
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
head, script, style, title, meta, link, base, template, datalist, area, param,
source, track, [hidden] { display: none }
noscript { display: none !important }
dialog { display: block }
dialog:not([open]) { display: none }
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
sub { vertical-align: sub }
sup { vertical-align: super }
big { font-size: larger }
mark { background: yellow; color: black }
center { text-align: center }
hr { border-top: 1px solid #888; margin: 8px 0 }
tr { display: flex }
td, th { display: block; flex: 1 1 0; padding: 1px }
th { text-align: center }
img { display: inline-block; color: #666; font-size: 12px; overflow: hidden }
[popover]:not(:popover-open) { display: none }
input, button, select, textarea { display: inline-block; margin: 0; font: 11px system-ui;
  color: #000; letter-spacing: normal; text-transform: none; text-align: start; cursor: auto }
input, textarea { background-color: #fff; padding: 0.25em 0.5em; border: 2px solid #c0c0c0 }
textarea { border-width: 1px; white-space: pre-wrap }
input[type=search] { box-sizing: border-box }
button, input[type=submit], input[type=button], input[type=reset] { box-sizing: border-box;
  padding: 0 6px 1px; border: 2px solid #c0c0c0; background-color: #c0c0c0;
  color: rgba(0, 0, 0, 0.847); text-align: center; cursor: default }
input[type=submit], input[type=button], input[type=reset], select { white-space: pre }
select { box-sizing: border-box; padding: 2px 23px 3px 8px; border: 1px solid rgba(0, 0, 0, 0.847);
  border-radius: 5px; background-color: #fff; color: rgba(0, 0, 0, 0.847); cursor: default }
input[type=checkbox], input[type=radio] { box-sizing: border-box; width: 12px; height: 12px;
  margin: 3px 2px; padding: 0; border: 1px solid #808080; border-radius: 3px }
input[type=radio] { border-radius: 50% }
input[type=range] { width: 129px; height: 16px; margin: 2px; padding: 0; border: 0; background: none }
input[type=color] { width: 44px; height: 23px; padding: 1px; box-sizing: border-box }
input[type=file] { width: 238px; height: 18px; padding: 0; border: 0; background: none }
input[type=hidden] { display: none }
fieldset { margin: 0 2px; padding: 0.35em 0.75em 0.625em; border: 2px solid #c0c0c0 }
legend { padding: 0 2px }
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

/// The stylesheet `<link>`, style preload `<link>` and `<img>` elements in the
/// document with the source each names, in document order. A preload fires its
/// `load` event too: pages switch it to a stylesheet from that listener.
fn resource_nodes(dom: &Dom) -> Vec<(NodeId, Box<str>)> {
    let preload_style = |n: NodeId| {
        let rel = dom.attr(n, atom::REL).unwrap_or("");
        rel.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("preload"))
            && dom.atoms.get("as").and_then(|a| dom.attr(n, a)).is_some_and(|v| v.trim().eq_ignore_ascii_case("style"))
    };
    let mut out = Vec::new();
    let mut n = 0;
    while n != NONE {
        let src = match dom.nodes[n as usize].tag {
            atom::LINK if dom.attr(n, atom::REL).is_some_and(is_stylesheet) || preload_style(n) => dom.attr(n, atom::HREF),
            atom::IMG => dom.attr(n, atom::SRC),
            _ => None,
        };
        out.extend(src.map(str::trim).filter(|s| !s.is_empty()).map(|s| (n, s.into())));
        n = dom.next_in(n, 0);
    }
    out
}

/// What happens for one script when its turn comes.
enum Run {
    Classic { node: NodeId, name: String, code: String, url: String, external: bool },
    /// A module, with its code when it is in the page.
    Module { name: String, code: Option<String>, href: String },
    /// An external classic script that could not be had: its `error` event fires.
    Failed(NodeId),
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
        if matches!(dom.nodes[n as usize].tag, atom::STYLE | atom::LINK) && dom.kind_of(n) == HAS_SHEET {
            owners.push(n);
        }
        n = dom.next_in(n, 0);
    }
    for n in owners {
        match dom.nodes[n as usize].tag {
            atom::STYLE => {
                let mut css = String::new();
                dom.text_content(n, &mut css);
                let media = dom.attr(n, atom::MEDIA).unwrap_or("").to_string();
                sheet.add_in(&css, &mut dom.atoms, 1, "", &media);
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
                    let media = dom.attr(n, atom::MEDIA).unwrap_or("").to_string();
                    sheet.add_in(&css, &mut dom.atoms, 1, &at, &media);
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
    /// The stylesheet links and images the parser made, with their sources, which
    /// fire `load` or `error` before the document's load event. Those a script
    /// gives another source fire from the prelude instead.
    parsed_resources: Vec<(NodeId, Box<str>)>,
    /// Downloads are waited for (one-shot output) rather than taken in later.
    wait: bool,
    /// Directory a local page's relative URLs resolve against.
    base: Option<PathBuf>,
    /// The document a script asked to go to, and whether it replaces this one in
    /// the history; see `take_navigation`.
    pub(crate) navigation: Option<(String, bool)>,
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
        // XHTML: served as XML, or a file named as such.
        let xml = site.as_ref().map_or_else(|| url.path.ends_with(".xht") || url.path.ends_with(".xhtml"), |s| s.xml);
        let mut dom = if xml { parse_xhtml(html) } else { parse_html(html) };
        let arrivals = site.as_ref().map_or(0, |s| s.arrivals());
        let (mut sheet, css_units, css_left_out) = style_sheet(&mut dom, base, site.as_deref(), wait, "");
        let web_faces = take_faces(&mut sheet);
        let viewport_units = css_units || html.contains("vw") || html.contains("vh");
        let mut title = String::new();
        if let Some(t) = (0..dom.nodes.len() as NodeId).find(|&n| dom.nodes[n as usize].tag == atom::TITLE) {
            dom.text_content(t, &mut title);
        }
        let scripts = scripts_of(&dom);
        // Only scripts are owed the load and error events of these.
        let parsed_resources = if scripts.is_empty() { Vec::new() } else { resource_nodes(&dom) };
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
            parsed_resources,
            wait,
            base: base.map(Path::to_path_buf),
            navigation: None,
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
            parsed_resources: Vec::new(),
            wait: false,
            base: None,
            navigation: None,
        }
    }

    /// Where a script asked to go (`location.href = ...` and the like): a web URL, or
    /// the path of a local file, and whether the new page replaces this one in the
    /// history. Navigations elsewhere are reported and dropped.
    pub fn take_navigation(&mut self) -> Option<(PathBuf, bool)> {
        let (href, replace) = self.navigation.take()?;
        let url = Url::parse(&href, None)?;
        let path = match url.scheme.as_str() {
            "http" | "https" => PathBuf::from(href),
            "file" => url.to_path()?,
            _ => {
                eprintln!("[js] navigation to {href} is not followed");
                return None;
            }
        };
        Some((path, replace))
    }

    /// Starts downloading the image `src` names in the background, for one-shot output,
    /// which later waits for it where the page needs it: images that scripts name one
    /// after another then download together instead of one at a time.
    pub fn prefetch_image(&self, src: &str) {
        if let Some(site) = self.site.as_ref().filter(|_| self.wait) {
            site.prefetch_later(vec![src.to_string()]);
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

    /// Runs the first `count` waiting scripts in order; with `last` the load event
    /// follows, after the `load` or `error` events of the page's resources.
    fn run_scripts(&mut self, fonts: &mut Fonts, count: usize, last: bool) {
        let scripts: Vec<Script> = self.pending.drain(..count.min(self.pending.len())).collect();
        let page_url = self.url.href();
        let mut runs = Vec::with_capacity(scripts.len());
        for s in scripts {
            match (s.module, s.src.is_empty()) {
                (true, true) => runs.push(Run::Module { name: "inline script".to_string(), code: Some(s.code), href: page_url.clone() }),
                (true, false) => match Url::parse(&s.src, Some(&self.url)) {
                    Some(url) => runs.push(Run::Module { name: s.src, code: None, href: url.href() }),
                    None => eprintln!("[js] cannot read script {}", s.src),
                },
                (false, true) => runs.push(Run::Classic { node: s.node, name: "inline script".to_string(), code: s.code, url: page_url.clone(), external: false }),
                (false, false) => match self.script_code(&s.src) {
                    Some((code, url)) => runs.push(Run::Classic { node: s.node, name: s.src, code, url, external: true }),
                    None => {
                        eprintln!("[js] cannot read script {}", s.src);
                        runs.push(Run::Failed(s.node));
                    }
                },
            }
        }
        self.with_js(fonts, |js| {
            for run in runs {
                match run {
                    Run::Classic { node, name, code, url, external } => {
                        js.run_script(node, &code, &name, &url);
                        if external {
                            js.fire(&[(node, "load")]);
                        }
                    }
                    Run::Module { name, code, href } => js.run_module(code.as_deref(), &name, &href),
                    Run::Failed(node) => js.fire(&[(node, "error")]),
                }
            }
        });
        if last {
            let outcomes = self.resource_outcomes();
            self.with_js(fonts, |js| {
                js.fire(&outcomes);
                js.load();
            });
        }
    }

    /// The `load` or `error` event each stylesheet `<link>` and `<img>` the parser
    /// made owes the page's scripts, by whether its resource could be had, for
    /// those still in the document with the source they were parsed with. A page
    /// that waits for downloads waits for these, as a browser's load event does;
    /// one that does not leaves out those still on their way.
    fn resource_outcomes(&self) -> Vec<(NodeId, &'static str)> {
        let current = resource_nodes(&self.dom);
        let kept = self.parsed_resources.iter().filter(|r| current.contains(r));
        kept.filter_map(|(n, src)| Some((*n, if self.resource_had(src)? { "load" } else { "error" }))).collect()
    }

    /// Whether the resource at `src` could be had; `None` while it downloads for
    /// a page that does not wait, or when `src` does not resolve.
    fn resource_had(&self, src: &str) -> Option<bool> {
        let url = Url::parse(src, Some(&self.url))?;
        if url.scheme == "data" {
            return Some(true);
        }
        match &self.site {
            Some(site) if self.wait => Some(site.file(src).is_some()),
            Some(site) => site.file_if_ready(src).map(|file| file.is_some()),
            None => Some(url.to_path().is_some_and(|p| p.is_file())),
        }
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

    /// Notes a change to what the style sheet is built from: the text and `media`
    /// of the document's `<style>` elements and the `rel`, `href` and `media` of its links.
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
        let tag = self.dom.nodes[n as usize].tag;
        if (tag == atom::LINK && matches!(name, atom::REL | atom::HREF | atom::MEDIA)) || (tag == atom::STYLE && name == atom::MEDIA) {
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
        // Inline style, a poster that makes a `<video>` show an image, and the size
        // attributes of an `<svg>` are read when styling the element itself.
        let tag = self.dom.nodes[n as usize].tag;
        let own = name == atom::STYLE
            || (name == atom::POSTER && tag == atom::VIDEO)
            || (matches!(name, atom::WIDTH | atom::HEIGHT) && tag == atom::SVG);
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
        let (mut kinds, mut root) = (0, false);
        self.dom.each_including(n, &mut |c| {
            kinds |= self.dom.kind_of(c);
            root |= self.dom.nodes[c as usize].tag == atom::HTML;
        });
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

    /// Notes a change that can restyle any element, such as a custom element definition.
    pub(crate) fn restyle_all(&mut self) {
        self.dirty = true;
        self.restyle_all = true;
    }

    /// Sets the CSS of the sheets the document, or with `root` a shadow root, adopted.
    pub(crate) fn adopt(&mut self, css: String, root: Option<NodeId>) {
        match root {
            Some(root) => drop(self.dom.adopted.insert(root, css)),
            None => self.adopted = css,
        }
        self.sheet_changed();
        self.dirty = true;
    }

    /// A node's border box `[x, y, w, h]` from a layout brought up to date first.
    pub(crate) fn fresh_rect(&mut self, fonts: &mut Fonts, n: NodeId) -> [f32; 4] {
        let (vw, vh) = if self.vw < 0.0 { self.script_size } else { (self.vw, self.vh) };
        self.update(fonts, vw, vh);
        self.rects.get(&n).copied().unwrap_or([0.0; 4])
    }

    /// The border box of a node in the last layout, if it has one.
    pub(crate) fn rect_of(&self, n: NodeId) -> Option<[f32; 4]> {
        self.rects.get(&n).copied()
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

    /// Where the first element matching a selector draws its picture, for elements such
    /// as `<img>` that have no box item of their own.
    pub fn image_rect(&mut self, selector: &str) -> Option<[f32; 4]> {
        let n = *crate::style::query(&mut self.dom, 0, selector, true).first()?;
        self.layout.items.iter().find(|i| i.node == n && i.kind == Kind::Image).map(|i| [i.x, i.y, i.w, i.h])
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
        crate::net::mark("scripts start");
        self.run_pending(fonts, vw, vh);
        crate::net::mark("scripts done");
        let start = Instant::now();
        let mut idle = Duration::ZERO;
        loop {
            self.update(fonts, vw, vh);
            // A page that navigates away is not waited for: its successor is what shows.
            if self.navigation.is_some() {
                break;
            }
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
        crate::net::mark("settled");
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
        Ok(Page::parse(&crate::encoding::decode_document(&bytes, None, true), Url::from_path(path), path.parent(), true, None, true))
    }

    /// The page behind an `http://` or `https://` location, or `None` for a file path.
    fn from_web(path: &Path, defer: bool) -> Option<std::io::Result<Page>> {
        let location = path.to_str().filter(|l| crate::net::is_url(l))?;
        Some(Page::load_url(location, defer).map_err(std::io::Error::other))
    }

    /// Downloads a page and starts downloading the stylesheets, scripts and images
    /// it names, then parses it while they arrive: each is waited for where the page
    /// needs it, a stylesheet when styling, a script when it runs, an image when it
    /// is laid out. With `defer` scripts are fetched in the background.
    pub fn load_url(location: &str, defer: bool) -> Result<Page, String> {
        let (site, html) = Site::open(location)?;
        crate::net::mark("html done");
        let (mut srcs, rest) = resource_srcs(&html, !defer);
        srcs.extend(rest);
        site.prefetch_later(srcs);
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
        Ok(Page::parse(&crate::encoding::decode_document(&bytes, None, true), Url::from_path(path), path.parent(), false, None, true))
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
        let resized = self.vw >= 0.0 && (vw, vh) != (self.vw, self.vh);
        self.refresh(fonts, vw, vh);
        // Scripts hear of a new size once it is laid out; whatever they change is laid out too.
        if resized && self.scripted && self.with_js(fonts, Js::resized).is_some() {
            self.refresh(fonts, vw, vh);
        }
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
    fn a_web_page_is_parsed_while_its_images_download() {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc, Mutex};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        let (release, held) = mpsc::channel::<()>();
        let held = Arc::new(Mutex::new(held));
        let answered = Arc::new(AtomicBool::new(false));
        let flag = answered.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let (held, flag) = (held.clone(), flag.clone());
                std::thread::spawn(move || {
                    let mut conn = BufReader::new(stream.unwrap());
                    loop {
                        let mut request = String::new();
                        while conn.read_line(&mut request).unwrap_or(0) > 0 && !request.ends_with("\r\n\r\n") {}
                        let Some(path) = request.split_whitespace().nth(1).map(str::to_string) else { return };
                        let body = match path.as_str() {
                            "/" => "<link rel=stylesheet href=s.css><p>text</p><img src=slow.png>",
                            "/s.css" => "p{color:red}",
                            _ => {
                                held.lock().unwrap().recv_timeout(std::time::Duration::from_secs(5)).ok();
                                flag.store(true, Ordering::SeqCst);
                                "png"
                            }
                        };
                        let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                        conn.get_mut().write_all(reply.as_bytes()).unwrap();
                    }
                });
            }
        });
        let page = Page::load_url(&origin, false).unwrap();
        assert!(!answered.load(Ordering::SeqCst), "the page waited for its image before parsing");
        assert!(!page.sheet.rules.is_empty());
        release.send(()).unwrap();
    }

    #[test]
    fn scripts_see_http_failures_as_browsers_report_them() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                std::thread::spawn(move || {
                    let mut conn = BufReader::new(stream.unwrap());
                    loop {
                        let mut request = String::new();
                        while conn.read_line(&mut request).unwrap_or(0) > 0 && !request.ends_with("\r\n\r\n") {}
                        let Some(path) = request.split_whitespace().nth(1).map(str::to_string) else { return };
                        let (status, body) = match path.as_str() {
                            "/" => ("200 OK", concat!(
                                "<script>function mark(t){var d=document.createElement('p');d.setAttribute('data-m',t);document.documentElement.appendChild(d);}</script>",
                                "<script src=s404.js onload=\"mark('s404 load')\" onerror=\"mark('s404 error')\"></script>",
                                "<link rel=stylesheet href=c500.css onload=\"mark('c500 load')\" onerror=\"mark('c500 error')\">",
                                "<img src=i404.png onload=\"mark('i404 load')\" onerror=\"mark('i404 error')\">",
                                "<script>fetch('f404').then(r=>r.text().then(t=>mark('fetch '+r.ok+' '+r.status+' '+r.statusText+' '+t)));",
                                "var x=new XMLHttpRequest();x.open('GET','x404');x.onload=()=>mark('xhr '+x.status+' '+x.statusText+' '+x.responseText);x.send();</script>",
                            )),
                            "/s404.js" => ("404 Not Found", "mark('s404 ran')"),
                            "/c500.css" => ("500 Internal Server Error", "p{color:red}"),
                            _ => ("404 Not Found", "missing"),
                        };
                        let reply = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                        conn.get_mut().write_all(reply.as_bytes()).unwrap();
                    }
                });
            }
        });
        let mut page = Page::load_url(&origin, false).unwrap();
        page.settle(&mut Fonts::new(), 800.0, 600.0, std::time::Duration::from_millis(100));
        let mut marks: Vec<String> = query(&mut page.dom, 0, "[data-m]", false).into_iter().filter_map(|n| page.dom.attr(n, page.dom.atoms.get("data-m")?).map(str::to_string)).collect();
        marks.sort();
        let expected = ["c500 error", "fetch false 404 Not Found missing", "i404 error", "s404 error", "xhr 404 Not Found missing"];
        assert_eq!(marks, expected);
    }

    #[test]
    fn the_imports_of_a_module_download_together() {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::{Arc, Condvar, Mutex};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        // Imports asked for and not answered yet, and the most of them at once.
        let state = Arc::new((Mutex::new((0usize, 0usize)), Condvar::new()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let state = shared.clone();
                std::thread::spawn(move || {
                    let mut conn = BufReader::new(stream.unwrap());
                    loop {
                        let mut request = String::new();
                        while conn.read_line(&mut request).unwrap_or(0) > 0 && !request.ends_with("\r\n\r\n") {}
                        let Some(path) = request.split_whitespace().nth(1).map(str::to_string) else { return };
                        let body = match path.as_str() {
                            "/" => "<script type=module src=a.js></script>",
                            "/a.js" => "import {b} from './b.js';\nimport'./c.js';\ndocument.documentElement.setAttribute('data-b', b);",
                            _ => {
                                let (lock, changed) = &*state;
                                let mut s = lock.lock().unwrap();
                                s.0 += 1;
                                s.1 = s.1.max(s.0);
                                changed.notify_all();
                                // Held until both imports were asked for, or for a while.
                                let mut s = changed.wait_timeout_while(s, std::time::Duration::from_millis(1500), |s| s.1 < 2).unwrap().0;
                                s.0 -= 1;
                                if path == "/b.js" { "export const b = 'from b';" } else { "" }
                            }
                        };
                        let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                        conn.get_mut().write_all(reply.as_bytes()).unwrap();
                    }
                });
            }
        });
        let mut page = Page::load_url(&origin, false).unwrap();
        page.run_pending(&mut Fonts::new(), 800.0, 600.0);
        assert_eq!(query(&mut page.dom, 0, "[data-b='from b']", false).len(), 1);
        assert_eq!(state.0.lock().unwrap().1, 2);
    }

    #[test]
    fn modules_a_module_imports_with_import_calls_download_together() {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::{Arc, Condvar, Mutex};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        // Modules asked for and not answered yet, and the most of them at once.
        let state = Arc::new((Mutex::new((0usize, 0usize)), Condvar::new()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let state = shared.clone();
                std::thread::spawn(move || {
                    let mut conn = BufReader::new(stream.unwrap());
                    loop {
                        let mut request = String::new();
                        while conn.read_line(&mut request).unwrap_or(0) > 0 && !request.ends_with("\r\n\r\n") {}
                        let Some(path) = request.split_whitespace().nth(1).map(str::to_string) else { return };
                        let body = match path.as_str() {
                            "/" => "<script type=module src=a.js></script>".to_string(),
                            // Each import() in a turn of its own, as pages load parts on demand.
                            "/a.js" => "const root = document.documentElement;\n\
                                setTimeout(() => import('./b.js').then((m) => root.setAttribute('data-b', m.v)), 0);\n\
                                setTimeout(() => import('./c.js').then((m) => root.setAttribute('data-c', m.v)), 0);"
                                .to_string(),
                            _ => {
                                let (lock, changed) = &*state;
                                let mut s = lock.lock().unwrap();
                                s.0 += 1;
                                s.1 = s.1.max(s.0);
                                changed.notify_all();
                                // Held until both modules were asked for, or for a while.
                                let mut s = changed.wait_timeout_while(s, std::time::Duration::from_millis(1500), |s| s.1 < 2).unwrap().0;
                                s.0 -= 1;
                                format!("export const v = '{}';", &path[1..2])
                            }
                        };
                        let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                        conn.get_mut().write_all(reply.as_bytes()).unwrap();
                    }
                });
            }
        });
        let mut page = Page::load_url(&origin, false).unwrap();
        page.settle(&mut Fonts::new(), 800.0, 600.0, std::time::Duration::from_millis(100));
        assert_eq!(query(&mut page.dom, 0, "[data-b=b][data-c=c]", false).len(), 1);
        assert_eq!(state.0.lock().unwrap().1, 2);
    }

    #[test]
    fn images_a_script_names_download_together() {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::{Arc, Condvar, Mutex};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
        // Images asked for and not answered yet, and the most of them at once.
        let state = Arc::new((Mutex::new((0usize, 0usize)), Condvar::new()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let state = shared.clone();
                std::thread::spawn(move || {
                    let mut conn = BufReader::new(stream.unwrap());
                    loop {
                        let mut request = String::new();
                        while conn.read_line(&mut request).unwrap_or(0) > 0 && !request.ends_with("\r\n\r\n") {}
                        let Some(path) = request.split_whitespace().nth(1).map(str::to_string) else { return };
                        let body = match path.as_str() {
                            "/" => "<body><script>for (const n of ['a', 'b']) { const i = document.createElement('img'); i.onload = () => document.body.setAttribute('data-' + n, 'load'); i.src = n + '.svg'; document.body.append(i); }</script>",
                            _ => {
                                let (lock, changed) = &*state;
                                let mut s = lock.lock().unwrap();
                                s.0 += 1;
                                s.1 = s.1.max(s.0);
                                changed.notify_all();
                                // Held until both images were asked for, or for a while.
                                let mut s = changed.wait_timeout_while(s, std::time::Duration::from_millis(1500), |s| s.1 < 2).unwrap().0;
                                s.0 -= 1;
                                "<svg xmlns='http://www.w3.org/2000/svg' width='4' height='4'/>"
                            }
                        };
                        let kind = if path.ends_with(".svg") { "image/svg+xml" } else { "text/html" };
                        let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                        conn.get_mut().write_all(reply.as_bytes()).unwrap();
                    }
                });
            }
        });
        let mut page = Page::load_url(&origin, false).unwrap();
        page.settle(&mut Fonts::new(), 800.0, 600.0, std::time::Duration::from_millis(100));
        assert_eq!(query(&mut page.dom, 0, "body[data-a=load][data-b=load]", false).len(), 1);
        assert_eq!(state.0.lock().unwrap().1, 2);
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
            ("moved into svg", ".x{color:#f00}", "<svg id=s></svg><p class=x id=p></p>", "$('s').appendChild($('p'));", "p", &["ff0000"]),
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
    fn inline_svg_takes_its_size_from_attributes_viewbox_and_css_as_in_webkit() {
        // Each <svg> in a 400px wide block, with the size WebKit gives it.
        let cases = [
            ("<svg id=a viewBox='0 0 24 24'></svg>", (400.0, 400.0)),
            ("<svg id=a></svg>", (300.0, 150.0)),
            ("<svg id=a width=7></svg>", (100.0, 150.0)),
            ("<svg id=a width=24 viewBox='0 0 48 24'></svg>", (24.0, 12.0)),
            ("<svg id=a width='50%' height=20></svg>", (200.0, 20.0)),
            ("<svg id=a class=h viewBox='0 0 100 20'></svg>", (95.0, 19.0)),
            ("<svg id=a width=30 height=30 viewBox='0 0 10 10' style='width:auto;height:auto'></svg>", (30.0, 30.0)),
            ("<svg id=a width=16 height=16 viewBox='0 0 24 24' style='height:10px'></svg>", (16.0, 10.0)),
            ("<span style='display:inline-block'><svg id=a viewBox='0 0 24 12'></svg></span>", (0.0, 0.0)),
            ("<div style='display:flex'><svg id=a viewBox='0 0 2 1'></svg></div>", (400.0, 200.0)),
            ("<div style='display:flex;height:19px'><svg id=a viewBox='0 6 94 94'></svg></div>", (19.0, 19.0)),
            ("<div style='height:19px'><svg id=a viewBox='116 0 408 84' style='height:100%;width:auto'></svg></div>", (92.28571, 19.0)),
        ];
        for (svg, want) in cases {
            // The width set by script is read as a presentation attribute, restyling the element.
            let script = if svg.contains("width=7") { "$('a').setAttribute('width','100');" } else { "" };
            let (mut page, mut fonts) = after_script(".h{height:19px}", &format!("<div style='width:400px'>{svg}</div>"), script);
            let n = query(&mut page.dom, 0, "#a", true)[0];
            let r = page.fresh_rect(&mut fonts, n);
            assert_eq!((r[2], r[3]), want, "{svg}");
        }
    }

    #[test]
    fn object_fit_and_position_place_the_picture_in_its_box_as_in_webkit() {
        let dir = std::env::temp_dir().join(format!("render-fit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::new(40, 20).save(dir.join("a.png")).unwrap();
        // Drawn rect and, inside it, where the whole 40x20 picture lies, in a 20x20 box at (0, 0).
        let cases = [
            ("", [0.0, 0.0, 20.0, 20.0], None),
            ("object-fit:contain", [0.0, 5.0, 20.0, 10.0], Some([0.0, 0.0, 20.0, 10.0])),
            ("object-fit:cover", [0.0, 0.0, 20.0, 20.0], Some([-10.0, 0.0, 40.0, 20.0])),
            ("object-fit:cover;object-position:left top", [0.0, 0.0, 20.0, 20.0], Some([0.0, 0.0, 40.0, 20.0])),
            ("object-fit:none;object-position:-5px 100%", [0.0, 0.0, 20.0, 20.0], Some([-5.0, 0.0, 40.0, 20.0])),
            ("object-fit:scale-down;object-position:right bottom", [0.0, 10.0, 20.0, 10.0], Some([0.0, 0.0, 20.0, 10.0])),
        ];
        for (css, rect, tile) in cases {
            let html = format!("<body style='margin:0'><img src=a.png style='display:block;width:20px;height:20px;{css}'>");
            let mut page = Page::from_html(&html, Some(&dir));
            page.update(&mut Fonts::new(), 1024.0, 768.0);
            let it = page.layout.items.iter().find(|i| i.kind == crate::layout::Kind::Image).unwrap();
            assert_eq!(([it.x, it.y, it.w, it.h], page.layout.pictures[it.data as usize].tile), (rect, tile), "{css}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_line_breaks_after_an_atomic_inline_without_a_space() {
        // WebKit puts the text on a second line, after a box that fills the first.
        for atomic in ["<span style='display:inline-block;width:95px;height:10px'></span>", "<svg width=95 height=10></svg>"] {
            let html = format!("<body style='margin:0;font:16px/20px sans-serif'><div style='width:100px'>{atomic}<b>bbb</b></div>");
            let mut page = Page::from_html(&html, None);
            page.update(&mut Fonts::new(), 1024.0, 768.0);
            let text = page.layout.items.iter().find(|i| i.kind == crate::layout::Kind::Text).unwrap();
            assert!(text.x < 1.0 && text.y >= 20.0, "{atomic}: text at {}, {}", text.x, text.y);
        }
    }

    #[test]
    fn a_mask_image_draws_the_background_colour_where_the_mask_is_opaque() {
        use crate::layout::Kind;
        let dir = std::env::temp_dir().join(format!("render-mask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("i.svg"), "<svg xmlns='http://www.w3.org/2000/svg' width='20' height='10'><rect width='20' height='10'/></svg>").unwrap();
        let html = "<body style='margin:0'><div id=a style='width:40px;height:40px;background:#c00;-webkit-mask:url(i.svg) no-repeat center/20px'></div>\
                    <div id=b style='width:40px;height:40px;background:#000;mask-image:url(none.svg)'></div>";
        let mut page = Page::from_html(html, Some(&dir));
        page.update(&mut Fonts::new(), 1024.0, 768.0);
        let (a, b) = (page.dom.by_id(0, "a"), page.dom.by_id(0, "b"));
        let l = &page.layout;
        assert!(!l.items.iter().any(|i| i.kind == Kind::Box && (i.node == a || i.node == b)), "a masked box paints no background of its own");
        let it = l.items.iter().find(|i| i.kind == Kind::Image).unwrap();
        let p = l.pictures[it.data as usize];
        assert_eq!((it.node, [it.x, it.y, it.w, it.h], p.tile, p.tint.map(|c| c.0)), (a, [10.0, 15.0, 20.0, 10.0], Some([0.0, 0.0, 20.0, 10.0]), Some(0xFFCC_0000)));
        assert_eq!(l.items.iter().filter(|i| i.kind == Kind::Image).count(), 1, "a mask that does not load hides the box");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn svg_use_draws_symbols_from_the_page_and_from_sprite_files() {
        let dir = std::env::temp_dir().join(format!("render-use-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s.svg"), "<svg xmlns='http://www.w3.org/2000/svg'><symbol id='sq' viewBox='0 0 2 2'><rect width='2' height='2' fill='#00f'/></symbol></svg>").unwrap();
        // The page's sprite is not shown: its symbol still draws, its gradient does not (as in WebKit).
        let html = "<body style='margin:0'><svg style='display:none'><symbol id='dot' viewBox='0 0 2 2'><rect width='2' height='2' fill='currentColor'/></symbol>\
                    <linearGradient id='g'><stop stop-color='#0f0'/></linearGradient></svg>\
                    <svg width=10 height=10 style='color:#f00;display:block'><use href='#dot'/></svg>\
                    <svg width=10 height=10 style='display:block'><use xlink:href='s.svg#sq'></use></svg>\
                    <svg width=10 height=10 style='display:block'><rect width='10' height='10' fill='url(#g)'/></svg>";
        let mut page = Page::from_html(html, Some(&dir));
        page.update(&mut Fonts::new(), 1024.0, 768.0);
        let ids: Vec<u32> = page.layout.items.iter().filter(|i| i.kind == crate::layout::Kind::Image).map(|i| page.layout.pictures[i.data as usize].id).collect();
        let centre: Vec<u32> = ids.iter().map(|&id| page.images.get(id, 10, 10, [0, 0, 10, 10]).map_or(0, |(p, _, _)| p.px[55])).collect();
        assert_eq!(centre, [0xFFFF_0000, 0xFF00_00FF, 0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn author_rules_paint_the_elements_inside_an_inline_svg() {
        let css = ".a path{fill:#c00} .b rect{fill:none;stroke:#00f;stroke-width:10} .c{fill:currentColor;color:#0c0}";
        let html = format!("<style>{css}</style><body style='margin:0'>\
                    <svg class=a width=10 height=10 style='display:block'><path d='M0 0h10v10H0z' fill='#000'/></svg>\
                    <svg class=b width=10 height=10 style='display:block'><rect width='10' height='10'/></svg>\
                    <svg class=c width=10 height=10 style='display:block'><rect width='10' height='10'/></svg>");
        let mut page = Page::from_html(&html, None);
        page.update(&mut Fonts::new(), 1024.0, 768.0);
        let ids: Vec<u32> = page.layout.items.iter().filter(|i| i.kind == crate::layout::Kind::Image).map(|i| page.layout.pictures[i.data as usize].id).collect();
        let corner: Vec<u32> = ids.iter().map(|&id| page.images.get(id, 10, 10, [0, 0, 10, 10]).map_or(0, |(p, _, _)| p.px[11])).collect();
        assert_eq!(corner, [0xFFCC_0000, 0xFF00_00FF, 0xFF00_CC00]);
    }

    #[test]
    fn srcset_and_picture_sources_pick_an_image_and_its_density_as_webkit_does() {
        let dir = std::env::temp_dir().join(format!("render-srcset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::new(400, 200).save(dir.join("a.png")).unwrap();
        image::RgbaImage::new(40, 20).save(dir.join("b.png")).unwrap();
        // The size a 1x display gives each <img>, in a 1024px wide viewport.
        let cases = [
            ("<img id=i src=b.png srcset='a.png 2x'>", (40.0, 20.0)),
            ("<img id=i src=b.png srcset='b.png 1x, a.png 2x'>", (40.0, 20.0)),
            ("<img id=i srcset='b.png 40w, a.png 400w' sizes='(max-width: 500px) 10px, 200px'>", (200.0, 100.0)),
            ("<img id=i srcset='b.png 40w, a.png 400w' sizes='20px'>", (20.0, 10.0)),
            ("<img id=i srcset='b.png 40w, a.png 400w'>", (1024.0, 512.0)),
            ("<picture><source type=image/jxl srcset=b.png><source media='(max-width: 100px)' srcset=b.png><source srcset='a.png 2x'><img id=i src=b.png></picture>", (200.0, 100.0)),
            // A flex line of definite height stretches the image, whose ratio gives its width.
            ("<div style='display:flex;height:19px'><img id=i src=a.png></div>", (38.0, 19.0)),
        ];
        for (html, want) in cases {
            let mut page = Page::from_html(&format!("<body style='margin:0'>{html}"), Some(&dir));
            page.update(&mut Fonts::new(), 1024.0, 768.0);
            let it = page.layout.items.iter().find(|i| i.kind == crate::layout::Kind::Image);
            assert_eq!(it.map(|i| (i.w, i.h)), Some(want), "{html}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
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
    fn a_new_viewport_size_fires_resize_then_media_query_changes() {
        // The order and values WebKit reports for the same page resized 1024 -> 600 -> 900.
        let script = "const log=[],mq=matchMedia('(max-width: 700px)'),note=(s)=>{log.push(s);$('out').setAttribute('title',log.join(' '))};\
            mq.onchange=(e)=>note('onchange:'+e.matches);\
            mq.addEventListener('change',(e)=>note('change:'+e.matches+':'+(e instanceof MediaQueryListEvent)));\
            mq.addListener((e)=>note('listener:'+e.matches));\
            matchMedia('(min-width: 100px)').addEventListener('change',()=>note('wide'));\
            addEventListener('resize',()=>note('resize:'+innerWidth));";
        let (mut page, mut fonts) = after_script("", "<p id=out></p>", script);
        let log = |page: &Page| {
            let out = page.dom.by_id(0, "out");
            page.dom.attr(out, page.dom.atoms.get("title").unwrap_or(0)).unwrap_or("").to_string()
        };
        page.update(&mut fonts, 1024.0, 768.0);
        assert_eq!(log(&page), "", "the same size is no resize");
        page.update(&mut fonts, 600.0, 768.0);
        assert_eq!(log(&page), "resize:600 onchange:true change:true:true listener:true");
        page.update(&mut fonts, 900.0, 768.0);
        assert!(log(&page).ends_with("listener:true resize:900 onchange:false change:false:true listener:false"), "{}", log(&page));
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
    fn form_controls_take_their_size_from_size_cols_and_rows_and_sit_on_their_text_baseline() {
        let css = "body{margin:0} input,textarea,select{font:10px/20px monospace;padding:0;border:0;vertical-align:baseline}\
                   #f{display:flex;width:300px} #g{flex:1} #l{font:10px/40px monospace}";
        let body = "<input id=a><input id=b size=5><textarea id=t cols=4 rows=3>x</textarea>\
                    <select id=s><option>ab</option><option selected>abcdef</option></select>\
                    <div id=f><input id=g></div><input id=p style='width:50%'><div id=l>x<input id=q></div>";
        let (mut page, mut fonts) = after_script(css, body, "$('a').title='x';");
        let face = fonts.face(Some("monospace"), crate::css::Family::Mono, 400, false);
        let zero = fonts.lookup(face, '0').2 * 10.0;
        let six = fonts.measure(face, "abcdef", 10.0, 0.0);
        let mut rect = |sel: &str| page.describe(sel).and_then(|d| d.0).unwrap();
        assert_eq!(rect("#a")[2..], [(zero * 20.0).ceil(), 20.0]);
        assert_eq!(rect("#b")[2..], [(zero * 5.0).ceil(), 20.0]);
        assert_eq!(rect("#t")[2..], [(zero * 4.0).ceil(), 60.0]);
        assert_eq!(rect("#s")[2..], [six, 20.0]);
        assert_eq!(rect("#g")[2], 300.0);
        assert_eq!(rect("#p")[2], 512.0);
        let (l, q) = (rect("#l"), rect("#q"));
        assert_eq!(q[1] - l[1], 10.0, "the field's text is on the line's baseline");
    }

    #[test]
    fn checkboxes_sliders_and_text_areas_sit_on_the_baseline_where_webkit_puts_them() {
        let css = "body{margin:0} div{font:20px/60px sans-serif} .m{display:inline-block;width:1px;height:0}";
        let mark = "x<span class=m></span>";
        let body = format!("<div id=a>{mark}<input type=checkbox id=c></div><div id=b>{mark}<input type=range id=r></div><div id=d>{mark}<textarea id=t></textarea></div>");
        let (mut page, _) = after_script(css, &body, "$('c').title='x';");
        let mut rect = |sel: &str| page.describe(sel).and_then(|d| d.0).unwrap();
        let baseline = |m: [f32; 4]| m[1] + m[3];
        for (div, control, above) in [("#a", "#c", 10.0), ("#b", "#r", 16.0)] {
            let (m, c) = (rect(&format!("{div} .m")), rect(control));
            assert_eq!((c[2], c[3]), if control == "#c" { (12.0, 12.0) } else { (129.0, 16.0) });
            assert_eq!(baseline(m) - c[1], above, "{control}");
        }
        let (m, t) = (rect("#d .m"), rect("#t"));
        assert_eq!(baseline(m), t[1] + t[3]);
    }

    #[test]
    fn block_level_buttons_and_fields_are_as_wide_as_their_content() {
        let css = "body{margin:0;width:400px} button,input{display:block;font:10px/20px monospace;padding:0 5px;border:0}\
                   #f{display:flex} #c{margin:0 auto} #w{width:100%}";
        let body = "<button id=b>abcd</button><button id=f><i>ab</i><i>cd</i></button><button id=c>abcd</button><input id=i size=2><button id=w>x</button>";
        let (mut page, mut fonts) = after_script(css, body, "$('b').title='x';");
        let face = fonts.face(Some("monospace"), crate::css::Family::Mono, 400, false);
        let four = fonts.measure(face, "abcd", 10.0, 0.0) + 10.0;
        let zero = fonts.lookup(face, '0').2 * 10.0;
        let mut rect = |sel: &str| page.describe(sel).and_then(|d| d.0).unwrap();
        assert_eq!(rect("#b")[2], four);
        assert_eq!(rect("#f")[2], four);
        assert_eq!(rect("#c")[..3], [(400.0 - four) / 2.0, 40.0, four]);
        assert_eq!(rect("#i")[2], (zero * 2.0).ceil() + 10.0);
        assert_eq!(rect("#w")[2], 400.0);
    }

    #[test]
    fn a_shrinking_flex_line_keeps_lists_and_text_areas_at_their_content_width() {
        let css = "body{margin:0} div{display:flex;width:50px} select,textarea{font:10px/20px monospace;padding:0;border:0} i{display:block;width:100px}";
        let body = "<div><select id=s><option>ab cd ef</option></select><i></i></div><div><textarea id=t cols=6>ab cd</textarea><i></i></div>";
        let (mut page, mut fonts) = after_script(css, body, "$('s').title='x';");
        let face = fonts.face(Some("monospace"), crate::css::Family::Mono, 400, false);
        let (list, six) = (fonts.measure(face, "ab cd ef", 10.0, 0.0), (fonts.lookup(face, '0').2 * 60.0).ceil());
        let mut width = |sel: &str| page.describe(sel).and_then(|d| d.0).unwrap()[2];
        assert_eq!(width("#s"), list, "a list is not as narrow as its widest word");
        assert_eq!(width("#t"), six);
    }

    #[test]
    fn a_legend_sits_on_its_fieldset_top_border_above_the_content() {
        let css = "body{margin:0} fieldset{margin:0;padding:5px 10px;border:2px solid red} legend{padding:0;height:20px;width:30px} #d{height:10px}";
        let body = "<fieldset id=f><legend id=l></legend><div id=d></div></fieldset>";
        let (mut page, _) = after_script(css, body, "$('f').title='x';");
        let mut rect = |sel: &str| page.describe(sel).and_then(|d| d.0).unwrap();
        assert_eq!(rect("#f"), [0.0, 0.0, 1024.0, 20.0 + 5.0 + 10.0 + 5.0 + 2.0]);
        assert_eq!(rect("#l")[..2], [12.0, 0.0]);
        assert_eq!(rect("#d")[1], 25.0);
    }

    #[test]
    fn a_button_taller_than_its_content_centres_it() {
        let css = "body{margin:0} button{padding:0;border:0;height:60px;width:50px} #f{display:flex}";
        let body = "<button id=b><div id=d style='height:20px'></div></button><button id=f><div id=e style='height:20px;width:5px'></div></button>";
        let (mut page, _) = after_script(css, body, "$('b').title='x';");
        let mut rect = |sel: &str| page.describe(sel).and_then(|d| d.0).unwrap();
        assert_eq!(rect("#d")[1] - rect("#b")[1], 20.0);
        assert_eq!(rect("#e")[1] - rect("#f")[1], 0.0, "a flex button places its items itself");
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
    /// Border boxes of the elements with these ids, rounded to tenths.
    fn rects(page: &mut Page, ids: &[&str]) -> Vec<[f32; 4]> {
        ids.iter().map(|id| page.describe(&format!("#{id}")).and_then(|d| d.0).map_or([-1.0; 4], |r| r.map(|v| (v * 10.0).round() / 10.0))).collect()
    }

    #[test]
    fn a_flex_item_shrinks_past_an_image_with_a_percentage_max_width() {
        let css = "body{margin:0} .row{display:flex;width:300px} img{display:block;max-width:100%;height:auto}";
        let body = "<div class=row><div id=item><img id=i width=600 height=300></div></div>";
        let (mut page, _) = after_script(css, body, "$('i').title='x';");
        assert_eq!(rects(&mut page, &["item", "i"]), [[0.0, 0.0, 300.0, 150.0], [0.0, 0.0, 300.0, 150.0]]);
    }

    #[test]
    fn an_inline_block_svg_without_a_width_fits_the_room_it_has() {
        let css = "body{margin:0} .box{width:24px;height:24px} svg{display:inline-block}";
        let body = "<div class=box><svg id=s viewBox='0 0 960 960'></svg></div>";
        let (mut page, _) = after_script(css, body, "$('s').id='s';");
        assert_eq!(rects(&mut page, &["s"])[0][2], 24.0);
    }

    #[test]
    fn margins_collapse_through_empty_blocks() {
        let css = "body{margin:0} .a{height:10px} .e{margin:20px 0}";
        let body = "<div class=a id=a></div><div class=e></div><div class=e><div class=e></div></div><div class=a id=b></div>\
                    <div id=p><div class=e></div><div class=a id=c></div></div>\
                    <div id=q><div class=a></div><div style=margin-top:100px></div></div><div class=a id=d></div>";
        let (mut page, _) = after_script(css, body, "$('a').title='x';");
        let got = rects(&mut page, &["b", "p", "c", "q", "d"]);
        let w = 1024.0;
        assert_eq!(got, [[0.0, 30.0, w, 10.0], [0.0, 60.0, w, 10.0], [0.0, 60.0, w, 10.0], [0.0, 70.0, w, 10.0], [0.0, 180.0, w, 10.0]]);
    }

    #[test]
    fn a_last_child_margin_collapses_through_a_parent_with_min_height() {
        let css = "body{margin:0} .a{height:10px}";
        let body = "<div id=r style=min-height:5px><div class=a style=margin-bottom:50px></div></div><div class=a id=s></div>";
        let (mut page, _) = after_script(css, body, "$('s').title='x';");
        assert_eq!(rects(&mut page, &["r", "s"]), [[0.0, 0.0, 1024.0, 10.0], [0.0, 60.0, 1024.0, 10.0]]);
    }

    #[test]
    fn flex_items_shrink_and_grow_no_further_than_their_min_content() {
        let css = "body{margin:0} .f{display:flex;width:100px} b{display:block;height:10px} .s{width:80px;height:10px} .z{flex:1 1 0;height:10px}";
        let body = "<div class=f><div id=x1><b style=width:80px></b></div><div class=s id=x2></div></div>\
                    <div class=f><div class=z id=y1><b style=width:70px></b></div><div class=z id=y2></div></div>\
                    <div class=f><div id=z1 style=overflow:hidden><b style=width:80px></b></div><div class=s id=z2></div></div>\
                    <div class=f style=align-items:start><img id=g width=100 height=10><div class=s id=g2></div></div>";
        let (mut page, _) = after_script(css, body, "$('x1').title='x';");
        let got = rects(&mut page, &["x1", "x2", "y1", "y2", "z1", "z2", "g", "g2"]);
        assert_eq!(got, [[0.0, 0.0, 80.0, 10.0], [80.0, 0.0, 20.0, 10.0], [0.0, 10.0, 70.0, 10.0], [70.0, 10.0, 30.0, 10.0], [0.0, 20.0, 50.0, 10.0], [50.0, 20.0, 50.0, 10.0], [0.0, 30.0, 100.0, 10.0], [100.0, 30.0, 0.0, 10.0]]);
    }

    #[test]
    fn vertical_align_places_inline_boxes_on_their_line() {
        let css = "body{margin:0;font:20px/20px sans-serif} i{display:inline-block;width:10px;height:10px} .tall{height:40px}";
        let body = "<div id=l><i class=tall id=i></i><i id=m style=vertical-align:middle></i><i id=t style=vertical-align:top></i><i id=b style=vertical-align:bottom></i></div>\
                    <div><i id=r></i><sup><i id=q></i></sup></div>";
        let (mut page, _) = after_script(css, body, "$('l').title='x';");
        let [l, i, m, t, b, r, q] = rects(&mut page, &["l", "i", "m", "t", "b", "r", "q"]).try_into().unwrap();
        let baseline = i[1] + 40.0;
        assert_eq!(t[1], l[1]);
        assert_eq!(b[1] + 10.0, l[1] + l[3]);
        assert!((m[1] + 5.0 - (baseline - 5.0)).abs() < 0.2, "{m:?} {baseline}");
        assert!((r[1] - q[1] - (20.0 / 3.0 + 1.0)).abs() < 0.2, "{r:?} {q:?}");
    }

    #[test]
    fn positioned_boxes_of_one_z_index_paint_in_tree_order() {
        let css = "body{margin:0} div{position:relative;height:10px} #a{position:absolute;width:10px;background:red} #b{background:blue}";
        let (mut page, _) = after_script(css, "<div id=p><div id=a></div><div id=b></div></div>", "$('a').title='x';");
        let a = query(&mut page.dom, 0, "#a", false)[0];
        let b = query(&mut page.dom, 0, "#b", false)[0];
        let at = |n| page.layout.items.iter().position(|it| it.node == n);
        assert!(at(a).unwrap() < at(b).unwrap(), "{:?} {:?}", at(a), at(b));
    }

    #[test]
    fn auto_margins_center_absolutely_positioned_boxes() {
        let css = "body{margin:0} #c{position:relative;width:100px;height:100px} i{position:absolute;top:0;right:0;bottom:0;left:0}\
                   #m{margin:auto;width:20px;height:30px} #n{margin:auto 0 auto auto;width:20px;height:10px}";
        let (mut page, _) = after_script(css, "<div id=c><i id=m></i><i id=n></i></div>", "$('m').title='x';");
        assert_eq!(rects(&mut page, &["m", "n"]), [[40.0, 35.0, 20.0, 30.0], [80.0, 45.0, 20.0, 10.0]]);
    }

    #[test]
    fn a_transformed_box_contains_absolutely_positioned_boxes() {
        let css = "body{margin:0} #t{transform:translate(5px,0);margin-top:20px;height:10px} i{position:absolute;top:3px;left:0;width:1px;height:1px}";
        let (mut page, _) = after_script(css, "<div id=t><i id=i></i></div>", "$('t').title='x';");
        assert_eq!(rects(&mut page, &["i"]), [[5.0, 23.0, 1.0, 1.0]]);
    }

    #[test]
    fn margins_of_a_formatting_context_may_overlap_the_floats_beside_it() {
        let css = "body{margin:0} .c{width:100px;display:flow-root} .fl{float:left;width:50px;height:10px} .n{overflow:hidden;height:10px}";
        let body = "<div class=c><div class=fl></div><div class=n id=n style=margin-right:1px;width:50px></div></div>\
                    <div class=c><div class=fl></div><div class=n id=o style=margin-left:10px></div></div>\
                    <div class=c><div style=float:right;width:50px;height:10px></div><div class=n id=p style=width:20px;margin-left:40px></div></div>";
        let (mut page, _) = after_script(css, body, "$('n').title='x';");
        assert_eq!(rects(&mut page, &["n", "o", "p"]), [[50.0, 0.0, 50.0, 10.0], [50.0, 10.0, 50.0, 10.0], [40.0, 30.0, 20.0, 10.0]]);
    }

    #[test]
    fn flex_items_aligned_by_baseline_share_their_first_baseline() {
        let css = "body{margin:0;font:10px/10px sans-serif} .f{display:flex;align-items:baseline} i{display:block;width:5px}";
        let body = "<div class=f><i id=p style=height:30px></i><i id=q style=height:10px></i><i id=r style=height:10px;margin-top:5px></i></div>\
                    <div class=f><div id=t1>a<br>b</div><div id=t2>c</div></div>";
        let (mut page, _) = after_script(css, body, "$('p').title='x';");
        let got = rects(&mut page, &["p", "q", "r"]);
        assert_eq!(got, [[0.0, 0.0, 5.0, 30.0], [5.0, 20.0, 5.0, 10.0], [10.0, 20.0, 5.0, 10.0]]);
        let [t1, t2] = rects(&mut page, &["t1", "t2"]).try_into().unwrap();
        assert_eq!((t1[1], t2[1]), (30.0, 30.0));
    }

    #[test]
    fn intrinsic_width_keywords_size_blocks_by_their_content() {
        let css = "body{margin:0} b{display:block;height:5px}";
        let body = "<div id=a style=width:max-content><b style=width:40px></b></div>\
                    <div id=c style='width:fit-content;margin:0 auto'><b style=width:30px></b></div>\
                    <div id=d style=width:min-content><b style=width:20px></b><b style=width:25px></b></div>";
        let (mut page, _) = after_script(css, body, "$('a').title='x';");
        assert_eq!(rects(&mut page, &["a", "c", "d"]), [[0.0, 0.0, 40.0, 5.0], [497.0, 5.0, 30.0, 5.0], [0.0, 10.0, 25.0, 10.0]]);
    }

    #[test]
    fn the_initial_containing_block_is_the_size_of_the_viewport() {
        let css = "body{margin:0} #tall{height:2000px} #b{position:absolute;bottom:0;width:10px;height:10px} #p{position:absolute;top:0;height:50%;width:10px}";
        let (mut page, _) = after_script(css, "<div id=tall></div><i id=b></i><i id=p></i>", "$('b').title='x';");
        assert_eq!(rects(&mut page, &["b", "p"]), [[0.0, 758.0, 10.0, 10.0], [0.0, 0.0, 10.0, 384.0]]);
    }

    #[test]
    fn inline_flex_boxes_sit_on_their_first_baseline() {
        let css = "body{margin:0;font:10px/10px sans-serif} b{display:block}";
        let body = "<div><span id=f style='display:inline-flex;flex-direction:column;overflow:hidden'><b>a</b><b>b</b></span>\
                    <span id=k style=display:inline-block><b>a</b><b>b</b></span></div>";
        let (mut page, _) = after_script(css, body, "$('f').title='x';");
        let [f, k] = rects(&mut page, &["f", "k"]).try_into().unwrap();
        assert_eq!(f[1] - k[1], 10.0, "{f:?} {k:?}");
    }

    #[test]
    fn shrink_to_fit_boxes_are_no_narrower_than_their_content_allows() {
        let css = "body{margin:0} i{display:inline-block;height:5px} .a{width:80px} .b{width:30px} #c{width:50px;height:30px;position:relative}";
        let body = "<div id=c><div id=f style=float:left><i class=a></i> <i class=b></i></div>\
                    <div id=g style=position:absolute;top:10px;left:0><i class=a></i> <i class=b></i></div></div>";
        let (mut page, _) = after_script(css, body, "$('c').title='x';");
        let [f, g] = rects(&mut page, &["f", "g"]).try_into().unwrap();
        assert_eq!((f[2], g[2]), (80.0, 80.0));
    }

    #[test]
    fn aspect_ratio_gives_boxes_their_height_from_their_width() {
        let css = "body{margin:0} b{display:block;height:30px}";
        let body = "<div id=a style='aspect-ratio:2/1;width:100px'></div>\
                    <div id=b style='aspect-ratio:16 / 9;width:160px;padding:10px'></div>\
                    <div id=c style='aspect-ratio:1;width:50px;box-sizing:border-box;padding:5px'></div>\
                    <div id=d style='aspect-ratio:4/1;width:40px'><b></b></div>";
        let (mut page, _) = after_script(css, body, "$('a').title='x';");
        let got = rects(&mut page, &["a", "b", "c", "d"]);
        assert_eq!(got, [[0.0, 0.0, 100.0, 50.0], [0.0, 50.0, 180.0, 110.0], [0.0, 160.0, 50.0, 50.0], [0.0, 210.0, 40.0, 30.0]]);
    }

    #[test]
    fn margin_trim_drops_the_margins_at_a_block_containers_edges() {
        let css = "body{margin:0} b{display:block} .c{margin-trim:block;margin:10px 0} .i{height:20px;margin:30px 0} .e{margin:50px 0}";
        let body = "<div class=c id=c><b class=e></b><b class=i id=i1></b><b class=i id=i2></b><b class=e></b></div><b class=i id=after style=margin-top:0></b>";
        let (mut page, _) = after_script(css, body, "$('c').title='x';");
        let got = rects(&mut page, &["c", "i1", "i2", "after"]);
        let w = 1024.0;
        assert_eq!(got, [[0.0, 10.0, w, 70.0], [0.0, 10.0, w, 20.0], [0.0, 60.0, w, 20.0], [0.0, 90.0, w, 20.0]]);
    }

    #[test]
    fn replaced_flex_items_take_their_width_from_a_definite_height() {
        let css = "body{margin:0} .f{display:flex;width:24px;height:24px}";
        let (mut page, _) = after_script(css, "<div class=f id=f><img id=v width=56 height=56 style=height:100%></div>", "$('f').title='x';");
        assert_eq!(rects(&mut page, &["v"]), [[0.0, 0.0, 24.0, 24.0]]);
    }

    #[test]
    fn floats_shorten_lines_and_formatting_contexts_keep_clear_of_them() {
        let css = "body{margin:0;font:0/0 sans-serif} .c{width:400px} .fl{float:left;width:100px;height:50px} .fr{float:right;width:120px;height:40px}\
                   .m{display:inline-block;width:10px;height:10px}";
        let body = "<div class=c id=c1><div class=fl></div><i class=m id=m1></i></div>\
                    <div class=c id=c2 style=overflow:hidden><div class=fr id=f2></div><i class=m></i></div>\
                    <div class=c style=display:flow-root id=c3><div class=fl></div><div id=b3 style=clear:left;height:5px></div></div>\
                    <div class=c id=c4 style=display:flow-root><div class=fl></div><div id=b4 style=overflow:hidden;height:5px></div></div>\
                    <div class=c style=display:flow-root><span><i class=m></i><i class=fr id=f5 style=width:50px;height:30px></i><i class=m id=m5></i></span></div>";
        let (mut page, _) = after_script(css, body, "$('c1').title='x';");
        let got = rects(&mut page, &["m1", "c2", "f2", "b3", "c3", "b4", "f5", "m5"]);
        assert_eq!(got, [[100.0, 0.0, 10.0, 10.0], [100.0, 10.0, 400.0, 40.0], [380.0, 10.0, 120.0, 40.0], [0.0, 100.0, 400.0, 5.0], [0.0, 50.0, 400.0, 55.0], [100.0, 105.0, 300.0, 5.0], [350.0, 155.0, 50.0, 30.0], [10.0, 155.0, 10.0, 10.0]]);
    }

    #[test]
    fn a_style_sheet_applies_only_while_its_media_attribute_matches() {
        let css = "p{margin:0;height:1px}";
        let body = "<style media=print>#a{width:11px}</style><style media='only screen'>#b{width:22px}</style>\
                    <style media='not print'>#c{width:33px}</style><style media='screen and (max-width: 100px)'>#d{width:44px}</style>\
                    <style media='print, (min-width: 1000px)'>#e{width:55px}</style><style media=print>@media (min-width:1px){#f{width:66px}}</style>\
                    <style media=print id=s>#g{width:77px}</style>\
                    <p id=a></p><p id=b></p><p id=c></p><p id=d></p><p id=e></p><p id=f></p><p id=g></p>";
        let (mut page, _) = after_script(css, body, ";;$('s').setAttribute('media','all');");
        let w: Vec<f32> = rects(&mut page, &["a", "b", "c", "d", "e", "f", "g"]).iter().map(|r| r[2]).collect();
        assert_eq!(w, [1008.0, 22.0, 33.0, 1008.0, 55.0, 1008.0, 77.0], "print sheets apply to no screen; a script switching media to all applies the sheet");
    }

    #[test]
    fn a_style_preload_fires_load_so_a_page_can_switch_it_to_a_stylesheet() {
        let body = "<link class=d rel=preload as=style href='data:text/css,p{}'><link class=d rel=preload as=font href='data:,'><p id=p></p>\
                    <script>var n=0,p=document.getElementById('p');document.querySelectorAll('.d').forEach(function(l){l.addEventListener('load',function(){n+=33;p.style.width=n+'px'})})</script>";
        let (mut page, _) = after_script("body{margin:0}", body, "");
        assert_eq!(rects(&mut page, &["p"])[0][2], 33.0, "the style preload fires load once; the font preload is not handled here");
    }

    #[test]
    fn table_cells_outside_a_table_row_line_up_in_an_anonymous_row() {
        let css = "body{margin:0;font:0/0 sans-serif} ul{margin:0;padding:0;list-style:none} li{display:table-cell} .a{width:50px;height:20px} .b{width:70px;height:30px}";
        let body = "<ul><li id=l1><i style=display:block class=a></i></li>\n<li id=l2><i style=display:block class=b></i></li></ul>\
                    <div><span style=display:table-cell id=m1>x</span><p id=m2 style=margin:0;height:5px></p></div>";
        let (mut page, _) = after_script(css, body, "");
        let got = rects(&mut page, &["l1", "l2", "m2"]);
        assert_eq!(got, [[0.0, 0.0, 50.0, 30.0], [50.0, 0.0, 70.0, 30.0], [0.0, 30.0, 1024.0, 5.0]], "a run of cells sits in one row; a cell beside a block does not");
    }

    #[test]
    fn grid_items_go_on_their_lines_in_tracks_sized_by_their_functions() {
        let css = "body{margin:0} i{height:10px} .g{display:grid;width:400px;grid-template-columns:100px 1fr 2fr;column-gap:10px}\
                   .a{display:grid;width:400px;grid-template-areas:'h h' 's m';grid-template-columns:80px 1fr;grid-template-rows:20px 30px}\
                   .hd{grid-area:h} .mn{grid-area:m} .f{display:grid;width:400px;grid-template-columns:repeat(auto-fill,minmax(90px,1fr));gap:4px}\
                   .s{display:grid;width:400px;grid-template-columns:repeat(4,1fr)} .s2{grid-column:span 2} .c3{grid-column:3 / 5;grid-row:2}";
        let body = "<div class=g><i id=g1></i><i id=g2></i><i id=g3></i></div><div class=a><i class=mn id=mn></i><i class=hd id=hd></i></div>\
                    <div class=f><i id=f1></i><i></i><i></i><i id=f4></i><i id=f5></i></div>\
                    <div class=s><i class=s2 id=s1></i><i id=s2></i><i class=c3 id=s3></i><i id=s4></i><i id=s5></i></div>";
        let (mut page, _) = after_script(css, body, "$('g1').title='x';");
        let got = rects(&mut page, &["g1", "g2", "g3", "hd", "mn", "f1", "f4", "f5", "s1", "s2", "s3", "s4", "s5"]);
        let want = [
            [0.0, 0.0, 100.0, 10.0],
            [110.0, 0.0, 93.3, 10.0],
            [213.3, 0.0, 186.7, 10.0],
            [0.0, 10.0, 400.0, 10.0],
            [80.0, 30.0, 320.0, 10.0],
            [0.0, 60.0, 97.0, 10.0],
            [303.0, 60.0, 97.0, 10.0],
            [0.0, 74.0, 97.0, 10.0],
            [0.0, 84.0, 200.0, 10.0],
            [200.0, 84.0, 100.0, 10.0],
            [200.0, 94.0, 200.0, 10.0],
            [300.0, 84.0, 100.0, 10.0],
            [0.0, 94.0, 100.0, 10.0],
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn css_wide_keywords_reset_grid_tracks_and_flexible_tracks_hold_fixed_width_items() {
        let css = "body{margin:0} i{display:block;height:10px} .t{display:grid;width:400px;grid-template-columns:repeat(4,100px)}\
                   .u{grid-template-columns:unset;grid-auto-flow:column;grid-auto-columns:1fr;column-gap:20px} .u i{width:300px}\
                   .n{grid-template-columns:initial}";
        let body = "<div class='t u'><i id=u1></i><i id=u2></i></div><div class='t n'><i id=n1></i><i id=n2></i></div>";
        let (mut page, _) = after_script(css, body, "$('u1').title='x';");
        let got = rects(&mut page, &["u1", "u2", "n1", "n2"]);
        assert_eq!(got, [[0.0, 0.0, 300.0, 10.0], [320.0, 0.0, 300.0, 10.0], [0.0, 10.0, 400.0, 10.0], [0.0, 20.0, 400.0, 10.0]]);
    }
}
