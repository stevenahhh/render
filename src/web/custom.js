// Custom elements: the registry, upgrades, and the reactions that the tree
// changes in prelude.js report once a definition exists.
(function ($) {
  'use strict';
  const { g, TAG, wrap, def, domError, illegal, reportError, nodes } = $;
  const defs = new Map();
  const byCtor = new Map();
  const waiting = new Map();
  const STATE = Symbol('custom');
  const QUEUE = Symbol('reactions');
  // Marks the element of a construction stack whose constructor already ran.
  const ALREADY = {};
  const RESERVED = new Set(['annotation-xml', 'color-profile', 'font-face', 'font-face-src', 'font-face-uri', 'font-face-format', 'font-face-name', 'missing-glyph']);
  const valid = (name) => /^[a-z][-.0-9_a-z\u00b7\u00c0-\uffff]*-[-.0-9_a-z\u00b7\u00c0-\uffff]*$/.test(name) && !RESERVED.has(name);
  const CUSTOM = 2;

  const call = (el, f, args) => {
    try { f.apply(el, args); } catch (e) { reportError(e); }
  };

  // Custom element reactions, as the spec queues them: each element keeps its
  // own queue, and a tree change runs the queues of the elements it touched once
  // it is done. A callback that changes the tree runs the queues of the elements
  // it touched itself, so an element that a callback moves sees its callbacks in
  // the order they arose (connected, disconnected, connected) and none twice.
  const enqueue = (el, reaction, frame) => {
    (el[QUEUE] || (el[QUEUE] = [])).push(reaction);
    frame.push(el);
  };
  const run = (frame) => {
    for (const el of frame) {
      const q = el[QUEUE];
      while (q && q.length) q.shift()();
    }
  };
  const upgradeAll = (ids) => {
    const frame = [];
    for (const id of ids) {
      const d = defs.get(__tag(id));
      if (d) {
        const el = wrap(id);
        enqueue(el, () => upgrade(el, d), frame);
      }
    }
    run(frame);
  };

  const upgrade = (el, d) => {
    if (el[STATE] !== undefined) return;
    el[STATE] = false;
    d.stack.push(el);
    try {
      const made = Reflect.construct(d.ctor, []);
      if (made !== el) throw new TypeError('Custom element constructors must return the element they are given.');
    } catch (e) {
      d.stack.pop();
      reportError(e);
      return;
    }
    d.stack.pop();
    el[STATE] = d;
    if (d.changed) {
      for (const name of el.getAttributeNames()) if (d.observed.has(name)) call(el, d.changed, [name, null, el.getAttribute(name), null]);
    }
    if (d.connected && el.isConnected) call(el, d.connected, []);
  };

  let leaving = [];
  const hooks = {
    // HTMLElement's constructor: the element being upgraded, or a new one.
    construct(nt) {
      const d = byCtor.get(nt);
      if (!d) illegal();
      const proto = nt.prototype;
      if (!d.stack.length) {
        const el = wrap(__create(d.name, ''));
        Object.setPrototypeOf(el, proto);
        el[STATE] = d;
        return el;
      }
      const el = d.stack[d.stack.length - 1];
      if (el === ALREADY) throw domError('InvalidStateError', 'This custom element is already constructed.');
      Object.setPrototypeOf(el, proto);
      d.stack[d.stack.length - 1] = ALREADY;
      return el;
    },
    create(name) {
      const d = defs.get(name);
      if (!d) return null;
      try {
        return Reflect.construct(d.ctor, []);
      } catch (e) {
        reportError(e);
        const el = wrap(__create(name, ''));
        el[STATE] = false;
        return el;
      }
    },
    connected(root) {
      const frame = [];
      for (const id of __tagged(root, CUSTOM)) {
        const el = wrap(id), s = el[STATE];
        if (s) {
          if (s.connected) enqueue(el, () => call(el, s.connected, []), frame);
        } else if (s === undefined) {
          const d = defs.get(el[TAG]);
          if (d) enqueue(el, () => upgrade(el, d), frame);
        }
      }
      run(frame);
    },
    // Queues the reactions of elements about to leave the document; `disconnected`
    // runs them once they have left.
    disconnecting(root) {
      for (const id of __tagged(root, CUSTOM)) {
        const el = nodes.get(id), s = el && el[STATE];
        if (s && s.disconnected) enqueue(el, () => call(el, s.disconnected, []), leaving);
      }
    },
    disconnected() {
      if (!leaving.length) return;
      const frame = leaving;
      leaving = [];
      run(frame);
    },
    attr(el, name, old, value) {
      const d = el[STATE];
      if (d && d.changed && d.observed.has(name)) {
        const frame = [];
        enqueue(el, () => call(el, d.changed, [name, old, value, null]), frame);
        run(frame);
      }
    },
    cloned(root) {
      upgradeAll(__tagged(root, CUSTOM));
    },
    adopted(node, from, to) {
      const frame = [];
      for (const id of __tagged(node[$.ID], CUSTOM)) {
        const el = nodes.get(id), s = el && el[STATE];
        if (s && s.adopted) enqueue(el, () => call(el, s.adopted, [from, to]), frame);
      }
      run(frame);
    },
  };

  class CustomElementRegistry {
    constructor() { illegal(); }
    define(name, ctor, options) {
      name = String(name);
      if (typeof ctor !== 'function') throw new TypeError("Failed to execute 'define' on 'CustomElementRegistry': parameter 2 is not a constructor.");
      if (!valid(name)) throw domError('SyntaxError', "Failed to execute 'define' on 'CustomElementRegistry': \"" + name + '" is not a valid custom element name');
      if (defs.has(name)) throw domError('NotSupportedError', "Failed to execute 'define' on 'CustomElementRegistry': the name \"" + name + '" has already been used with this registry');
      if (byCtor.has(ctor)) throw domError('NotSupportedError', "Failed to execute 'define' on 'CustomElementRegistry': this constructor has already been used with this registry");
      const proto = ctor.prototype;
      if (proto === null || typeof proto !== 'object') throw new TypeError("Failed to execute 'define' on 'CustomElementRegistry': The prototype of the constructor is not an object.");
      const callback = (k) => {
        const f = proto[k];
        if (f !== undefined && typeof f !== 'function') throw new TypeError("Failed to execute 'define' on 'CustomElementRegistry': The '" + k + "' callback is not a function.");
        return f;
      };
      const d = { name, ctor, stack: [], observed: new Set(), connected: callback('connectedCallback'), disconnected: callback('disconnectedCallback'), adopted: callback('adoptedCallback'), changed: callback('attributeChangedCallback') };
      if (d.changed) {
        const list = ctor.observedAttributes;
        if (list !== undefined && list !== null) for (const a of list) d.observed.add(String(a));
      }
      if (options && options.extends) d.extends = String(options.extends);
      defs.set(name, d);
      byCtor.set(ctor, d);
      __define(name);
      $.ce = hooks;
      upgradeAll(__tagged(0, CUSTOM).filter((id) => __tag(id) === name));
      const resolve = waiting.get(name);
      if (resolve) {
        waiting.delete(name);
        resolve.forEach((r) => r(ctor));
      }
    }
    get(name) { const d = defs.get(String(name)); return d ? d.ctor : undefined; }
    getName(ctor) { const d = byCtor.get(ctor); return d ? d.name : null; }
    whenDefined(name) {
      name = String(name);
      if (!valid(name)) return Promise.reject(domError('SyntaxError', "Failed to execute 'whenDefined' on 'CustomElementRegistry': \"" + name + '" is not a valid custom element name'));
      const d = defs.get(name);
      if (d) return Promise.resolve(d.ctor);
      return new Promise((r) => {
        const list = waiting.get(name);
        if (list) list.push(r); else waiting.set(name, [r]);
      });
    }
    upgrade(root) {
      upgradeAll(__tagged(root[$.ID], CUSTOM));
    }
  }
  $.interfaces({ customElements: Object.create(CustomElementRegistry.prototype), CustomElementRegistry });
})
