//! A loaded document: DOM, stylesheet, computed styles and layout.

use crate::css::{FontFace, Sheet};
use crate::dom::{atom, parse_html, Dom, NodeId, NONE};
use crate::font::Fonts;
use crate::img::Images;
use crate::js::{Host, Js};
use crate::FxMap;
use crate::layout::{layout, Kind, Layout};
use crate::style::{compute_styles, Style};
use crate::net::{Site, Wake};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a page shown progressively waits for its linked stylesheets after the
/// HTML has arrived; later ones restyle the page when they come. The preload
/// scanner requests them as soon as their tag arrives, over a connection opened
/// while the HTML was requested, so one usually lands one round trip after the
/// HTML: a few hundred milliseconds even on a 170 ms RTT server. One second is
/// twice that, while keeping a page whose HTML arrives within half a second
/// well inside a two-second first frame.
const CSS_BUDGET: Duration = Duration::from_millis(1000);

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

/// The stylesheet references in a document, then its image and (when asked)
/// script references, so they can be downloaded together before the page is built.
fn resource_srcs(html: &str, scripts: bool) -> (Vec<String>, Vec<String>) {
    let dom = parse_html(html);
    let first = |set: &str| set.split(',').next().and_then(|c| c.split_ascii_whitespace().next()).map(str::to_string);
    let (mut css, mut out) = (Vec::new(), Vec::new());
    for n in 0..dom.nodes.len() as NodeId {
        match dom.nodes[n as usize].tag {
            atom::LINK if dom.attr(n, atom::REL).is_some_and(|r| r.eq_ignore_ascii_case("stylesheet")) => {
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
/// stylesheet in document order. With `wait` a linked one is downloaded first;
/// without, one still downloading is left out. Returns the sheet, whether its CSS
/// uses viewport units, and the stylesheets left out.
fn style_sheet(dom: &mut Dom, base: Option<&Path>, site: Option<&Site>, wait: bool) -> (Sheet, bool, Vec<String>) {
    let mut sheet = Sheet::default();
    sheet.add(UA_CSS, &mut dom.atoms, 0, "");
    let (mut viewport_units, mut left_out) = (false, Vec::new());
    for n in 0..dom.nodes.len() as NodeId {
        match dom.nodes[n as usize].tag {
            atom::STYLE => {
                let mut css = String::new();
                dom.text_content(n, &mut css);
                sheet.add(&css, &mut dom.atoms, 1, "");
            }
            atom::LINK => {
                let is_css = dom.attr(n, atom::REL).is_some_and(|r| r.eq_ignore_ascii_case("stylesheet"));
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
    /// Present once the page has run a script.
    js: Option<Js>,
    rects: FxMap<u32, [f32; 4]>,
    scroll: f32,
    dirty: bool,
    /// Scripts still to run, in document order: `(name, code, module path)`.
    deferred: Vec<Script>,
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

/// Moves a sheet's `@font-face` rules out, to be shared with the font matcher.
fn take_faces(sheet: &mut Sheet) -> Rc<[FontFace]> {
    Rc::from(std::mem::take(&mut sheet.fonts))
}

impl Page {
    /// Parses HTML and every stylesheet it references. `base` is the
    /// directory that relative `<link href>` paths resolve against.
    pub fn from_html(html: &str, base: Option<&Path>) -> Page {
        Page::parse(html, base, false, None, true)
    }

    /// With `defer`, every script is kept for `run_deferred`, so the time to the
    /// first frame does not depend on how much script a page carries. The scripts
    /// still run in document order.
    ///
    /// With `site`, stylesheets, scripts and images are downloaded from the web.
    ///
    /// Without `wait` (for a page from the web), nothing waits for a download: a
    /// stylesheet or image that has not arrived is left out until it does.
    fn parse(html: &str, base: Option<&Path>, defer: bool, site: Option<Arc<Site>>, wait: bool) -> Page {
        let mut dom = parse_html(html);
        let arrivals = site.as_ref().map_or(0, |s| s.arrivals());
        let (mut sheet, css_units, css_left_out) = style_sheet(&mut dom, base, site.as_deref(), wait);
        let web_faces = take_faces(&mut sheet);
        let viewport_units = css_units || html.contains("vw") || html.contains("vh");
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
                            if site.is_some() && defer {
                                // Downloaded when it is about to run. An empty path marks a module.
                                scripts.push((src.to_string(), String::new(), module.then(std::path::PathBuf::new)));
                                continue;
                            }
                            let file = match &site {
                                Some(site) => site.file(src),
                                None => base.map(|b| b.join(src)),
                            };
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
                atom::TITLE if title.is_empty() => dom.text_content(n, &mut title),
                _ => {}
            }
        }
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
                let later = scripts.iter().filter(|s| s.1.is_empty() && s.0 != "inline script").map(|s| s.0.clone());
                site.prefetch_later(later.collect());
            }
            page.images.fetch_from(site, wait);
        }
        if !scripts.is_empty() {
            let js = Js::new(base, page.site.clone());
            // Only the window defers: there a page may roll its dice. One-shot output is repeatable.
            page.js = Some(if defer { js } else { js.repeatable() });
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
        let site = self.site.clone();
        self.with_js(|js| {
            for (name, code, module) in scripts {
                let fetched;
                let (code, module) = match &site {
                    Some(site) if code.is_empty() && name != "inline script" => {
                        let Some(file) = site.file(name) else {
                            eprintln!("[js] cannot read script {name}");
                            continue;
                        };
                        let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap_or_default()).into_owned();
                        fetched = (text, module.as_ref().and_then(|_| std::fs::canonicalize(&file).ok()));
                        (&fetched.0, &fetched.1)
                    }
                    _ => (code, module),
                };
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

    /// Lays the page out and lets script timers run for up to `budget` of page
    /// time, for one-shot output where no event loop keeps the page alive. Nothing
    /// is painted until the end, so the waits between timers are skipped rather
    /// than slept: only the time spent working is real.
    pub fn settle(&mut self, fonts: &mut Fonts, vw: f32, vh: f32, budget: std::time::Duration) {
        let start = std::time::Instant::now();
        let mut skipped = std::time::Duration::ZERO;
        loop {
            self.update(fonts, vw, vh);
            let wait = match self.tick() {
                Some(ms) => std::time::Duration::from_secs_f64(ms.max(1.0) / 1e3),
                None => break,
            };
            if start.elapsed() + skipped + wait > budget {
                break;
            }
            skipped += wait;
            self.with_js(|js| js.skip(wait.as_secs_f64() * 1e3));
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
    pub fn scroll_to(&mut self, y: f32) {
        self.scroll = y;
        self.with_js(|js| js.dispatch(0, "scroll"));
    }

    /// Like `load`, but holds deferred scripts back until `run_deferred`.
    pub fn load_deferred(path: &Path) -> std::io::Result<Page> {
        if let Some(page) = Page::from_web(path, true) {
            return page;
        }
        let bytes = std::fs::read(path)?;
        Ok(Page::parse(&String::from_utf8_lossy(&bytes), path.parent(), true, None, true))
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
        Ok(Page::parse(&html, None, defer, Some(site), true))
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
        Page::parse(&download.html, None, true, Some(download.site), false)
    }

    /// Calls `wake` from a downloading thread whenever a resource of this page arrives.
    pub fn on_arrival(&self, wake: Wake) {
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
            let (mut sheet, units, left_out) = style_sheet(&mut self.dom, None, Some(&site), false);
            css = self.css_left_out.len().saturating_sub(left_out.len());
            self.web_faces = take_faces(&mut sheet);
            self.sheet = sheet;
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

    pub fn load(path: &Path) -> std::io::Result<Page> {
        if let Some(page) = Page::from_web(path, false) {
            return page;
        }
        let bytes = std::fs::read(path)?;
        Ok(Page::from_html(&String::from_utf8_lossy(&bytes), path.parent()))
    }

    fn restyle(&mut self, vw: f32, vh: f32) {
        self.styles = compute_styles(&self.dom, &self.sheet, vw, vh, self.hover);
    }

    fn relayout(&mut self, fonts: &mut Fonts, vw: f32, vh: f32) {
        fonts.set_web_faces(&self.web_faces);
        loop {
            let mut old = std::mem::take(&mut self.layout);
            old.record_boxes = self.record_boxes || self.js.is_some();
            self.layout = layout(&self.dom, &self.styles, fonts, &mut self.images, vw, vh, old);
            // Text measured with a stand-in face is measured again with the web font.
            if !self.fetch_fonts(fonts) {
                break;
            }
        }
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
        if restyle || resized || std::mem::take(&mut self.stale) {
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
