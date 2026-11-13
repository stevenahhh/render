// DOM and window objects for page scripts. Every node is addressed by its
// arena index; the `__*` functions are native and implemented in js.rs. Rarer
// parts of the platform are in web/*.js and are compiled when a page first uses
// them; `$` holds what they share with this file. The value of this script is
// the set of entry points the host calls.
(function (g) {
  'use strict';
  const ID = Symbol('id');
  const TAG = Symbol('tag');
  const nodes = new Map();
  const geval = eval;
  const $ = { g, ID, TAG, nodes, mo: null, ce: null, version: 0, attrVersion: 0, dataVersion: 0, intersections: null };

  const illegal = () => { throw new TypeError('Illegal constructor'); };
  const def = (o, props) => {
    for (const k of Object.keys(props)) Object.defineProperty(o, k, { configurable: true, writable: true, enumerable: false, value: props[k] });
  };
  const getters = (o, props) => {
    for (const k of Object.keys(props)) {
      const d = props[k];
      Object.defineProperty(o, k, typeof d === 'function' ? { configurable: true, enumerable: true, get: d } : { configurable: true, enumerable: true, get: d[0], set: d[1] });
    }
  };

  // Parts of the platform in web/*.js. A part defines its globals as plain
  // properties, replacing the accessors that loaded it.
  const loaded = new Set();
  const need = (part) => {
    if (loaded.has(part)) return;
    loaded.add(part);
    (0, geval)(__chunk(part))($);
  };
  // Every interface the platform adds names itself through @@toStringTag, as WebIDL
  // has it: `Object.prototype.toString` gives `[object NodeList]`, not `[object Object]`.
  // Globals are defined through `interfaces`, which tags the classes among them.
  const interfaces = (o) => {
    def(g, o);
    for (const k of Object.keys(o)) {
      const C = o[k];
      if (typeof C !== 'function' || C.name !== k || !C.prototype || Object.prototype.hasOwnProperty.call(C.prototype, Symbol.toStringTag)) continue;
      Object.defineProperty(C.prototype, Symbol.toStringTag, { configurable: true, value: k });
    }
  };
  $.interfaces = interfaces;
  const lazy = (part, names) => {
    for (const k of names) {
      Object.defineProperty(g, k, {
        configurable: true,
        get() { need(part); return g[k]; },
        set(v) { Object.defineProperty(g, k, { configurable: true, writable: true, value: v }); },
      });
    }
  };
  const lazyMethods = (proto, part, names) => {
    for (const k of names) def(proto, { [k]: function (...a) { need(part); return this[k](...a); } });
  };
  // Shadow roots the parser declared need the shadow part before scripts first see them.
  const shadowPart = () => { need('shadow'); return true; };
  const slotOf = (n) => ($.assignedSlot || (__shadow(__link(n[ID], 0)) >= 0 && shadowPart()) ? $.assignedSlot(n) : null);
  $.need = need;
  $.def = def;
  $.getters = getters;

  class DOMException extends Error {
    constructor(message = '', name = 'Error') {
      super(message);
      def(this, { name: String(name), message: String(message) });
    }
    get code() { return DOM_CODES[this.name] || 0; }
  }
  const DOM_CODES = { IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4, InvalidCharacterError: 5, NotFoundError: 8, NotSupportedError: 9, InvalidStateError: 11, SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14, InvalidAccessError: 15, SecurityError: 18, NetworkError: 19, AbortError: 20, QuotaExceededError: 22, TimeoutError: 23, DataCloneError: 25 };
  const domError = (name, message) => new DOMException(message, name);
  $.DOMException = DOMException;
  $.domError = domError;

  const fmt = (args) => args.map((x) => {
    if (typeof x === 'string') return x;
    if (x instanceof Error) return x.name + ': ' + x.message;
    try { const s = JSON.stringify(x); return s === undefined ? String(x) : s; } catch (_) { return String(x); }
  }).join(' ');
  const log = (prefix) => (...a) => __log(prefix + fmt(a));
  const noop = () => {};
  // Describing an exception can run the page's getters and toString, which a browser
  // does not run: when they throw too, the report goes on without what they would say.
  const described = (f, otherwise) => {
    try {
      return f();
    } catch (_) {
      return otherwise;
    }
  };
  // An exception from a callback the page gave us: reported like an uncaught one, with where it came from.
  const reportError = (e) => {
    const stack = described(() => (e instanceof Error && typeof e.stack === 'string' ? e.stack.split('\n').filter((l) => /^\s+at /.test(l)).slice(0, 6) : []), []);
    __log('error: ' + described(() => fmt([e]), 'an exception that throws when described') + (stack.length ? '\n' + stack.join('\n') : ''));
    fireError(e);
  };
  $.reportError = reportError;


  const NONE = 0, CAPTURING = 1, AT_TARGET = 2, BUBBLING = 3;
  class Event {
    constructor(type, init) {
      if (arguments.length === 0) throw new TypeError("Failed to construct 'Event': 1 argument required, but only 0 present.");
      this.type = String(type);
      this.bubbles = !!(init && init.bubbles);
      this.cancelable = !!(init && init.cancelable);
      this.composed = !!(init && init.composed);
      this.defaultPrevented = false;
      this.isTrusted = false;
      this.target = null;
      this.currentTarget = null;
      this.eventPhase = NONE;
      this.timeStamp = __now();
      this._path = null;
      this._stop = 0;
    }
    get srcElement() { return this.target; }
    get cancelBubble() { return this._stop > 0; }
    set cancelBubble(v) { if (v) this._stop = Math.max(this._stop, 1); }
    get returnValue() { return !this.defaultPrevented; }
    set returnValue(v) { if (!v) this.preventDefault(); }
    composedPath() { return this._path ? this._path.slice() : []; }
    stopPropagation() { this._stop = Math.max(this._stop, 1); }
    stopImmediatePropagation() { this._stop = 2; }
    preventDefault() { if (this.cancelable && !this._passive) this.defaultPrevented = true; }
    initEvent(type, bubbles, cancelable) {
      if (this._path) return;
      this.type = String(type);
      this.bubbles = !!bubbles;
      this.cancelable = !!cancelable;
      this.defaultPrevented = false;
      this._stop = 0;
    }
  }
  const PHASES = { NONE, CAPTURING_PHASE: CAPTURING, AT_TARGET, BUBBLING_PHASE: BUBBLING };
  Object.assign(Event, PHASES);
  Object.assign(Event.prototype, PHASES);

  const events = { Event };
  // Event interfaces: name, parent and the fields an init dictionary may set, with defaults.
  const EVENT_TYPES = [
    ['CustomEvent', 'Event', { detail: null }],
    ['UIEvent', 'Event', { view: null, detail: 0, which: 0 }],
    ['FocusEvent', 'UIEvent', { relatedTarget: null }],
    ['MouseEvent', 'UIEvent', { screenX: 0, screenY: 0, clientX: 0, clientY: 0, pageX: 0, pageY: 0, offsetX: 0, offsetY: 0, movementX: 0, movementY: 0, ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, button: 0, buttons: 0, relatedTarget: null }],
    ['WheelEvent', 'MouseEvent', { deltaX: 0, deltaY: 0, deltaZ: 0, deltaMode: 0 }],
    ['PointerEvent', 'MouseEvent', { pointerId: 0, width: 1, height: 1, pressure: 0, tangentialPressure: 0, tiltX: 0, tiltY: 0, twist: 0, pointerType: '', isPrimary: false }],
    ['DragEvent', 'MouseEvent', { dataTransfer: null }],
    ['KeyboardEvent', 'UIEvent', { key: '', code: '', location: 0, ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, repeat: false, isComposing: false, charCode: 0, keyCode: 0 }],
    ['InputEvent', 'UIEvent', { data: null, inputType: '', isComposing: false, dataTransfer: null }],
    ['CompositionEvent', 'UIEvent', { data: '' }],
    ['TouchEvent', 'UIEvent', { touches: [], targetTouches: [], changedTouches: [], ctrlKey: false, shiftKey: false, altKey: false, metaKey: false }],
    ['ErrorEvent', 'Event', { message: '', filename: '', lineno: 0, colno: 0, error: undefined }],
    ['PromiseRejectionEvent', 'Event', { promise: null, reason: undefined }],
    ['MessageEvent', 'Event', { data: null, origin: '', lastEventId: '', source: null, ports: [] }],
    ['PopStateEvent', 'Event', { state: null }],
    ['HashChangeEvent', 'Event', { oldURL: '', newURL: '' }],
    ['PageTransitionEvent', 'Event', { persisted: false }],
    ['MediaQueryListEvent', 'Event', { matches: false, media: '' }],
    ['AnimationPlaybackEvent', 'Event', { currentTime: null, timelineTime: null }],
    ['ProgressEvent', 'Event', { lengthComputable: false, loaded: 0, total: 0 }],
    ['AnimationEvent', 'Event', { animationName: '', elapsedTime: 0, pseudoElement: '' }],
    ['TransitionEvent', 'Event', { propertyName: '', elapsedTime: 0, pseudoElement: '' }],
    ['StorageEvent', 'Event', { key: null, oldValue: null, newValue: null, url: '', storageArea: null }],
    ['SubmitEvent', 'Event', { submitter: null }],
    ['ToggleEvent', 'Event', { oldState: '', newState: '' }],
    ['ClipboardEvent', 'Event', { clipboardData: null }],
    ['SecurityPolicyViolationEvent', 'Event', { violatedDirective: '', blockedURI: '' }],
  ];
  for (const [name, parent, fields] of EVENT_TYPES) {
    const keys = Object.keys(fields);
    const C = { [name]: class extends events[parent] {
      constructor(type, init) {
        super(type, init);
        for (const k of keys) this[k] = init && init[k] !== undefined ? init[k] : fields[k];
      }
    } }[name];
    events[name] = C;
  }
  events.CustomEvent.prototype.initCustomEvent = function (type, bubbles, cancelable, detail) { this.initEvent(type, bubbles, cancelable); this.detail = detail; };
  for (const C of [events.MouseEvent, events.KeyboardEvent]) C.prototype.getModifierState = function (k) { return !!this[{ Control: 'ctrlKey', Shift: 'shiftKey', Alt: 'altKey', Meta: 'metaKey' }[k]]; };
  events.MouseEvent.prototype.initMouseEvent = function (type, bubbles, cancelable, view, detail, sx, sy, cx, cy, ctrl, alt, shift, meta, button, related) {
    this.initEvent(type, bubbles, cancelable);
    Object.assign(this, { view, detail, screenX: sx, screenY: sy, clientX: cx, clientY: cy, ctrlKey: ctrl, altKey: alt, shiftKey: shift, metaKey: meta, button, relatedTarget: related });
  };
  events.UIEvent.prototype.initUIEvent = function (type, bubbles, cancelable, view, detail) { this.initEvent(type, bubbles, cancelable); this.view = view; this.detail = detail; };
  events.KeyboardEvent.prototype.initKeyboardEvent = function (type, bubbles, cancelable, view, key) { this.initEvent(type, bubbles, cancelable); this.view = view; this.key = key; };
  Object.assign(events.KeyboardEvent, { DOM_KEY_LOCATION_STANDARD: 0, DOM_KEY_LOCATION_LEFT: 1, DOM_KEY_LOCATION_RIGHT: 2, DOM_KEY_LOCATION_NUMPAD: 3 });
  const EVENT_NAMES = { event: 'Event', events: 'Event', htmlevents: 'Event', svgevents: 'Event', customevent: 'CustomEvent', uievent: 'UIEvent', uievents: 'UIEvent', mouseevent: 'MouseEvent', mouseevents: 'MouseEvent', keyboardevent: 'KeyboardEvent', keyevents: 'KeyboardEvent', focusevent: 'FocusEvent', messageevent: 'MessageEvent', touchevent: 'TouchEvent', wheelevent: 'WheelEvent', dragevent: 'DragEvent', compositionevent: 'CompositionEvent', hashchangeevent: 'HashChangeEvent', storageevent: 'StorageEvent', errorevent: 'ErrorEvent', popstateevent: 'PopStateEvent', textevent: 'CompositionEvent' };

  const LISTENERS = Symbol('listeners');
  // Event types some listener was added for in the capture phase, ever.
  const capturing = new Set();
  // Listener options: a boolean is the capture flag.
  const flags = (o) => (o !== null && typeof o === 'object' ? { capture: !!o.capture, once: !!o.once, passive: !!o.passive, signal: o.signal } : { capture: !!o });

  class EventTarget {
    addEventListener(type, callback, options) {
      const t = this ?? g;
      if (callback === null || callback === undefined) return;
      const o = flags(options);
      if (o.signal && o.signal.aborted) return;
      type = String(type);
      let map = t[LISTENERS];
      if (!map) {
        map = new Map();
        Object.defineProperty(t, LISTENERS, { value: map, configurable: true });
      }
      let list = map.get(type);
      if (!list) map.set(type, list = []);
      for (const l of list) if (l.callback === callback && l.capture === o.capture) return;
      const l = { callback, capture: o.capture, once: o.once, passive: o.passive, removed: false };
      list.push(l);
      if (o.capture) capturing.add(type);
      if (o.signal) o.signal.addEventListener('abort', () => removeListener(t, type, l));
    }
    removeEventListener(type, callback, options) {
      const t = this ?? g;
      const list = t[LISTENERS] && t[LISTENERS].get(String(type));
      if (!list) return;
      const capture = flags(options).capture;
      const l = list.find((l) => l.callback === callback && l.capture === capture);
      if (l) removeListener(t, String(type), l);
    }
    dispatchEvent(event) {
      if (!(event instanceof Event)) throw new TypeError("Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'.");
      if (event._path) throw domError('InvalidStateError', 'The event is already being dispatched.');
      event.isTrusted = false;
      return dispatch(this ?? g, event);
    }
  }
  const removeListener = (t, type, l) => {
    l.removed = true;
    const list = t[LISTENERS].get(type);
    const i = list.indexOf(l);
    if (i >= 0) list.splice(i, 1);
  };

  // The node an event goes to after `n` on its way up.
  const parentFor = (n, ev) => {
    if (n === g) return null;
    if (n === document) return ev.type === 'load' ? null : g;
    if (n instanceof Node) {
      const p = n.parentNode;
      if (p === null && n.nodeType === 11 && n.host) return ev.composed ? n.host : null;
      return p;
    }
    return null;
  };

  // The handler set with `on<type>`: a property, or for elements an attribute.
  const HANDLER = Symbol('handler');
  const handlerOf = (n, type) => {
    const f = n['on' + type];
    if (typeof f === 'function') return f;
    if (f !== undefined) return null;
    return n === g && !WINDOW_HANDLERS.has(type) ? null : attrHandler(n === g ? document.body : n, type);
  };
  // The function an element's `on<type>` attribute compiles to, compiled again when the attribute changes.
  const attrHandler = (host, type) => {
    if (!host || !host[ID] && host[ID] !== 0 || host.nodeType !== 1) return null;
    const src = __attr(host[ID], 'on' + type);
    if (src === null) return null;
    let cache = host[HANDLER];
    if (!cache) Object.defineProperty(host, HANDLER, { value: cache = new Map() });
    let h = cache.get(type);
    if (!h || h.src !== src) {
      try { h = { src, f: new Function('event', src) }; } catch (e) { reportError(e); h = { src, f: null }; }
      cache.set(type, h);
    }
    return h.f;
  };
  const WINDOW_HANDLERS = new Set(['load', 'error', 'resize', 'scroll', 'popstate', 'hashchange', 'message', 'pageshow', 'pagehide', 'beforeunload', 'unload', 'online', 'offline', 'storage']);

  const invoke = (n, ev, capture) => {
    ev.currentTarget = n;
    if (!capture) {
      const h = handlerOf(n, ev.type);
      if (h) {
        let r;
        try {
          r = n === g && ev.type === 'error' && ev instanceof events.ErrorEvent ? h.call(n, ev.message, ev.filename, ev.lineno, ev.colno, ev.error) : h.call(n, ev);
        } catch (e) { reportError(e); }
        if (r === false && ev.type !== 'error' || r === true && ev.type === 'error') ev.preventDefault();
        if (ev._stop === 2) return;
      }
    }
    const list = n[LISTENERS] && n[LISTENERS].get(ev.type);
    if (!list || !list.length) return;
    for (const l of list.slice()) {
      if (l.removed || l.capture !== capture) continue;
      if (l.once) removeListener(n, ev.type, l);
      ev._passive = l.passive;
      try {
        const cb = l.callback;
        if (typeof cb === 'function') cb.call(n, ev);
        else if (cb && typeof cb.handleEvent === 'function') cb.handleEvent(ev);
      } catch (e) { reportError(e); }
      ev._passive = false;
      if (ev._stop === 2) return;
    }
  };

  // Dispatches as the DOM standard does: the path is computed once, then the
  // capture phase runs down to the target and the bubble phase back up.
  const dispatch = (target, ev) => {
    const path = [];
    for (let n = target; n; n = parentFor(n, ev)) path.push(n);
    ev._path = path;
    ev.target = target;
    for (let i = path.length - 1; i >= 0 && !ev._stop; i--) {
      ev.eventPhase = i === 0 ? AT_TARGET : CAPTURING;
      invoke(path[i], ev, true);
    }
    for (let i = 0; i < path.length && !ev._stop; i++) {
      if (i > 0 && !ev.bubbles) break;
      ev.eventPhase = i === 0 ? AT_TARGET : BUBBLING;
      invoke(path[i], ev, false);
    }
    ev.eventPhase = NONE;
    ev.currentTarget = null;
    ev._path = null;
    ev._stop = 0;
    return !ev.defaultPrevented;
  };
  $.dispatch = dispatch;
  const fire = (target, type, init, C = Event) => dispatch(target, new C(type, init));
  $.fire = fire;

  let inError = false;
  const fireError = (e, filename = '') => {
    if (inError) return false;
    inError = true;
    try {
      const message = 'Uncaught ' + described(() => (e instanceof Error ? e.name + ': ' + e.message : String(e)), 'exception');
      return !dispatch(g, new events.ErrorEvent('error', { message, filename, error: e, cancelable: true }));
    } finally {
      inError = false;
    }
  };


  class AbortSignal extends EventTarget {
    constructor() { super(); illegal(); }
    get aborted() { return this._aborted; }
    get reason() { return this._reason; }
    throwIfAborted() { if (this._aborted) throw this._reason; }
    static abort(reason) { const s = newSignal(); signalAbort(s, reason); return s; }
    static timeout(ms) {
      const s = newSignal();
      addTimer(() => signalAbort(s, domError('TimeoutError', 'signal timed out')), ms, false, []);
      return s;
    }
    static any(signals) {
      const s = newSignal();
      for (const x of signals) if (x.aborted) { signalAbort(s, x.reason); return s; }
      for (const x of signals) x.addEventListener('abort', () => signalAbort(s, x.reason));
      return s;
    }
  }
  const newSignal = () => {
    const s = Object.create(AbortSignal.prototype);
    s._aborted = false;
    s._reason = undefined;
    s.onabort = null;
    return s;
  };
  const signalAbort = (s, reason) => {
    if (s._aborted) return;
    s._aborted = true;
    s._reason = reason === undefined ? domError('AbortError', 'signal is aborted without reason') : reason;
    dispatch(s, new Event('abort'));
  };
  class AbortController {
    constructor() { this.signal = newSignal(); }
    abort(reason) { signalAbort(this.signal, reason); }
  }


  const idOf = (n) => (n ? n[ID] : -1);
  const protos = new Map();
  const protoOf = (t) => protos.get(t) || (t.includes('-') ? HTMLElement.prototype : HTMLUnknownElement.prototype);
  const wrap = (id) => {
    if (id < 0) return null;
    let n = nodes.get(id);
    if (n === undefined) {
      const t = __tag(id);
      n = Object.create(protoOf(t));
      n[ID] = id;
      if (t.charCodeAt(0) !== 35) n[TAG] = t;
      nodes.set(id, n);
    }
    return n;
  };
  $.wrap = wrap;
  const toNode = (x) => (x instanceof Node ? x : document.createTextNode(String(x)));

  // Live lists of child nodes and child elements, refreshed when the document changed.
  class NodeList {
    constructor() { illegal(); }
    get length() { return this._sync(); }
    item(i) { this._sync(); return this[i >>> 0] || null; }
    forEach(f, that) { const n = this._sync(); for (let i = 0; i < n; i++) f.call(that, this[i], i, this); }
    entries() { this._sync(); return Array.prototype.slice.call(this, 0, this._n).entries(); }
    keys() { this._sync(); return Array.prototype.slice.call(this, 0, this._n).keys(); }
    values() { this._sync(); return Array.prototype.slice.call(this, 0, this._n).values(); }
    [Symbol.iterator]() { return this.values(); }
    _sync() {
      const v = this._attrs ? $.version + $.attrVersion * 1e9 : $.version;
      if (this._fill && this._v !== v) {
        const ids = this._fill();
        for (let i = 0; i < ids.length; i++) this[i] = wrap(ids[i]);
        for (let i = ids.length; i < this._n; i++) delete this[i];
        this._n = ids.length;
        this._v = v;
      }
      return this._n;
    }
  }
  class HTMLCollection extends NodeList {
    get length() { return this._sync(); }
    item(i) { this._sync(); return this[i >>> 0] || null; }
    namedItem(name) {
      const n = this._sync();
      for (let i = 0; i < n; i++) if (this[i].id === name || this[i].getAttribute('name') === name) return this[i];
      return null;
    }
  }
  delete HTMLCollection.prototype.forEach;
  // A live list; `attrs` when its members depend on attributes as well as on the tree.
  const liveList = (C, fill, attrs) => {
    const l = Object.create(C.prototype);
    l._fill = fill;
    l._attrs = !!attrs;
    l._v = -1;
    l._n = 0;
    l._sync();
    return l;
  };
  const staticList = (items) => {
    const l = Object.create(NodeList.prototype);
    for (let i = 0; i < items.length; i++) l[i] = items[i];
    l._n = items.length;
    l._fill = null;
    return l;
  };
  $.staticList = staticList;
  const KIDS = Symbol('kids');
  const ELEMS = Symbol('elements');

  const DOC = 9, FRAGMENT = 11;
  const KIND = Symbol('nodeType'), NAME = Symbol('nodeName');
  // Nodes made by a document other than the page's, until they join a tree.
  const OWNER = Symbol('owner');

  class Node extends EventTarget {
    constructor() { super(); illegal(); }
    // One accessor on Node.prototype, as in WebIDL, that each kind of node answers.
    get nodeType() { return this[KIND]; }
    get nodeName() { return this[NAME]; }
    get parentNode() { return wrap(__link(this[ID], 0)); }
    get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
    get firstChild() { return wrap(__link(this[ID], 1)); }
    get lastChild() { return wrap(__link(this[ID], 2)); }
    get previousSibling() { return wrap(__link(this[ID], 3)); }
    get nextSibling() { return wrap(__link(this[ID], 4)); }
    get childNodes() {
      let l = this[KIDS];
      if (!l) {
        const id = this[ID];
        Object.defineProperty(this, KIDS, { value: l = liveList(NodeList, () => __kids(id, false)) });
      }
      l._sync();
      return l;
    }
    get ownerDocument() {
      if (this.nodeType === DOC) return null;
      const r = __root(this[ID]);
      if (r === 0) return document;
      const t = __tag(r);
      if (t === '#document') return wrap(r);
      if (t === '#shadow-root') return wrap(__host(r)).ownerDocument;
      if (__host(r) >= 0) return templateDocument();
      return this[OWNER] || document;
    }
    get isConnected() { return __connected(this[ID]); }
    get baseURI() { return baseURL(); }
    get nodeValue() { return null; }
    set nodeValue(v) {}
    get textContent() { return __content(this[ID]); }
    set textContent(v) { replaceAll(this, v === null || v === undefined || v === '' ? null : document.createTextNode(String(v))); }
    hasChildNodes() { return __link(this[ID], 1) >= 0; }
    contains(o) {
      if (!o) return false;
      for (let n = o[ID]; n >= 0; n = __link(n, 0)) if (n === this[ID]) return true;
      return false;
    }
    getRootNode(options) {
      let r = wrap(__root(this[ID]));
      while (options && options.composed && r.nodeType === FRAGMENT && r.host) r = wrap(__root(r.host[ID]));
      return r;
    }
    appendChild(c) { return insert(this, c, null); }
    insertBefore(c, ref) { return insert(this, c, ref === undefined ? null : ref); }
    removeChild(c) {
      if (!c || __link(c[ID], 0) !== this[ID]) throw domError('NotFoundError', "Failed to execute 'removeChild' on 'Node': The node to be removed is not a child of this node.");
      removeNode(c);
      return c;
    }
    replaceChild(n, old) {
      if (!old || __link(old[ID], 0) !== this[ID]) throw domError('NotFoundError', "Failed to execute 'replaceChild' on 'Node': The node to be replaced is not a child of this node.");
      if (n === old) return old;
      const next = old.nextSibling;
      removeNode(old);
      insert(this, n, next === n ? n.nextSibling : next);
      return old;
    }
    cloneNode(deep) { return cloneNode(this, !!deep); }
    isSameNode(o) { return this === o; }
    isEqualNode(o) {
      if (!o || o.nodeType !== this.nodeType) return false;
      if (this.nodeType === 1) return this.outerHTML === o.outerHTML;
      return this.nodeType === 3 || this.nodeType === 8 ? this.data === o.data : this.innerHTML === o.innerHTML;
    }
    compareDocumentPosition(o) {
      if (o === this) return 0;
      const chain = (n) => { const out = []; for (let i = n[ID]; i >= 0; i = __link(i, 0)) out.unshift(i); return out; };
      const a = chain(this), b = chain(o);
      if (a[0] !== b[0]) return 1 | 32 | (this[ID] < o[ID] ? 4 : 2);
      let i = 0;
      while (i < a.length && i < b.length && a[i] === b[i]) i++;
      if (i === a.length) return 16 | 4;
      if (i === b.length) return 8 | 2;
      for (let c = __link(a[i - 1], 1); c >= 0; c = __link(c, 4)) {
        if (c === a[i]) return 4;
        if (c === b[i]) return 2;
      }
      return 0;
    }
    normalize() {
      for (let c = this.firstChild; c;) {
        const next = c.nextSibling;
        if (c.nodeType === 3) {
          if (c.data === '') removeNode(c);
          else while (next && next.nodeType === 3 && c.nextSibling === next) { c.data += next.data; removeNode(next); break; }
        } else if (c.nodeType === 1) c.normalize();
        c = c.nextSibling === next ? next : c.nextSibling;
      }
    }
    lookupNamespaceURI() { return null; }
    lookupPrefix() { return null; }
    isDefaultNamespace(ns) { return ns === null || ns === '' || ns === HTML_NS; }
  }
  Object.assign(Node, { ELEMENT_NODE: 1, ATTRIBUTE_NODE: 2, TEXT_NODE: 3, CDATA_SECTION_NODE: 4, ENTITY_REFERENCE_NODE: 5, ENTITY_NODE: 6, PROCESSING_INSTRUCTION_NODE: 7, COMMENT_NODE: 8, DOCUMENT_NODE: 9, DOCUMENT_TYPE_NODE: 10, DOCUMENT_FRAGMENT_NODE: 11, NOTATION_NODE: 12, DOCUMENT_POSITION_DISCONNECTED: 1, DOCUMENT_POSITION_PRECEDING: 2, DOCUMENT_POSITION_FOLLOWING: 4, DOCUMENT_POSITION_CONTAINS: 8, DOCUMENT_POSITION_CONTAINED_BY: 16, DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC: 32 });
  Object.assign(Node.prototype, Node);

  // Methods of nodes that have children: elements, documents and fragments.
  const ParentNode = {
    get children() {
      let l = this[ELEMS];
      if (!l) {
        const id = this[ID];
        Object.defineProperty(this, ELEMS, { value: l = liveList(HTMLCollection, () => __kids(id, true)) });
      }
      l._sync();
      return l;
    },
    get childElementCount() { return __kids(this[ID], true).length; },
    get firstElementChild() { const k = __kids(this[ID], true); return k.length ? wrap(k[0]) : null; },
    get lastElementChild() { const k = __kids(this[ID], true); return k.length ? wrap(k[k.length - 1]) : null; },
    append(...a) { for (const x of a) insert(this, toNode(x), null); },
    prepend(...a) { const f = this.firstChild; for (const x of a) insert(this, toNode(x), f); },
    replaceChildren(...a) {
      const f = document.createDocumentFragment();
      for (const x of a) insert(f, toNode(x), null);
      replaceAll(this, f);
    },
    querySelector(s) { const r = query(this, s, true); return r.length ? wrap(r[0]) : null; },
    querySelectorAll(s) { return staticList(query(this, s, false).map(wrap)); },
    getElementsByTagName(t) {
      const id = this[ID], sel = String(t) === '*' ? '*' : String(t).toLowerCase();
      return liveList(HTMLCollection, () => __query(id, sel, false), true);
    },
    getElementsByTagNameNS(ns, t) { return this.getElementsByTagName(t); },
    getElementsByClassName(c) {
      const id = this[ID], names = String(c).trim().split(/\s+/).filter(Boolean);
      const sel = names.length ? names.map((x) => '.' + cssEscape(x)).join('') : ':not(*)';
      return liveList(HTMLCollection, () => __query(id, sel, false), true);
    },
  };
  // Methods of nodes that have a parent: elements, character data and doctypes.
  const ChildNode = {
    before(...a) { const p = this.parentNode; if (p) for (const x of a) insert(p, toNode(x), this); },
    after(...a) {
      const p = this.parentNode;
      if (!p) return;
      const next = this.nextSibling;
      for (const x of a) insert(p, toNode(x), next);
    },
    replaceWith(...a) {
      const p = this.parentNode;
      if (!p) return;
      const next = this.nextSibling;
      removeNode(this);
      for (const x of a) insert(p, toNode(x), next && next.parentNode === p ? next : null);
    },
    remove() { if (__link(this[ID], 0) >= 0) removeNode(this); },
    get previousElementSibling() { let n = this.previousSibling; while (n && n.nodeType !== 1) n = n.previousSibling; return n; },
    get nextElementSibling() { let n = this.nextSibling; while (n && n.nodeType !== 1) n = n.nextSibling; return n; },
  };
  const mixin = (C, m) => Object.defineProperties(C.prototype, Object.getOwnPropertyDescriptors(m));

  // `:scope` names the element a query runs on; the selector engine knows it as an attribute.
  const scopeSelector = (s) => s.replace(/:scope/g, '[data-render-scope]');
  const query = (root, s, first) => {
    s = String(s);
    if (!s.includes(':scope')) return __query(root[ID], s, first);
    const out = __scoped(root[ID], root[ID], scopeSelector(s));
    return first ? out.slice(0, 1) : out;
  };
  const childByTag = (el, tag) => {
    if (!el) return null;
    for (const id of __kids(el[ID], true)) if (__tag(id) === tag) return wrap(id);
    return null;
  };


  const watched = () => $.mo !== null || $.ce !== null;
  const SCRIPT = 1, CUSTOM = 2;

  const insert = (parent, node, child) => {
    if (!(node instanceof Node)) throw new TypeError("Failed to execute 'insertBefore' on 'Node': parameter 1 is not of type 'Node'.");
    if (child !== null && __link(child[ID], 0) !== parent[ID]) throw domError('NotFoundError', "Failed to execute 'insertBefore' on 'Node': The node before which the new node is to be inserted is not a child of this node.");
    if (node.nodeType === FRAGMENT) {
      const ids = __kids(node[ID], false);
      if ($.mo && ids.length) $.mo.childList(node, ids.map(wrap), [], null, null, false);
      for (const id of ids) insertOne(parent, id, child, true);
    } else {
      insertOne(parent, node[ID], child, false);
    }
    return node;
  };

  const insertOne = (parent, id, child, fromFragment) => {
    let before = child === null ? -1 : child[ID];
    if (before === id) before = __link(id, 4);
    const old = watched() && !fromFragment ? __link(id, 0) : -1;
    if (old >= 0) beforeRemove(wrap(old), id);
    const prev = $.mo ? (before >= 0 ? __link(before, 3) : __link(parent[ID], 2)) : -1;
    const got = __insert(parent[ID], id, before);
    if (got < 0) throw domError('HierarchyRequestError', "Failed to execute 'insertBefore' on 'Node': The new child element contains the parent.");
    $.version++;
    if ($.mo) $.mo.childList(parent, [wrap(id)], [], wrap(prev), wrap(before), true);
    // A node moved within the document leaves before it arrives.
    if (old >= 0 && $.ce) $.ce.disconnected();
    if (got & CUSTOM && $.ce) $.ce.connected(id);
    if (got & SCRIPT) scriptsInserted(id);
    else if (parent[TAG] === 'script') prepareScript(parent);
    const n = nodes.get(id);
    if (n !== undefined && n[TAG] === 'link') linkInserted(n);
  };

  // Before `id` leaves `parent`: mutation records and custom element reactions.
  const beforeRemove = (parent, id) => {
    if ($.mo) $.mo.childList(parent, [], [wrap(id)], wrap(__link(id, 3)), wrap(__link(id, 4)), true);
    if ($.ce && __connected(id)) $.ce.disconnecting(id);
  };

  const removeNode = (n) => {
    const id = n[ID], p = __link(id, 0);
    if (p < 0) return;
    if (watched()) beforeRemove(wrap(p), id);
    __remove(id);
    $.version++;
    if ($.ce) $.ce.disconnected();
  };

  // Replaces all children of `parent` with `node` (null: none), as one change.
  const replaceAll = (parent, node) => {
    const pid = parent[ID];
    const removed = $.mo ? __kids(pid, false).map(wrap) : null;
    if ($.ce && __connected(pid)) for (const c of __kids(pid, false)) $.ce.disconnecting(c);
    for (const c of __kids(pid, false)) __remove(c);
    if ($.ce) $.ce.disconnected();
    $.version++;
    const added = node ? (node.nodeType === FRAGMENT ? __kids(node[ID], false).map(wrap) : [node]) : [];
    const mo = $.mo;
    $.mo = null;
    try {
      if (node) insert(parent, node, null);
    } finally {
      $.mo = mo;
    }
    if (mo && (removed.length || added.length)) mo.childList(parent, added, removed, null, null, true);
  };

  // Attribute changes from scripts; `value` null removes the attribute.
  const setAttr = (el, name, value) => {
    const id = el[ID];
    if ($.mo || $.ce) {
      const old = __attr(id, name);
      __set_attr(id, name, value);
      if ($.mo) $.mo.attr(el, name.toLowerCase(), old);
      if ($.ce) $.ce.attr(el, name.toLowerCase(), old, value);
    } else {
      __set_attr(id, name, value);
    }
    $.attrVersion++;
    // An element's classList keeps its indexed properties in step with the attribute.
    if (name.length === 5 && el[CLASSES] && name.toLowerCase() === 'class') el[CLASSES]._sync();
    const tag = el[TAG];
    if (tag === 'script' && value !== null && name.toLowerCase() === 'src') prepareScript(el);
    else if (tag === 'link' && name.toLowerCase() === 'href') linkInserted(el);
    else if (tag === 'img' && /^(src|srcset)$/i.test(name)) imageChanged(el);
  };
  $.setAttr = setAttr;

  const setData = (n, value) => {
    $.dataVersion++;
    if ($.mo) {
      const old = __text(n[ID]);
      __set_text(n[ID], value);
      $.mo.data(n, old);
    } else {
      __set_text(n[ID], value);
    }
    const p = __link(n[ID], 0);
    if (p >= 0 && nodes.get(p) && nodes.get(p)[TAG] === 'script') prepareScript(nodes.get(p));
  };

  const cloneNode = (n, deep) => {
    const c = wrap(__clone(n[ID], deep));
    if (n[OWNER]) c[OWNER] = n[OWNER];
    if ($.ce) $.ce.cloned(c[ID]);
    return c;
  };

  // Parses markup the way `innerHTML` does: scripts in it never run.
  // `how`: 1 lets the markup declare shadow roots, 2 parses a whole document.
  const parseInto = (el, html, how = 0) => {
    const id = el[ID];
    const removed = $.mo ? __kids(id, false).map(wrap) : null;
    if ($.ce && __connected(id)) for (const c of __kids(id, false)) $.ce.disconnecting(c);
    const got = __set_html(id, html, how);
    if ($.ce) $.ce.disconnected();
    $.version++;
    if (got & SCRIPT) for (const s of __tagged(id, SCRIPT)) started.add(s);
    if ($.mo) $.mo.childList(el, __kids(id, false).map(wrap), removed, null, null, true);
    if (got & CUSTOM && $.ce) for (const c of __kids(id, false)) $.ce.connected(c);
    return got;
  };
  const fragmentFrom = (html) => {
    const f = document.createDocumentFragment();
    parseInto(f, String(html));
    return f;
  };
  $.fragmentFrom = fragmentFrom;


  const started = new Set();
  let currentScript = null;
  $.setCurrentScript = (s) => { currentScript = s; };
  const SCRIPT_TYPES = /^(|text\/javascript|application\/javascript|text\/ecmascript|application\/ecmascript|module|text\/jscript|text\/livescript|application\/x-javascript|application\/x-ecmascript)$/i;
  const scriptsInserted = (root) => { for (const s of __tagged(root, SCRIPT)) prepareScript(wrap(s)); };
  const prepareScript = (s) => {
    const id = s[ID];
    if (started.has(id) || !__connected(id)) return;
    const type = (__attr(id, 'type') || '').trim();
    if (!SCRIPT_TYPES.test(type)) return;
    const src = __attr(id, 'src');
    const module = type.toLowerCase() === 'module';
    if (src === null) {
      const code = __content(id);
      if (!code) return;
      started.add(id);
      if (module) return __module(code, docURL, 'inline script');
      const prev = currentScript;
      currentScript = s;
      try { __run(code, docURL, 'inline script'); } finally { currentScript = prev; }
      return;
    }
    started.add(id);
    if (__attr(id, 'nomodule') !== null && !module) return;
    const url = resolveURL(src);
    addTimer(() => {
      if (module) {
        __module("import '" + String(url).replace(/[\\']/g, '\\$&') + "';", url, src);
        fire(s, 'load');
        return;
      }
      const got = url === null ? null : __load(url, false);
      if (got === null) return fire(s, 'error');
      const prev = currentScript;
      currentScript = s;
      try { __run(got[2], url, src); } finally { currentScript = prev; }
      fire(s, 'load');
    }, 0, false, []);
  };
  // A stylesheet `<link>` that joined the document fires `load` once its sheet is in.
  const linkInserted = (l) => {
    const id = l[ID];
    if (!__connected(id) || !/(^|\s)stylesheet(\s|$)/i.test(__attr(id, 'rel') || '')) return;
    const href = __attr(id, 'href');
    addTimer(() => {
      const url = href === null ? null : resolveURL(href);
      fire(l, url !== null && __load(url, false) !== null ? 'load' : 'error');
    }, 0, false, []);
  };
  // An <img> that a script gives a source loads it, and a task then fires `load` or
  // `error` at it, as in browsers: pages often show an image only once it loaded.
  // One-shot output waits for the download; the window looks again on each turn.
  const loadingImages = new Map();
  const imageSource = (id) => {
    const src = __attr(id, 'src');
    const set = src ? null : __attr(id, 'srcset');
    return set ? set.split(',')[0].trim().split(/\s+/)[0] : src;
  };
  const imageLoaded = (img, src) => {
    if (imageSource(img[ID]) !== src) return;
    const got = src === '' ? null : /^data:/i.test(src) ? [] : __image(src);
    if (got === 0) loadingImages.set(img, src);
    else fire(img, got === null ? 'error' : 'load');
  };
  const imageChanged = (img) => {
    const src = imageSource(img[ID]);
    if (src === null) return;
    if (src !== '' && !/^data:/i.test(src)) __prefetch(src);
    addTimer(() => imageLoaded(img, src), 0, false, []);
  };


  class CharacterData extends Node {
    constructor() { super(); illegal(); }
    get data() { return __text(this[ID]); }
    set data(v) { setData(this, v === null ? '' : String(v)); }
    get nodeValue() { return this.data; }
    set nodeValue(v) { this.data = v; }
    get textContent() { return this.data; }
    set textContent(v) { this.data = v; }
    get length() { return this.data.length; }
    substringData(o, n) { return this.data.substr(o, n); }
    appendData(s) { this.data += s; }
    insertData(o, s) { const d = this.data; this.data = d.slice(0, o) + s + d.slice(o); }
    deleteData(o, n) { const d = this.data; this.data = d.slice(0, o) + d.slice(o + n); }
    replaceData(o, n, s) { const d = this.data; this.data = d.slice(0, o) + s + d.slice(o + n); }
  }
  mixin(CharacterData, ChildNode);
  class Text extends CharacterData {
    constructor(data = '') { return wrap(__create(null, String(data))); }
    get [NAME]() { return '#text'; }
    get wholeText() { return this.data; }
    get assignedSlot() { return slotOf(this); }
    splitText(offset) {
      const d = this.data, rest = document.createTextNode(d.slice(offset));
      this.data = d.slice(0, offset);
      const p = this.parentNode;
      if (p) insert(p, rest, this.nextSibling);
      return rest;
    }
  }
  class Comment extends CharacterData {
    constructor(data = '') { return wrap(__comment(String(data))); }
    get [NAME]() { return '#comment'; }
  }
  class CDATASection extends Text { constructor() { super(); illegal(); } }
  class ProcessingInstruction extends CharacterData { constructor() { super(); illegal(); } }
  class DocumentType extends Node {
    constructor() { super(); illegal(); }
    get [NAME]() { return this.name; }
  }
  mixin(DocumentType, ChildNode);
  protos.set('#text', Text.prototype);
  protos.set('#comment', Comment.prototype);

  const HTML_NS = 'http://www.w3.org/1999/xhtml', SVG_NS = 'http://www.w3.org/2000/svg';
  const ATTR_NODES = Symbol('attrs');
  const CLASSES = Symbol('classList');

  class Element extends Node {
    constructor() { super(); illegal(); }
    get tagName() { const t = this[TAG]; return this instanceof SVGElement ? t : t.toUpperCase(); }
    get [NAME]() { return this.tagName; }
    get localName() { return this[TAG]; }
    get namespaceURI() { return this instanceof SVGElement ? SVG_NS : HTML_NS; }
    get prefix() { return null; }
    getAttribute(n) { return __attr(this[ID], String(n)); }
    getAttributeNS(ns, n) { return __attr(this[ID], String(n)); }
    setAttribute(n, v) {
      n = String(n);
      if (!/^[^\s"'>/=\u0000-\u001f\u007f]+$/.test(n)) throw domError('InvalidCharacterError', "Failed to execute 'setAttribute' on 'Element': '" + n + "' is not a valid attribute name.");
      setAttr(this, n, String(v));
    }
    setAttributeNS(ns, n, v) { n = String(n); setAttr(this, n.includes(':') && !/^(xlink|xml|xmlns):/.test(n) ? n.slice(n.indexOf(':') + 1) : n, String(v)); }
    removeAttribute(n) { if (__attr(this[ID], String(n)) !== null) setAttr(this, String(n), null); }
    removeAttributeNS(ns, n) { this.removeAttribute(n); }
    hasAttribute(n) { return __attr(this[ID], String(n)) !== null; }
    hasAttributeNS(ns, n) { return this.hasAttribute(n); }
    hasAttributes() { return __attrs(this[ID]).length > 0; }
    toggleAttribute(n, force) {
      const has = this.hasAttribute(n);
      const on = force === undefined ? !has : !!force;
      if (on && !has) this.setAttribute(n, '');
      if (!on && has) this.removeAttribute(n);
      return on;
    }
    getAttributeNames() { const a = __attrs(this[ID]); const out = []; for (let i = 0; i < a.length; i += 2) out.push(a[i]); return out; }
    get attributes() {
      let m = this[ATTR_NODES];
      if (!m) {
        m = Object.create(NamedNodeMap.prototype);
        m._el = this;
        m._v = -1;
        m._n = 0;
        m._cache = new Map();
        m._named = [];
        Object.defineProperty(this, ATTR_NODES, { value: m });
      }
      m._sync();
      return m;
    }
    getAttributeNode(n) { return this.hasAttribute(n) ? this.attributes.getNamedItem(n) : null; }
    getAttributeNodeNS(ns, n) { return this.getAttributeNode(n); }
    setAttributeNode(a) { const old = this.getAttributeNode(a.name); this.setAttribute(a.name, a.value); return old; }
    setAttributeNodeNS(a) { return this.setAttributeNode(a); }
    removeAttributeNode(a) { this.removeAttribute(a.name); return a; }
    get id() { return this.getAttribute('id') || ''; }
    set id(v) { this.setAttribute('id', v); }
    get className() { return this.getAttribute('class') || ''; }
    set className(v) { this.setAttribute('class', v); }
    get classList() {
      let l = this[CLASSES];
      if (!l) Object.defineProperty(this, CLASSES, { value: l = tokenList(this, 'class') });
      return l;
    }
    set classList(v) { this.setAttribute('class', v); }
    get slot() { return this.getAttribute('slot') || ''; }
    set slot(v) { this.setAttribute('slot', v); }
    get role() { return this.getAttribute('role'); }
    set role(v) { if (v === null) this.removeAttribute('role'); else this.setAttribute('role', v); }
    get innerHTML() { return __html(this[TAG] === 'template' ? __template(this[ID]) : this[ID], false); }
    set innerHTML(v) { parseInto(this[TAG] === 'template' ? wrap(__template(this[ID])) : this, v === null ? '' : String(v)); }
    get outerHTML() { return __html(this[ID], true); }
    set outerHTML(v) {
      const p = this.parentNode;
      if (!p) return;
      if (p.nodeType === DOC) throw domError('NoModificationAllowedError', 'This element has no parent node.');
      const f = fragmentFrom(v);
      const next = this.nextSibling;
      removeNode(this);
      insert(p, f, next);
    }
    insertAdjacentHTML(pos, html) { adjacent(this, pos, fragmentFrom(html)); }
    insertAdjacentElement(pos, el) { return adjacent(this, pos, el) ? el : null; }
    insertAdjacentText(pos, s) { adjacent(this, pos, document.createTextNode(String(s))); }
    matches(s) { s = String(s); return s.includes(':scope') ? __scoped(this[ID], __root(this[ID]), scopeSelector(s)).includes(this[ID]) : __matches(this[ID], s); }
    webkitMatchesSelector(s) { return this.matches(s); }
    closest(s) {
      for (let n = this; n && n.nodeType === 1; n = n.parentNode) if (n.matches(s)) return n;
      return null;
    }
    getBoundingClientRect() {
      return rectOf(__rect(this[ID], true));
    }
    getClientRects() { const r = this.getBoundingClientRect(); return r.width || r.height ? [r] : []; }
    get clientWidth() { return Math.round(__rect(this[ID])[2]); }
    get clientHeight() { return Math.round(__rect(this[ID])[3]); }
    get clientTop() { return 0; }
    get clientLeft() { return 0; }
    get scrollWidth() { return this.clientWidth; }
    get scrollHeight() { return this.clientHeight; }
    get scrollTop() { return 0; }
    set scrollTop(v) {}
    get scrollLeft() { return 0; }
    set scrollLeft(v) {}
    scrollIntoView() {}
    scrollIntoViewIfNeeded() {}
    scrollTo() {}
    scrollBy() {}
    scroll() {}
    get shadowRoot() { return $.shadowOf || (__shadow(this[ID]) >= 0 && shadowPart()) ? $.shadowOf(this) : null; }
    get assignedSlot() { return slotOf(this); }
    requestFullscreen() { return Promise.reject(domError('NotSupportedError', 'Fullscreen is not supported')); }
    requestPointerLock() {}
    setPointerCapture() {}
    releasePointerCapture() {}
    hasPointerCapture() { return false; }
    checkVisibility() { const r = __rect(this[ID]); return r[2] > 0 || r[3] > 0; }
  }
  mixin(Element, ParentNode);
  mixin(Element, ChildNode);
  lazyMethods(Element.prototype, 'shadow', ['attachShadow', 'setHTMLUnsafe']);
  lazyMethods(Element.prototype, 'animation', ['animate', 'getAnimations']);

  const adjacent = (el, pos, node) => {
    switch (String(pos).toLowerCase()) {
      case 'beforebegin': if (!el.parentNode) return false; insert(el.parentNode, node, el); return true;
      case 'afterbegin': insert(el, node, el.firstChild); return true;
      case 'beforeend': insert(el, node, null); return true;
      case 'afterend': if (!el.parentNode) return false; insert(el.parentNode, node, el.nextSibling); return true;
      default: throw domError('SyntaxError', "The value provided ('" + pos + "') is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'.");
    }
  };

  class Attr extends Node {
    constructor() { super(); illegal(); }
    get [NAME]() { return this.name; }
    get localName() { return this.name; }
    get namespaceURI() { return null; }
    get prefix() { return null; }
    get specified() { return true; }
    get name() { return this._name; }
    get ownerElement() { return this._owner; }
    get value() { return this.ownerElement ? (this.ownerElement.getAttribute(this.name) ?? '') : this._value; }
    set value(v) { if (this.ownerElement) this.ownerElement.setAttribute(this.name, v); else this._value = String(v); }
    get nodeValue() { return this.value; }
    set nodeValue(v) { this.value = v; }
    get textContent() { return this.value; }
    set textContent(v) { this.value = v; }
  }
  const newAttr = (el, name, value) => {
    const a = Object.create(Attr.prototype);
    def(a, { _name: name, _owner: el, _value: value });
    return a;
  };
  class NamedNodeMap {
    constructor() { illegal(); }
    get length() { return this._sync(); }
    item(i) { this._sync(); return this[i] || null; }
    getNamedItem(n) {
      const k = String(n).toLowerCase(), c = this._sync();
      for (let i = 0; i < c; i++) if (this[i].name === k) return this[i];
      return null;
    }
    getNamedItemNS(ns, n) { return this.getNamedItem(n); }
    setNamedItem(a) { return this._el.setAttributeNode(a); }
    setNamedItemNS(a) { return this.setNamedItem(a); }
    removeNamedItem(n) {
      const a = this.getNamedItem(n);
      if (!a) throw domError('NotFoundError', "No item with name '" + n + "' was found.");
      this._el.removeAttribute(n);
      return a;
    }
    removeNamedItemNS(ns, n) { return this.removeNamedItem(n); }
    [Symbol.iterator]() { this._sync(); return Array.prototype.slice.call(this, 0, this._n)[Symbol.iterator](); }
    _sync() {
      if (this._v === $.attrVersion) return this._n;
      const raw = __attrs(this._el[ID]);
      const n = raw.length / 2;
      // Attributes are also named properties of the map (`attributes.id`), unenumerable
      // and never shadowing a property of the interface.
      for (const name of this._named) delete this[name];
      this._named = [];
      for (let i = 0; i < n; i++) {
        const name = raw[2 * i];
        let a = this._cache.get(name);
        if (!a) this._cache.set(name, a = newAttr(this._el, name, ''));
        this[i] = a;
        if (!(name in this) && !/^\d+$/.test(name)) {
          Object.defineProperty(this, name, { configurable: true, enumerable: false, writable: true, value: a });
          this._named.push(name);
        }
      }
      for (let i = n; i < this._n; i++) delete this[i];
      this._n = n;
      this._v = $.attrVersion;
      return n;
    }
  }

  // `classList` and `relList`: the tokens of an attribute.
  class DOMTokenList {
    constructor() { illegal(); }
    _get() { this._sync(); return Array.prototype.slice.call(this, 0, this._n); }
    _set(a) {
      const raw = a.join(' ');
      this._take(raw, a);
      this._el.setAttribute(this._attr, raw);
    }
    // The tokens are also the list's indexed properties, renewed when the attribute changes.
    _sync() {
      const raw = __attr(this._el[ID], this._attr) || '';
      if (raw !== this._raw) this._take(raw, raw.split(/[\t\n\f\r ]+/).filter(Boolean));
    }
    _take(raw, a) {
      this._raw = raw;
      for (let i = 0; i < a.length; i++) this[i] = a[i];
      for (let i = a.length; i < this._n; i++) delete this[i];
      this._n = a.length;
    }
    get length() { this._sync(); return this._n; }
    get value() { return this._el.getAttribute(this._attr) || ''; }
    set value(v) { this._el.setAttribute(this._attr, v); }
    item(i) { this._sync(); return this[i >>> 0] ?? null; }
    contains(c) {
      this._sync();
      c = String(c);
      for (let i = 0; i < this._n; i++) if (this[i] === c) return true;
      return false;
    }
    add(...c) {
      const a = this._get();
      for (const x of c) {
        const s = String(x);
        if (s === '' || /\s/.test(s)) throw domError(s === '' ? 'SyntaxError' : 'InvalidCharacterError', 'The token provided contains HTML space characters, which are not valid in tokens.');
        if (!a.includes(s)) a.push(s);
      }
      this._set(a);
    }
    remove(...c) { const r = c.map(String); this._set(this._get().filter((x) => !r.includes(x))); }
    toggle(c, force) {
      c = String(c);
      const has = this.contains(c);
      const on = force === undefined ? !has : !!force;
      if (on && !has) this.add(c);
      if (!on && has) this.remove(c);
      return on;
    }
    replace(a, b) {
      const l = this._get();
      const i = l.indexOf(String(a));
      if (i < 0) return false;
      l[i] = String(b);
      this._set(l.filter((x, j) => l.indexOf(x) === j));
      return true;
    }
    supports() { return true; }
    forEach(f, that) { this._get().forEach((x, i) => f.call(that, x, i, this)); }
    entries() { return this._get().entries(); }
    keys() { return this._get().keys(); }
    values() { return this._get().values(); }
    toString() { return this.value; }
    [Symbol.iterator]() { return this._get()[Symbol.iterator](); }
  }
  const tokenList = (el, attr) => {
    const l = Object.create(DOMTokenList.prototype);
    l._el = el;
    l._attr = attr;
    l._raw = null;
    l._n = 0;
    l._sync();
    return l;
  };

  // `el.style`: the inline style attribute as a CSSStyleDeclaration.
  const kebab = (s) => (s.startsWith('--') ? s : s.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase()).replace(/^(webkit|moz|ms)-/, '-$1-'));
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
      setProperty(k, v, priority) {
        const m = parseStyle(el);
        if (v == null || v === '') m.delete(k); else m.set(k, String(v) + (priority === 'important' ? ' !important' : ''));
        writeStyle(el, m);
      },
      getPropertyValue(k) { return parseStyle(el).get(k) || ''; },
      getPropertyPriority(k) { return /!important$/.test(parseStyle(el).get(k) || '') ? 'important' : ''; },
      removeProperty(k) {
        const m = parseStyle(el);
        const old = m.get(k) || '';
        m.delete(k);
        writeStyle(el, m);
        return old;
      },
      item(i) { return [...parseStyle(el).keys()][i] || ''; },
    };
    return new Proxy(api, {
      get(t, k) {
        if (typeof k !== 'string') return undefined;
        if (k in t) return t[k];
        if (k === 'cssText') return el.getAttribute('style') || '';
        if (k === 'length') return parseStyle(el).size;
        if (/^\d+$/.test(k)) return t.item(+k);
        if (k === 'parentRule') return null;
        return t.getPropertyValue(kebab(k));
      },
      set(t, k, v) {
        if (k === 'cssText') el.setAttribute('style', String(v)); else t.setProperty(kebab(String(k)), v);
        return true;
      },
      has(t, k) { return typeof k === 'string'; },
    });
  };
  // getComputedStyle: resolved values from the style system, read each time one
  // is asked for; a property it does not resolve falls back to the inline style.
  const computedStyle = (el) => {
    if (!(el instanceof Element)) throw new TypeError("Failed to execute 'getComputedStyle' on 'Window': parameter 1 is not of type 'Element'.");
    const id = el[ID];
    const inline = el.style || styleProxy(el);
    const value = (k) => { const v = __computed(id, k); return v === undefined ? inline.getPropertyValue(k) : v; };
    const readOnly = () => { throw domError('NoModificationAllowedError', 'These styles are computed, and therefore read-only.'); };
    const api = {
      getPropertyValue(k) { k = String(k); return value(k.startsWith('--') ? k : k.toLowerCase()); },
      getPropertyPriority() { return ''; },
      setProperty: readOnly,
      removeProperty: readOnly,
      item(i) { return __computed(id)[2 * i] || ''; },
    };
    return new Proxy(api, {
      get(t, k) {
        if (typeof k !== 'string') return undefined;
        if (k in t) return t[k];
        if (k === 'length') return __computed(id).length / 2;
        if (k === 'cssText') return '';
        if (k === 'parentRule') return null;
        if (/^\d+$/.test(k)) return t.item(+k);
        return value(kebab(k));
      },
      set() { return true; },
      has(t, k) { return typeof k === 'string'; },
    });
  };
  const STYLE = Symbol('style');
  const DATASET = Symbol('dataset');
  const camel = (s) => s.replace(/-([a-z])/g, (_, c) => c.toUpperCase());
  const datasetProxy = (el) => new Proxy({}, {
    get(_, k) { return typeof k === 'string' ? (el.getAttribute('data-' + kebab(k)) ?? undefined) : undefined; },
    set(_, k, v) { el.setAttribute('data-' + kebab(String(k)), v); return true; },
    has(_, k) { return typeof k === 'string' && el.hasAttribute('data-' + kebab(k)); },
    deleteProperty(_, k) { el.removeAttribute('data-' + kebab(String(k))); return true; },
    ownKeys() { return el.getAttributeNames().filter((n) => n.startsWith('data-')).map((n) => camel(n.slice(5))); },
    getOwnPropertyDescriptor(_, k) {
      const v = typeof k === 'string' ? el.getAttribute('data-' + kebab(k)) : null;
      return v === null ? undefined : { value: v, writable: true, enumerable: true, configurable: true };
    },
  });

  // The custom element constructor: returns the element being upgraded, or a new one.
  function HTMLElement() {
    if (!new.target || !$.ce) illegal();
    return $.ce.construct(new.target);
  }
  HTMLElement.prototype = Object.create(Element.prototype, { constructor: { value: HTMLElement, writable: true, configurable: true } });
  Object.setPrototypeOf(HTMLElement, Element);
  const focusable = /^(a|area|button|input|select|textarea|iframe|summary)$/;
  getters(HTMLElement.prototype, {
    style: [function () { let s = this[STYLE]; if (!s) Object.defineProperty(this, STYLE, { value: s = styleProxy(this) }); return s; }, function (v) { this.setAttribute('style', v); }],
    dataset() { let d = this[DATASET]; if (!d) Object.defineProperty(this, DATASET, { value: d = datasetProxy(this) }); return d; },
    innerText: [function () { return this.textContent; }, function (v) { this.textContent = v; }],
    outerText: [function () { return this.textContent; }, function (v) { this.replaceWith(String(v)); }],
    offsetWidth() { return Math.round(__rect(this[ID])[2]); },
    offsetHeight() { return Math.round(__rect(this[ID])[3]); },
    offsetLeft() { return Math.round(__rect(this[ID])[0]); },
    offsetTop() { return Math.round(__rect(this[ID])[1]); },
    offsetParent() { return this.parentElement; },
    tabIndex: [function () { const v = parseInt(this.getAttribute('tabindex'), 10); return isNaN(v) ? (focusable.test(this[TAG]) ? 0 : -1) : v; }, function (v) { this.setAttribute('tabindex', v); }],
    isContentEditable() { return false; },
    contentEditable: [function () { return this.getAttribute('contenteditable') ?? 'inherit'; }, function (v) { this.setAttribute('contenteditable', v); }],
  });
  def(HTMLElement.prototype, {
    click() { fire(this, 'click', { bubbles: true, cancelable: true, composed: true, view: g, detail: 1 }, events.MouseEvent); },
    focus() {
      if (activeElement === this) return;
      const old = activeElement;
      activeElement = this;
      if (old) { fire(old, 'blur', { relatedTarget: this }, events.FocusEvent); fire(old, 'focusout', { bubbles: true, relatedTarget: this }, events.FocusEvent); }
      fire(this, 'focus', { relatedTarget: old }, events.FocusEvent);
      fire(this, 'focusin', { bubbles: true, relatedTarget: old }, events.FocusEvent);
    },
    blur() {
      if (activeElement !== this) return;
      activeElement = null;
      fire(this, 'blur', {}, events.FocusEvent);
      fire(this, 'focusout', { bubbles: true }, events.FocusEvent);
    },
    attachInternals() { return { setFormValue: noop, setValidity: noop, checkValidity: () => true, reportValidity: () => true, states: new Set(), form: null, labels: [], shadowRoot: this.shadowRoot }; },
    showPopover: noop,
    hidePopover: noop,
    togglePopover: noop,
  });
  let activeElement = null;

  // Attributes that properties reflect: `prop`, `prop:attr`, `prop:attr:kind` or
  // `prop::kind`, where kind is b(oolean), n(umber) or u(RL); the default is a string.
  const reflect = (C, list) => {
    for (const item of list.split(' ')) {
      const [prop, a, kind] = item.split(':');
      const attr = a || prop.toLowerCase();
      const d = kind === 'b'
        ? { get() { return this.hasAttribute(attr); }, set(v) { this.toggleAttribute(attr, !!v); } }
        : kind === 'n'
          ? { get() { const v = parseInt(this.getAttribute(attr), 10); return isNaN(v) ? 0 : v; }, set(v) { this.setAttribute(attr, String(Math.trunc(v) || 0)); } }
          : kind === 'u'
            ? { get() { const v = this.getAttribute(attr); return v === null ? '' : (resolveURL(v) ?? v); }, set(v) { this.setAttribute(attr, v); } }
            : { get() { return this.getAttribute(attr) ?? ''; }, set(v) { this.setAttribute(attr, v); } };
      Object.defineProperty(C.prototype, prop, { configurable: true, enumerable: true, ...d });
    }
  };
  reflect(HTMLElement, 'title lang dir accessKey:accesskey hidden::b translate draggable autofocus::b inert::b nonce enterKeyHint:enterkeyhint inputMode:inputmode popover');

  // Element interfaces by tag. Every listed interface extends HTMLElement unless
  // its parent is named with `<`.
  const IFACES = 'HTMLHtmlElement:html HTMLHeadElement:head HTMLBodyElement:body HTMLTitleElement:title HTMLMetaElement:meta HTMLLinkElement:link HTMLStyleElement:style HTMLScriptElement:script HTMLBaseElement:base ' +
    'HTMLDivElement:div HTMLSpanElement:span HTMLParagraphElement:p HTMLHeadingElement:h1,h2,h3,h4,h5,h6 HTMLAnchorElement:a HTMLImageElement:img HTMLPictureElement:picture HTMLSourceElement:source ' +
    'HTMLInputElement:input HTMLButtonElement:button HTMLFormElement:form HTMLLabelElement:label HTMLSelectElement:select HTMLOptionElement:option HTMLOptGroupElement:optgroup HTMLTextAreaElement:textarea ' +
    'HTMLFieldSetElement:fieldset HTMLLegendElement:legend HTMLDataListElement:datalist HTMLOutputElement:output HTMLProgressElement:progress HTMLMeterElement:meter ' +
    'HTMLUListElement:ul HTMLOListElement:ol HTMLLIElement:li HTMLDListElement:dl HTMLMenuElement:menu HTMLTableElement:table HTMLTableSectionElement:thead,tbody,tfoot HTMLTableRowElement:tr ' +
    'HTMLTableCellElement:td,th HTMLTableCaptionElement:caption HTMLTableColElement:col,colgroup HTMLPreElement:pre,listing,xmp HTMLQuoteElement:q,blockquote HTMLHRElement:hr HTMLBRElement:br ' +
    'HTMLModElement:ins,del HTMLTimeElement:time HTMLDataElement:data HTMLIFrameElement:iframe HTMLEmbedElement:embed HTMLObjectElement:object HTMLParamElement:param HTMLCanvasElement:canvas ' +
    'HTMLMapElement:map HTMLAreaElement:area HTMLMediaElement: HTMLVideoElement<HTMLMediaElement:video HTMLAudioElement<HTMLMediaElement:audio HTMLTrackElement:track HTMLTemplateElement:template ' +
    'HTMLSlotElement:slot HTMLDialogElement:dialog HTMLDetailsElement:details HTMLUnknownElement: HTMLFrameSetElement:frameset HTMLFrameElement:frame HTMLFontElement:font HTMLMarqueeElement:marquee';
  const SVG_IFACES = 'SVGGraphicsElement<SVGElement: SVGGeometryElement<SVGGraphicsElement: SVGSVGElement<SVGGraphicsElement:svg SVGGElement<SVGGraphicsElement:g SVGPathElement<SVGGeometryElement:path ' +
    'SVGCircleElement<SVGGeometryElement:circle SVGRectElement<SVGGeometryElement:rect SVGLineElement<SVGGeometryElement:line SVGPolylineElement<SVGGeometryElement:polyline ' +
    'SVGPolygonElement<SVGGeometryElement:polygon SVGEllipseElement<SVGGeometryElement:ellipse SVGTextContentElement<SVGGraphicsElement: SVGTextElement<SVGTextContentElement:text ' +
    'SVGTSpanElement<SVGTextContentElement:tspan SVGUseElement<SVGGraphicsElement:use SVGImageElement<SVGGraphicsElement:image SVGForeignObjectElement<SVGGraphicsElement:foreignobject ' +
    'SVGDefsElement<SVGGraphicsElement:defs SVGSymbolElement<SVGElement:symbol SVGLinearGradientElement<SVGElement:lineargradient SVGRadialGradientElement<SVGElement:radialgradient ' +
    'SVGStopElement<SVGElement:stop SVGClipPathElement<SVGElement:clippath SVGMaskElement<SVGElement:mask SVGPatternElement<SVGElement:pattern SVGFilterElement<SVGElement:filter ' +
    'SVGDescElement<SVGElement:desc SVGMarkerElement<SVGElement:marker SVGAnimateElement<SVGElement:animate SVGFEGaussianBlurElement<SVGElement:fegaussianblur';
  class SVGElement extends Element {
    constructor() { super(); illegal(); }
    get ownerSVGElement() { let p = this.parentNode; while (p && p[TAG] !== 'svg') p = p.parentNode; return p; }
    get viewportElement() { return this.ownerSVGElement; }
  }
  getters(SVGElement.prototype, {
    style: Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'style').get,
    dataset: Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'dataset').get,
    className() { const el = this; return { get baseVal() { return el.getAttribute('class') || ''; }, set baseVal(v) { el.setAttribute('class', v); }, get animVal() { return this.baseVal; } }; },
  });
  def(SVGElement.prototype, { focus: HTMLElement.prototype.focus, blur: HTMLElement.prototype.blur, getBBox() { const r = __rect(this[ID]); return new DOMRect(0, 0, r[2], r[3]); }, getCTM() { return null; }, getScreenCTM() { return null; } });
  const ifaces = { HTMLElement, SVGElement, Element };
  for (const [list, root] of [[IFACES, 'HTMLElement'], [SVG_IFACES, 'SVGElement']]) {
    for (const item of list.split(' ')) {
      const [head, tags] = item.split(':');
      const [name, parentName] = head.split('<');
      const parent = ifaces[parentName || root];
      const C = { [name]: function () { illegal(); } }[name];
      C.prototype = Object.create(parent.prototype, { constructor: { value: C, writable: true, configurable: true } });
      Object.setPrototypeOf(C, parent);
      ifaces[name] = C;
      if (tags) for (const t of tags.split(',')) protos.set(t, C.prototype);
    }
  }
  for (const t of 'article section nav aside header footer main address hgroup figure figcaption summary b i u s em strong small mark abbr cite code kbd samp var sub sup dfn bdi bdo ruby rt rp wbr noscript search center big tt strike acronym nobr'.split(' ')) protos.set(t, HTMLElement.prototype);
  for (const [name, C] of Object.entries(ifaces)) Object.defineProperty(C.prototype, Symbol.toStringTag, { configurable: true, value: name });
  const { HTMLUnknownElement, HTMLMediaElement, HTMLTemplateElement, HTMLAnchorElement, HTMLAreaElement, HTMLImageElement, HTMLInputElement, HTMLScriptElement, HTMLLinkElement, HTMLSelectElement, HTMLOptionElement, HTMLTextAreaElement, HTMLFormElement, HTMLButtonElement, HTMLCanvasElement, HTMLIFrameElement, HTMLVideoElement } = ifaces;
  $.ifaces = ifaces;

  reflect(HTMLAnchorElement, 'href::u target download rel hreflang type referrerPolicy:referrerpolicy ping');
  reflect(HTMLAreaElement, 'href::u alt coords shape target download rel');
  reflect(HTMLImageElement, 'src::u alt srcset sizes crossOrigin:crossorigin useMap:usemap isMap:ismap:b width::n height::n loading decoding referrerPolicy:referrerpolicy fetchPriority:fetchpriority');
  reflect(HTMLInputElement, 'accept alt autocomplete defaultValue:value defaultChecked:checked:b dirName:dirname disabled::b formAction:formaction::u max maxLength:maxlength:n min minLength:minlength:n multiple::b name pattern placeholder readOnly:readonly:b required::b size::n src::u step value checked::b indeterminate::b');
  reflect(HTMLButtonElement, 'disabled::b name value autofocus::b formAction:formaction:u');
  reflect(HTMLFormElement, 'action::u method enctype target name acceptCharset:accept-charset autocomplete noValidate:novalidate:b');
  reflect(HTMLLinkElement, 'href::u rel media hreflang type as crossOrigin:crossorigin integrity sizes disabled::b imageSrcset:imagesrcset referrerPolicy:referrerpolicy fetchPriority:fetchpriority');
  reflect(HTMLScriptElement, 'src::u type noModule:nomodule:b async::b defer::b crossOrigin:crossorigin integrity charset referrerPolicy:referrerpolicy event htmlFor:for');
  reflect(ifaces.HTMLStyleElement, 'media type');
  reflect(ifaces.HTMLMetaElement, 'name content httpEquiv:http-equiv charset media');
  reflect(HTMLIFrameElement, 'src::u srcdoc name width height allow allowFullscreen:allowfullscreen:b loading referrerPolicy:referrerpolicy sandbox');
  reflect(HTMLSelectElement, 'disabled::b multiple::b name required::b size::n autofocus::b');
  reflect(HTMLOptionElement, 'disabled::b label defaultSelected:selected:b selected::b');
  reflect(HTMLTextAreaElement, 'cols::n rows::n disabled::b maxLength:maxlength:n minLength:minlength:n name placeholder readOnly:readonly:b required::b wrap autocomplete');
  reflect(ifaces.HTMLLabelElement, 'htmlFor:for');
  reflect(ifaces.HTMLTableCellElement, 'colSpan:colspan:n rowSpan:rowspan:n headers abbr scope');
  reflect(ifaces.HTMLOListElement, 'start::n reversed::b type');
  reflect(ifaces.HTMLLIElement, 'value::n');
  reflect(HTMLMediaElement, 'src::u autoplay::b loop::b muted::b defaultMuted:muted:b controls::b preload crossOrigin:crossorigin playsInline:playsinline:b');
  reflect(HTMLVideoElement, 'poster::u width::n height::n');
  reflect(ifaces.HTMLSourceElement, 'src::u srcset sizes media type');
  reflect(ifaces.HTMLTrackElement, 'src::u kind srclang label default::b');
  reflect(ifaces.HTMLDetailsElement, 'open::b name');
  reflect(ifaces.HTMLDialogElement, 'open::b');
  reflect(ifaces.HTMLSlotElement, 'name');
  reflect(HTMLCanvasElement, 'width::n height::n');
  reflect(ifaces.HTMLTimeElement, 'dateTime:datetime');
  reflect(ifaces.HTMLDataElement, 'value');
  reflect(ifaces.HTMLQuoteElement, 'cite::u');
  reflect(ifaces.HTMLModElement, 'cite::u dateTime:datetime');
  reflect(ifaces.HTMLBaseElement, 'target');
  reflect(ifaces.HTMLEmbedElement, 'src::u type width height');
  reflect(ifaces.HTMLObjectElement, 'data::u type name width height');
  reflect(ifaces.HTMLProgressElement, 'max');
  reflect(ifaces.HTMLMeterElement, 'min max low high optimum');
  reflect(ifaces.HTMLFieldSetElement, 'disabled::b name');
  reflect(ifaces.HTMLOptGroupElement, 'disabled::b label');
  reflect(ifaces.HTMLTableElement, 'border width');
  reflect(ifaces.HTMLHtmlElement, 'version');

  getters(ifaces.HTMLBaseElement.prototype, { href: [function () { return resolveURL(this.getAttribute('href') || '', docURL) || ''; }, function (v) { this.setAttribute('href', v); }] });
  const relList = { relList() { return tokenList(this, 'rel'); } };
  getters(HTMLAnchorElement.prototype, relList);
  getters(HTMLAreaElement.prototype, relList);
  getters(HTMLLinkElement.prototype, relList);
  getters(HTMLLinkElement.prototype, { sheet() { need('cssom'); return $.sheetOf(this); } });
  getters(ifaces.HTMLStyleElement.prototype, { sheet() { need('cssom'); return $.sheetOf(this); } });
  // The parts of a link's URL, as URL objects have them.
  for (const C of [HTMLAnchorElement, HTMLAreaElement]) {
    const parts = { protocol: 1, username: 2, password: 3, host: 4, hostname: 5, port: 6, pathname: 7, search: 8, hash: 9, origin: 10 };
    for (const [k, i] of Object.entries(parts)) {
      Object.defineProperty(C.prototype, k, {
        configurable: true,
        enumerable: true,
        get() { const raw = this.getAttribute('href'); const u = raw === null ? null : __url(raw, baseURL()); return u ? u[i] : (k === 'origin' ? 'null' : ''); },
        set(v) {
          if (k === 'origin') return;
          const u = new URL(this.href || baseURL());
          u[k] = v;
          this.setAttribute('href', u.href);
        },
      });
    }
    C.prototype.toString = function () { return this.href; };
  }
  getters(HTMLScriptElement.prototype, { text: [function () { return this.textContent; }, function (v) { this.textContent = v; }] });
  getters(HTMLImageElement.prototype, {
    complete() { return true; },
    currentSrc() { return this.src; },
    naturalWidth() { return Math.round(__rect(this[ID])[2]); },
    naturalHeight() { return Math.round(__rect(this[ID])[3]); },
    x() { return Math.round(__rect(this[ID])[0]); },
    y() { return Math.round(__rect(this[ID])[1]); },
  });
  def(HTMLImageElement.prototype, { decode() { return Promise.resolve(); } });
  getters(HTMLTemplateElement.prototype, { content() { return wrap(__template(this[ID])); } });
  getters(HTMLIFrameElement.prototype, { contentWindow() { return null; }, contentDocument() { return null; } });
  def(HTMLCanvasElement.prototype, { getContext() { return null; }, toDataURL() { return 'data:,'; }, toBlob(cb) { addTimer(() => cb(null), 0, false, []); }, captureStream() { return null; } });
  const formAncestor = { form() { return this.closest('form'); }, labels() { return staticList([]); }, validity() { return { valid: true, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false }; }, validationMessage() { return ''; }, willValidate() { return true; } };
  const formMethods = { checkValidity() { return true; }, reportValidity() { return true; }, setCustomValidity: noop };
  for (const C of [HTMLInputElement, HTMLButtonElement, HTMLSelectElement, HTMLTextAreaElement, ifaces.HTMLOutputElement, ifaces.HTMLFieldSetElement]) {
    getters(C.prototype, formAncestor);
    def(C.prototype, formMethods);
  }
  getters(HTMLInputElement.prototype, {
    type: [function () { const t = (this.getAttribute('type') || '').toLowerCase(); return /^(hidden|text|search|tel|url|email|password|date|month|week|time|datetime-local|number|range|color|checkbox|radio|file|submit|image|reset|button)$/.test(t) ? t : 'text'; }, function (v) { this.setAttribute('type', v); }],
    valueAsNumber: [function () { return parseFloat(this.value); }, function (v) { this.value = String(v); }],
    selectionStart: [function () { return this.value.length; }, noop],
    selectionEnd: [function () { return this.value.length; }, noop],
    files() { return null; },
    list() { const id = this.getAttribute('list'); return id ? document.getElementById(id) : null; },
  });
  def(HTMLInputElement.prototype, { select: noop, setSelectionRange: noop, setRangeText: noop, showPicker: noop, stepUp: noop, stepDown: noop });
  getters(HTMLButtonElement.prototype, { type: [function () { const t = (this.getAttribute('type') || '').toLowerCase(); return t === 'reset' || t === 'button' ? t : 'submit'; }, function (v) { this.setAttribute('type', v); }] });
  getters(HTMLTextAreaElement.prototype, {
    value: [function () { return this.textContent; }, function (v) { this.textContent = v; }],
    defaultValue: [function () { return this.textContent; }, function (v) { this.textContent = v; }],
    type() { return 'textarea'; },
    textLength() { return this.textContent.length; },
  });
  def(HTMLTextAreaElement.prototype, { select: noop, setSelectionRange: noop });
  getters(HTMLOptionElement.prototype, {
    value: [function () { return this.getAttribute('value') ?? this.text; }, function (v) { this.setAttribute('value', v); }],
    text: [function () { return this.textContent.replace(/\s+/g, ' ').trim(); }, function (v) { this.textContent = v; }],
    index() { const s = this.closest('select'); return s ? Array.prototype.indexOf.call(s.options, this) : 0; },
  });
  getters(HTMLSelectElement.prototype, {
    options() { return staticList(this.querySelectorAll('option')); },
    selectedOptions() { return staticList(Array.prototype.filter.call(this.options, (o) => o.selected)); },
    selectedIndex: [function () { const o = this.options; for (let i = 0; i < o.length; i++) if (o[i].selected) return i; return o.length ? 0 : -1; }, function (v) { const o = this.options; for (let i = 0; i < o.length; i++) o[i].selected = i === v; }],
    value: [function () { const i = this.selectedIndex; return i < 0 ? '' : this.options[i].value; }, function (v) { const o = this.options; for (let i = 0; i < o.length; i++) o[i].selected = o[i].value === String(v); }],
    length() { return this.options.length; },
    type() { return this.multiple ? 'select-multiple' : 'select-one'; },
  });
  def(HTMLSelectElement.prototype, { item(i) { return this.options[i] || null; }, add(o, before) { this.insertBefore(o, typeof before === 'number' ? this.options[before] || null : before || null); } });
  getters(HTMLFormElement.prototype, { elements() { return staticList(this.querySelectorAll('input,button,select,textarea,fieldset,output,object')); }, length() { return this.elements.length; } });
  def(HTMLFormElement.prototype, {
    submit: noop,
    requestSubmit() { fire(this, 'submit', { bubbles: true, cancelable: true }, events.SubmitEvent); },
    reset() { fire(this, 'reset', { bubbles: true, cancelable: true }); },
    checkValidity() { return true; },
    reportValidity() { return true; },
  });
  def(ifaces.HTMLDialogElement.prototype, {
    show() { this.setAttribute('open', ''); },
    showModal() { this.setAttribute('open', ''); __modal(this[ID], true); },
    close(v) { if (v !== undefined) this.returnValue = String(v); this.removeAttribute('open'); __modal(this[ID], false); fire(this, 'close'); },
  });

  // A <video> or <audio> that never loads: nothing buffered, paused at the start.
  const MEDIA_STATES = { HAVE_NOTHING: 0, HAVE_METADATA: 1, HAVE_CURRENT_DATA: 2, HAVE_FUTURE_DATA: 3, HAVE_ENOUGH_DATA: 4, NETWORK_EMPTY: 0, NETWORK_IDLE: 1, NETWORK_LOADING: 2, NETWORK_NO_SOURCE: 3 };
  Object.assign(HTMLMediaElement, MEDIA_STATES);
  Object.assign(HTMLMediaElement.prototype, MEDIA_STATES);
  getters(HTMLMediaElement.prototype, {
    readyState() { return 0; },
    networkState() { return 0; },
    paused() { return true; },
    ended() { return false; },
    seeking() { return false; },
    currentTime: [() => 0, noop],
    duration() { return NaN; },
    volume: [() => 1, noop],
    playbackRate: [() => 1, noop],
    error() { return null; },
    currentSrc() { return this.getAttribute('src') || ''; },
    buffered() { return { length: 0, start() { return 0; }, end() { return 0; } }; },
    textTracks() { return []; },
  });
  def(HTMLMediaElement.prototype, {
    // Media cannot play here, which is what a browser reports when autoplay is blocked.
    play() {
      const e = new Error('playback is not supported');
      e.name = 'NotAllowedError';
      return new Promise((_, reject) => addTimer(() => reject(e), 0, false, []));
    },
    pause: noop,
    load: noop,
    canPlayType() { return ''; },
    addTextTrack() { return { mode: 'disabled', cues: [], addCue: noop }; },
  });
  class MediaError {
    constructor(code) { this.code = code; this.message = ''; }
  }
  Object.assign(MediaError, { MEDIA_ERR_ABORTED: 1, MEDIA_ERR_NETWORK: 2, MEDIA_ERR_DECODE: 3, MEDIA_ERR_SRC_NOT_SUPPORTED: 4 });

  class DocumentFragment extends Node {
    constructor() { return wrap(__create('#document-fragment', '')); }
    get [NAME]() { return '#document-fragment'; }
    getElementById(id) { return wrap(__by_id(this[ID], String(id))); }
  }
  mixin(DocumentFragment, ParentNode);
  class ShadowRoot extends DocumentFragment {
    constructor() { super(); illegal(); }
    get [NAME]() { return '#document-fragment'; }
    // A root the parser declared gets these when first asked.
    get host() { need('shadow'); return $.rootOf(wrap(__host(this[ID]))).host; }
    get mode() { need('shadow'); return $.rootOf(wrap(__host(this[ID]))).mode; }
  }
  protos.set('#document-fragment', DocumentFragment.prototype);
  protos.set('#shadow-root', ShadowRoot.prototype);

  // document.all: every element of the document, in a collection browsers keep for old
  // pages and make undetectable: falsy, of type "undefined", == undefined but !== undefined.
  class HTMLAllCollection {
    constructor() { illegal(); }
    get length() { return document.getElementsByTagName('*').length; }
    item(i) { return typeof i === 'string' ? this.namedItem(i) : document.getElementsByTagName('*')[i] || null; }
    namedItem(name) { return document.getElementById(name) || document.getElementsByName(name)[0] || null; }
  }
  let allElements = null;

  class Document extends Node {
    constructor() { return newDocument(false); }
    get [NAME]() { return '#document'; }
    get documentElement() { const k = __kids(this[ID], true); return k.length ? wrap(k[0]) : null; }
    get head() { return childByTag(this.documentElement, 'head'); }
    get body() { const h = this.documentElement; return childByTag(h, 'body') || childByTag(h, 'frameset'); }
    set body(b) { const old = this.body, h = this.documentElement; if (old) h.replaceChild(b, old); else if (h) h.appendChild(b); }
    get children() { return ParentNode.children; }
    getElementById(id) { return wrap(__by_id(this[ID], String(id))); }
    getElementsByName(n) { const id = this[ID], sel = '[name="' + cssEscape(String(n)) + '"]'; return liveList(NodeList, () => __query(id, sel, false), true); }
    createElement(t, options) {
      t = String(t);
      if (!/^[a-zA-Z:_\u00c0-\uffff][^\s/>"'=\u0000-\u001f]*$/.test(t)) throw domError('InvalidCharacterError', "Failed to execute 'createElement' on 'Document': The tag name provided ('" + t + "') is not a valid name.");
      const name = t.toLowerCase();
      const el = $.ce && $.ce.create(name, options) || wrap(__create(name, ''));
      if (this !== document) el[OWNER] = this;
      return el;
    }
    createElementNS(ns, q, options) {
      const name = String(q).includes(':') ? String(q).slice(String(q).indexOf(':') + 1) : String(q);
      return this.createElement(name, options);
    }
    createTextNode(s) { return wrap(__create(null, String(s))); }
    createComment(s) { return wrap(__comment(String(s))); }
    createCDATASection(s) { return this.createTextNode(s); }
    createProcessingInstruction(target, data) { const c = this.createComment('?' + target + ' ' + data + '?'); return c; }
    createDocumentFragment() { return wrap(__create('#document-fragment', '')); }
    createAttribute(n) { return newAttr(null, String(n).toLowerCase(), ''); }
    createAttributeNS(ns, n) { return this.createAttribute(n); }
    createEvent(name) {
      const C = events[EVENT_NAMES[String(name).toLowerCase()]];
      if (!C) throw domError('NotSupportedError', "Failed to execute 'createEvent' on 'Document': The provided event type ('" + name + "') is invalid.");
      return new C('');
    }
    importNode(n, deep) {
      if (n.nodeType === DOC) throw domError('NotSupportedError', 'Documents cannot be imported.');
      const c = cloneNode(n, !!deep);
      delete c[OWNER];
      return c;
    }
    adoptNode(n) {
      if (n.nodeType === DOC) throw domError('NotSupportedError', 'Documents cannot be adopted.');
      const old = n.ownerDocument;
      if (n.parentNode) removeNode(n);
      delete n[OWNER];
      if ($.ce && old !== this) $.ce.adopted(n, old, this);
      return n;
    }
    get title() { const t = this.querySelector('title'); return t ? t.textContent.replace(/\s+/g, ' ').trim() : ''; }
    set title(v) {
      let t = this.querySelector('title');
      if (!t && this.head) t = this.head.appendChild(this.createElement('title'));
      if (t) t.textContent = v;
    }
    get readyState() { return this === document ? readyState : 'complete'; }
    get URL() { return this === document ? docURL : 'about:blank'; }
    get documentURI() { return this.URL; }
    get baseURI() { return baseURL(); }
    get location() { return this === document ? location : null; }
    get defaultView() { return this === document ? g : null; }
    get all() { return this === document ? allElements || (allElements = __undetectable(HTMLAllCollection.prototype)) : undefined; }
    get currentScript() { return currentScript; }
    get visibilityState() { return pageHidden ? 'hidden' : 'visible'; }
    get hidden() { return pageHidden; }
    get referrer() { return ''; }
    get cookie() { return this === document ? __cookie(docURL) : ''; }
    set cookie(v) { if (this === document) __cookie(docURL, String(v)); }
    get domain() { return location.hostname; }
    set domain(v) {}
    get lastModified() { return new Date().toLocaleString(); }
    get characterSet() { return 'UTF-8'; }
    get charset() { return 'UTF-8'; }
    get inputEncoding() { return 'UTF-8'; }
    get contentType() { return 'text/html'; }
    get compatMode() { return 'CSS1Compat'; }
    get doctype() { return null; }
    get dir() { const h = this.documentElement; return h ? h.getAttribute('dir') || '' : ''; }
    set dir(v) { const h = this.documentElement; if (h) h.setAttribute('dir', v); }
    get activeElement() { return activeElement && activeElement.isConnected ? activeElement : this.body; }
    get scrollingElement() { return this.documentElement; }
    get fullscreenElement() { return null; }
    get fullscreenEnabled() { return false; }
    get pointerLockElement() { return null; }
    get designMode() { return 'off'; }
    set designMode(v) {}
    get fonts() { return fontFaces; }
    get scripts() { return this.getElementsByTagName('script'); }
    get images() { return this.getElementsByTagName('img'); }
    get embeds() { return this.getElementsByTagName('embed'); }
    get plugins() { return this.getElementsByTagName('embed'); }
    get forms() { return this.getElementsByTagName('form'); }
    get links() { const id = this[ID]; return liveList(HTMLCollection, () => __query(id, 'a[href],area[href]', false), true); }
    get anchors() { const id = this[ID]; return liveList(HTMLCollection, () => __query(id, 'a[name]', false), true); }
    hasFocus() { return true; }
    execCommand() { return false; }
    queryCommandSupported() { return false; }
    queryCommandEnabled() { return false; }
    elementFromPoint() { return null; }
    elementsFromPoint() { return []; }
    caretRangeFromPoint() { return null; }
    exitFullscreen() { return Promise.resolve(); }
    exitPointerLock() {}
    hasStorageAccess() { return Promise.resolve(true); }
    requestStorageAccess() { return Promise.resolve(); }
    startViewTransition(cb) {
      const done = Promise.resolve().then(() => cb && cb());
      return { finished: done, ready: done, updateCallbackDone: done, skipTransition: noop };
    }
  }
  mixin(Document, ParentNode);
  lazyMethods(Document.prototype, 'write', ['open', 'close', 'write', 'writeln']);
  lazyMethods(Document.prototype, 'animation', ['getAnimations']);
  lazyMethods(Document.prototype, 'traversal', ['createTreeWalker', 'createNodeIterator', 'createRange', 'getSelection']);
  getters(Document.prototype, {
    implementation() { need('traversal'); return $.implementation; },
    styleSheets() { need('cssom'); return $.styleSheets(this); },
    adoptedStyleSheets: [function () { need('cssom'); return this.adoptedStyleSheets; }, function (v) { need('cssom'); this.adoptedStyleSheets = v; }],
  });
  protos.set('#document', Document.prototype);
  // What `nodeType` reports for each kind of node, as data the accessor reads without a call.
  for (const [C, kind] of [[Element, 1], [Attr, 2], [Text, 3], [Comment, 8], [Document, 9], [DocumentType, 10], [DocumentFragment, 11]]) Object.defineProperty(C.prototype, KIND, { value: kind });
  def(Document, { parseHTMLUnsafe(v) { need('shadow'); return Document.parseHTMLUnsafe(v); } });

  // A document without a window, as `createHTMLDocument` makes.
  const newDocument = (html, title) => {
    const d = wrap(__create('#document', ''));
    if (html) {
      const root = d.appendChild(d.createElement('html'));
      const head = root.appendChild(d.createElement('head'));
      if (title !== undefined) head.appendChild(d.createElement('title')).textContent = String(title);
      root.appendChild(d.createElement('body'));
    }
    return d;
  };
  $.newDocument = newDocument;
  let templateDoc = null;
  const templateDocument = () => templateDoc || (templateDoc = newDocument(false));

  const fontFaces = Object.assign(new EventTarget(), {
    ready: Promise.resolve(),
    status: 'loaded',
    size: 0,
    load() { return Promise.resolve([]); },
    check() { return true; },
    add: noop,
    delete() { return false; },
    clear: noop,
    has() { return false; },
    forEach: noop,
    [Symbol.iterator]() { return [][Symbol.iterator](); },
  });
  class FontFace {
    constructor(family, source) { this.family = family; this.source = source; this.status = 'loaded'; this.loaded = Promise.resolve(this); }
    load() { return Promise.resolve(this); }
  }

  const document = wrap(0);


  let docURL = 'about:blank';
  const baseURL = () => {
    const href = __base();
    const u = href !== null && __url(href, docURL);
    return u ? u[0] : docURL;
  };
  const resolveURL = (input, base = baseURL()) => { const u = __url(String(input), base); return u ? u[0] : null; };
  $.resolveURL = resolveURL;
  $.baseURL = baseURL;

  const cssEscape = (s) => String(s).replace(/[\0-\x1f\x7f]|^-?\d|^-$|[^\w\-\u0080-\uffff]/g, (c, i) => {
    if (c === '\0') return '\ufffd';
    if (/[\x01-\x1f\x7f]|\d/.test(c)) return '\\' + c.charCodeAt(0).toString(16) + ' ';
    return '\\' + c;
  }).replace(/^(-)(\d)/, '$1\\3$2 ');

  const URL_PARTS = ['href', 'protocol', 'username', 'password', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin'];
  class URLSearchParams {
    constructor(init) {
      this._l = [];
      this._url = null;
      if (init instanceof URLSearchParams) this._l = init._l.map((p) => p.slice());
      else if (init && typeof init === 'object') {
        if (typeof init[Symbol.iterator] === 'function') for (const p of init) this._l.push([String(p[0]), String(p[1])]);
        else for (const k of Object.keys(init)) this._l.push([k, String(init[k])]);
      } else this._parse(String(init ?? ''));
    }
    _parse(s) {
      this._l = [];
      for (const part of s.replace(/^\?/, '').split('&')) {
        if (!part) continue;
        const i = part.indexOf('=');
        const dec = (x) => { try { return decodeURIComponent(x.replace(/\+/g, ' ')); } catch (_) { return x; } };
        this._l.push([dec(i < 0 ? part : part.slice(0, i)), dec(i < 0 ? '' : part.slice(i + 1))]);
      }
    }
    _update() { if (this._url) this._url._set(this._url._p[1] + (this._url._p[4] ? '//' + this._url._p[4] : '') + this._url._p[7] + (this._l.length ? '?' + this : '') + this._url._p[9]); }
    get size() { return this._l.length; }
    get(k) { for (const p of this._l) if (p[0] === String(k)) return p[1]; return null; }
    getAll(k) { return this._l.filter((p) => p[0] === String(k)).map((p) => p[1]); }
    has(k, v) { return this._l.some((p) => p[0] === String(k) && (v === undefined || p[1] === String(v))); }
    set(k, v) {
      k = String(k);
      const i = this._l.findIndex((p) => p[0] === k);
      if (i < 0) this._l.push([k, String(v)]);
      else { this._l[i][1] = String(v); this._l = this._l.filter((p, j) => j <= i || p[0] !== k); }
      this._update();
    }
    append(k, v) { this._l.push([String(k), String(v)]); this._update(); }
    delete(k, v) { this._l = this._l.filter((p) => !(p[0] === String(k) && (v === undefined || p[1] === String(v)))); this._update(); }
    sort() { this._l.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)); this._update(); }
    forEach(f, that) { for (const p of this._l) f.call(that, p[1], p[0], this); }
    keys() { return this._l.map((p) => p[0])[Symbol.iterator](); }
    values() { return this._l.map((p) => p[1])[Symbol.iterator](); }
    entries() { return this._l.map((p) => [p[0], p[1]])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    toString() { const e = (s) => encodeURIComponent(s).replace(/%20/g, '+').replace(/[!'()~]/g, (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase()); return this._l.map((p) => e(p[0]) + '=' + e(p[1])).join('&'); }
  }
  const invalidURL = (s) => new TypeError("Failed to construct 'URL': Invalid URL" + (s ? ": '" + s + "'" : ''));
  class URL {
    constructor(input, base) {
      const p = base === undefined ? __url(String(input), null) : __url(String(input), String(base));
      if (!p) throw invalidURL(String(input));
      this._p = p;
      this._sp = null;
    }
    _set(href) {
      const p = __url(href, null);
      if (p) this._p = p;
      if (this._sp) this._sp._parse(this._p[8]);
    }
    static canParse(i, b) { return (b === undefined ? __url(String(i), null) : __url(String(i), String(b))) !== null; }
    static parse(i, b) { try { return new URL(i, b); } catch (_) { return null; } }
    static createObjectURL(o) { need('fetch'); return $.createObjectURL(o); }
    static revokeObjectURL(u) { need('fetch'); $.revokeObjectURL(u); }
    get searchParams() {
      if (!this._sp) { this._sp = new URLSearchParams(this._p[8]); this._sp._url = this; }
      return this._sp;
    }
    toString() { return this._p[0]; }
    toJSON() { return this._p[0]; }
  }
  // URL setters rebuild the href with the new part and parse it again.
  const rebuild = (p, k, v) => {
    const [, protocol, user, pass, host, hostname, port, path, search, hash] = p;
    const auth = (u, w) => (u || w ? u + (w ? ':' + w : '') + '@' : '');
    const special = /^(https?|wss?|ftp|file):$/.test(protocol);
    v = String(v);
    const parts = { protocol, user, pass, hostname, port, path, search, hash };
    switch (k) {
      case 'protocol': parts.protocol = v.replace(/:?$/, ':'); break;
      case 'username': parts.user = encodeURIComponent(v); break;
      case 'password': parts.pass = encodeURIComponent(v); break;
      case 'host': { const m = /^([^:]*)(?::(\d*))?$/.exec(v); if (m) { parts.hostname = m[1]; if (m[2] !== undefined) parts.port = m[2]; } break; }
      case 'hostname': parts.hostname = v; break;
      case 'port': parts.port = v.replace(/\D.*$/, ''); break;
      case 'pathname': parts.path = (special && !v.startsWith('/') ? '/' : '') + v; break;
      case 'search': parts.search = v === '' || v === '?' ? '' : (v[0] === '?' ? v : '?' + v); break;
      case 'hash': parts.hash = v === '' || v === '#' ? '' : (v[0] === '#' ? v : '#' + v); break;
    }
    const authority = host || hostname || special ? '//' + auth(parts.user, parts.pass) + parts.hostname + (parts.port ? ':' + parts.port : '') : '';
    return parts.protocol + authority + parts.path + parts.search + parts.hash;
  };
  URL_PARTS.forEach((k, i) => {
    Object.defineProperty(URL.prototype, k, {
      configurable: true,
      enumerable: true,
      get() { return this._p[i]; },
      set(v) {
        if (k === 'origin') return;
        if (k === 'href') { const p = __url(String(v), null); if (!p) throw invalidURL(String(v)); this._p = p; if (this._sp) this._sp._parse(p[8]); return; }
        this._set(rebuild(this._p, k, v));
      },
    });
  });

  // `location`: the page's URL. Only changes within the page take effect.
  const entries = [{ url: 'about:blank', state: null }];
  let entry = 0;
  const changeURL = (url, push, state, fireHash) => {
    const old = docURL;
    docURL = url;
    if (push) {
      entries.splice(entry + 1);
      entries.push({ url, state });
      entry = entries.length - 1;
    } else {
      entries[entry] = { url, state };
    }
    if (fireHash && old.split('#')[0] === url.split('#')[0] && old !== url) {
      addTimer(() => fire(g, 'hashchange', { oldURL: old, newURL: url }, events.HashChangeEvent), 0, false, []);
    }
  };
  const navigate = (input, push) => {
    const url = resolveURL(input, docURL);
    if (url === null) throw domError('SyntaxError', "'" + input + "' is not a valid URL.");
    if (url.split('#')[0] === docURL.split('#')[0] && url.includes('#')) changeURL(url, push, null, true);
    else __navigate(url, !push);
  };
  class Location {
    constructor() { illegal(); }
    get href() { return docURL; }
    set href(v) { navigate(String(v), true); }
    assign(u) { navigate(String(u), true); }
    replace(u) { navigate(String(u), false); }
    reload() {}
    toString() { return docURL; }
    get ancestorOrigins() { return staticList([]); }
  }
  URL_PARTS.slice(1).forEach((k, i) => {
    Object.defineProperty(Location.prototype, k, {
      configurable: true,
      enumerable: true,
      get() { return __url(docURL, null)[i + 1]; },
      set(v) { if (k !== 'origin') navigate(rebuild(__url(docURL, null), k, v), true); },
    });
  });
  const location = Object.create(Location.prototype);

  class History {
    constructor() { illegal(); }
    get length() { return entries.length; }
    get state() { return entries[entry].state; }
    get scrollRestoration() { return 'auto'; }
    set scrollRestoration(v) {}
    pushState(state, title, url) { this._go(state, url, true); }
    replaceState(state, title, url) { this._go(state, url, false); }
    _go(state, url, push) {
      const to = url === undefined || url === null ? docURL : resolveURL(String(url), docURL);
      if (to === null) throw domError('SecurityError', "A history state object with URL '" + url + "' cannot be created.");
      if (__url(to, null)[10] !== __url(docURL, null)[10]) throw domError('SecurityError', "A history state object with URL '" + to + "' cannot be created in a document with origin '" + location.origin + "'.");
      changeURL(to, push, state === undefined ? null : state, false);
    }
    go(delta) {
      const to = entry + (+delta || 0);
      if (!delta || to < 0 || to >= entries.length) return;
      addTimer(() => {
        const old = docURL;
        entry = to;
        docURL = entries[to].url;
        fire(g, 'popstate', { state: entries[to].state }, events.PopStateEvent);
        if (old.split('#')[0] === docURL.split('#')[0] && old !== docURL) fire(g, 'hashchange', { oldURL: old, newURL: docURL }, events.HashChangeEvent);
      }, 0, false, []);
    }
    back() { this.go(-1); }
    forward() { this.go(1); }
  }
  const history = Object.create(History.prototype);


  const tasks = [];
  let nextTask = 1, seq = 0, nesting = 0;
  const addTimer = (fn, ms, every, args, kind) => {
    if (typeof fn === 'string') fn = new Function(fn);
    if (typeof fn !== 'function') return 0;
    let delay = Math.max(0, +ms || 0);
    // Deeply nested timers wait at least 4 ms, as in browsers.
    if (nesting >= 5 && delay < 4) delay = 4;
    tasks.push({ id: nextTask, fn, at: __now() + delay, seq: ++seq, every: every ? Math.max(4, delay) : 0, args, kind, nest: nesting + 1, want: ms });
    return nextTask++;
  };
  $.addTimer = addTimer;
  const clearTimer = (id) => {
    const i = tasks.findIndex((t) => t.id === id);
    if (i >= 0) tasks.splice(i, 1);
  };
  // One-shot output, which has no frames from a host, runs due tasks one after
  // another for as long as they come due, and tasks that take longer than the
  // timers they set keep coming due. A turn that has been busy for 250 ms hands
  // control back to the host, so its limit on the page's time holds in a turn too.
  let busySince = -1;
  // The task `tick` ran last and how late it ran, for the host's timer log.
  let ran = null, ranLate = 0;
  // An exception a task threw that could not be reported then.
  let unreported = false, unreportedError;
  // Runs the earliest due task. Returns 0 after running one, else milliseconds
  // until the next, or -1 when there is none.
  const tick = () => {
    if (unreported) {
      unreported = false;
      reportError(unreportedError);
    }
    if ($.intersections) $.intersections();
    for (const [img, src] of loadingImages) {
      if (__image(src) === 0) continue;
      loadingImages.delete(img);
      addTimer(() => imageLoaded(img, src), 0, false, []);
    }
    if (!tasks.length) {
      busySince = -1;
      return -1;
    }
    let best = 0;
    for (let i = 1; i < tasks.length; i++) {
      const t = tasks[i], b = tasks[best];
      if (t.at < b.at || (t.at === b.at && t.seq < b.seq)) best = i;
    }
    const t = tasks[best], now = __now();
    if (t.at > now) {
      busySince = -1;
      return t.at - now;
    }
    if (!framesFromHost) {
      if (busySince < 0) {
        busySince = now;
      } else if (now - busySince > 250) {
        busySince = -1;
        return 0.001;
      }
    }
    ran = t;
    ranLate = now - t.at;
    if (t.every) { t.at = now + t.every; t.seq = ++seq; } else tasks.splice(best, 1);
    nesting = t.nest;
    try {
      if (t.kind === 'raf') t.fn.call(g, now);
      else if (t.kind === 'idle') t.fn.call(g, idleDeadline(now));
      else t.fn.apply(g, t.args);
    } catch (e) {
      // Right after a stack overflow the engine can be left without room for the calls
      // a report makes until this call returns; the next call reports it then, so that
      // the exception does not escape and stop the tasks after this one.
      try {
        reportError(e);
      } catch (_) {
        unreportedError = e;
        unreported = true;
      }
    } finally {
      nesting = 0;
    }
    return 0;
  };
  // In a window the host draws frames: an animation frame callback waits for the next
  // frame, and all of a frame's callbacks get that frame's time. One-shot output has no
  // frames and runs them as 16 ms timers.
  let framesFromHost = false, frameTime = 0, pageHidden = false;
  let frameCallbacks = [], frameBatch = [];
  const requestFrame = (fn) => {
    if (!framesFromHost) return addTimer(fn, 16, false, [], 'raf');
    if (typeof fn === 'string') fn = new Function(fn);
    if (typeof fn !== 'function') return 0;
    frameCallbacks.push({ id: nextTask, fn });
    return nextTask++;
  };
  const cancelFrame = (id) => {
    for (const list of [frameCallbacks, frameBatch]) {
      const i = list.findIndex((c) => c.id === id);
      if (i >= 0) return void list.splice(i, 1);
    }
    clearTimer(id);
  };
  // Idle callbacks get the time left until the next task, up to 50 ms.
  const idleDeadline = (start) => {
    let until = start + 50;
    for (const t of tasks) if (t.kind !== 'idle' && t.at < until) until = Math.max(start, t.at);
    return { didTimeout: false, timeRemaining: () => Math.max(0, until - __now()) };
  };


  const STORAGE_KEYS = Symbol('keys');
  class Storage {
    constructor() { illegal(); }
    getItem(k) { return this[STORAGE_KEYS].has(String(k)) ? this[STORAGE_KEYS].get(String(k)) : null; }
    setItem(k, v) { this[STORAGE_KEYS].set(String(k), String(v)); }
    removeItem(k) { this[STORAGE_KEYS].delete(String(k)); }
    clear() { this[STORAGE_KEYS].clear(); }
    key(i) { return [...this[STORAGE_KEYS].keys()][i] ?? null; }
    get length() { return this[STORAGE_KEYS].size; }
  }
  const newStorage = () => {
    const s = Object.create(Storage.prototype);
    Object.defineProperty(s, STORAGE_KEYS, { value: new Map() });
    return s;
  };

  class DOMRectReadOnly {
    constructor(x = 0, y = 0, width = 0, height = 0) { this.x = +x; this.y = +y; this.width = +width; this.height = +height; }
    get left() { return Math.min(this.x, this.x + this.width); }
    get top() { return Math.min(this.y, this.y + this.height); }
    get right() { return Math.max(this.x, this.x + this.width); }
    get bottom() { return Math.max(this.y, this.y + this.height); }
    toJSON() { const { x, y, width, height, top, right, bottom, left } = this; return { x, y, width, height, top, right, bottom, left }; }
    static fromRect(r = {}) { return new this(r.x, r.y, r.width, r.height); }
  }
  class DOMRect extends DOMRectReadOnly {}
  // The type of `href` on SVG links in browsers; tested with `instanceof`.
  class SVGAnimatedString { constructor() { illegal(); } }
  // The rects the platform makes, built as a literal: the class constructor costs three times as much.
  const rectOf = (r) => ({ __proto__: DOMRect.prototype, x: r[0], y: r[1], width: r[2], height: r[3] });
  class DOMPoint {
    constructor(x = 0, y = 0, z = 0, w = 1) { this.x = +x; this.y = +y; this.z = +z; this.w = +w; }
    static fromPoint(p = {}) { return new this(p.x, p.y, p.z, p.w); }
  }
  class DOMMatrix {
    constructor() { Object.assign(this, { a: 1, b: 0, c: 0, d: 1, e: 0, f: 0, m11: 1, m12: 0, m21: 0, m22: 1, m33: 1, m41: 0, m42: 0, m43: 0, m44: 1, is2D: true, isIdentity: true }); }
    multiply() { return new DOMMatrix(); }
    translate() { return new DOMMatrix(); }
    scale() { return new DOMMatrix(); }
    inverse() { return new DOMMatrix(); }
    transformPoint(p) { return DOMPoint.fromPoint(p); }
    toString() { return 'matrix(1, 0, 0, 1, 0, 0)'; }
  }

  // Lists with `change` listeners, each with whether it matched when last looked at.
  const watchedMedia = new Map();
  class MediaQueryList extends EventTarget {
    constructor() { super(); illegal(); }
    get matches() { return __media(this.media); }
    addListener(f) { this.addEventListener('change', f); }
    removeListener(f) { this.removeEventListener('change', f); }
    addEventListener(type, f, o) {
      if (type === 'change' && !watchedMedia.has(this)) watchedMedia.set(this, this.matches);
      super.addEventListener(type, f, o);
    }
    get onchange() { return this._onchange || null; }
    set onchange(f) {
      this._onchange = typeof f === 'function' ? f : null;
      if (!watchedMedia.has(this)) watchedMedia.set(this, this.matches);
    }
  }
  // `atob` and `btoa` get null from their native for input they cannot convert.
  const invalidCharacter = (s) => {
    if (s === null) throw domError('InvalidCharacterError', 'The string contains invalid characters.');
    return s;
  };
  const matchMedia = (q) => {
    const m = Object.create(MediaQueryList.prototype);
    m.media = String(q);
    return m;
  };
  // The viewport changed size: `resize` on the window, then `change` on each
  // watched list whose match changed.
  const resized = () => {
    const changed = [];
    for (const [m, was] of watchedMedia) {
      const now = m.matches;
      if (now !== was) changed.push(m);
      watchedMedia.set(m, now);
    }
    dispatch(g, new Event('resize'));
    dispatch(visualViewport, new Event('resize'));
    for (const m of changed) dispatch(m, new events.MediaQueryListEvent('change', { matches: watchedMedia.get(m), media: m.media }));
  };

  const marks = [];
  const timeOrigin = Date.now() - __now();
  // Members of Performance.prototype, as in WebIDL.
  class Performance extends EventTarget { constructor() { super(); illegal(); } }
  const timing = { navigationStart: timeOrigin, fetchStart: timeOrigin, responseStart: timeOrigin, responseEnd: timeOrigin, domLoading: timeOrigin, domInteractive: timeOrigin, domContentLoadedEventStart: timeOrigin, domContentLoadedEventEnd: timeOrigin, domComplete: timeOrigin, loadEventStart: timeOrigin, loadEventEnd: timeOrigin };
  const navigation = { type: 0, redirectCount: 0 };
  getters(Performance.prototype, { timeOrigin: () => timeOrigin, timing: () => timing, navigation: () => navigation });
  def(Performance.prototype, {
    now() { return __now(); },
    mark(name, o) { const m = { name: String(name), entryType: 'mark', startTime: o && o.startTime !== undefined ? o.startTime : __now(), duration: 0, detail: o ? o.detail : null }; marks.push(m); return m; },
    measure(name, a, b) {
      const at = (x) => (typeof x === 'string' ? (marks.filter((m) => m.name === x).pop() || { startTime: 0 }).startTime : x);
      const start = a && typeof a === 'object' ? at(a.start) || 0 : at(a) || 0, end = a && typeof a === 'object' ? at(a.end) ?? __now() : (b === undefined ? __now() : at(b));
      const m = { name: String(name), entryType: 'measure', startTime: start, duration: end - start };
      marks.push(m);
      return m;
    },
    getEntries() { return marks.slice(); },
    getEntriesByType(t) { return marks.filter((m) => m.entryType === t); },
    getEntriesByName(n, t) { return marks.filter((m) => m.name === n && (!t || m.entryType === t)); },
    clearMarks(n) { for (let i = marks.length - 1; i >= 0; i--) if (marks[i].entryType === 'mark' && (n === undefined || marks[i].name === n)) marks.splice(i, 1); },
    clearMeasures(n) { for (let i = marks.length - 1; i >= 0; i--) if (marks[i].entryType === 'measure' && (n === undefined || marks[i].name === n)) marks.splice(i, 1); },
    clearResourceTimings: noop,
    setResourceTimingBufferSize: noop,
    toJSON() { return { timeOrigin }; },
  });
  const performance = Object.create(Performance.prototype);

  // The page's own `crypto` dice; seeded when `Math.random` is, so one-shot output repeats.
  let cryptoSeed = 0x2545F491;
  const randomUint32 = () => {
    cryptoSeed = (cryptoSeed + 0x6D2B79F5) | 0;
    let t = Math.imul(cryptoSeed ^ (cryptoSeed >>> 15), 1 | cryptoSeed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return (t ^ (t >>> 14)) >>> 0;
  };
  const crypto = {
    getRandomValues(a) {
      if (!ArrayBuffer.isView(a) || a instanceof Float32Array || a instanceof Float64Array) throw domError('TypeMismatchError', "The provided ArrayBufferView is of type '" + (a && a.constructor && a.constructor.name) + "', which is not an integer array type.");
      if (a.byteLength > 65536) throw domError('QuotaExceededError', "The ArrayBufferView's byte length (" + a.byteLength + ') exceeds the number of bytes of entropy available via this API (65536).');
      const bytes = new Uint8Array(a.buffer, a.byteOffset, a.byteLength);
      for (let i = 0; i < bytes.length; i += 4) {
        const r = randomUint32();
        for (let j = 0; j < 4 && i + j < bytes.length; j++) bytes[i + j] = r >>> (8 * j);
      }
      return a;
    },
    randomUUID() {
      const b = crypto.getRandomValues(new Uint8Array(16));
      b[6] = (b[6] & 0x0f) | 0x40;
      b[8] = (b[8] & 0x3f) | 0x80;
      const h = [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
      return h.slice(0, 8) + '-' + h.slice(8, 12) + '-' + h.slice(12, 16) + '-' + h.slice(16, 20) + '-' + h.slice(20);
    },
    subtle: {},
  };

  class TrustedHTML { constructor() { illegal(); } toString() { return this._s; } toJSON() { return this._s; } }
  class TrustedScript { constructor() { illegal(); } toString() { return this._s; } toJSON() { return this._s; } }
  class TrustedScriptURL { constructor() { illegal(); } toString() { return this._s; } toJSON() { return this._s; } }
  const trusted = (C, s) => { const t = Object.create(C.prototype); t._s = String(s); return t; };
  // Trusted Types with a policy that lets everything through, as without enforcement.
  const trustedTypes = {
    createPolicy(name, rules = {}) {
      const via = (C, f) => (s, ...a) => trusted(C, f ? f(s, ...a) : s);
      return { name, createHTML: via(TrustedHTML, rules.createHTML), createScript: via(TrustedScript, rules.createScript), createScriptURL: via(TrustedScriptURL, rules.createScriptURL) };
    },
    isHTML: (x) => x instanceof TrustedHTML,
    isScript: (x) => x instanceof TrustedScript,
    isScriptURL: (x) => x instanceof TrustedScriptURL,
    get emptyHTML() { return trusted(TrustedHTML, ''); },
    get emptyScript() { return trusted(TrustedScript, ''); },
    getAttributeType: () => null,
    getPropertyType: () => null,
    defaultPolicy: null,
  };

  // `CSS.supports` asks the style engine, as `@supports` does in a browser.
  const supportsCondition = (c) => {
    c = c.trim();
    for (const op of [' or ', ' and ']) {
      let depth = 0;
      for (let i = 0; i < c.length; i++) {
        if (c[i] === '(') depth++;
        else if (c[i] === ')') depth--;
        else if (depth === 0 && c.startsWith(op, i)) {
          const a = supportsCondition(c.slice(0, i)), b = supportsCondition(c.slice(i + op.length));
          return op === ' or ' ? a || b : a && b;
        }
      }
    }
    if (/^not\s/i.test(c)) return !supportsCondition(c.slice(4));
    const sel = /^selector\((.*)\)$/i.exec(c);
    if (sel) { try { __matches(0, sel[1]); return true; } catch (_) { return false; } }
    if (c[0] === '(' && c[c.length - 1] === ')') c = c.slice(1, -1).trim();
    if (c[0] === '(') return supportsCondition(c);
    const i = c.indexOf(':');
    return i > 0 && __supports(c.slice(0, i), c.slice(i + 1));
  };
  const CSS = {
    supports: (a, b) => (b === undefined ? supportsCondition(String(a)) : __supports(String(a), String(b))),
    escape: cssEscape,
    registerProperty: noop,
    highlights: new Map(),
  };

  const console = { log: log(''), info: log(''), debug: log(''), warn: log('warning: '), error: log('error: '), trace: noop, group: noop, groupCollapsed: noop, groupEnd: noop, table: noop, time: noop, timeEnd: noop, timeLog: noop, assert: noop, count: noop, countReset: noop, dir: noop, dirxml: noop, profile: noop, profileEnd: noop, timeStamp: noop, clear: noop };

  const view = (i) => ({ configurable: true, enumerable: true, get: () => __view()[i] });
  const screen = Object.assign(new EventTarget(), {
    get width() { return __view()[0]; },
    get height() { return __view()[1]; },
    get availWidth() { return __view()[0]; },
    get availHeight() { return __view()[1]; },
    availLeft: 0,
    availTop: 0,
    colorDepth: 24,
    pixelDepth: 24,
    isExtended: false,
    orientation: Object.assign(new EventTarget(), { type: 'landscape-primary', angle: 0, onchange: null, lock: () => Promise.reject(domError('NotSupportedError', 'screen.orientation.lock() is not available on this device.')), unlock: noop }),
  });

  // The visual viewport: the layout viewport itself, as the page is never pinch-zoomed.
  class VisualViewport extends EventTarget { constructor() { super(); illegal(); } }
  getters(VisualViewport.prototype, {
    offsetLeft: () => 0,
    offsetTop: () => 0,
    pageLeft: () => 0,
    pageTop: () => __view()[2],
    width: () => __view()[0],
    height: () => __view()[1],
    scale: () => 1,
  });
  const visualViewport = Object.create(VisualViewport.prototype);
  for (const t of ['resize', 'scroll', 'scrollend']) Object.defineProperty(VisualViewport.prototype, 'on' + t, { configurable: true, enumerable: true, get() { return this['_on' + t] ?? null; }, set(v) { this['_on' + t] = typeof v === 'function' ? v : null; } });

  // Sites branch on the engine name; like every browser, claim to be a known one.
  // The attributes are accessors of Navigator.prototype, as in WebIDL.
  class Navigator { constructor() { illegal(); } }
  const navigatorMembers = {
    userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15 render/0.1',
    appVersion: '5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15',
    appName: 'Netscape',
    appCodeName: 'Mozilla',
    product: 'Gecko',
    productSub: '20030107',
    vendor: 'Apple Computer, Inc.',
    vendorSub: '',
    language: 'en-US',
    languages: ['en-US'],
    platform: 'MacIntel',
    hardwareConcurrency: 8,
    maxTouchPoints: 0,
    onLine: false,
    cookieEnabled: true,
    doNotTrack: null,
    webdriver: false,
    pdfViewerEnabled: false,
    plugins: [],
    mimeTypes: [],
    javaEnabled: () => false,
    sendBeacon: () => true,
    vibrate: () => false,
    registerProtocolHandler: noop,
    getGamepads: () => [],
  };
  for (const k of Object.keys(navigatorMembers)) {
    const v = navigatorMembers[k];
    if (typeof v === 'function') def(Navigator.prototype, { [k]: v });
    else Object.defineProperty(Navigator.prototype, k, { configurable: true, enumerable: true, get: () => v });
  }
  const navigator = Object.create(Navigator.prototype);

  const timers = {
    setTimeout: (fn, ms, ...args) => addTimer(fn, ms, false, args),
    setInterval: (fn, ms, ...args) => addTimer(fn, ms, true, args),
    clearTimeout: (id) => clearTimer(id),
    clearInterval: (id) => clearTimer(id),
    requestAnimationFrame: (fn) => requestFrame(fn),
    cancelAnimationFrame: (id) => cancelFrame(id),
    requestIdleCallback: (fn, o) => addTimer(fn, Math.min(1, o && o.timeout || 1), false, [], 'idle'),
    cancelIdleCallback: (id) => clearTimer(id),
    queueMicrotask: (fn) => __microtask(fn),
  };

  let windowOrigin = 'null';
  const postMessage = (data, origin, transfer) => {
    if (data !== null && typeof data === 'object') { need('encoding'); data = g.structuredClone(data, { transfer }); }
    addTimer(() => fire(g, 'message', { data, origin: location.origin, source: g }, events.MessageEvent), 0, false, []);
  };

  // `window` is an EventTarget whose interface object is `Window`.
  const Window = function Window() { illegal(); };
  Window.prototype = Object.create(EventTarget.prototype, { constructor: { value: Window, writable: true, configurable: true }, [Symbol.toStringTag]: { value: 'Window', configurable: true } });
  Object.setPrototypeOf(Window, EventTarget);
  Object.setPrototypeOf(g, Window.prototype);

  // Event handler properties such as `el.oninput`: the handler a script set, else
  // the one the `on<type>` attribute compiles to, else null. Sites test
  // `'oninput' in document` to choose how they listen. Touch handlers stay out,
  // as on a desktop browser.
  const GLOBAL_HANDLERS = ('abort auxclick beforeinput beforetoggle blur cancel canplay canplaythrough change click close contextmenu copy cuechange cut dblclick drag dragend dragenter dragleave dragover dragstart drop durationchange emptied ended error focus formdata input invalid keydown keypress keyup load loadeddata loadedmetadata loadstart mousedown mouseenter mouseleave mousemove mouseout mouseover mouseup paste pause play playing progress ratechange reset resize scroll scrollend securitypolicyviolation seeked seeking select selectionchange selectstart slotchange stalled submit suspend timeupdate toggle volumechange waiting wheel ' +
    'animationcancel animationend animationiteration animationstart transitioncancel transitionend transitionrun transitionstart gotpointercapture lostpointercapture pointercancel pointerdown pointerenter pointerleave pointermove pointerout pointerover pointerup').split(' ');
  const WINDOW_ONLY = ['afterprint', 'beforeprint', 'beforeunload', 'hashchange', 'languagechange', 'message', 'messageerror', 'offline', 'online', 'pagehide', 'pageshow', 'popstate', 'rejectionhandled', 'storage', 'unhandledrejection', 'unload'];
  const ON_SET = Symbol('on');
  const handlerProp = (type, attrHost) => ({
    configurable: true,
    enumerable: true,
    get() {
      const set = this[ON_SET];
      return set && set.has(type) ? set.get(type) : attrHandler(attrHost(this, type), type);
    },
    set(v) {
      if (!Object.prototype.hasOwnProperty.call(this, ON_SET)) Object.defineProperty(this, ON_SET, { value: new Map() });
      this[ON_SET].set(type, typeof v === 'function' || (v !== null && typeof v === 'object') ? v : null);
    },
  });
  const itself = (n) => n, noHost = () => null, bodyFor = (w, type) => (WINDOW_HANDLERS.has(type) ? document.body : null);
  for (const t of GLOBAL_HANDLERS) {
    const own = handlerProp(t, itself);
    Object.defineProperty(HTMLElement.prototype, 'on' + t, own);
    Object.defineProperty(SVGElement.prototype, 'on' + t, own);
    Object.defineProperty(Document.prototype, 'on' + t, handlerProp(t, noHost));
  }
  for (const t of GLOBAL_HANDLERS.concat(WINDOW_ONLY)) Object.defineProperty(g, 'on' + t, handlerProp(t, bodyFor));
  for (const t of ['readystatechange', 'visibilitychange']) Object.defineProperty(Document.prototype, 'on' + t, handlerProp(t, noHost));

  // The page's document is an HTML one, whose interface WebKit reports as HTMLDocument.
  Object.defineProperty(Document.prototype, Symbol.toStringTag, { configurable: true, value: 'HTMLDocument' });
  interfaces({
    globalThis: g, Navigator, Performance, VisualViewport, trustedTypes, CSS, console,
    Window, EventTarget, Node, CharacterData, Text, Comment, CDATASection, ProcessingInstruction, DocumentType, Element, Attr, NamedNodeMap, NodeList, HTMLCollection, DOMTokenList,
    Document, HTMLDocument: Document, XMLDocument: Document, DocumentFragment, ShadowRoot, DOMException, AbortController, AbortSignal, URL, URLSearchParams, webkitURL: URL, Location, History, Storage,
    DOMRect, DOMRectReadOnly, SVGAnimatedString, DOMPoint, DOMPointReadOnly: DOMPoint, DOMMatrix, DOMMatrixReadOnly: DOMMatrix, WebKitCSSMatrix: DOMMatrix, MediaQueryList, MediaError, FontFace,
    TrustedHTML, TrustedScript, TrustedScriptURL, TrustedTypePolicyFactory: Object,
    matchMedia, postMessage,
    getComputedStyle: (el) => computedStyle(el),
    btoa: (s) => invalidCharacter(__btoa(String(s))),
    atob: (s) => invalidCharacter(__atob(String(s))),
    Image: function Image(w, h) { const i = document.createElement('img'); if (w !== undefined) i.width = w; if (h !== undefined) i.height = h; return i; },
    Audio: function Audio(src) { const a = document.createElement('audio'); if (src !== undefined) a.src = src; return a; },
    Option: function Option(text, value, d, selected) { const o = document.createElement('option'); if (text !== undefined) o.text = text; if (value !== undefined) o.value = value; if (selected) o.selected = true; return o; },
    scrollTo: noop, scroll: noop, scrollBy: noop, alert: log('alert: '), confirm: () => false, prompt: () => null, open: () => null, close: noop, focus: noop, blur: noop, print: noop, stop: noop,
    getSelection: () => document.getSelection(),
    reportError: (e) => reportError(e),
  });
  // The attributes of Window are accessors of the window itself, the global object. Four
  // cannot be replaced; [Replaceable] ones become a plain property when a script assigns.
  let windowName = '', windowStatus = '', opener = null;
  const localStorage = newStorage(), sessionStorage = newStorage();
  const fixed = (get, set) => ({ configurable: false, enumerable: true, get, set });
  const attribute = (get, set) => ({ configurable: true, enumerable: true, get, set });
  const replaceable = (k, get) => attribute(get, function (v) { Object.defineProperty(g, k, { configurable: true, enumerable: true, writable: true, value: v }); });
  Object.defineProperties(g, {
    window: fixed(() => g),
    document: fixed(() => document),
    location: fixed(() => location, (v) => { location.href = v; }),
    top: fixed(() => g),
    self: replaceable('self', () => g),
    parent: replaceable('parent', () => g),
    frames: replaceable('frames', () => g),
    length: replaceable('length', () => 0),
    origin: replaceable('origin', () => windowOrigin),
    screen: replaceable('screen', () => screen),
    performance: replaceable('performance', () => performance),
    devicePixelRatio: replaceable('devicePixelRatio', () => 1),
    visualViewport: replaceable('visualViewport', () => visualViewport),
    opener: attribute(() => opener, (v) => { opener = v; }),
    name: attribute(() => windowName, (v) => { windowName = String(v); }),
    status: attribute(() => windowStatus, (v) => { windowStatus = String(v); }),
    closed: attribute(() => false),
    frameElement: attribute(() => null),
    isSecureContext: attribute(() => true),
    crossOriginIsolated: attribute(() => false),
    history: attribute(() => history),
    navigator: attribute(() => navigator),
    crypto: attribute(() => crypto),
    localStorage: attribute(() => localStorage),
    sessionStorage: attribute(() => sessionStorage),
  });
  def(g, timers);
  interfaces(events);
  interfaces(ifaces);
  Object.defineProperties(g, {
    innerWidth: view(0), innerHeight: view(1), outerWidth: view(0), outerHeight: view(1),
    scrollY: view(2), pageYOffset: view(2),
    scrollX: { configurable: true, enumerable: true, get: () => 0 }, pageXOffset: { configurable: true, enumerable: true, get: () => 0 },
    event: { configurable: true, get: () => undefined, set: noop },
  });
  lazy('custom', ['customElements', 'CustomElementRegistry']);
  lazy('observers', ['MutationObserver', 'WebKitMutationObserver', 'MutationRecord', 'IntersectionObserver', 'IntersectionObserverEntry', 'ResizeObserver', 'ResizeObserverEntry', 'ResizeObserverSize', 'PerformanceObserver', 'PerformanceEntry', 'PerformanceObserverEntryList']);
  lazy('traversal', ['NodeFilter', 'TreeWalker', 'NodeIterator', 'Range', 'StaticRange', 'AbstractRange', 'Selection', 'DOMParser', 'XMLSerializer', 'DOMImplementation']);
  lazy('messaging', ['MessageChannel', 'MessagePort', 'BroadcastChannel']);
  lazy('fetch', ['fetch', 'Headers', 'Request', 'Response', 'XMLHttpRequest', 'XMLHttpRequestEventTarget', 'XMLHttpRequestUpload', 'FormData', 'Blob', 'File', 'FileReader', 'ReadableStream', 'WritableStream', 'TransformStream', 'TextEncoderStream', 'TextDecoderStream']);
  lazy('encoding', ['TextEncoder', 'TextDecoder', 'structuredClone'].concat(typeof Intl === 'undefined' ? ['Intl'] : []));
  lazy('animation', ['Animation', 'KeyframeEffect', 'AnimationEffect']);
  lazy('cssom', ['CSSStyleSheet', 'StyleSheet', 'StyleSheetList', 'CSSRule', 'CSSStyleRule', 'CSSRuleList', 'CSSStyleDeclaration']);

  Object.assign($, { events, document, location, docURL: () => docURL, started, idOf, insert, removeNode, toNode, cloneNode, query, wrap, parseInto, illegal, noop, fireError, reportError, OWNER, invoke });


  let readyState = 'loading';
  return {
    start(url) {
      docURL = url;
      entries[0].url = url;
      const p = __url(url, null);
      if (p) windowOrigin = p[10];
    },
    tick,
    frames() { framesFromHost = true; },
    // Starts a frame: the callbacks queued until now are the ones it runs.
    frameStart() {
      frameBatch = frameCallbacks;
      frameCallbacks = [];
      frameTime = __now();
      return frameBatch.length;
    },
    // Runs the frame's next callback; returns how many of its callbacks are left.
    frameRun() {
      const c = frameBatch.shift();
      if (c) {
        try { c.fn.call(g, frameTime); } catch (e) { reportError(e); }
      }
      return frameBatch.length;
    },
    framesQueued() { return frameCallbacks.length; },
    // The task `tick` ran last, for the host's timer log.
    lastTask() {
      const t = ran;
      if (!t) return '';
      const kind = t.kind || (t.every ? 'interval' : 'timeout');
      const src = String(__unbound(t.fn)).slice(0, 160).replace(/\s+/g, ' ');
      return `id ${t.id} ${kind} want ${t.want} nest ${t.nest} late ${ranLate.toFixed(1)} queued ${tasks.length} frames ${frameCallbacks.length} fn ${t.fn.name || '?'}: ${src}`;
    },
    visibility(hidden, quiet) {
      if (pageHidden === hidden) return;
      pageHidden = hidden;
      if (!quiet) fire(document, 'visibilitychange', { bubbles: true });
    },
    dispatch(id, type) {
      const ev = type === 'click'
        ? new events.MouseEvent(type, { bubbles: true, cancelable: true, composed: true, view: g, detail: 1 })
        : new Event(type, { bubbles: true, cancelable: true });
      ev.isTrusted = true;
      return !dispatch(wrap(id), ev);
    },
    // An event that does not bubble reaches other nodes than its target only
    // through capture listeners: with none for its type, and no listener or
    // handler on the target, nothing can see it and it is not dispatched.
    fire(list) {
      for (let i = 0; i < list.length; i += 2) {
        const n = wrap(list[i]), type = list[i + 1];
        const own = n[LISTENERS] && n[LISTENERS].get(type);
        if (!capturing.has(type) && !(own && own.length) && !handlerOf(n, type)) continue;
        const ev = new Event(type);
        ev.isTrusted = true;
        dispatch(n, ev);
      }
    },
    load() {
      readyState = 'interactive';
      fire(document, 'readystatechange');
      fire(document, 'DOMContentLoaded', { bubbles: true });
      readyState = 'complete';
      fire(document, 'readystatechange');
      const ev = new Event('load');
      ev.isTrusted = true;
      dispatch(g, ev);
      const show = new events.PageTransitionEvent('pageshow', { persisted: false });
      dispatch(g, show);
    },
    error(e, filename) { fireError(e, filename === undefined ? docURL : filename); },
    resized,
    rejected(promise, reason) {
      return !dispatch(g, new events.PromiseRejectionEvent('unhandledrejection', { promise, reason, cancelable: true }));
    },
    begin(id) {
      started.add(id);
      currentScript = $.parserScript = wrap(id);
    },
    end() {
      currentScript = $.parserScript = null;
      if ($.parsed) $.parsed();
    },
    fetched(id, result) { if ($.fetched) $.fetched(id, result); },
  };
})(globalThis);
