//! JavaScript support: a boa engine plus native functions that expose the DOM.
//!
//! The DOM API that scripts see is written in JavaScript (`prelude.js`) on top
//! of a few native functions that take node indices. While a script runs, the
//! page lends its DOM to a thread-local `Host` that those functions reach.

use crate::dom::{atom, Dom, NodeId, NONE};
use crate::{style, FxMap};
use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::object::builtins::JsArray;
use boa_engine::builtins::promise::PromiseState;
use boa_engine::{Context, JsArgs, JsResult, JsString, JsValue, Module, NativeFunction, Source};
use crate::net::{Site, Url};
use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer, SimpleModuleLoader};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

const PRELUDE: &str = include_str!("prelude.js");

/// Replaces `Math.random` with mulberry32 from a fixed seed.
const FIXED_RANDOM: &str = "{ let s = 0x9E3779B9; Math.random = () => { s = (s + 0x6D2B79F5) | 0; let t = Math.imul(s ^ (s >>> 15), 1 | s); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; }; }";
/// Stops runaway loops in page scripts instead of freezing the window.
const LOOP_LIMIT: u64 = 20_000_000;
/// Nested calls a script may make, about what Chrome allows for plain recursion.
/// Calls made through native code (`map`, getters, `JSON.stringify`) also use
/// the machine stack: on an 8 MB main thread the costliest measured pattern
/// overflowed at about 13,500 of these, so the limit keeps that out of reach.
const RECURSION_LIMIT: usize = 10_000;
/// Values on the script stack across all those calls.
const STACK_LIMIT: usize = 1_000_000;

/// Page state that scripts can read and change.
pub struct Host {
    pub dom: Dom,
    /// Set by any native call that changes the document.
    pub dirty: bool,
    /// Border boxes from the last layout, by node.
    pub rects: FxMap<u32, [f32; 4]>,
    /// Viewport width, height and scroll offset in CSS pixels.
    pub view: [f32; 3],
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    static START: Instant = Instant::now();
    /// Milliseconds the script clock runs ahead of the wall clock; see `Js::skip`.
    static SKIPPED: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

fn with<R>(f: impl FnOnce(&mut Host) -> Option<R>) -> Option<R> {
    HOST.with(|h| h.borrow_mut().as_mut().and_then(f))
}

fn id_arg(a: &[JsValue], i: usize) -> NodeId {
    match a.get(i).and_then(JsValue::as_number) {
        Some(n) if n >= 0.0 => n as NodeId,
        _ => NONE,
    }
}

fn str_arg(a: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<String> {
    Ok(a.get_or_undefined(i).to_string(ctx)?.to_std_string_escaped())
}

fn js_str(s: &str) -> JsValue {
    JsString::from(s).into()
}

fn js_id(n: NodeId) -> JsValue {
    if n == NONE {
        JsValue::from(-1)
    } else {
        JsValue::from(n as f64)
    }
}

fn js_array(items: Vec<JsValue>, ctx: &mut Context) -> JsValue {
    JsArray::from_iter(items, ctx).into()
}

/// A node index that scripts passed in, checked against the arena.
fn live(h: &Host, n: NodeId) -> Option<NodeId> {
    ((n as usize) < h.dom.nodes.len()).then_some(n)
}

fn log(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    eprintln!("[js] {}", str_arg(a, 0, ctx)?);
    Ok(JsValue::undefined())
}

fn tag(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    Ok(with(|h| live(h, n).map(|n| js_str(h.dom.atoms.name(h.dom.nodes[n as usize].tag)))).unwrap_or(JsValue::null()))
}

/// `which`: 0 parent, 1 first child, 2 last child, 3 previous, 4 next.
fn link(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (n, which) = (id_arg(a, 0), id_arg(a, 1));
    let r = with(|h| {
        let node = &h.dom.nodes[live(h, n)? as usize];
        Some(match which {
            0 => node.parent,
            1 => node.first_child,
            2 => node.last_child,
            3 => node.prev,
            _ => node.next,
        })
    });
    Ok(js_id(r.unwrap_or(NONE)))
}

fn attr(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, name) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    let r = with(|h| {
        let n = live(h, n)?;
        let name = h.dom.atoms.intern_lower(&name);
        h.dom.attr(n, name).map(js_str)
    });
    Ok(r.unwrap_or(JsValue::null()))
}

fn set_attr(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, name) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    let value = if a.get_or_undefined(2).is_null_or_undefined() { None } else { Some(str_arg(a, 2, ctx)?) };
    with(|h| {
        let n = live(h, n)?;
        h.dom.set_attr(n, &name, value.as_deref());
        h.dirty = true;
        Some(())
    });
    Ok(JsValue::undefined())
}

/// Flat `[name, value, name, value, ...]`.
fn attrs(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    let items = with(|h| {
        let (s, c) = h.dom.nodes[live(h, n)? as usize].attrs;
        let mut out = Vec::new();
        for at in &h.dom.attrs[s as usize..(s + c) as usize] {
            let (vs, vl) = at.value;
            out.push(js_str(h.dom.atoms.name(at.name)));
            out.push(js_str(&h.dom.text[vs as usize..(vs + vl) as usize]));
        }
        Some(out)
    });
    Ok(js_array(items.unwrap_or_default(), ctx))
}

fn text(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    Ok(with(|h| live(h, n).map(|n| js_str(h.dom.text_of(n)))).unwrap_or(js_str("")))
}

fn set_text(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, s) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    with(|h| {
        let n = live(h, n)?;
        h.dom.set_text(n, &s);
        h.dirty = true;
        Some(())
    });
    Ok(JsValue::undefined())
}

/// `__create(tag, text)`: a text node when `tag` is null, else an element.
fn create(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let is_text = a.get_or_undefined(0).is_null_or_undefined();
    let s = str_arg(a, if is_text { 1 } else { 0 }, ctx)?;
    Ok(js_id(with(|h| Some(if is_text { h.dom.create_text(&s) } else { h.dom.create_element(&s) })).unwrap_or(NONE)))
}

fn insert(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (parent, child, before) = (id_arg(a, 0), id_arg(a, 1), id_arg(a, 2));
    with(|h| {
        let (parent, child) = (live(h, parent)?, live(h, child)?);
        let before = live(h, before).unwrap_or(NONE);
        h.dom.insert_before(parent, child, before);
        h.dirty = true;
        Some(())
    });
    Ok(JsValue::undefined())
}

fn remove(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    with(|h| {
        let n = live(h, n)?;
        h.dom.detach(n);
        h.dirty = true;
        Some(())
    });
    Ok(JsValue::undefined())
}

fn html(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (n, outer) = (id_arg(a, 0), a.get_or_undefined(1).to_boolean());
    let r = with(|h| {
        let mut out = String::new();
        h.dom.serialize(live(h, n)?, outer, &mut out);
        Some(js_str(&out))
    });
    Ok(r.unwrap_or(js_str("")))
}

fn set_html(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, s) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    with(|h| {
        let n = live(h, n)?;
        h.dom.set_inner_html(n, &s);
        h.dirty = true;
        Some(())
    });
    Ok(JsValue::undefined())
}

fn content(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    let r = with(|h| {
        let n = live(h, n)?;
        if h.dom.nodes[n as usize].tag == atom::TEXT {
            return Some(js_str(h.dom.text_of(n)));
        }
        let mut out = String::new();
        h.dom.text_content(n, &mut out);
        Some(js_str(&out))
    });
    Ok(r.unwrap_or(js_str("")))
}

fn query(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (root, sel, first) = (id_arg(a, 0), str_arg(a, 1, ctx)?, a.get_or_undefined(2).to_boolean());
    let found = with(|h| {
        let root = live(h, root)?;
        Some(style::query(&mut h.dom, root, &sel, first))
    });
    Ok(js_array(found.unwrap_or_default().into_iter().map(js_id).collect(), ctx))
}

fn matches(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, sel) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    let r = with(|h| {
        let n = live(h, n)?;
        Some(style::matches(&mut h.dom, n, &sel))
    });
    Ok(r.unwrap_or(false).into())
}

fn rect(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    let r = with(|h| h.rects.get(&n).copied()).unwrap_or([0.0; 4]);
    Ok(js_array(r.iter().map(|v| JsValue::from(*v as f64)).collect(), ctx))
}

fn view(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let v = with(|h| Some(h.view)).unwrap_or([0.0; 3]);
    Ok(js_array(v.iter().map(|v| JsValue::from(*v as f64)).collect(), ctx))
}

/// `__media(query)`: whether a media query matches the current viewport.
fn media(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let q = str_arg(a, 0, ctx)?;
    let [w, h, _] = with(|h| Some(h.view)).unwrap_or([0.0; 3]);
    Ok(crate::css::media_matches(&q, w, h).into())
}

fn now(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(START.with(|s| s.elapsed().as_secs_f64() * 1e3) + SKIPPED.get()))
}

const NATIVES: &[(&str, usize, NativeFunctionPointer)] = &[
    ("__log", 1, log),
    ("__tag", 1, tag),
    ("__link", 2, link),
    ("__attr", 2, attr),
    ("__set_attr", 3, set_attr),
    ("__attrs", 1, attrs),
    ("__text", 1, text),
    ("__set_text", 2, set_text),
    ("__create", 2, create),
    ("__insert", 3, insert),
    ("__remove", 1, remove),
    ("__html", 2, html),
    ("__set_html", 2, set_html),
    ("__content", 1, content),
    ("__query", 3, query),
    ("__matches", 2, matches),
    ("__rect", 1, rect),
    ("__view", 0, view),
    ("__media", 1, media),
    ("__now", 0, now),
];

/// Loads the modules of a page from the web. A specifier resolves against the URL
/// of the module that imports it, is downloaded through the page's `Site`, and
/// each URL is parsed (and so evaluated) once.
struct WebModules {
    site: Arc<Site>,
    /// The URL of every parsed module, by the local path it was parsed with.
    urls: RefCell<FxMap<PathBuf, Url>>,
    modules: RefCell<FxMap<Url, Module>>,
}

impl WebModules {
    /// The module at `url` parsed from `code`, kept at `path`; the one parsed
    /// before when there is one.
    fn parse(&self, url: Url, path: &Path, code: &[u8], ctx: &mut Context) -> JsResult<Module> {
        if let Some(m) = self.modules.borrow().get(&url) {
            return Ok(m.clone());
        }
        let module = Module::parse(Source::from_bytes(code).with_path(path), None, ctx)?;
        self.urls.borrow_mut().insert(path.to_path_buf(), url.clone());
        self.modules.borrow_mut().insert(url, module.clone());
        Ok(module)
    }

    /// Downloads and parses the module `specifier` names, imported by `referrer`.
    fn load(&self, referrer: &Referrer, specifier: &str, ctx: &mut Context) -> JsResult<Module> {
        let base = referrer.path().and_then(|p| self.urls.borrow().get(p).cloned()).unwrap_or_else(|| self.site.url.clone());
        let url = base.join(specifier).ok_or_else(|| js_error(format!("cannot resolve module {specifier}")))?;
        if let Some(m) = self.modules.borrow().get(&url) {
            return Ok(m.clone());
        }
        let path = self.site.file(&url.to_string()).ok_or_else(|| js_error(format!("cannot download module {url}")))?;
        let code = std::fs::read(&path).map_err(|e| js_error(format!("{url}: {e}")))?;
        self.parse(url, &path, &code, ctx)
    }
}

fn js_error(message: String) -> boa_engine::JsError {
    boa_engine::JsNativeError::typ().with_message(message).into()
}

impl ModuleLoader for WebModules {
    fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> impl std::future::Future<Output = JsResult<Module>> {
        let specifier = request.specifier().to_std_string_escaped();
        let result = self.load(&referrer, &specifier, &mut context.borrow_mut());
        async { result }
    }
}

pub struct Js {
    ctx: Context,
    booted: bool,
    /// `Math.random` is seeded, so every run computes the same page.
    repeatable: bool,
    /// Set for a page from the web.
    web: Option<Rc<WebModules>>,
}

impl Js {
    /// Module imports load from `site` for a page from the web, else from the
    /// directory `base`.
    pub fn new(base: Option<&Path>, site: Option<Arc<Site>>) -> Js {
        let web = site.map(|site| Rc::new(WebModules { site, urls: RefCell::default(), modules: RefCell::default() }));
        let local = base.filter(|_| web.is_none()).and_then(|b| SimpleModuleLoader::new(b).ok());
        let builder = Context::builder();
        let builder = match (&web, local) {
            (Some(w), _) => builder.module_loader(w.clone()),
            (None, Some(l)) => builder.module_loader(Rc::new(l)),
            (None, None) => builder,
        };
        let mut ctx = builder.build().expect("create script context");
        ctx.runtime_limits_mut().set_loop_iteration_limit(LOOP_LIMIT);
        ctx.runtime_limits_mut().set_recursion_limit(RECURSION_LIMIT);
        ctx.runtime_limits_mut().set_stack_size_limit(STACK_LIMIT);
        for &(name, len, f) in NATIVES {
            ctx.register_global_callable(JsString::from(name), len, NativeFunction::from_fn_ptr(f)).expect("register native function");
        }
        START.with(|_| ());
        Js { ctx, booted: false, repeatable: false, web }
    }

    /// Makes `Math.random` return the same sequence in every run. One-shot output
    /// (a screenshot, a box dump) must not depend on a page's dice.
    pub fn repeatable(mut self) -> Js {
        self.repeatable = true;
        self
    }

    /// Lends the page state to scripts. Every `enter` needs a `leave`.
    pub fn enter(&mut self, host: Host) {
        HOST.with(|h| *h.borrow_mut() = Some(host));
        if !self.booted {
            self.booted = true;
            self.eval(PRELUDE, "prelude");
            if self.repeatable {
                self.eval(FIXED_RANDOM, "fixed random");
            }
        }
    }

    pub fn leave(&mut self) -> Host {
        HOST.with(|h| h.borrow_mut().take()).expect("leave without enter")
    }

    /// Runs a script and then the pending promise jobs. Errors are reported
    /// on stderr and never abort the page.
    pub fn eval(&mut self, code: &str, name: &str) -> Option<JsValue> {
        let report = |e: String| eprintln!("[js] {name}: {}", e.chars().take(300).collect::<String>());
        let out = match self.ctx.eval(Source::from_bytes(code)) {
            Ok(v) => Some(v),
            Err(e) => {
                report(e.to_string());
                None
            }
        };
        if let Err(e) = self.ctx.run_jobs() {
            report(e.to_string());
        }
        out
    }

    /// Runs a `<script type="module">` stored at `path`; relative imports
    /// resolve against it.
    pub fn eval_module(&mut self, code: &str, name: &str, path: &Path) {
        let report = |e: String| eprintln!("[js] {name}: {}", e.chars().take(300).collect::<String>());
        let parsed = match &self.web {
            Some(web) => match web.site.url.join(name) {
                Some(url) => web.parse(url, path, code.as_bytes(), &mut self.ctx),
                None => return report(format!("cannot resolve module {name}")),
            },
            None => Module::parse(Source::from_bytes(code).with_path(path), None, &mut self.ctx),
        };
        let module = match parsed {
            Ok(m) => m,
            Err(e) => return report(e.to_string()),
        };
        let promise = module.load_link_evaluate(&mut self.ctx);
        if let Err(e) = self.ctx.run_jobs() {
            report(e.to_string());
        }
        if let PromiseState::Rejected(e) = promise.state() {
            report(e.display().to_string());
        }
    }

    /// Fires the document's load events.
    pub fn load(&mut self) {
        self.eval("__load()", "load");
    }

    /// Dispatches a bubbling event; true when a listener prevented the default.
    pub fn dispatch(&mut self, node: NodeId, event: &str) -> bool {
        self.eval(&format!("__dispatch({node}, \"{event}\")"), event).is_some_and(|v| v.to_boolean())
    }

    /// Moves the script clock (`performance.now`, timer and frame times) `ms`
    /// ahead, as if that time had passed with nothing to do.
    pub fn skip(&mut self, ms: f64) {
        SKIPPED.set(SKIPPED.get() + ms);
    }

    /// Runs due timers; returns milliseconds until the next one.
    pub fn timers(&mut self) -> Option<f64> {
        self.eval("__timers()", "timer").and_then(|v| v.as_number()).filter(|ms| *ms >= 0.0)
    }
}
