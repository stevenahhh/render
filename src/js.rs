//! JavaScript support: a boa engine plus native functions that expose the DOM.
//!
//! The DOM API that scripts see is written in JavaScript (`prelude.js`, with
//! rarer parts in `web/*.js` that load on first use) on top of native functions
//! that take node indices. While a script runs, the page lends itself to a
//! thread-local `Host` that those functions reach.

use crate::dom::{atom, Dom, NodeId, Parse, HAS_CUSTOM, NONE};
use crate::font::Fonts;
use crate::net::{Site, Wake};
use crate::page::Page;
use crate::url::Url;
use crate::{style, FxMap};
use boa_engine::builtins::promise::{OperationType, Promise, PromiseState};
use boa_engine::context::HostHooks;
use boa_engine::job::PromiseJob;
use boa_engine::module::{ModuleLoader, ModuleRequest, Referrer};
use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::object::builtins::{JsArray, JsArrayBuffer, JsPromise, JsUint8Array};
use boa_engine::{Context, JsArgs, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Module, NativeFunction, Source};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const PRELUDE: &str = include_str!("prelude.js");

/// Parts of the platform that most pages never touch, compiled on first use.
fn chunk(name: &str) -> Option<&'static str> {
    Some(match name {
        "custom" => include_str!("web/custom.js"),
        "observers" => include_str!("web/observers.js"),
        "traversal" => include_str!("web/traversal.js"),
        "messaging" => include_str!("web/messaging.js"),
        "fetch" => include_str!("web/fetch.js"),
        "encoding" => include_str!("web/encoding.js"),
        "shadow" => include_str!("web/shadow.js"),
        "cssom" => include_str!("web/cssom.js"),
        _ => return None,
    })
}

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

/// The page, lent to scripts while they run, and the fonts its layout uses.
pub struct Host {
    pub page: Page,
    pub fonts: Fonts,
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

fn js_error(message: String) -> JsError {
    JsNativeError::typ().with_message(message).into()
}

/// A node index that scripts passed in, checked against the arena.
fn live(dom: &Dom, n: NodeId) -> Option<NodeId> {
    ((n as usize) < dom.nodes.len()).then_some(n)
}

fn log(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    eprintln!("[js] {}", str_arg(a, 0, ctx)?);
    Ok(JsValue::undefined())
}

fn tag(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    let r = with(|h| {
        let dom = &h.page.dom;
        let n = live(dom, n)?;
        Some(js_str(if dom.is_comment(n) { "#comment" } else { dom.atoms.name(dom.nodes[n as usize].tag) }))
    });
    Ok(r.unwrap_or(JsValue::null()))
}

/// `which`: 0 parent, 1 first child, 2 last child, 3 previous, 4 next.
fn link(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (n, which) = (id_arg(a, 0), id_arg(a, 1));
    let r = with(|h| {
        let dom = &h.page.dom;
        let node = &dom.nodes[live(dom, n)? as usize];
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
        let dom = &mut h.page.dom;
        let n = live(dom, n)?;
        let name = dom.atoms.intern_lower(&name);
        dom.attr(n, name).map(js_str)
    });
    Ok(r.unwrap_or(JsValue::null()))
}

fn set_attr(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, name) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    let value = if a.get_or_undefined(2).is_null_or_undefined() { None } else { Some(str_arg(a, 2, ctx)?) };
    with(|h| {
        let dom = &mut h.page.dom;
        let n = live(dom, n)?;
        let atom = dom.atoms.intern_lower(&name);
        if dom.attr(n, atom) == value.as_deref() {
            return Some(());
        }
        let old = dom.attr(n, atom).map(str::to_string);
        dom.set_attr(n, &name, value.as_deref());
        h.page.attr_changed(n, atom, old.as_deref());
        Some(())
    });
    Ok(JsValue::undefined())
}

/// Flat `[name, value, name, value, ...]`.
fn attrs(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    let items = with(|h| {
        let dom = &h.page.dom;
        let (s, c) = dom.nodes[live(dom, n)? as usize].attrs;
        let mut out = Vec::new();
        for at in &dom.attrs[s as usize..(s + c) as usize] {
            let (vs, vl) = at.value;
            out.push(js_str(dom.atoms.name(at.name)));
            out.push(js_str(&dom.text[vs as usize..(vs + vl) as usize]));
        }
        Some(out)
    });
    Ok(js_array(items.unwrap_or_default(), ctx))
}

/// The data of a text or comment node.
fn text(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    Ok(with(|h| live(&h.page.dom, n).map(|n| js_str(h.page.dom.data_of(n)))).unwrap_or(js_str("")))
}

fn set_text(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, s) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    with(|h| {
        let n = live(&h.page.dom, n)?;
        if h.page.dom.data_of(n) == s {
            return Some(());
        }
        h.page.dom.set_data(n, &s);
        h.page.text_changed(n);
        Some(())
    });
    Ok(JsValue::undefined())
}

/// `__create(tag, text)`: a text node when `tag` is null, else an element.
fn create(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let is_text = a.get_or_undefined(0).is_null_or_undefined();
    let s = str_arg(a, if is_text { 1 } else { 0 }, ctx)?;
    Ok(js_id(with(|h| Some(if is_text { h.page.dom.create_text(&s) } else { h.page.dom.create_element(&s) })).unwrap_or(NONE)))
}

fn comment(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = str_arg(a, 0, ctx)?;
    Ok(js_id(with(|h| Some(h.page.dom.create_comment(&s))).unwrap_or(NONE)))
}

/// Inserts and returns `HAS_SCRIPT | HAS_CUSTOM` for what was connected, or -1
/// when the insertion would make a node its own ancestor.
fn insert(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (parent, child, before) = (id_arg(a, 0), id_arg(a, 1), id_arg(a, 2));
    let r = with(|h| {
        let dom = &mut h.page.dom;
        let (parent, child) = (live(dom, parent)?, live(dom, child)?);
        let before = live(dom, before).unwrap_or(NONE);
        let old = dom.nodes[child as usize].parent;
        if old != NONE {
            h.page.moved(child);
        }
        if !h.page.dom.insert_before(parent, child, before) {
            return Some(-1.0);
        }
        Some(h.page.moved(child) as f64)
    });
    Ok(JsValue::from(r.unwrap_or(0.0)))
}

/// Detaches a node; returns `HAS_CUSTOM` when it took custom elements out of the document.
fn remove(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    let r = with(|h| {
        let n = live(&h.page.dom, n)?;
        let flags = h.page.moved(n);
        h.page.dom.detach(n);
        Some(flags & HAS_CUSTOM)
    });
    Ok(JsValue::from(r.unwrap_or(0)))
}

fn html(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (n, outer) = (id_arg(a, 0), a.get_or_undefined(1).to_boolean());
    let r = with(|h| {
        let mut out = String::new();
        h.page.dom.serialize(live(&h.page.dom, n)?, outer, &mut out);
        Some(js_str(&out))
    });
    Ok(r.unwrap_or(js_str("")))
}

/// `__set_html(id, html, how)`: replaces the children with parsed HTML, `how`
/// 0 as `innerHTML`, 1 as `setHTMLUnsafe`, 2 a whole document; returns what
/// the new ones connected.
fn set_html(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, s) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    let how = match a.get_or_undefined(2).as_number() {
        Some(1.0) => Parse::Unsafe,
        Some(2.0) => Parse::Document,
        _ => Parse::Fragment,
    };
    let r = with(|h| {
        let n = live(&h.page.dom, n)?;
        let mut c = h.page.dom.nodes[n as usize].first_child;
        while c != NONE {
            h.page.moved(c);
            c = h.page.dom.nodes[c as usize].next;
        }
        h.page.dom.parse_into(n, &s, how);
        let mut flags = 0;
        let mut c = h.page.dom.nodes[n as usize].first_child;
        while c != NONE {
            flags |= h.page.moved(c);
            c = h.page.dom.nodes[c as usize].next;
        }
        h.page.text_changed(n);
        Some(flags)
    });
    Ok(JsValue::from(r.unwrap_or(0)))
}

fn content(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    let r = with(|h| {
        let dom = &h.page.dom;
        let n = live(dom, n)?;
        if dom.nodes[n as usize].tag == atom::TEXT {
            return Some(js_str(dom.data_of(n)));
        }
        let mut out = String::new();
        dom.text_content(n, &mut out);
        Some(js_str(&out))
    });
    Ok(r.unwrap_or(js_str("")))
}

fn query(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (root, sel, first) = (id_arg(a, 0), str_arg(a, 1, ctx)?, a.get_or_undefined(2).to_boolean());
    let found = with(|h| {
        let root = live(&h.page.dom, root)?;
        Some(style::query(&mut h.page.dom, root, &sel, first))
    });
    Ok(js_array(found.unwrap_or_default().into_iter().map(js_id).collect(), ctx))
}

/// `__scoped(scope, from, selector)`: elements below `from` matching a selector
/// whose `:scope` was written as the attribute `data-render-scope`, which the
/// scope element carries only while the query runs. Styles are not touched.
fn scoped(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (scope, from, sel) = (id_arg(a, 0), id_arg(a, 1), str_arg(a, 2, ctx)?);
    let found = with(|h| {
        let dom = &mut h.page.dom;
        let (scope, from) = (live(dom, scope)?, live(dom, from)?);
        let old = dom.attr(scope, dom.atoms.get("data-render-scope").unwrap_or(0)).map(str::to_string);
        dom.set_attr(scope, "data-render-scope", Some(""));
        let found = style::query(dom, from, &sel, false);
        dom.set_attr(scope, "data-render-scope", old.as_deref());
        Some(found)
    });
    Ok(js_array(found.unwrap_or_default().into_iter().map(js_id).collect(), ctx))
}

fn matches(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, sel) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    let r = with(|h| {
        let n = live(&h.page.dom, n)?;
        Some(style::matches(&mut h.page.dom, n, &sel))
    });
    Ok(r.unwrap_or(false).into())
}

fn by_id(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (root, id) = (id_arg(a, 0), str_arg(a, 1, ctx)?);
    Ok(js_id(with(|h| live(&h.page.dom, root).map(|r| h.page.dom.by_id(r, &id))).unwrap_or(NONE)))
}

/// `__kids(id, elements)`: the ids of a node's children, with `elements` only of element children.
fn kids(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, elements) = (id_arg(a, 0), a.get_or_undefined(1).to_boolean());
    let ids = with(|h| {
        let dom = &h.page.dom;
        let mut out = Vec::new();
        let mut c = dom.nodes[live(dom, n)? as usize].first_child;
        while c != NONE {
            if !elements || dom.is_element(c) {
                out.push(js_id(c));
            }
            c = dom.nodes[c as usize].next;
        }
        Some(out)
    });
    Ok(js_array(ids.unwrap_or_default(), ctx))
}

/// `__host(id)`: the template whose content a fragment is, or -1.
fn host(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    Ok(js_id(with(|h| h.page.dom.hosts.get(&n).copied()).unwrap_or(NONE)))
}

fn clone(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (n, deep) = (id_arg(a, 0), a.get_or_undefined(1).to_boolean());
    Ok(js_id(with(|h| live(&h.page.dom, n).map(|n| h.page.dom.clone_node(n, deep))).unwrap_or(NONE)))
}

fn template(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    Ok(js_id(with(|h| live(&h.page.dom, n).map(|n| h.page.dom.template_content(n))).unwrap_or(NONE)))
}

fn root(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    Ok(js_id(with(|h| live(&h.page.dom, n).map(|n| h.page.dom.root_of(n))).unwrap_or(NONE)))
}

fn connected(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    Ok(with(|h| live(&h.page.dom, n).map(|n| h.page.dom.connected(n))).unwrap_or(false).into())
}

/// `__shadow(id)`: the shadow root of a host, or -1.
fn shadow(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    Ok(js_id(with(|h| h.page.dom.shadows.get(&n).copied()).unwrap_or(NONE)))
}

fn attach_shadow(_: &JsValue, a: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let n = id_arg(a, 0);
    Ok(js_id(with(|h| {
        let n = live(&h.page.dom, n)?;
        let root = h.page.dom.attach_shadow(n);
        h.page.moved(n);
        Some(root)
    })
    .unwrap_or(NONE)))
}

/// `__adopt(css)`: the style sheets the document adopted, applied after its own.
fn adopt(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let css = str_arg(a, 0, ctx)?;
    with(|h| {
        h.page.adopt(css);
        Some(())
    });
    Ok(JsValue::undefined())
}

/// Scripts (`HAS_SCRIPT`) or possible custom elements (`HAS_CUSTOM`) in the
/// subtree of a node, the node and shadow trees included, in shadow-including tree order.
fn tagged(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, kind) = (id_arg(a, 0), id_arg(a, 1));
    let ids = with(|h| {
        let dom = &h.page.dom;
        let n = live(dom, n)?;
        let mut out = Vec::new();
        dom.each_including(n, &mut |c| {
            if dom.kind_of(c) & kind != 0 {
                out.push(js_id(c));
            }
        });
        Some(out)
    });
    Ok(js_array(ids.unwrap_or_default(), ctx))
}

/// `__rect(node, client)`: `[x, y, w, h]` of a node's border box from an
/// up-to-date layout, with `client` relative to the viewport, not the document.
fn rect(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (n, client) = (id_arg(a, 0), a.get_or_undefined(1).to_boolean());
    let r = with(|h| {
        let mut r = h.page.fresh_rect(&mut h.fonts, n);
        if client {
            r[1] -= h.page.script_view()[2];
        }
        Some(r)
    })
    .unwrap_or([0.0; 4]);
    Ok(js_array(r.iter().map(|v| JsValue::from(*v as f64)).collect(), ctx))
}

fn view(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let v = with(|h| Some(h.page.script_view())).unwrap_or([0.0; 3]);
    Ok(js_array(v.iter().map(|v| JsValue::from(*v as f64)).collect(), ctx))
}

/// `__image(src)`: `[width, height]` of the image `src` names, looked up as layout
/// looks images up; null when it cannot be had, 0 while it is still downloading.
fn image(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let src = str_arg(a, 0, ctx)?;
    let found = with(|h| {
        let images = &mut h.page.images;
        let misses = images.misses();
        let id = images.file(&src);
        Some((id.map(|id| images.size(id)), images.misses() != misses))
    });
    Ok(match found {
        Some((Some((w, h)), _)) => js_array(vec![JsValue::from(w as f64), JsValue::from(h as f64)], ctx),
        Some((None, true)) => JsValue::from(0),
        _ => JsValue::null(),
    })
}

/// `__media(query)`: whether a media query matches the current viewport.
fn media(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let q = str_arg(a, 0, ctx)?;
    let [w, h, _] = with(|h| Some(h.page.script_view())).unwrap_or([0.0; 3]);
    Ok(crate::css::media_matches(&q, w, h).into())
}

/// `__supports(property, value)`: whether the style engine understands a declaration.
fn supports(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (name, value) = (str_arg(a, 0, ctx)?, str_arg(a, 1, ctx)?);
    let mut decls = Vec::new();
    crate::css::parse_decl(name.trim(), value.trim(), &mut decls);
    Ok((!decls.is_empty() || name.trim().starts_with("--")).into())
}

fn now(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(START.with(|s| s.elapsed().as_secs_f64() * 1e3) + SKIPPED.get()))
}

/// `__url(input, base)`: the parts of the URL `input` names, resolved against
/// `base`: `[href, protocol, username, password, host, hostname, port, pathname,
/// search, hash, origin]`, or null when it is not a valid URL.
fn url(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let input = str_arg(a, 0, ctx)?;
    let base = if a.get_or_undefined(1).is_null_or_undefined() { None } else { Url::parse(&str_arg(a, 1, ctx)?, None) };
    if !a.get_or_undefined(1).is_null_or_undefined() && base.is_none() {
        return Ok(JsValue::null());
    }
    let Some(u) = Url::parse(&input, base.as_ref()) else { return Ok(JsValue::null()) };
    let opt = |s: &Option<String>, p: &str| s.as_deref().filter(|s| !s.is_empty()).map_or(String::new(), |s| format!("{p}{s}"));
    let parts = [
        u.href(),
        format!("{}:", u.scheme),
        u.username.clone(),
        u.password.clone(),
        u.host_port(),
        u.host.clone().unwrap_or_default(),
        u.port.map_or(String::new(), |p| p.to_string()),
        u.path.clone(),
        opt(&u.query, "?"),
        opt(&u.fragment, "#"),
        u.origin(),
    ];
    Ok(js_array(parts.iter().map(|s| js_str(s)).collect(), ctx))
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { BASE64[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Decodes base64 as `atob` does: whitespace is skipped and padding is optional.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() % 4 == 0 {
        while digits.last() == Some(&b'=') {
            digits.pop();
        }
    }
    if digits.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for d in digits {
        acc = acc << 6 | BASE64.iter().position(|&c| c == d)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn invalid_character() -> JsError {
    JsNativeError::error().with_message("The string contains invalid characters.").into()
}

/// `btoa`: each UTF-16 unit of the input is one byte; a unit above 255 is an error.
fn btoa(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = a.get_or_undefined(0).to_string(ctx)?;
    let bytes: Option<Vec<u8>> = s.iter().map(|u| u8::try_from(u).ok()).collect();
    Ok(js_str(&base64_encode(&bytes.ok_or_else(invalid_character)?)))
}

fn atob(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = str_arg(a, 0, ctx)?;
    let bytes = base64_decode(&s).ok_or_else(invalid_character)?;
    let units: Vec<u16> = bytes.into_iter().map(u16::from).collect();
    Ok(JsString::from(&units[..]).into())
}

/// `__encode(string)`: UTF-8 bytes as a Uint8Array.
fn encode_utf8(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = str_arg(a, 0, ctx)?;
    Ok(JsUint8Array::from_iter(s.into_bytes(), ctx)?.into())
}

/// The bytes of an ArrayBuffer, or of the part of one a typed array or DataView covers.
fn bytes_of(v: &JsValue, ctx: &mut Context) -> JsResult<Vec<u8>> {
    let Some(obj) = v.as_object() else { return Ok(Vec::new()) };
    if let Ok(buf) = JsArrayBuffer::from_object(obj.clone()) {
        return Ok(buf.data().map(|d| d.to_vec()).unwrap_or_default());
    }
    let buffer = obj.get(JsString::from("buffer"), ctx)?;
    let offset = obj.get(JsString::from("byteOffset"), ctx)?.to_length(ctx)? as usize;
    let len = obj.get(JsString::from("byteLength"), ctx)?.to_length(ctx)? as usize;
    let Some(buf) = buffer.as_object().and_then(|b| JsArrayBuffer::from_object(b.clone()).ok()) else { return Ok(Vec::new()) };
    let data = buf.data();
    Ok(data.as_deref().and_then(|d| d.get(offset..offset + len)).map(<[u8]>::to_vec).unwrap_or_default())
}

/// `__decode(bytes)`: a string from UTF-8, invalid sequences replaced.
fn decode_utf8(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let bytes = bytes_of(a.get_or_undefined(0), ctx)?;
    let s = String::from_utf8_lossy(&bytes);
    Ok(js_str(s.strip_prefix('\u{feff}').unwrap_or(&s)))
}

/// `__microtask(fn)`: queues `fn` with the promise jobs, as `queueMicrotask` does.
fn microtask(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let Some(f) = a.get_or_undefined(0).as_callable() else {
        return Err(js_error("queueMicrotask: callback is not a function".into()));
    };
    ctx.enqueue_job(
        PromiseJob::new(move |ctx| {
            if let Err(e) = f.call(&JsValue::undefined(), &[], ctx) {
                report(ctx, e, "microtask", None)?;
            }
            Ok(JsValue::undefined())
        })
        .into(),
    );
    Ok(JsValue::undefined())
}

fn chunk_source(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = str_arg(a, 0, ctx)?;
    chunk(&name).map(js_str).ok_or_else(|| js_error(format!("no platform part named {name}")))
}

/// `__run(code, url, name)`: runs a classic script that a script inserted.
fn run_script(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (code, url, name) = (str_arg(a, 0, ctx)?, str_arg(a, 1, ctx)?, str_arg(a, 2, ctx)?);
    if let Err(e) = ctx.eval(Source::from_bytes(&code).with_path(Path::new(&url))) {
        report(ctx, e, &name, Some(&url))?;
    }
    Ok(JsValue::undefined())
}

/// `__module(code, url, name)`: runs a module script that a script inserted.
fn run_module(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (code, url, name) = (str_arg(a, 0, ctx)?, str_arg(a, 1, ctx)?, str_arg(a, 2, ctx)?);
    let Some(modules) = ctx.get_data::<Rc<Modules>>().cloned() else { return Ok(JsValue::undefined()) };
    match modules.parse_inline(&code, &url, ctx) {
        Ok(module) => drop(module.load_link_evaluate(ctx)),
        Err(e) => report(ctx, e, &name, Some(&url))?,
    }
    Ok(JsValue::undefined())
}

/// `__load(url, binary)`: `[status, content type, body]` for a resource a script
/// asked for, the body a string or with `binary` a Uint8Array; null when it cannot be had.
fn load(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (href, binary) = (str_arg(a, 0, ctx)?, a.get_or_undefined(1).to_boolean());
    let Some(modules) = ctx.get_data::<Rc<Modules>>().cloned() else { return Ok(JsValue::null()) };
    let Some((kind, body)) = modules.fetch(&href) else { return Ok(JsValue::null()) };
    let body = if binary { JsUint8Array::from_iter(body, ctx)?.into() } else { js_str(&String::from_utf8_lossy(&body)) };
    Ok(js_array(vec![JsValue::from(200), js_str(&kind), body], ctx))
}

/// Requests that scripts made, running on worker threads, and the results
/// waiting to be handed to the page.
struct Fetches {
    done: Arc<Mutex<Vec<(u32, Result<crate::net::Fetched, crate::net::FetchError>)>>>,
    aborts: RefCell<FxMap<u32, Arc<AtomicBool>>>,
    referrer: String,
    wake: RefCell<Option<Wake>>,
}

/// A response as scripts get it: `[status, statusText, url, [name, value, ...], body]`,
/// or the message of the error that stopped it.
fn response(r: Result<crate::net::Fetched, crate::net::FetchError>, ctx: &mut Context) -> JsResult<JsValue> {
    let mut f = match r {
        Ok(f) => f,
        Err(e) => return Ok(js_str(&e.to_string())),
    };
    // A local file comes without headers; give it the type its name implies, as browsers do.
    if f.url.starts_with("file:") && !f.headers.iter().any(|(n, _)| n == "content-type") {
        let ext = f.url.rsplit('.').next().unwrap_or("");
        f.headers.push(("content-type".into(), content_type(ext).into()));
    }
    let headers: Vec<JsValue> = f.headers.iter().flat_map(|(n, v)| [js_str(n), js_str(v)]).collect();
    let headers = js_array(headers, ctx);
    let body = JsUint8Array::from_iter(f.body, ctx)?;
    Ok(js_array(vec![JsValue::from(f.status), js_str(&f.status_text), js_str(&f.url), headers, body.into()], ctx))
}

fn content_type(ext: &str) -> &'static str {
    match ext {
        "json" => "application/json",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "txt" => "text/plain",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

/// `__fetch(id, method, url, [name, value, ...], body, sync)`: sends a request. With
/// `sync` the response is returned; otherwise the host hands it to the page's
/// `fetched` entry point once it arrives.
fn fetch(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = id_arg(a, 0);
    let method = str_arg(a, 1, ctx)?;
    let url = str_arg(a, 2, ctx)?;
    let mut headers = Vec::new();
    if let Some(list) = a.get_or_undefined(3).as_object() {
        let len = list.get(JsString::from("length"), ctx)?.to_length(ctx)?;
        for i in (0..len.saturating_sub(1)).step_by(2) {
            let name = list.get(i, ctx)?.to_string(ctx)?.to_std_string_escaped();
            let value = list.get(i + 1, ctx)?.to_string(ctx)?.to_std_string_escaped();
            headers.push((name, value));
        }
    }
    let body = match a.get_or_undefined(4) {
        v if v.is_null_or_undefined() => None,
        v if v.is_string() => Some(v.to_string(ctx)?.to_std_string_escaped().into_bytes()),
        v => Some(bytes_of(v, ctx)?),
    };
    let Some(fetches) = ctx.get_data::<Rc<Fetches>>().cloned() else { return Ok(JsValue::undefined()) };
    let Some(method) = crate::net::Method::parse(&method) else { return Ok(js_str(&format!("{method}: unsupported method"))) };
    let abort = Arc::new(AtomicBool::new(false));
    let request = crate::net::Request { method, url, headers, body, referrer: Some(fetches.referrer.clone()), abort: Some(abort.clone()) };
    if a.get_or_undefined(5).to_boolean() {
        return response(crate::net::fetch(request), ctx);
    }
    fetches.aborts.borrow_mut().insert(id, abort);
    let (done, wake) = (fetches.done.clone(), fetches.wake.borrow().clone());
    std::thread::spawn(move || {
        let result = crate::net::fetch(request);
        done.lock().expect("fetch results").push((id, result));
        if let Some(wake) = wake {
            wake();
        }
    });
    Ok(JsValue::undefined())
}

fn abort_fetch(_: &JsValue, a: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = id_arg(a, 0);
    if let Some(flag) = ctx.get_data::<Rc<Fetches>>().and_then(|f| f.aborts.borrow_mut().remove(&id)) {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(JsValue::undefined())
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
    ("__comment", 1, comment),
    ("__insert", 3, insert),
    ("__remove", 1, remove),
    ("__html", 2, html),
    ("__set_html", 3, set_html),
    ("__content", 1, content),
    ("__query", 3, query),
    ("__scoped", 3, scoped),
    ("__matches", 2, matches),
    ("__by_id", 2, by_id),
    ("__kids", 2, kids),
    ("__host", 1, host),
    ("__clone", 2, clone),
    ("__template", 1, template),
    ("__root", 1, root),
    ("__connected", 1, connected),
    ("__attach_shadow", 1, attach_shadow),
    ("__shadow", 1, shadow),
    ("__adopt", 1, adopt),
    ("__fetch", 6, fetch),
    ("__abort_fetch", 1, abort_fetch),
    ("__tagged", 2, tagged),
    ("__rect", 1, rect),
    ("__view", 0, view),
    ("__image", 1, image),
    ("__media", 1, media),
    ("__supports", 2, supports),
    ("__now", 0, now),
    ("__url", 2, url),
    ("__btoa", 1, btoa),
    ("__atob", 1, atob),
    ("__encode", 1, encode_utf8),
    ("__decode", 1, decode_utf8),
    ("__microtask", 1, microtask),
    ("__chunk", 1, chunk_source),
    ("__run", 3, run_script),
    ("__module", 3, run_module),
    ("__load", 2, load),
];

/// The prelude's entry points for the host: `{ start, tick, dispatch, load, error,
/// begin, end, rejected }`.
struct Entry(JsObject);

fn call_entry(ctx: &mut Context, name: &str, args: &[JsValue]) -> JsResult<JsValue> {
    let Some(entry) = ctx.get_data::<Entry>().map(|e| e.0.clone()) else { return Ok(JsValue::undefined()) };
    let f = entry.get(JsString::from(name), ctx)?;
    match f.as_callable() {
        Some(f) => f.call(&JsValue::undefined(), args, ctx),
        None => Ok(JsValue::undefined()),
    }
}

/// Reports an error from page script on stderr and to the page as an `error`
/// event. Errors that scripts cannot catch, such as hitting a runtime limit,
/// are passed on. The script's own frames keep the `(unknown at :line:col)`
/// form they had before scripts were named by their URL: the line already
/// starts with the script's name, and this text is what tools compare.
fn report(ctx: &mut Context, e: JsError, name: &str, url: Option<&str>) -> JsResult<()> {
    let mut text = e.to_string();
    if let Some(url) = url {
        text = text.replace(&format!("({url}:"), "(unknown at :");
    }
    eprintln!("[js] {name}: {}", text.chars().take(300).collect::<String>());
    let Ok(value) = e.into_opaque(ctx) else { return Ok(()) };
    let url = url.map_or(JsValue::undefined(), js_str);
    call_entry(ctx, "error", &[value, url]).map(drop)
}

/// Loads the modules of a page. A specifier resolves against the URL of the
/// module that imports it (the page for a classic script), each URL is fetched
/// and parsed once, and is what `import.meta.url` reports.
struct Modules {
    page: Url,
    site: Option<Arc<Site>>,
    /// The URL of every parsed module, by the path it was parsed with.
    urls: RefCell<FxMap<PathBuf, String>>,
    modules: RefCell<FxMap<String, Module>>,
}

impl Modules {
    /// The content type and body of the resource at `href`: a local file, a
    /// download through the page's site, or a `data:` URL.
    fn fetch(&self, href: &str) -> Option<(String, Vec<u8>)> {
        let url = Url::parse(href, Some(&self.page))?;
        if url.scheme == "data" {
            let (head, body) = url.path.split_once(',')?;
            let kind = head.trim_end_matches(";base64");
            let decoded = crate::url::percent_decode(body);
            let bytes = if head.ends_with(";base64") { base64_decode(&decoded)? } else { decoded.into_bytes() };
            return Some((if kind.is_empty() { "text/plain".into() } else { kind.to_string() }, bytes));
        }
        let path = self.path_of(&url)?;
        let kind = content_type(path.extension().and_then(|e| e.to_str()).unwrap_or(""));
        Some((kind.to_string(), std::fs::read(path).ok()?))
    }

    /// Where the resource at `url` is on disk, downloading it first when it is on the web.
    fn path_of(&self, url: &Url) -> Option<PathBuf> {
        match (url.scheme.as_str(), &self.site) {
            ("file", _) => url.to_path().filter(|p| p.is_file()),
            ("http" | "https", Some(site)) => {
                let mut plain = url.clone();
                plain.fragment = None;
                site.file(&plain.href())
            }
            _ => None,
        }
    }

    fn parse(&self, href: String, path: &Path, code: &[u8], ctx: &mut Context) -> JsResult<Module> {
        let module = Module::parse(Source::from_bytes(code).with_path(path), None, ctx)?;
        self.urls.borrow_mut().insert(path.to_path_buf(), href.clone());
        self.modules.borrow_mut().insert(href, module.clone());
        Ok(module)
    }

    /// A `<script type=module>` with its code in the page, or inserted by a script.
    fn parse_inline(&self, code: &str, href: &str, ctx: &mut Context) -> JsResult<Module> {
        let path = PathBuf::from(href);
        let module = Module::parse(Source::from_bytes(code).with_path(&path), None, ctx)?;
        self.urls.borrow_mut().insert(path, href.to_string());
        Ok(module)
    }

    /// The module at `href`, fetched and parsed unless that happened before.
    fn module(&self, href: &str, ctx: &mut Context) -> JsResult<Module> {
        if let Some(m) = self.modules.borrow().get(href) {
            return Ok(m.clone());
        }
        let url = Url::parse(href, None).ok_or_else(|| js_error(format!("cannot resolve module {href}")))?;
        let path = self.path_of(&url).ok_or_else(|| js_error(format!("cannot load module {href}")))?;
        let code = std::fs::read(&path).map_err(|e| js_error(format!("{href}: {e}")))?;
        self.parse(href.to_string(), &path, &code, ctx)
    }

    fn resolve(&self, referrer: &Referrer, specifier: &str) -> JsResult<String> {
        let base = referrer.path().and_then(|p| self.urls.borrow().get(p).cloned());
        let base = base.and_then(|b| Url::parse(&b, None)).unwrap_or_else(|| self.page.clone());
        let mut url = Url::parse(specifier, Some(&base)).ok_or_else(|| js_error(format!("cannot resolve module {specifier}")))?;
        url.fragment = None;
        Ok(url.href())
    }
}

impl ModuleLoader for Modules {
    fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> impl std::future::Future<Output = JsResult<Module>> {
        let specifier = request.specifier().to_std_string_escaped();
        let result = self.resolve(&referrer, &specifier).and_then(|href| self.module(&href, &mut context.borrow_mut()));
        async { result }
    }

    fn init_import_meta(self: Rc<Self>, import_meta: &JsObject, module: &Module, context: &mut Context) {
        let href = module.path().and_then(|p| self.urls.borrow().get(p).cloned()).unwrap_or_else(|| self.page.href());
        // Defining a property on a fresh ordinary object cannot fail.
        let _ = import_meta.set(JsString::from("url"), js_str(&href), false, context);
    }
}

/// Keeps the promises that were rejected while nothing handled them, so they
/// can be reported once the microtasks have had their chance.
struct Rejections(Rc<RefCell<Vec<JsObject>>>);

impl HostHooks for Rejections {
    fn promise_rejection_tracker(&self, promise: &boa_engine::object::JsObject<Promise>, operation: OperationType, _: &mut Context) {
        let promise = promise.clone().upcast();
        let mut list = self.0.borrow_mut();
        match operation {
            OperationType::Reject => list.push(promise),
            OperationType::Handle => list.retain(|p| !JsObject::equals(p, &promise)),
        }
    }
}

pub struct Js {
    ctx: Context,
    booted: bool,
    /// `Math.random` is seeded, so every run computes the same page.
    repeatable: bool,
    /// The page's URL, which inline scripts report errors at.
    url: String,
    rejected: Rc<RefCell<Vec<JsObject>>>,
    fetches: Rc<Fetches>,
    /// Animation frame callbacks wait for `frame` instead of running as timers.
    frames: bool,
    /// The page is not visible: `document.hidden`.
    hidden: bool,
}

impl Js {
    /// A script engine for the page at `url`; resources on the web are
    /// downloaded through `site`.
    pub fn new(url: &Url, site: Option<Arc<Site>>) -> Js {
        let modules = Rc::new(Modules { page: url.clone(), site, urls: RefCell::default(), modules: RefCell::default() });
        let rejected = Rc::new(RefCell::new(Vec::new()));
        let builder = Context::builder().module_loader(modules.clone()).host_hooks(Rc::new(Rejections(rejected.clone())));
        let mut ctx = builder.build().expect("create script context");
        ctx.runtime_limits_mut().set_loop_iteration_limit(LOOP_LIMIT);
        ctx.runtime_limits_mut().set_recursion_limit(RECURSION_LIMIT);
        ctx.runtime_limits_mut().set_stack_size_limit(STACK_LIMIT);
        for &(name, len, f) in NATIVES {
            ctx.register_global_callable(JsString::from(name), len, NativeFunction::from_fn_ptr(f)).expect("register native function");
        }
        ctx.insert_data(modules);
        let fetches = Rc::new(Fetches { done: Arc::default(), aborts: RefCell::default(), referrer: url.href(), wake: RefCell::new(None) });
        ctx.insert_data(fetches.clone());
        START.with(|_| ());
        Js { ctx, booted: false, repeatable: false, url: url.href(), rejected, fetches, frames: false, hidden: false }
    }

    /// Calls `wake` from a worker thread when a request a script made has its answer.
    pub fn on_answer(&self, wake: Wake) {
        *self.fetches.wake.borrow_mut() = Some(wake);
    }

    /// True while a request a script made has not been answered.
    pub fn waiting(&self) -> bool {
        !self.fetches.aborts.borrow().is_empty()
    }

    /// Hands the answers that arrived to the page, each as a task of its own.
    fn answers(&mut self) {
        let done = std::mem::take(&mut *self.fetches.done.lock().expect("fetch results"));
        for (id, result) in done {
            if self.fetches.aborts.borrow_mut().remove(&id).is_none() {
                continue;
            }
            match response(result, &mut self.ctx) {
                Ok(v) => drop(self.call("fetched", &[js_id(id), v])),
                Err(e) => eprintln!("[js] fetch: {e}"),
            }
        }
    }

    /// Makes `Math.random` return the same sequence in every run. One-shot output
    /// (a screenshot, a box dump) must not depend on a page's dice.
    pub fn repeatable(mut self) -> Js {
        self.repeatable = true;
        self
    }

    /// Lends the page to scripts. Every `enter` needs a `leave`.
    pub fn enter(&mut self, host: Host) {
        HOST.with(|h| *h.borrow_mut() = Some(host));
        if !self.booted {
            self.booted = true;
            match self.ctx.eval(Source::from_bytes(PRELUDE)) {
                Ok(v) => {
                    if let Some(entry) = v.as_object() {
                        self.ctx.insert_data(Entry(entry.clone()));
                    }
                }
                Err(e) => eprintln!("[js] prelude: {e}"),
            }
            self.call("start", &[js_str(&self.url)]);
            if self.repeatable {
                self.eval(FIXED_RANDOM, "fixed random");
            }
            if self.frames {
                self.call("frames", &[]);
            }
            if self.hidden {
                self.call("visibility", &[JsValue::from(true), JsValue::from(true)]);
            }
        }
    }

    /// True once the page's first script has started the script engine.
    pub fn booted(&self) -> bool {
        self.booted
    }

    /// From now on animation frame callbacks wait for `frame`, which the window calls
    /// once per display refresh. Before the engine starts this needs no `enter`.
    pub fn frames_from_host(&mut self) {
        self.frames = true;
        if self.booted {
            self.call("frames", &[]);
        }
    }

    /// Runs the animation frame callbacks queued before this frame, each followed by
    /// its microtasks. True when callbacks are waiting for the next frame.
    pub fn frame(&mut self) -> bool {
        let mut left = self.call("frameStart", &[]).and_then(|v| v.as_number()).unwrap_or(0.0);
        while left > 0.0 {
            left = self.call("frameRun", &[]).and_then(|v| v.as_number()).unwrap_or(0.0);
        }
        self.frames_waiting()
    }

    /// True when animation frame callbacks are waiting for a frame.
    pub fn frames_waiting(&mut self) -> bool {
        self.booted && self.call("framesQueued", &[]).and_then(|v| v.as_number()).is_some_and(|n| n > 0.0)
    }

    /// Shows or hides the page: `document.visibilityState`, and a `visibilitychange`
    /// event once the engine runs. Before it starts this needs no `enter`.
    pub fn set_hidden(&mut self, hidden: bool) {
        if self.hidden != hidden {
            self.hidden = hidden;
            if self.booted {
                self.call("visibility", &[JsValue::from(hidden)]);
            }
        }
    }

    pub fn leave(&mut self) -> Host {
        HOST.with(|h| h.borrow_mut().take()).expect("leave without enter")
    }

    /// Calls a prelude entry point, then lets the microtasks run.
    fn call(&mut self, name: &str, args: &[JsValue]) -> Option<JsValue> {
        let out = match call_entry(&mut self.ctx, name, args) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("[js] {name}: {}", e.to_string().chars().take(300).collect::<String>());
                None
            }
        };
        self.run_jobs(name);
        out
    }

    /// Runs the pending promise jobs, then reports the rejections nothing handled.
    fn run_jobs(&mut self, name: &str) {
        if let Err(e) = self.ctx.run_jobs() {
            eprintln!("[js] {name}: {}", e.to_string().chars().take(300).collect::<String>());
        }
        let rejected = std::mem::take(&mut *self.rejected.borrow_mut());
        for promise in rejected {
            let Ok(p) = JsPromise::from_object(promise.clone()) else { continue };
            let PromiseState::Rejected(reason) = p.state() else { continue };
            let handled = call_entry(&mut self.ctx, "rejected", &[promise.into(), reason.clone()]).is_ok_and(|v| v.to_boolean());
            if !handled {
                let shown = JsError::from_opaque(reason).to_string();
                eprintln!("[js] uncaught (in promise): {}", shown.chars().take(300).collect::<String>());
            }
            if let Err(e) = self.ctx.run_jobs() {
                eprintln!("[js] {name}: {e}");
            }
        }
    }

    /// Runs a script and then the pending promise jobs. Errors are reported
    /// on stderr and never abort the page.
    pub fn eval(&mut self, code: &str, name: &str) -> Option<JsValue> {
        let out = match self.ctx.eval(Source::from_bytes(code)) {
            Ok(v) => Some(v),
            Err(e) => {
                let url = self.url.clone();
                if let Err(e) = report(&mut self.ctx, e, name, Some(&url)) {
                    eprintln!("[js] {name}: {e}");
                }
                None
            }
        };
        self.run_jobs(name);
        out
    }

    /// Runs one of the page's classic scripts as `document.currentScript`;
    /// `url` is where errors in it are reported.
    pub fn run_script(&mut self, node: NodeId, code: &str, name: &str, url: &str) {
        self.call("begin", &[js_id(node)]);
        if let Err(e) = self.ctx.eval(Source::from_bytes(code).with_path(Path::new(url))) {
            if let Err(e) = report(&mut self.ctx, e, name, Some(url)) {
                eprintln!("[js] {name}: {e}");
            }
        }
        call_entry(&mut self.ctx, "end", &[]).ok();
        self.run_jobs(name);
    }

    /// Runs a `<script type="module">`: its code from the page or from `href`.
    pub fn run_module(&mut self, code: Option<&str>, name: &str, href: &str) {
        let Some(modules) = self.ctx.get_data::<Rc<Modules>>().cloned() else { return };
        let parsed = match code {
            Some(code) => modules.parse_inline(code, href, &mut self.ctx),
            None => modules.module(href, &mut self.ctx),
        };
        let module = match parsed {
            Ok(m) => m,
            Err(e) => {
                if let Err(e) = report(&mut self.ctx, e, name, Some(href)) {
                    eprintln!("[js] {name}: {e}");
                }
                return;
            }
        };
        let promise = module.load_link_evaluate(&mut self.ctx);
        if let Err(e) = self.ctx.run_jobs() {
            eprintln!("[js] {name}: {}", e.to_string().chars().take(300).collect::<String>());
        }
        if let PromiseState::Rejected(e) = promise.state() {
            let p: JsObject = promise.into();
            self.rejected.borrow_mut().retain(|q| !JsObject::equals(q, &p));
            eprintln!("[js] {name}: {}", e.display().to_string().chars().take(300).collect::<String>());
        }
        self.run_jobs(name);
    }

    /// Fires the document's load events.
    pub fn load(&mut self) {
        self.call("load", &[]);
    }

    /// Dispatches a bubbling event; true when a listener prevented the default.
    pub fn dispatch(&mut self, node: NodeId, event: &str) -> bool {
        self.call("dispatch", &[js_id(node), js_str(event)]).is_some_and(|v| v.to_boolean())
    }

    /// Moves the script clock (`performance.now`, timer and frame times) `ms`
    /// ahead, as if that time had passed with nothing to do.
    pub fn skip(&mut self, ms: f64) {
        SKIPPED.set(SKIPPED.get() + ms);
    }

    /// Runs due tasks, each followed by its microtasks; returns milliseconds
    /// until the next task, or `None` when none is waiting.
    pub fn timers(&mut self) -> Option<f64> {
        loop {
            self.answers();
            let next = self.call("tick", &[])?.as_number()?;
            if next != 0.0 {
                return (next > 0.0).then_some(next);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{base64_decode, base64_encode};
    use crate::font::Fonts;
    use crate::page::Page;

    fn text(page: &Page, id: &str) -> String {
        let mut out = String::new();
        page.dom.text_content(page.dom.by_id(0, id), &mut out);
        out
    }

    #[test]
    fn in_a_window_animation_frames_wait_for_the_host_and_share_one_time() {
        // Loaded as the window loads a page: its scripts wait until it has been shown.
        let file = std::env::temp_dir().join(format!("js-frames-test-{}.html", std::process::id()));
        std::fs::write(
            &file,
            "<p id=out></p><script>
               const out = document.getElementById('out'), seen = [];
               const note = (s) => { seen.push(s); out.textContent = seen.join(' '); };
               let first;
               requestAnimationFrame((t) => { first = t; note('a'); requestAnimationFrame(() => note('c')); });
               requestAnimationFrame((t) => note(t === first ? 'b' : 'b-at-another-time'));
               cancelAnimationFrame(requestAnimationFrame(() => note('cancelled')));
             </script>",
        )
        .expect("write the test page");
        let page = Page::load_deferred(&file);
        std::fs::remove_file(&file).expect("remove the test page");
        let (mut page, mut fonts) = (page.expect("load the test page"), Fonts::new());
        page.frames_from_host(&mut fonts);
        while page.run_deferred(&mut fonts).is_some() {}
        assert!(page.wants_frame(&mut fonts));
        assert_eq!(page.tick(&mut fonts), None, "frame callbacks are no timers");
        assert_eq!(text(&page, "out"), "");
        assert!(page.frame(&mut fonts), "the callback requested during the frame waits for the next");
        assert_eq!(text(&page, "out"), "a b");
        assert!(!page.frame(&mut fonts));
        assert_eq!(text(&page, "out"), "a b c");
        assert!(!page.wants_frame(&mut fonts));
    }

    #[test]
    fn hiding_the_window_changes_visibility_state_and_fires_visibilitychange_once() {
        let mut fonts = Fonts::new();
        let mut page = Page::from_html(
            "<p id=out></p><script>
               document.addEventListener('visibilitychange', () => {
                 document.getElementById('out').textContent += document.visibilityState + ':' + document.hidden + ' ';
               });
             </script>",
            None,
        );
        page.run_pending(&mut fonts, 800.0, 600.0);
        page.set_hidden(&mut fonts, true);
        page.set_hidden(&mut fonts, true);
        page.set_hidden(&mut fonts, false);
        assert_eq!(text(&page, "out"), "hidden:true visible:false ");
    }

    #[test]
    fn base64_round_trips_like_atob_and_btoa() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(&[0xff, 0xfe]), "//4=");
        assert_eq!(base64_decode("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64_decode(" aGVs bG8 ").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64_decode("aGVsbG8").as_deref(), Some(&b"hello"[..]));
        assert!(base64_decode("a").is_none());
        assert!(base64_decode("a$==").is_none());
    }
}
