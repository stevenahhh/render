// Shadow roots and slots. A shadow root is a fragment the host owns; layout
// draws the host's shadow tree with its children placed in their slots.
(function ($) {
  'use strict';
  const { g, ID, TAG, wrap, def, domError } = $;
  const ROOT = Symbol('shadowRoot');
  const HOSTS = /^(article|aside|blockquote|body|div|footer|h[1-6]|header|main|nav|p|section|span)$/;

  def(g.Element.prototype, {
    attachShadow(init) {
      const mode = init && init.mode;
      if (mode !== 'open' && mode !== 'closed') throw new TypeError("Failed to execute 'attachShadow' on 'Element': The provided value '" + mode + "' is not a valid enum value of type ShadowRootMode.");
      const tag = this[TAG];
      if (!(this instanceof g.HTMLElement) || !(tag.includes('-') || HOSTS.test(tag))) throw domError('NotSupportedError', "Failed to execute 'attachShadow' on 'Element': This element does not support attachShadow");
      if (this[ROOT]) throw domError('NotSupportedError', "Failed to execute 'attachShadow' on 'Element': Shadow root cannot be created on a host which already hosts a shadow tree.");
      const root = wrap(__attach_shadow(this[ID]));
      def(root, { mode, host: this, delegatesFocus: !!init.delegatesFocus, slotAssignment: init.slotAssignment || 'named', clonable: !!init.clonable, serializable: !!init.serializable });
      Object.defineProperty(this, ROOT, { value: root });
      return root;
    },
  });
  $.shadowOf = (el) => (el[ROOT] && el[ROOT].mode === 'open' ? el[ROOT] : null);

  const html = Object.getOwnPropertyDescriptor(g.Element.prototype, 'innerHTML');
  Object.defineProperties(g.ShadowRoot.prototype, {
    innerHTML: { configurable: true, get() { return __html(this[ID], false); }, set(v) { $.parseInto(this, v === null ? '' : String(v)); } },
    activeElement: { configurable: true, get() { const a = $.document.activeElement; return a && this.contains(a) ? a : null; } },
  });
  def(g.ShadowRoot.prototype, { getHTML() { return html.get.call(this); }, setHTMLUnsafe(v) { this.innerHTML = v; } });

  // The children of the host that a slot shows: those naming it with `slot`,
  // or for the default slot those naming none. Only the first slot of a name gets them.
  const assigned = (slot) => {
    const root = slot.getRootNode();
    const host = root && root.host;
    if (!host) return [];
    const name = slot.getAttribute('name') || '';
    const first = Array.prototype.find.call(root.querySelectorAll('slot'), (s) => (s.getAttribute('name') || '') === name);
    if (first !== slot) return [];
    return Array.prototype.filter.call(host.childNodes, (n) => (n.nodeType === 1 ? n.getAttribute('slot') || '' : n.nodeType === 3 ? '' : null) === name);
  };
  $.assignedSlot = (node) => {
    const host = node.parentNode;
    const root = host && host[ROOT];
    if (!root || root.mode !== 'open') return null;
    const name = node.nodeType === 1 ? node.getAttribute('slot') || '' : '';
    return Array.prototype.find.call(root.querySelectorAll('slot'), (s) => (s.getAttribute('name') || '') === name) || null;
  };
  def(g.HTMLSlotElement.prototype, {
    assignedNodes(options) {
      const nodes = assigned(this);
      if (!options || !options.flatten) return nodes;
      const out = [];
      for (const n of nodes.length ? nodes : Array.from(this.childNodes)) {
        if (n[TAG] === 'slot') out.push(...n.assignedNodes(options));
        else out.push(n);
      }
      return out;
    },
    assignedElements(options) { return this.assignedNodes(options).filter((n) => n.nodeType === 1); },
    assign() {},
  });
})
