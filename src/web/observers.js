// MutationObserver, IntersectionObserver, ResizeObserver and PerformanceObserver.
(function ($) {
  'use strict';
  const { g, ID, nodes, def, illegal, reportError, staticList, addTimer } = $;
  const REGS = Symbol('observers');
  const live = [];
  let registrations = 0;
  let queued = false;

  class MutationRecord { constructor() { illegal(); } }
  const record = (type, target, o) => {
    const r = Object.create(MutationRecord.prototype);
    r.type = type;
    r.target = target;
    r.addedNodes = staticList(o.added || []);
    r.removedNodes = staticList(o.removed || []);
    r.previousSibling = o.prev || null;
    r.nextSibling = o.next || null;
    r.attributeName = o.name === undefined ? null : o.name;
    r.attributeNamespace = null;
    r.oldValue = o.old === undefined ? null : o.old;
    return r;
  };

  const deliver = () => {
    queued = false;
    for (const o of live.slice()) {
      if (!o._records.length) continue;
      const records = o._records;
      o._records = [];
      try { o._callback.call(o, records, o); } catch (e) { reportError(e); }
    }
  };

  // Queues a record for every observer of `target` or, with `subtree`, of an ancestor.
  const enqueue = (type, target, o) => {
    const wants = new Map();
    for (let id = target[ID], self = true; id >= 0; id = __link(id, 0), self = false) {
      const n = nodes.get(id);
      const regs = n && n[REGS];
      if (!regs) continue;
      for (const { observer, options: opt } of regs) {
        if (!self && !opt.subtree) continue;
        let old = false;
        if (type === 'attributes') {
          if (!opt.attributes || (opt.attributeFilter && !opt.attributeFilter.includes(o.name))) continue;
          old = !!opt.attributeOldValue;
        } else if (type === 'characterData') {
          if (!opt.characterData) continue;
          old = !!opt.characterDataOldValue;
        } else if (!opt.childList) continue;
        wants.set(observer, wants.get(observer) || old);
      }
    }
    for (const [observer, old] of wants) observer._records.push(record(type, target, old ? o : { ...o, old: undefined }));
    if (wants.size && !queued) {
      queued = true;
      __microtask(deliver);
    }
  };
  const hooks = {
    childList(target, added, removed, prev, next) { enqueue('childList', target, { added, removed, prev, next }); },
    attr(target, name, old) { enqueue('attributes', target, { name, old }); },
    data(target, old) { enqueue('characterData', target, { old }); },
  };

  class MutationObserver {
    constructor(callback) {
      if (typeof callback !== 'function') throw new TypeError("Failed to construct 'MutationObserver': The callback provided as parameter 1 is not a function.");
      this._callback = callback;
      this._records = [];
      this._targets = [];
    }
    observe(target, options = {}) {
      const o = Object.assign({}, options);
      if ((o.attributeOldValue !== undefined || o.attributeFilter !== undefined) && o.attributes === undefined) o.attributes = true;
      if (o.characterDataOldValue !== undefined && o.characterData === undefined) o.characterData = true;
      if (!o.childList && !o.attributes && !o.characterData) throw new TypeError("Failed to execute 'observe' on 'MutationObserver': The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.");
      if (o.attributeFilter) o.attributeFilter = Array.from(o.attributeFilter, (a) => String(a).toLowerCase());
      let regs = target[REGS];
      if (!regs) Object.defineProperty(target, REGS, { value: regs = [] });
      const old = regs.find((r) => r.observer === this);
      if (old) old.options = o;
      else {
        regs.push({ observer: this, options: o });
        this._targets.push(target);
        registrations++;
      }
      if (!live.includes(this)) live.push(this);
      $.mo = hooks;
    }
    disconnect() {
      for (const t of this._targets) {
        const regs = t[REGS];
        const i = regs.findIndex((r) => r.observer === this);
        if (i >= 0) {
          regs.splice(i, 1);
          registrations--;
        }
      }
      this._targets = [];
      this._records = [];
      const i = live.indexOf(this);
      if (i >= 0) live.splice(i, 1);
      if (!registrations) $.mo = null;
    }
    takeRecords() {
      const r = this._records;
      this._records = [];
      return r;
    }
  }

  // Each observer looks at its targets when it starts observing one, and again a
  // frame after the document changed or scrolled, and hears about the targets whose
  // intersection with its root changed since it last looked, as in browsers.
  const viewport = () => new g.DOMRect(0, 0, g.innerWidth, g.innerHeight);
  // `rootMargin` as pixels [top, right, bottom, left]; percentages are of the root's size.
  const margins = (s, root) => {
    const p = String(s || '0px').trim().split(/\s+/);
    const [t, r = t, b = t, l = r] = p;
    const px = (v, size) => (parseFloat(v) || 0) * (v.endsWith('%') ? size / 100 : 1);
    return [px(t, root.height), px(r, root.width), px(b, root.height), px(l, root.width)];
  };
  const watching = new Set();
  let looked = '', looking = false;
  // A scroll moves every target against the viewport as much as a change to the document does.
  const stamp = () => $.version + ' ' + $.attrVersion + ' ' + $.dataVersion + ' ' + g.scrollY;
  const look = () => {
    looking = false;
    looked = stamp();
    for (const o of [...watching]) o._look();
  };
  // Called before each task: once the document changed, a look is due next frame.
  $.intersections = () => {
    if (looking || !watching.size || looked === stamp()) return;
    looking = true;
    addTimer(look, 16, false, [], 'raf');
  };
  // An entry's attributes are getters on its prototype, as in browsers: feature
  // checks look for them there, and the IntersectionObserver polyfill replaces
  // the native observer when they are missing.
  const ENTRY = Symbol('entry');
  class IntersectionObserverEntry {
    constructor(o) { this[ENTRY] = o; }
  }
  $.getters(IntersectionObserverEntry.prototype, Object.fromEntries(['time', 'rootBounds', 'boundingClientRect', 'intersectionRect', 'isIntersecting', 'isVisible', 'intersectionRatio', 'target'].map((k) => [k, function () { return this[ENTRY][k]; }])));
  class IntersectionObserver {
    constructor(callback, options = {}) {
      if (typeof callback !== 'function') throw new TypeError("Failed to construct 'IntersectionObserver': The callback provided as parameter 1 is not a function.");
      this._callback = callback;
      this._targets = [];
      // Per target: [threshold index, intersecting] when last looked at.
      this._seen = new Map();
      this._first = false;
      def(this, { root: options.root || null, rootMargin: options.rootMargin || '0px 0px 0px 0px', scrollMargin: '0px 0px 0px 0px', thresholds: [].concat(options.threshold ?? 0) });
    }
    observe(el) {
      if (this._targets.includes(el)) return;
      this._targets.push(el);
      watching.add(this);
      if (this._first) return;
      this._first = true;
      addTimer(() => {
        this._first = false;
        this._look();
      }, 0, false, []);
    }
    _look() {
      if (!this._targets.length) return;
      const base = this.root ? this.root.getBoundingClientRect() : viewport();
      const [t, r, b, l] = margins(this.rootMargin, base);
      const root = new g.DOMRect(base.x - l, base.y - t, base.width + l + r, base.height + t + b);
      const entries = [];
      for (const el of this._targets) {
        const box = el.getBoundingClientRect();
        const x = Math.max(box.left, root.left), y = Math.max(box.top, root.top);
        const w = Math.min(box.right, root.right) - x, h = Math.min(box.bottom, root.bottom) - y;
        const hit = el.isConnected && w >= 0 && h >= 0;
        const area = box.width * box.height;
        const ratio = hit ? (area ? (w * h) / area : 1) : 0;
        let index = this.thresholds.findIndex((v) => v > ratio);
        if (index < 0) index = this.thresholds.length;
        const seen = this._seen.get(el);
        if (seen && seen[0] === index && seen[1] === hit) continue;
        this._seen.set(el, [index, hit]);
        const inter = hit ? new g.DOMRect(x, y, w, h) : new g.DOMRect(0, 0, 0, 0);
        entries.push(new IntersectionObserverEntry({ target: el, time: performance.now(), rootBounds: root, boundingClientRect: box, intersectionRect: inter, isIntersecting: hit, isVisible: false, intersectionRatio: ratio }));
      }
      if (!entries.length) return;
      try { this._callback.call(this, entries, this); } catch (e) { reportError(e); }
    }
    unobserve(el) {
      this._targets = this._targets.filter((x) => x !== el);
      this._seen.delete(el);
      if (!this._targets.length) watching.delete(this);
    }
    disconnect() {
      this._targets = [];
      this._seen.clear();
      watching.delete(this);
    }
    takeRecords() { return []; }
  }

  class ResizeObserverSize {
    constructor(inline, block) { this.inlineSize = inline; this.blockSize = block; }
  }
  class ResizeObserverEntry {
    constructor(el) {
      const r = el.getBoundingClientRect();
      const size = [new ResizeObserverSize(r.width, r.height)];
      def(this, { target: el, contentRect: new g.DOMRectReadOnly(0, 0, r.width, r.height), borderBoxSize: size, contentBoxSize: size, devicePixelContentBoxSize: size });
    }
  }
  class ResizeObserver {
    constructor(callback) {
      if (typeof callback !== 'function') throw new TypeError("Failed to construct 'ResizeObserver': The callback provided as parameter 1 is not a function.");
      this._callback = callback;
      this._targets = [];
      this._pending = [];
    }
    observe(el) {
      if (this._targets.includes(el)) return;
      this._targets.push(el);
      this._pending.push(el);
      if (this._pending.length === 1) addTimer(() => this._notify(), 16, false, [], 'raf');
    }
    _notify() {
      const pending = this._pending.filter((el) => this._targets.includes(el));
      this._pending = [];
      if (!pending.length) return;
      try { this._callback.call(this, pending.map((el) => new ResizeObserverEntry(el)), this); } catch (e) { reportError(e); }
    }
    unobserve(el) { this._targets = this._targets.filter((x) => x !== el); }
    disconnect() { this._targets = []; this._pending = []; }
  }

  class PerformanceEntry { constructor() { illegal(); } }
  class PerformanceObserverEntryList {
    constructor(list) { this._list = list; }
    getEntries() { return this._list.slice(); }
    getEntriesByType(t) { return this._list.filter((e) => e.entryType === t); }
    getEntriesByName(n) { return this._list.filter((e) => e.name === n); }
  }
  class PerformanceObserver {
    constructor(callback) { this._callback = callback; }
    observe() {}
    disconnect() {}
    takeRecords() { return []; }
    static get supportedEntryTypes() { return ['mark', 'measure', 'navigation', 'resource', 'paint']; }
  }
  const performance = g.performance;

  $.interfaces({ MutationObserver, WebKitMutationObserver: MutationObserver, MutationRecord, IntersectionObserver, IntersectionObserverEntry, ResizeObserver, ResizeObserverEntry, ResizeObserverSize, PerformanceObserver, PerformanceEntry, PerformanceObserverEntryList });
})
