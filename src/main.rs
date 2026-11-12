use render::font::Fonts;
use render::page::{Download, Page};
use render::paint::{paint, scrollbar, Target};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

/// Script execution allocates heavily; mimalloc is faster than the system
/// allocator for that pattern (measured on the apple.com page load).
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Idle page time that one-shot output (--screenshot, --boxes) lets pass between
/// script tasks before the frame is taken; the tasks themselves do not count.
/// Component frameworks such as apple.com's mount their components after about
/// half a second of timers and animation frames.
const SETTLE: Duration = Duration::from_millis(1000);

const USAGE: &str = "usage: render <file.html | http(s) URL> [options]

  (no option)            open the page in a window
  --screenshot <out.bmp> render one frame to a BMP file and exit
  --bench <iterations>   time parse, style, layout and paint, then exit
  --width <px>           viewport width for --screenshot/--bench  (default 1024)
  --height <px>          viewport height for --screenshot/--bench (default 768)
  --scale <factor>       device pixels per CSS pixel              (default 1)
  --scroll <px>          vertical scroll offset for --screenshot  (default 0)
  --stats                print per-frame timings in window mode
  --inspect <selector>   with --screenshot, print the computed box of matching elements
  --boxes <file>         print the box and computed style of each selector in the file

window keys: arrows, space, PageUp/PageDown, Home/End scroll; Backspace goes back;
             F5 or Cmd/Ctrl+R reloads; Esc quits
address bar: click it or press Cmd/Ctrl+L, type a URL or a file path, Enter opens it;
             Cmd/Ctrl+V pastes; Esc leaves it. Without a file the window starts empty";

struct Args {
    path: PathBuf,
    screenshot: Option<PathBuf>,
    bench: Option<u32>,
    width: f32,
    height: f32,
    scale: f32,
    scroll: f32,
    stats: bool,
    inspect: Option<String>,
    boxes: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { path: PathBuf::new(), screenshot: None, bench: None, width: 1024.0, height: 768.0, scale: 1.0, scroll: 0.0, stats: false, inspect: None, boxes: None };
    let mut it = std::env::args().skip(1);
    let mut path = None;
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        let num = |name: &str, v: String| v.parse::<f32>().map_err(|_| format!("{name}: '{v}' is not a number"));
        match arg.as_str() {
            "--screenshot" => a.screenshot = Some(value("--screenshot")?.into()),
            "--bench" => {
                let v = value("--bench")?;
                a.bench = Some(v.parse().map_err(|_| format!("--bench: '{v}' is not a count"))?);
            }
            "--width" => a.width = num("--width", value("--width")?)?,
            "--height" => a.height = num("--height", value("--height")?)?,
            "--scale" => a.scale = num("--scale", value("--scale")?)?,
            "--scroll" => a.scroll = num("--scroll", value("--scroll")?)?,
            "--stats" => a.stats = true,
            "--inspect" => a.inspect = Some(value("--inspect")?),
            "--boxes" => a.boxes = Some(value("--boxes")?.into()),
            "-h" | "--help" => return Err(String::new()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => path = Some(PathBuf::from(s)),
        }
    }
    a.path = match path {
        Some(path) => path,
        // The window can start empty: the page is then chosen in its address bar.
        None if a.screenshot.is_none() && a.boxes.is_none() && a.bench.is_none() => PathBuf::new(),
        None => return Err("missing HTML file".into()),
    };
    if !(a.width >= 1.0 && a.height >= 1.0 && a.scale > 0.0) {
        return Err("--width, --height and --scale must be positive".into());
    }
    Ok(a)
}

fn write_bmp(path: &Path, buf: &[u32], w: usize, h: usize) -> std::io::Result<()> {
    let row = (w * 3 + 3) & !3;
    let mut out = Vec::with_capacity(54 + row * h);
    out.extend_from_slice(b"BM");
    for v in [(54 + row * h) as u32, 0, 54, 40, w as u32, h as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&[1, 0, 24, 0]);
    for v in [0u32, (row * h) as u32, 2835, 2835, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for y in (0..h).rev() {
        for px in &buf[y * w..(y + 1) * w] {
            out.extend_from_slice(&px.to_le_bytes()[..3]);
        }
        out.resize(out.len() + row - w * 3, 0);
    }
    std::fs::write(path, out)
}

fn device_size(a: &Args) -> (usize, usize) {
    ((a.width * a.scale).round() as usize, (a.height * a.scale).round() as usize)
}

fn screenshot(a: &Args, out: &Path) -> std::io::Result<()> {
    let mut fonts = Fonts::new();
    let mut page = Page::load(&a.path)?;
    page.run_pending(&mut fonts, a.width, a.height);
    if a.scroll > 0.0 {
        page.update(&mut fonts, a.width, a.height);
        page.scroll_to(&mut fonts, a.scroll.clamp(0.0, (page.layout.height - a.height).max(0.0)));
    }
    page.settle(&mut fonts, a.width, a.height, SETTLE);
    if let Some(sel) = &a.inspect {
        for line in page.inspect(sel) {
            eprintln!("{line}");
        }
    }
    let (w, h) = device_size(a);
    let mut buf = vec![0u32; w * h];
    let mut target = Target { buf: &mut buf, w: w as i32, h: h as i32 };
    let scroll = a.scroll.clamp(0.0, (page.layout.height - a.height).max(0.0));
    paint(&page.layout, &mut fonts, &mut page.images, &mut target, scroll, a.scale);
    scrollbar(&mut target, scroll, page.layout.height, a.scale);
    write_bmp(out, &buf, w, h)
}

/// Machine-readable geometry and computed style for tools/check.ts, one
/// line per selector: `index x y w h | style`, with `-` in place of a
/// missing box and `?` when nothing matches the selector.
fn boxes(a: &Args, selectors: &Path) -> std::io::Result<()> {
    let list = std::fs::read_to_string(selectors)?;
    let mut fonts = Fonts::new();
    let mut page = Page::load(&a.path)?;
    page.record_boxes = true;
    page.settle(&mut fonts, a.width, a.height, SETTLE);
    let mut out = String::new();
    for (i, sel) in list.lines().enumerate() {
        match page.describe(sel) {
            Some((Some([x, y, w, h]), css)) => out += &format!("{i} {x:.1} {y:.1} {w:.1} {h:.1} | {css}\n"),
            Some((None, css)) => out += &format!("{i} - | {css}\n"),
            None => out += &format!("{i} ?\n"),
        }
    }
    out += &format!("height {:.1}\n", page.layout.height);
    print!("{out}");
    Ok(())
}

fn bench(a: &Args, iters: u32) -> std::io::Result<()> {
    use render::{dom::parse_html, layout::layout, style::compute_styles};
    let html = String::from_utf8_lossy(&std::fs::read(&a.path)?).into_owned();
    let base = a.path.parent();
    let mut fonts = Fonts::new();
    let (w, h) = device_size(a);
    let mut buf = vec![0u32; w * h];

    let cold = Instant::now();
    let mut page = Page::from_html(&html, base);
    page.update(&mut fonts, a.width, a.height);
    paint(&page.layout, &mut fonts, &mut page.images, &mut Target { buf: &mut buf, w: w as i32, h: h as i32 }, 0.0, a.scale);
    let cold = cold.elapsed();

    let time = |f: &mut dyn FnMut()| {
        let (mut best, mut total) = (Duration::MAX, Duration::ZERO);
        for _ in 0..iters.max(1) {
            let t = Instant::now();
            f();
            let d = t.elapsed();
            best = best.min(d);
            total += d;
        }
        (best, total / iters.max(1))
    };
    let rows = [
        ("parse html", time(&mut || drop(std::hint::black_box(parse_html(&html))))),
        ("parse html+css", time(&mut || drop(std::hint::black_box(Page::from_html(&html, base))))),
        ("style", time(&mut || drop(std::hint::black_box(compute_styles(&page.dom, &page.sheet, a.width, a.height, u32::MAX))))),
        ("layout", time(&mut || {
            let old = std::mem::take(&mut page.layout);
            page.layout = layout(&page.dom, &page.styles, &mut fonts, &mut page.images, a.width, a.height, old);
        })),
        ("paint", time(&mut || {
            let mut target = Target { buf: &mut buf, w: w as i32, h: h as i32 };
            paint(&page.layout, &mut fonts, &mut page.images, &mut target, a.scroll, a.scale);
        })),
    ];
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    println!("{}: {} bytes, {} nodes, {} rules, {} display items", a.path.display(), html.len(), page.dom.nodes.len(), page.sheet.rules.len(), page.layout.items.len());
    println!("viewport {}x{} css px at {}x = {w}x{h} device px, document height {:.0} px", a.width, a.height, a.scale, page.layout.height);
    println!("cold load to first frame (fonts and glyph cache empty): {:.3} ms", ms(cold));
    println!("{:<16}{:>12}{:>12}", "stage", "best ms", "mean ms");
    for (name, (best, mean)) in rows {
        println!("{name:<16}{:>12.3}{:>12.3}", ms(best), ms(mean));
    }
    Ok(())
}

struct App {
    path: PathBuf,
    page: Page,
    fonts: Fonts,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    scroll: f32,
    mouse: (f32, f32),
    mods: ModifiersState,
    history: Vec<(PathBuf, f32)>,
    pointer: bool,
    stats: bool,
    proxy: EventLoopProxy<()>,
    /// The current page has been shown at least once, so its held-back scripts may run.
    painted: bool,
    /// The text being typed into the address bar, while it is being edited.
    address: Option<String>,
    /// The next typed text replaces the whole address, as after select-all.
    replace: bool,
    /// The web page being downloaded; the address bar shows "Loading" meanwhile.
    loading: Option<Navigation>,
    /// The number of the newest navigation. A worker with an older one gives up.
    current: Arc<AtomicU64>,
    /// Where navigation workers send what they downloaded.
    fetched: (Sender<Fetched>, Receiver<Fetched>),
    /// When the page now shown was asked for, until its first frame is reported.
    opened: Option<Instant>,
    /// Images of the current page that arrived after it was laid out without them.
    late_images: usize,
    /// Why the last location could not be opened.
    notice: Option<String>,
    /// The address bar as last drawn: its markup, width in pixels and pixels.
    bar: (String, u32, Vec<u32>),
}

/// Height of the address bar in CSS pixels.
const BAR: f32 = 34.0;

/// A web page being downloaded on a worker thread, and how to show it once it is there.
struct Navigation {
    id: u64,
    path: PathBuf,
    scroll: f32,
    remember: bool,
    started: Instant,
}

/// What a navigation's worker sends back.
struct Fetched {
    id: u64,
    result: Result<Download, String>,
}

fn millis(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// What the address bar opens for typed text: a URL, an existing file, or a host
/// name that gets `https://` in front.
fn locate(input: &str) -> PathBuf {
    let host_like = !input.contains(' ') && (input.contains('.') || input.starts_with("localhost"));
    if !render::net::is_url(input) && !Path::new(input).exists() && host_like {
        PathBuf::from(format!("https://{input}"))
    } else {
        PathBuf::from(input)
    }
}

/// Text on the system clipboard, read with the platform's own tool.
fn clipboard() -> String {
    let tool = if cfg!(target_os = "macos") { "pbpaste" } else { "wl-paste" };
    std::process::Command::new(tool).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Draws the address bar by rendering a one-element page.
fn draw_bar(markup: &str, fonts: &mut Fonts, w: u32, h: u32, scale: f32) -> Vec<u32> {
    let mut page = Page::from_html(markup, None);
    page.update(fonts, w as f32 / scale, h as f32 / scale);
    let mut pixels = vec![0; (w * h) as usize];
    let mut target = Target { buf: &mut pixels, w: w as i32, h: h as i32 };
    paint(&page.layout, fonts, &mut page.images, &mut target, 0.0, scale);
    pixels
}

/// Decode the page's images off the main thread; a finished one wakes the event loop,
/// and so does a resource of the page that finishes downloading.
fn decode_in_background(page: &mut Page, proxy: &EventLoopProxy<()>) {
    let proxy = proxy.clone();
    let wake: render::net::Wake = Arc::new(move || {
        // The loop is gone only while the process is exiting.
        let _ = proxy.send_event(());
    });
    page.images.decode_in_background(wake.clone());
    page.on_arrival(wake);
}

impl App {
    fn view(&self) -> (f32, f32, f32) {
        let Some(w) = &self.window else { return (1.0, 1.0, 1.0) };
        let (size, scale) = (w.inner_size(), w.scale_factor() as f32);
        (size.width as f32 / scale, (size.height as f32 / scale - BAR).max(1.0), scale)
    }

    fn bar_markup(&self) -> String {
        let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let (text, border, color) = match (&self.loading, &self.address, &self.notice) {
            (Some(nav), _, _) => (format!("Loading {} ...", escape(&nav.path.display().to_string())), "#c8c8c8", "#777"),
            (None, Some(text), _) if text.is_empty() => ("<span style=\"color:#999\">Type a URL or a file path, then press Enter</span>".to_string(), "#0a84ff", "#222"),
            (None, Some(text), _) => (format!("{}<span style=\"color:#0a84ff\">|</span>", escape(text)), "#0a84ff", "#222"),
            (None, None, Some(notice)) => (escape(notice), "#d33", "#b00"),
            (None, None, None) => (escape(&self.path.display().to_string()), "#c8c8c8", "#222"),
        };
        format!(
            "<body style=\"margin:0;background:#ececec\"><div style=\"margin:5px 8px;padding:3px 10px;height:16px;background:#fff;border:1px solid {border};border-radius:6px;font-family:Arial;font-size:13px;line-height:16px;color:{color};white-space:nowrap;overflow:hidden\">{text}</div></body>"
        )
    }

    fn edit_address(&mut self) {
        self.address = Some(self.path.display().to_string());
        self.replace = true;
        self.notice = None;
        self.request_redraw();
    }

    fn address_key(&mut self, event: &KeyEvent, command: bool) {
        let mut text = self.address.take().unwrap_or_default();
        let replace = std::mem::take(&mut self.replace);
        let insert = |text: &mut String, typed: &str| {
            if replace {
                text.clear();
            }
            text.extend(typed.chars().filter(|c| !c.is_control()));
        };
        let mut editing = true;
        let mut target = None;
        match event.logical_key.as_ref() {
            Key::Named(NamedKey::Escape) => editing = false,
            Key::Named(NamedKey::Enter) => {
                editing = false;
                if !text.trim().is_empty() {
                    target = Some(locate(text.trim()));
                }
            }
            Key::Named(NamedKey::Backspace) if replace => text.clear(),
            Key::Named(NamedKey::Backspace) => drop(text.pop()),
            Key::Character("a") if command => self.replace = true,
            Key::Character("v") if command => insert(&mut text, clipboard().trim()),
            _ if command => self.replace = replace,
            _ => match &event.text {
                Some(typed) => insert(&mut text, typed),
                None => self.replace = replace,
            },
        }
        self.address = editing.then_some(text);
        self.request_redraw();
        if let Some(target) = target {
            self.open(target, 0.0, true);
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn scroll_to(&mut self, y: f32) {
        let max = (self.page.layout.height - self.view().1).max(0.0);
        let y = y.clamp(0.0, max);
        if y != self.scroll {
            self.scroll = y;
            self.page.set_scroll(y);
            self.request_redraw();
        }
    }

    /// Opens a location in place of the current page. A web page downloads on a
    /// worker thread while the window stays responsive; a local file is read here.
    fn open(&mut self, path: PathBuf, scroll: f32, remember: bool) {
        if path.to_str().is_some_and(render::net::is_url) {
            self.navigate(path, scroll, remember);
            return;
        }
        // Supersedes a navigation still in flight.
        self.current.fetch_add(1, Ordering::Relaxed);
        self.loading = None;
        let started = Instant::now();
        // Like the first page: show a frame first, run scripts after it.
        match Page::load_deferred(&path) {
            Ok(page) => self.show(page, path, scroll, remember, started),
            Err(e) => self.fail(&path, &e.to_string()),
        }
    }

    /// Starts downloading a web page on a worker thread; `arrive` shows it. A newer
    /// navigation supersedes this one: the worker then stops before the images and
    /// its result is dropped unseen.
    fn navigate(&mut self, path: PathBuf, scroll: f32, remember: bool) {
        let id = self.current.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, proxy, current) = (self.fetched.0.clone(), self.proxy.clone(), self.current.clone());
        let location = path.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            let live = || current.load(Ordering::Relaxed) == id;
            let result = Page::download(&location, &live);
            // The loop is gone only while the process is exiting.
            if live() && tx.send(Fetched { id, result }).is_ok() {
                let _ = proxy.send_event(());
            }
        });
        self.loading = Some(Navigation { id, path, scroll, remember, started: Instant::now() });
        self.notice = None;
        self.request_redraw();
    }

    /// Shows the page a navigation worker downloaded, unless a newer one superseded it.
    fn arrive(&mut self, fetched: Fetched) {
        let Some(nav) = self.loading.take_if(|nav| nav.id == fetched.id) else { return };
        match fetched.result {
            Ok(download) => {
                if self.stats {
                    let ((ready, total), site) = (download.stylesheets, &download.site);
                    eprintln!(
                        "html {}: first byte {:.1} ms, complete {:.1} ms; {ready} of {total} stylesheets ready after {:.1} ms",
                        site.url,
                        millis(site.first_byte),
                        millis(site.received),
                        millis(nav.started.elapsed())
                    );
                }
                let page = Page::from_download(download);
                self.show(page, nav.path, nav.scroll, nav.remember, nav.started);
            }
            Err(e) => self.fail(&nav.path, &e),
        }
    }

    fn show(&mut self, mut page: Page, path: PathBuf, scroll: f32, remember: bool, started: Instant) {
        if self.stats {
            eprintln!("opened {} in {:.1} ms", path.display(), millis(started.elapsed()));
            if let Some(start) = PROCESS_START.get().filter(|_| !FIRST_FRAME.load(Ordering::Relaxed)) {
                eprintln!("page loaded: {:.1} ms after process start", millis(start.elapsed()));
            }
        }
        decode_in_background(&mut page, &self.proxy);
        if remember {
            self.history.push((self.path.clone(), self.scroll));
        }
        self.page = page;
        self.path = path;
        self.scroll = scroll;
        self.notice = None;
        self.painted = false;
        self.opened = Some(started);
        self.late_images = 0;
        if let Some(w) = &self.window {
            w.set_title(&title(&self.page, &self.path));
        }
        // Resources that arrived while the page was being built.
        self.take_arrivals();
        self.request_redraw();
    }

    fn fail(&mut self, path: &Path, e: &str) {
        eprintln!("cannot open {}: {e}", path.display());
        self.notice = Some(format!("Cannot open {}: {e}", path.display()));
        self.request_redraw();
    }

    /// Takes in the current page's stylesheets, images and web fonts that arrived
    /// since it was last laid out, and repaints it with them.
    fn take_arrivals(&mut self) {
        let (css, images, fonts) = self.page.take_arrivals(&mut self.fonts);
        if css + images + fonts == 0 {
            return;
        }
        self.late_images += images;
        if let Some(start) = PROCESS_START.get().filter(|_| self.stats) {
            let at = millis(start.elapsed());
            eprintln!("late arrivals: {css} stylesheets, {images} images ({} images so far), {fonts} fonts; relayout at {at:.1} ms after process start", self.late_images);
        }
        self.request_redraw();
    }

    fn follow(&mut self, href: &str) {
        if let Some(id) = href.strip_prefix('#') {
            if let Some(y) = self.page.anchor_y(id) {
                self.scroll_to(y);
            }
            return;
        }
        let target = match self.page.url() {
            Some(url) => url.join(href).map(|u| u.to_string()),
            None => Some(href.to_string()).filter(|h| render::net::is_url(h)),
        };
        if let Some(target) = target {
            self.open(PathBuf::from(target), 0.0, true);
            return;
        }
        if self.page.url().is_some() || href.contains("://") || href.starts_with("mailto:") {
            eprintln!("only local files can be opened: {href}");
            return;
        }
        let file = href.split(['#', '?']).next().unwrap_or(href);
        let path = self.path.parent().unwrap_or(Path::new(".")).join(file);
        self.open(path, 0.0, true);
    }

    fn redraw(&mut self) {
        let markup = self.bar_markup();
        let (Some(window), Some(surface)) = (self.window.as_ref(), self.surface.as_mut()) else { return };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        let scale = window.scale_factor() as f32;
        // The address bar takes the top rows; the page gets the rest as its viewport.
        let bar_h = ((BAR * scale).round() as u32).min(size.height - 1);
        let (vw, vh) = (size.width as f32 / scale, (size.height - bar_h) as f32 / scale);
        if self.bar.0 != markup || self.bar.1 != size.width || self.bar.2.len() != (size.width * bar_h) as usize {
            self.bar = (markup.clone(), size.width, draw_bar(&markup, &mut self.fonts, size.width, bar_h, scale));
        }
        let t0 = Instant::now();
        self.page.update(&mut self.fonts, vw, vh);
        self.scroll = self.scroll.clamp(0.0, (self.page.layout.height - vh).max(0.0));
        let update = t0.elapsed();
        surface.resize(w, h).expect("resize window surface");
        let mut buf = surface.buffer_mut().expect("acquire frame buffer");
        let t1 = Instant::now();
        let (bar, rest) = buf.split_at_mut(self.bar.2.len());
        bar.copy_from_slice(&self.bar.2);
        let mut target = Target { buf: rest, w: size.width as i32, h: (size.height - bar_h) as i32 };
        paint(&self.page.layout, &mut self.fonts, &mut self.page.images, &mut target, self.scroll, scale);
        scrollbar(&mut target, self.scroll, self.page.layout.height, scale);
        let t2 = Instant::now();
        buf.present().expect("present frame");
        let present = t2.elapsed();
        self.painted = true;
        // While a navigation is in flight the frame still shows the page before it.
        if self.loading.is_none() {
            if !FIRST_FRAME.swap(true, Ordering::Relaxed) && self.stats {
                if let Some(start) = PROCESS_START.get() {
                    eprintln!("first frame: {:.1} ms after process start", start.elapsed().as_secs_f64() * 1e3);
                }
            }
            if let Some(started) = self.opened.take().filter(|_| self.stats) {
                eprintln!("first frame of {}: {:.1} ms after it was asked for", self.path.display(), millis(started.elapsed()));
            }
        }
        if self.stats {
            eprintln!(
                "frame {}x{} scroll {:.0}: style+layout {:.3} ms, paint {:.3} ms, present {:.3} ms",
                size.width,
                size.height,
                self.scroll,
                update.as_secs_f64() * 1e3,
                (t2 - t1).as_secs_f64() * 1e3,
                present.as_secs_f64() * 1e3
            );
        }
    }

    fn hover(&mut self) {
        // Above the page, on the address bar, nothing in the document is under the pointer.
        let node = if self.mouse.1 < 0.0 { render::dom::NONE } else { self.page.hit(self.mouse.0, self.mouse.1 + self.scroll) };
        let pointer = self.page.pointer(node);
        if pointer != self.pointer {
            self.pointer = pointer;
            if let Some(w) = &self.window {
                w.set_cursor(if pointer { CursorIcon::Pointer } else { CursorIcon::Default });
            }
        }
        if self.page.set_hover(&mut self.fonts, node) {
            self.request_redraw();
        }
    }
}

fn title(page: &Page, path: &Path) -> String {
    if page.title.is_empty() {
        path.display().to_string()
    } else {
        page.title.clone()
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title(title(&self.page, &self.path)).with_inner_size(LogicalSize::new(1024.0, 768.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = softbuffer::Context::new(window.clone()).expect("create display context");
        self.surface = Some(softbuffer::Surface::new(&context, window.clone()).expect("create window surface"));
        self.window = Some(window);
        if self.stats {
            if let Some(start) = PROCESS_START.get() {
                eprintln!("window ready: {:.1} ms after process start", start.elapsed().as_secs_f64() * 1e3);
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        while let Ok(fetched) = self.fetched.1.try_recv() {
            self.arrive(fetched);
        }
        self.take_arrivals();
        if self.page.images.poll() {
            self.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Once a frame is on screen, run the held-back scripts one per turn of the
        // loop, so input and repaints are served in between.
        if self.painted {
            if let Some(left) = self.page.run_deferred(&mut self.fonts) {
                if self.page.take_dirty() {
                    self.request_redraw();
                }
                if left == 0 && self.stats {
                    if let Some(start) = PROCESS_START.get() {
                        eprintln!("deferred scripts done: {:.1} ms after process start", start.elapsed().as_secs_f64() * 1e3);
                    }
                }
                event_loop.set_control_flow(ControlFlow::Poll);
                return;
            }
        }
        let next = self.page.tick(&mut self.fonts);
        if self.page.take_dirty() {
            self.request_redraw();
        }
        event_loop.set_control_flow(match next {
            Some(ms) => ControlFlow::WaitUntil(Instant::now() + Duration::from_secs_f64(ms.max(4.0) / 1e3)),
            None => ControlFlow::Wait,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.request_redraw(),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / self.view().2,
                };
                self.scroll_to(self.scroll - dy);
                self.hover();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.view().2;
                self.mouse = (position.x as f32 / scale, position.y as f32 / scale - BAR);
                self.hover();
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } if self.mouse.1 < 0.0 => self.edit_address(),
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.address = None;
                let node = self.page.hit(self.mouse.0, self.mouse.1 + self.scroll);
                let href = self.page.link(node).map(str::to_string);
                let cancelled = self.page.click(&mut self.fonts, node);
                if self.page.take_dirty() {
                    self.request_redraw();
                }
                if let (Some(href), false) = (href, cancelled) {
                    self.follow(&href);
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let page_step = self.view().1 * 0.9;
                let command = self.mods.super_key() || self.mods.control_key();
                if self.address.is_some() {
                    self.address_key(&event, command);
                    return;
                }
                match event.logical_key.as_ref() {
                    Key::Character("l") if command => self.edit_address(),
                    Key::Named(NamedKey::ArrowDown) => self.scroll_to(self.scroll + 48.0),
                    Key::Named(NamedKey::ArrowUp) => self.scroll_to(self.scroll - 48.0),
                    Key::Named(NamedKey::PageDown | NamedKey::Space) => self.scroll_to(self.scroll + page_step),
                    Key::Named(NamedKey::PageUp) => self.scroll_to(self.scroll - page_step),
                    Key::Named(NamedKey::Home) => self.scroll_to(0.0),
                    Key::Named(NamedKey::End) => self.scroll_to(f32::MAX),
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::Backspace) => {
                        if let Some((path, scroll)) = self.history.pop() {
                            self.open(path, scroll, false);
                        }
                    }
                    Key::Named(NamedKey::F5) => self.open(self.path.clone(), self.scroll, false),
                    Key::Character("r") if command => self.open(self.path.clone(), self.scroll, false),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn run() -> Result<(), String> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) if e.is_empty() => {
            println!("{USAGE}");
            return Ok(());
        }
        Err(e) => return Err(format!("{e}\n\n{USAGE}")),
    };
    let io = |e: std::io::Error| format!("{}: {e}", args.path.display());
    if let Some(out) = &args.screenshot {
        return screenshot(&args, out).map_err(io);
    }
    if let Some(file) = &args.boxes {
        return boxes(&args, file).map_err(io);
    }
    if let Some(n) = args.bench {
        return bench(&args, n).map_err(io);
    }
    let event_loop = EventLoop::new().map_err(|e| format!("cannot start the event loop: {e}"))?;
    let proxy = event_loop.create_proxy();
    let blank = args.path.as_os_str().is_empty();
    // A web page downloads while the window opens; until then the window is empty.
    let web = args.path.to_str().is_some_and(render::net::is_url);
    let mut page = if blank || web { Page::from_html("", None) } else { Page::load_deferred(&args.path).map_err(io)? };
    decode_in_background(&mut page, &proxy);
    if args.stats && !web {
        if let Some(start) = PROCESS_START.get() {
            eprintln!("page loaded: {:.1} ms after process start", start.elapsed().as_secs_f64() * 1e3);
        }
    }
    let mut app = App {
        path: args.path.clone(),
        page,
        fonts: Fonts::new(),
        window: None,
        surface: None,
        scroll: 0.0,
        mouse: (-1.0, -1.0),
        mods: ModifiersState::empty(),
        history: Vec::new(),
        pointer: false,
        stats: args.stats,
        proxy,
        painted: false,
        address: blank.then(String::new),
        replace: false,
        loading: None,
        current: Arc::new(AtomicU64::new(0)),
        fetched: channel(),
        opened: None,
        late_images: 0,
        notice: None,
        bar: (String::new(), 0, Vec::new()),
    };
    app.fonts.log = args.stats;
    if web {
        app.navigate(args.path.clone(), 0.0, false);
    }
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut app).map_err(|e| format!("event loop failed: {e}"))
}

/// Set at the top of `main`; `--stats` reports the first presented frame against it.
/// `FIRST_FRAME` turns true once a frame has been presented.
static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static FIRST_FRAME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn main() {
    let _ = PROCESS_START.set(Instant::now());
    if let Err(e) = run() {
        eprintln!("render: {e}");
        std::process::exit(1);
    }
}
