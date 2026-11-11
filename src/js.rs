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
use boa_engine::module::SimpleModuleLoader;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

const PRELUDE: &str = include_str!("prelude.js");
/// Stops runaway loops in page scripts instead of freezing the window.
const LOOP_LIMIT: u64 = 20_000_000;

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

fn now(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(START.with(|s| s.elapsed().as_secs_f64() * 1e3)))
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
    ("__now", 0, now),
];

pub struct Js {
    ctx: Context,
    booted: bool,
}

impl Js {
    /// `base` is the directory that module imports may load from.
    pub fn new(base: Option<&Path>) -> Js {
        let loader = base.and_then(|b| SimpleModuleLoader::new(b).ok());
        let mut ctx = match loader {
            Some(l) => Context::builder().module_loader(Rc::new(l)).build().expect("create script context"),
            None => Context::default(),
        };
        ctx.runtime_limits_mut().set_loop_iteration_limit(LOOP_LIMIT);
        for &(name, len, f) in NATIVES {
            ctx.register_global_callable(JsString::from(name), len, NativeFunction::from_fn_ptr(f)).expect("register native function");
        }
        START.with(|_| ());
        Js { ctx, booted: false }
    }

    /// Lends the page state to scripts. Every `enter` needs a `leave`.
    pub fn enter(&mut self, host: Host) {
        HOST.with(|h| *h.borrow_mut() = Some(host));
        if !self.booted {
            self.booted = true;
            self.eval(PRELUDE, "prelude");
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
        let module = match Module::parse(Source::from_bytes(code).with_path(path), None, &mut self.ctx) {
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

    /// Runs due timers; returns milliseconds until the next one.
    pub fn timers(&mut self) -> Option<f64> {
        self.eval("__timers()", "timer").and_then(|v| v.as_number()).filter(|ms| *ms >= 0.0)
    }
}
