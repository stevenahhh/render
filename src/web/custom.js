// Custom elements: the registry, upgrades, and the reactions that the tree
// changes in prelude.js report once a definition exists.
(function ($) {
  'use strict';
  const { g, TAG, wrap, def, domError, illegal, reportError, nodes } = $;
  const defs = new Map();
  const byCtor = new Map();
  const waiting = new Map();
  const STATE = Symbol('custom');
  // Marks the element of a construction stack whose constructor already ran.
  const ALREADY = {};
  const RESERVED = new Set(['annotation-xml', 'color-profile', 'font-face', 'font-face-src', 'font-face-uri', 'font-face-format', 'font-face-name', 'missing-glyph']);
  const valid = (name) => /^[a-z][-.0-9_a-z\u00b7\u00c0-\uffff]*-[-.0-9_a-z\u00b7\u00c0-\uffff]*$/.test(name) && !RESERVED.has(name);
  const CUSTOM = 2;

  const call = (el, f, args) => {
    try { f.apply(el, args); } catch (e) { reportError(e); }
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
      for (const id of __tagged(root, CUSTOM)) {
        const el = wrap(id), s = el[STATE];
        if (s) {
          if (s.connected) call(el, s.connected, []);
        } else if (s === undefined) {
          const d = defs.get(el[TAG]);
          if (d) upgrade(el, d);
        }
      }
    },
    disconnecting(root) {
      for (const id of __tagged(root, CUSTOM)) {
        const el = nodes.get(id);
        if (el && el[STATE] && el[STATE].disconnected) leaving.push(el);
      }
    },
    disconnected() {
      if (!leaving.length) return;
      const list = leaving;
      leaving = [];
      for (const el of list) call(el, el[STATE].disconnected, []);
    },
    attr(el, name, old, value) {
      const d = el[STATE];
      if (d && d.changed && d.observed.has(name)) call(el, d.changed, [name, old, value, null]);
    },
    cloned(root) {
      for (const id of __tagged(root, CUSTOM)) {
        const d = defs.get(__tag(id));
        if (d) upgrade(wrap(id), d);
      }
    },
    adopted(node, from, to) {
      for (const id of __tagged(node[$.ID], CUSTOM)) {
        const el = nodes.get(id);
        if (el && el[STATE] && el[STATE].adopted) call(el, el[STATE].adopted, [from, to]);
      }
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
      $.ce = hooks;
      for (const id of __tagged(0, CUSTOM)) if (__tag(id) === name) upgrade(wrap(id), d);
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
      for (const id of __tagged(root[$.ID], CUSTOM)) {
        const d = defs.get(__tag(id));
        if (d) upgrade(wrap(id), d);
      }
    }
  }
  def(g, { customElements: Object.create(CustomElementRegistry.prototype), CustomElementRegistry });
})
