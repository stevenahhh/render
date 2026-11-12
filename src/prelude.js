// DOM and window objects for page scripts. Every node is addressed by its
// arena index; the `__*` functions are native and implemented in js.rs.
(function (g) {
  'use strict';
  const nodes = new Map();
  const wrap = (id) => {
    if (id < 0) return null;
    let n = nodes.get(id);
    if (!n) {
      const t = __tag(id);
      n = t === '#text' ? new Text(id) : t === '#document' ? new Document(id) : t === 'video' || t === 'audio' ? new HTMLMediaElement(id, t) : new Element(id, t);
      nodes.set(id, n);
    }
    return n;
  };
  const kebab = (s) => s.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase());
  const idOf = (n) => (n ? n._id : -1);

  class Event {
    constructor(type, init) {
      this.type = type;
      this.bubbles = !!(init && init.bubbles);
      this.cancelable = !!(init && init.cancelable);
      this.detail = init ? init.detail : undefined;
      this.defaultPrevented = false;
      this.target = null;
      this.currentTarget = null;
      this.timeStamp = __now();
    }
    preventDefault() { this.defaultPrevented = true; }
    stopPropagation() { this._stop = true; }
    stopImmediatePropagation() { this._stop = true; this._halt = true; }
    initEvent(type, bubbles, cancelable) { this.type = type; this.bubbles = !!bubbles; this.cancelable = !!cancelable; }
    composedPath() { return []; }
  }
  class CustomEvent extends Event {}

  const call = (node, ev) => {
    ev.currentTarget = node;
    const list = ((node._l && node._l[ev.type]) || []).slice();
    const on = node['on' + ev.type];
    if (typeof on === 'function') {
      list.unshift(on);
    } else if (node.getAttribute) {
      const src = node.getAttribute('on' + ev.type);
      if (src) list.unshift(new Function('event', src));
    }
    for (const fn of list) {
      try { fn.call(node, ev); } catch (e) { console.error(e); }
      if (ev._halt) break;
    }
  };
  const fire = (target, ev) => {
    let n = target;
    while (n) {
      call(n, ev);
      if (ev._stop || !ev.bubbles || n === g) return;
      n = n.parentNode || (n === document ? g : null);
    }
  };

  class EventTarget {
    addEventListener(type, fn) {
      if (fn && typeof fn !== 'function' && typeof fn.handleEvent === 'function') {
        const o = fn;
        fn = (e) => o.handleEvent(e);
      }
      if (typeof fn !== 'function') return;
      const l = this._l || (this._l = {});
      (l[type] || (l[type] = [])).push(fn);
    }
    removeEventListener(type, fn) {
      const a = this._l && this._l[type];
      const i = a ? a.indexOf(fn) : -1;
      if (i >= 0) a.splice(i, 1);
    }
    dispatchEvent(ev) {
      ev.target = this;
      fire(this, ev);
      return !ev.defaultPrevented;
    }
  }

  const toNode = (x) => (x instanceof Node ? x : document.createTextNode(String(x)));

  class Node extends EventTarget {
    constructor(id) { super(); this._id = id; }
    get parentNode() { return wrap(__link(this._id, 0)); }
    get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
    get firstChild() { return wrap(__link(this._id, 1)); }
    get lastChild() { return wrap(__link(this._id, 2)); }
    get previousSibling() { return wrap(__link(this._id, 3)); }
    get nextSibling() { return wrap(__link(this._id, 4)); }
    get childNodes() {
      const out = [];
      for (let c = __link(this._id, 1); c >= 0; c = __link(c, 4)) out.push(wrap(c));
      return out;
    }
    get ownerDocument() { return document; }
    get isConnected() { return document.contains(this); }
    get textContent() { return __content(this._id); }
    set textContent(v) {
      for (const c of this.childNodes) __remove(c._id);
      const s = v == null ? '' : String(v);
      if (s) __insert(this._id, __create(null, s), -1);
    }
    hasChildNodes() { return __link(this._id, 1) >= 0; }
    contains(o) {
      for (let n = o; n; n = n.parentNode) if (n === this) return true;
      return false;
    }
    getRootNode() { let n = this; while (n.parentNode) n = n.parentNode; return n; }
    appendChild(c) { return this.insertBefore(c, null); }
    insertBefore(c, ref) {
      const kids = c._frag ? c.childNodes : [c];
      for (const k of kids) __insert(this._id, k._id, idOf(ref));
      return c;
    }
    removeChild(c) { __remove(c._id); return c; }
    replaceChild(n, old) { this.insertBefore(n, old); __remove(old._id); return old; }
    remove() { __remove(this._id); }
    append(...a) { for (const x of a) this.appendChild(toNode(x)); }
    prepend(...a) { const f = this.firstChild; for (const x of a) this.insertBefore(toNode(x), f); }
    before(...a) { const p = this.parentNode; if (p) for (const x of a) p.insertBefore(toNode(x), this); }
    after(...a) {
      const p = this.parentNode;
      const next = this.nextSibling;
      if (p) for (const x of a) p.insertBefore(toNode(x), next);
    }
    replaceWith(...a) { this.before(...a); this.remove(); }
    compareDocumentPosition() { return 0; }
  }
  Object.assign(Node, { ELEMENT_NODE: 1, TEXT_NODE: 3, DOCUMENT_NODE: 9, DOCUMENT_FRAGMENT_NODE: 11 });

  class Text extends Node {
    get nodeType() { return 3; }
    get nodeName() { return '#text'; }
    get data() { return __text(this._id); }
    set data(v) { __set_text(this._id, String(v)); }
    get nodeValue() { return this.data; }
    set nodeValue(v) { this.data = v; }
    get textContent() { return this.data; }
    set textContent(v) { this.data = v; }
    get length() { return this.data.length; }
    cloneNode() { return document.createTextNode(this.data); }
  }

  class ClassList {
    constructor(el) { this._el = el; }
    _get() { return this._el.className.split(/\s+/).filter(Boolean); }
    _set(a) { this._el.className = a.join(' '); }
    get length() { return this._get().length; }
    get value() { return this._el.className; }
    item(i) { return this._get()[i] || null; }
    contains(c) { return this._get().includes(c); }
    add(...c) { const a = this._get(); for (const x of c) if (!a.includes(x)) a.push(x); this._set(a); }
    remove(...c) { this._set(this._get().filter((x) => !c.includes(x))); }
    toggle(c, force) {
      const has = this.contains(c);
      const on = force === undefined ? !has : !!force;
      if (on && !has) this.add(c);
      if (!on && has) this.remove(c);
      return on;
    }
    replace(a, b) { if (this.contains(a)) { this.remove(a); this.add(b); } }
    forEach(f) { this._get().forEach(f); }
    toString() { return this._el.className; }
    [Symbol.iterator]() { return this._get()[Symbol.iterator](); }
  }

  const parseStyle = (el) => {
    const m = new Map();
    for (const d of (el.getAttribute('style') || '').split(';')) {
      const i = d.indexOf(':');
      if (i > 0) m.set(d.slice(0, i).trim(), d.slice(i + 1).trim());
    }
    return m;
  };
  const writeStyle = (el, m) => el.setAttribute('style', [...m].map(([k, v]) => k + ': ' + v).join('; '));
  const styleProxy = (el) => {
    const api = {
      setProperty(k, v) {
        const m = parseStyle(el);
        if (v == null || v === '') m.delete(k); else m.set(k, String(v));
        writeStyle(el, m);
      },
      getPropertyValue(k) { return parseStyle(el).get(k) || ''; },
      removeProperty(k) {
        const m = parseStyle(el);
        const old = m.get(k) || '';
        m.delete(k);
        writeStyle(el, m);
        return old;
      },
    };
    return new Proxy(api, {
      get(t, k) {
        if (typeof k !== 'string') return undefined;
        if (k in t) return t[k];
        if (k === 'cssText') return el.getAttribute('style') || '';
        return t.getPropertyValue(kebab(k));
      },
      set(t, k, v) {
        if (k === 'cssText') el.setAttribute('style', String(v)); else t.setProperty(kebab(String(k)), v);
        return true;
      },
    });
  };
  const datasetProxy = (el) => new Proxy({}, {
    get(_, k) { return typeof k === 'string' ? (el.getAttribute('data-' + kebab(k)) ?? undefined) : undefined; },
    set(_, k, v) { el.setAttribute('data-' + kebab(String(k)), v); return true; },
    has(_, k) { return typeof k === 'string' && el.hasAttribute('data-' + kebab(k)); },
    deleteProperty(_, k) { el.removeAttribute('data-' + kebab(String(k))); return true; },
  });

  class Element extends Node {
    constructor(id, tag) { super(id); this._tag = tag; }
    get nodeType() { return this._frag ? 11 : 1; }
    get tagName() { return this._tag.toUpperCase(); }
    get nodeName() { return this.tagName; }
    get localName() { return this._tag; }
    getAttribute(n) { return __attr(this._id, String(n)); }
    setAttribute(n, v) { __set_attr(this._id, String(n), String(v)); }
    removeAttribute(n) { __set_attr(this._id, String(n), null); }
    hasAttribute(n) { return __attr(this._id, String(n)) !== null; }
    toggleAttribute(n, force) {
      const on = force === undefined ? !this.hasAttribute(n) : !!force;
      if (on) this.setAttribute(n, ''); else this.removeAttribute(n);
      return on;
    }
    getAttributeNames() { return this.attributes.map((a) => a.name); }
    get attributes() {
      const a = __attrs(this._id);
      const out = [];
      for (let i = 0; i < a.length; i += 2) out.push({ name: a[i], value: a[i + 1] });
      return out;
    }
    get className() { return this.getAttribute('class') || ''; }
    set className(v) { this.setAttribute('class', v); }
    get classList() { return this._cl || (this._cl = new ClassList(this)); }
    get style() { return this._st || (this._st = styleProxy(this)); }
    set style(v) { this.setAttribute('style', v); }
    get dataset() { return this._ds || (this._ds = datasetProxy(this)); }
    get innerHTML() { return __html(this._id, false); }
    set innerHTML(v) { __set_html(this._id, String(v)); }
    get outerHTML() { return __html(this._id, true); }
    get innerText() { return this.textContent; }
    set innerText(v) { this.textContent = v; }
    get children() { return this.childNodes.filter((n) => n.nodeType === 1); }
    get childElementCount() { return this.children.length; }
    get firstElementChild() { return this.children[0] || null; }
    get lastElementChild() { const c = this.children; return c[c.length - 1] || null; }
    get nextElementSibling() { let n = this.nextSibling; while (n && n.nodeType !== 1) n = n.nextSibling; return n; }
    get previousElementSibling() { let n = this.previousSibling; while (n && n.nodeType !== 1) n = n.previousSibling; return n; }
    querySelector(s) { const r = __query(this._id, String(s), true); return r.length ? wrap(r[0]) : null; }
    querySelectorAll(s) { return __query(this._id, String(s), false).map(wrap); }
    getElementsByTagName(t) { return this.querySelectorAll(t); }
    getElementsByClassName(c) { return this.querySelectorAll(String(c).trim().split(/\s+/).map((x) => '.' + x).join('')); }
    matches(s) { return __matches(this._id, String(s)); }
    closest(s) {
      for (let n = this; n && n.nodeType === 1; n = n.parentNode) if (n.matches(s)) return n;
      return null;
    }
    cloneNode(deep) {
      const c = document.createElement(this._tag);
      for (const a of this.attributes) c.setAttribute(a.name, a.value);
      if (deep) for (const k of this.childNodes) c.appendChild(k.cloneNode(true));
      return c;
    }
    _adjacent(pos, kids) {
      switch (String(pos).toLowerCase()) {
        case 'beforebegin': this.before(...kids); break;
        case 'afterbegin': this.prepend(...kids); break;
        case 'beforeend': this.append(...kids); break;
        case 'afterend': this.after(...kids); break;
      }
    }
    insertAdjacentHTML(pos, html) {
      const t = document.createElement('div');
      t.innerHTML = html;
      this._adjacent(pos, t.childNodes);
    }
    insertAdjacentElement(pos, el) { this._adjacent(pos, [el]); return el; }
    insertAdjacentText(pos, s) { this._adjacent(pos, [String(s)]); }
    getBoundingClientRect() {
      const r = __rect(this._id);
      const y = r[1] - __view()[2];
      return { x: r[0], y, left: r[0], top: y, width: r[2], height: r[3], right: r[0] + r[2], bottom: y + r[3] };
    }
    getClientRects() { return [this.getBoundingClientRect()]; }
    get offsetWidth() { return Math.round(__rect(this._id)[2]); }
    get offsetHeight() { return Math.round(__rect(this._id)[3]); }
    get offsetLeft() { return Math.round(__rect(this._id)[0]); }
    get offsetTop() { return Math.round(__rect(this._id)[1]); }
    get clientWidth() { return this.offsetWidth; }
    get clientHeight() { return this.offsetHeight; }
    get scrollWidth() { return this.offsetWidth; }
    get scrollHeight() { return this.offsetHeight; }
    get scrollTop() { return 0; }
    set scrollTop(v) {}
    get scrollLeft() { return 0; }
    set scrollLeft(v) {}
    get offsetParent() { return this.parentElement; }
    focus() {}
    blur() {}
    scrollIntoView() {}
    click() { this.dispatchEvent(new Event('click', { bubbles: true, cancelable: true })); }
    animate() { return { cancel() {}, play() {}, pause() {}, finished: Promise.resolve() }; }
    attachShadow() { return document.createDocumentFragment(); }
    getContext() { return null; }
    // Media cannot play here, which is what a browser reports when autoplay is blocked.
    play() {
      const e = new Error('playback is not supported');
      e.name = 'NotAllowedError';
      return new Promise((_, reject) => setTimeout(() => reject(e), 0));
    }
    pause() {}
    load() {}
    canPlayType() { return ''; }
  }
  const MEDIA_STATES = { HAVE_NOTHING: 0, HAVE_METADATA: 1, HAVE_CURRENT_DATA: 2, HAVE_FUTURE_DATA: 3, HAVE_ENOUGH_DATA: 4, NETWORK_EMPTY: 0, NETWORK_IDLE: 1, NETWORK_LOADING: 2, NETWORK_NO_SOURCE: 3 };
  // A <video> or <audio> that never loads: nothing buffered, paused at the start.
  class HTMLMediaElement extends Element {
    get readyState() { return 0; }
    get networkState() { return 0; }
    get paused() { return true; }
    get ended() { return false; }
    get currentTime() { return 0; }
    set currentTime(v) {}
    get duration() { return NaN; }
    get error() { return null; }
    get poster() { return this.getAttribute('poster') || ''; }
    get currentSrc() { return this.getAttribute('src') || ''; }
  }
  Object.assign(HTMLMediaElement, MEDIA_STATES);
  Object.assign(HTMLMediaElement.prototype, MEDIA_STATES);
  class MediaError {
    constructor(code) { this.code = code; this.message = ''; }
  }
  Object.assign(MediaError, { MEDIA_ERR_ABORTED: 1, MEDIA_ERR_NETWORK: 2, MEDIA_ERR_DECODE: 3, MEDIA_ERR_SRC_NOT_SUPPORTED: 4 });

  for (const k of ['id', 'title', 'href', 'src', 'alt', 'name', 'type', 'value', 'placeholder', 'rel', 'lang', 'dir', 'role', 'target', 'media', 'srcset']) {
    Object.defineProperty(Element.prototype, k, {
      configurable: true,
      get() { return this.getAttribute(k) || ''; },
      set(v) { this.setAttribute(k, v); },
    });
  }
  for (const k of ['hidden', 'disabled', 'checked', 'selected', 'required', 'open']) {
    Object.defineProperty(Element.prototype, k, {
      configurable: true,
      get() { return this.hasAttribute(k); },
      set(v) { this.toggleAttribute(k, !!v); },
    });
  }

  class Document extends Node {
    get nodeType() { return 9; }
    get nodeName() { return '#document'; }
    get documentElement() { return this.querySelector('html'); }
    get head() { return this.querySelector('head'); }
    get body() { return this.querySelector('body'); }
    get children() { return this.childNodes.filter((n) => n.nodeType === 1); }
    getElementById(id) { return this.querySelector('[id="' + String(id) + '"]'); }
    getElementsByName(n) { return this.querySelectorAll('[name="' + String(n) + '"]'); }
    createElement(t) { return wrap(__create(String(t), '')); }
    createElementNS(ns, t) { return this.createElement(t); }
    createTextNode(s) { return wrap(__create(null, String(s))); }
    createComment() { return this.createTextNode(''); }
    createDocumentFragment() { const f = this.createElement('#fragment'); f._frag = true; return f; }
    createEvent() { return new Event(''); }
    get title() { const t = this.querySelector('title'); return t ? t.textContent : ''; }
    set title(v) { const t = this.querySelector('title'); if (t) t.textContent = v; }
    get readyState() { return g.__ready || 'loading'; }
    get cookie() { return ''; }
    set cookie(v) {}
    get location() { return g.location; }
    get defaultView() { return g; }
    get currentScript() { return null; }
    get visibilityState() { return 'visible'; }
    get hidden() { return false; }
    get referrer() { return ''; }
    get activeElement() { return this.body; }
    get scrollingElement() { return this.documentElement; }
    get fonts() { return { ready: Promise.resolve(), load() { return Promise.resolve([]); } }; }
    get styleSheets() { return []; }
    get scripts() { return this.querySelectorAll('script'); }
    get images() { return this.querySelectorAll('img'); }
    get links() { return this.querySelectorAll('a[href]'); }
    get forms() { return this.querySelectorAll('form'); }
    hasFocus() { return true; }
    write() {}
  }
  for (const k of ['querySelector', 'querySelectorAll', 'getElementsByTagName', 'getElementsByClassName']) {
    Document.prototype[k] = Element.prototype[k];
  }

  const document = wrap(0);

  const timers = [];
  let nextTimer = 1;
  const addTimer = (fn, ms, every, args, raf) => {
    if (typeof fn === 'string') fn = new Function(fn);
    if (typeof fn !== 'function') return 0;
    const delay = Math.max(0, +ms || 0);
    timers.push({ id: nextTimer, fn, at: __now() + delay, every: every ? Math.max(4, delay) : 0, args, raf });
    return nextTimer++;
  };
  const clearTimer = (id) => {
    const i = timers.findIndex((t) => t.id === id);
    if (i >= 0) timers.splice(i, 1);
  };
  // Runs every timer that is due and returns the delay until the next one, or -1.
  g.__timers = () => {
    const now = __now();
    const due = timers.filter((t) => t.at <= now).sort((a, b) => a.at - b.at);
    for (const t of due) {
      if (!timers.includes(t)) continue;
      if (t.every) t.at = now + t.every; else clearTimer(t.id);
      try { t.fn.apply(g, t.raf ? [now] : t.args); } catch (e) { console.error(e); }
    }
    let next = -1;
    for (const t of timers) {
      const d = Math.max(0, t.at - __now());
      if (next < 0 || d < next) next = d;
    }
    return next;
  };
  // Dispatches a bubbling event from native code; returns true when a listener cancelled it.
  g.__dispatch = (id, type) => {
    const ev = new Event(type, { bubbles: true, cancelable: true });
    ev.target = wrap(id);
    fire(ev.target, ev);
    return ev.defaultPrevented;
  };
  g.__load = () => {
    g.__ready = 'interactive';
    document.dispatchEvent(new Event('DOMContentLoaded', { bubbles: true }));
    g.__ready = 'complete';
    document.dispatchEvent(new Event('readystatechange'));
    const ev = new Event('load');
    ev.target = document;
    call(g, ev);
  };

  const fmt = (args) => args.map((x) => {
    if (typeof x === 'string') return x;
    if (x instanceof Error) return x.name + ': ' + x.message;
    try { const s = JSON.stringify(x); return s === undefined ? String(x) : s; } catch (_) { return String(x); }
  }).join(' ');
  const log = (prefix) => (...a) => __log(prefix + fmt(a));
  const noop = () => {};
  g.console = { log: log(''), info: log(''), debug: log(''), warn: log('warning: '), error: log('error: '), trace: noop, group: noop, groupEnd: noop, table: noop, time: noop, timeEnd: noop, assert: noop };

  class Observer { constructor() {} observe() {} unobserve() {} disconnect() {} takeRecords() { return []; } }
  class Storage {
    constructor() { this._m = new Map(); }
    getItem(k) { return this._m.has(String(k)) ? this._m.get(String(k)) : null; }
    setItem(k, v) { this._m.set(String(k), String(v)); }
    removeItem(k) { this._m.delete(String(k)); }
    clear() { this._m.clear(); }
    key(i) { return [...this._m.keys()][i] ?? null; }
    get length() { return this._m.size; }
  }
  class XMLHttpRequest extends EventTarget {
    open() {}
    setRequestHeader() {}
    abort() {}
    getAllResponseHeaders() { return ''; }
    send() {
      this.readyState = 4;
      this.status = 0;
      setTimeout(() => {
        const ev = new Event('error');
        if (typeof this.onerror === 'function') this.onerror(ev);
        this.dispatchEvent(ev);
      }, 0);
    }
  }
  class AbortController {
    constructor() { this.signal = new EventTarget(); this.signal.aborted = false; }
    abort() { this.signal.aborted = true; this.signal.dispatchEvent(new Event('abort')); }
  }
  class DOMMatrix {
    constructor() { Object.assign(this, { a: 1, b: 0, c: 0, d: 1, e: 0, f: 0, m11: 1, m22: 1, m33: 1, m41: 0, m42: 0, m43: 0 }); }
  }
  class URLSearchParams {
    constructor(s) {
      this._m = new Map();
      for (const p of String(s || '').replace(/^\?/, '').split('&')) {
        if (!p) continue;
        const i = p.indexOf('=');
        this._m.set(decodeURIComponent(i < 0 ? p : p.slice(0, i)), decodeURIComponent(i < 0 ? '' : p.slice(i + 1)));
      }
    }
    get(k) { return this._m.has(k) ? this._m.get(k) : null; }
    has(k) { return this._m.has(k); }
    set(k, v) { this._m.set(k, String(v)); }
    append(k, v) { this.set(k, v); }
    delete(k) { this._m.delete(k); }
    forEach(f) { this._m.forEach((v, k) => f(v, k)); }
    toString() { return [...this._m].map(([k, v]) => encodeURIComponent(k) + '=' + encodeURIComponent(v)).join('&'); }
  }
  class URL {
    constructor(u) {
      this.href = String(u);
      const m = /^([a-z][a-z0-9+.-]*:)?(\/\/([^\/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(this.href) || [];
      this.protocol = m[1] || 'file:';
      this.host = this.hostname = m[3] || '';
      this.pathname = m[4] || '/';
      this.search = m[5] || '';
      this.hash = m[6] || '';
      this.origin = this.protocol + '//' + this.host;
      this.searchParams = new URLSearchParams(this.search);
    }
    toString() { return this.href; }
  }
  const win = EventTarget.prototype;
  Object.assign(g, {
    window: g, self: g, top: g, parent: g, document,
    Event, CustomEvent, EventTarget, Node, Element, Text, Document, XMLHttpRequest, Storage,
    AbortController, DOMMatrix, DOMMatrixReadOnly: DOMMatrix, WebKitCSSMatrix: DOMMatrix, URL, URLSearchParams,
    HTMLElement: Element, HTMLDivElement: Element, HTMLAnchorElement: Element, HTMLImageElement: Element,
    HTMLInputElement: Element, HTMLButtonElement: Element, HTMLCanvasElement: Element, HTMLVideoElement: HTMLMediaElement,
    HTMLAudioElement: HTMLMediaElement, HTMLMediaElement, MediaError, SVGElement: Element, DocumentFragment: Element, ShadowRoot: Element,
    NodeList: Array, HTMLCollection: Array, DOMTokenList: ClassList,
    MutationObserver: Observer, IntersectionObserver: Observer, ResizeObserver: Observer, PerformanceObserver: Observer,
    addEventListener: win.addEventListener.bind(g),
    removeEventListener: win.removeEventListener.bind(g),
    dispatchEvent: (ev) => { ev.target = g; call(g, ev); return !ev.defaultPrevented; },
    setTimeout: (fn, ms, ...args) => addTimer(fn, ms, false, args),
    setInterval: (fn, ms, ...args) => addTimer(fn, ms, true, args),
    clearTimeout: clearTimer,
    clearInterval: clearTimer,
    requestAnimationFrame: (fn) => addTimer(fn, 16, false, [], true),
    cancelAnimationFrame: clearTimer,
    requestIdleCallback: (fn) => addTimer(() => fn({ didTimeout: false, timeRemaining: () => 10 }), 1),
    cancelIdleCallback: clearTimer,
    queueMicrotask: (fn) => { Promise.resolve().then(fn); },
    getComputedStyle: (el) => (el && el.style ? el.style : styleProxy(document.createElement('div'))),
    matchMedia: (q) => ({ matches: __media(String(q)), media: String(q), onchange: null, addListener: noop, removeListener: noop, addEventListener: noop, removeEventListener: noop }),
    fetch: () => Promise.reject(new Error('network access is disabled')),
    Image: function () { return document.createElement('img'); },
    localStorage: new Storage(),
    sessionStorage: new Storage(),
    // Sites branch on the engine name; like every browser, claim to be a known one.
    navigator: { userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15 render/0.1', language: 'en-US', languages: ['en-US'], platform: 'MacIntel', vendor: 'Apple Computer, Inc.', maxTouchPoints: 0, onLine: false, cookieEnabled: false },
    location: { href: 'about:blank', protocol: 'file:', host: '', hostname: '', pathname: '/', search: '', hash: '', origin: 'null', assign: noop, replace: noop, reload: noop, toString() { return this.href; } },
    history: { length: 1, state: null, pushState: noop, replaceState: noop, back: noop, forward: noop, go: noop },
    performance: { now: () => __now(), mark: noop, measure: noop, timing: {}, getEntriesByType: () => [], getEntriesByName: () => [] },
    customElements: { define: noop, get: noop, whenDefined: () => new Promise(noop) },
    CSS: { supports: () => false, escape: (s) => String(s) },
    scrollTo: noop, scroll: noop, scrollBy: noop, alert: log('alert: '), open: () => null, close: noop, focus: noop, blur: noop,
    devicePixelRatio: 1,
  });
  const view = (i) => ({ configurable: true, get: () => __view()[i] });
  Object.defineProperties(g, {
    innerWidth: view(0), innerHeight: view(1), outerWidth: view(0), outerHeight: view(1),
    scrollY: view(2), pageYOffset: view(2),
    scrollX: { configurable: true, get: () => 0 }, pageXOffset: { configurable: true, get: () => 0 },
    screen: { configurable: true, get: () => ({ width: __view()[0], height: __view()[1], availWidth: __view()[0], availHeight: __view()[1] }) },
  });
})(globalThis);
