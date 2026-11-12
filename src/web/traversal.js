// NodeFilter, TreeWalker, NodeIterator, Range, Selection, DOMParser,
// XMLSerializer and document.implementation.
(function ($) {
  'use strict';
  const { g, def, illegal, domError } = $;
  const NodeFilter = { FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3, SHOW_ALL: 0xffffffff, SHOW_ELEMENT: 0x1, SHOW_ATTRIBUTE: 0x2, SHOW_TEXT: 0x4, SHOW_CDATA_SECTION: 0x8, SHOW_ENTITY_REFERENCE: 0x10, SHOW_ENTITY: 0x20, SHOW_PROCESSING_INSTRUCTION: 0x40, SHOW_COMMENT: 0x80, SHOW_DOCUMENT: 0x100, SHOW_DOCUMENT_TYPE: 0x200, SHOW_DOCUMENT_FRAGMENT: 0x400, SHOW_NOTATION: 0x800 };
  const ACCEPT = 1, REJECT = 2, SKIP = 3;

  const accept = (w, n) => {
    if (!(w.whatToShow & (1 << (n.nodeType - 1)))) return SKIP;
    const f = w.filter;
    if (!f) return ACCEPT;
    return typeof f === 'function' ? f.call(null, n) : f.acceptNode(n);
  };

  class TreeWalker {
    constructor() { illegal(); }
    parentNode() {
      for (let n = this.currentNode; n && n !== this.root;) {
        n = n.parentNode;
        if (n && accept(this, n) === ACCEPT) return (this.currentNode = n);
      }
      return null;
    }
    _child(first) {
      let n = first ? this.currentNode.firstChild : this.currentNode.lastChild;
      while (n) {
        const r = accept(this, n);
        if (r === ACCEPT) return (this.currentNode = n);
        const c = r === SKIP && (first ? n.firstChild : n.lastChild);
        if (c) { n = c; continue; }
        while (n) {
          const s = first ? n.nextSibling : n.previousSibling;
          if (s) { n = s; break; }
          const p = n.parentNode;
          if (!p || p === this.root || p === this.currentNode) return null;
          n = p;
        }
      }
      return null;
    }
    firstChild() { return this._child(true); }
    lastChild() { return this._child(false); }
    _sibling(next) {
      let n = this.currentNode;
      if (n === this.root) return null;
      for (;;) {
        let s = next ? n.nextSibling : n.previousSibling;
        while (s) {
          n = s;
          const r = accept(this, n);
          if (r === ACCEPT) return (this.currentNode = n);
          s = next ? n.firstChild : n.lastChild;
          if (r === REJECT || !s) s = next ? n.nextSibling : n.previousSibling;
        }
        n = n.parentNode;
        if (!n || n === this.root || accept(this, n) === ACCEPT) return null;
      }
    }
    nextSibling() { return this._sibling(true); }
    previousSibling() { return this._sibling(false); }
    nextNode() {
      let n = this.currentNode, r = ACCEPT;
      for (;;) {
        while (r !== REJECT && n.firstChild) {
          n = n.firstChild;
          r = accept(this, n);
          if (r === ACCEPT) return (this.currentNode = n);
        }
        let s = null;
        for (let t = n; t; t = t.parentNode) {
          if (t === this.root) return null;
          s = t.nextSibling;
          if (s) break;
        }
        if (!s) return null;
        n = s;
        r = accept(this, n);
        if (r === ACCEPT) return (this.currentNode = n);
      }
    }
    previousNode() {
      let n = this.currentNode;
      while (n !== this.root) {
        let s = n.previousSibling;
        while (s) {
          n = s;
          let r = accept(this, n);
          while (r !== REJECT && n.lastChild) {
            n = n.lastChild;
            r = accept(this, n);
          }
          if (r === ACCEPT) return (this.currentNode = n);
          s = n.previousSibling;
        }
        const p = n.parentNode;
        if (n === this.root || !p) return null;
        n = p;
        if (accept(this, n) === ACCEPT) return (this.currentNode = n);
      }
      return null;
    }
  }
  const walker = (C, root, what, filter) => {
    if (!root || root[$.ID] === undefined) throw new TypeError("Failed to execute 'createTreeWalker' on 'Document': parameter 1 is not of type 'Node'.");
    const w = Object.create(C.prototype);
    w.root = root;
    w.whatToShow = what === undefined ? 0xffffffff : what >>> 0;
    w.filter = filter || null;
    w.currentNode = root;
    return w;
  };

  class NodeIterator {
    constructor() { illegal(); }
    get referenceNode() { return this._w.currentNode; }
    get pointerBeforeReferenceNode() { return this._before; }
    nextNode() {
      if (this._before) {
        this._before = false;
        if (accept(this, this._w.currentNode) === ACCEPT) return this._w.currentNode;
      }
      return this._w.nextNode();
    }
    previousNode() {
      if (!this._before) {
        this._before = true;
        if (accept(this, this._w.currentNode) === ACCEPT) return this._w.currentNode;
      }
      return this._w.previousNode();
    }
    detach() {}
  }

  class AbstractRange { constructor() { illegal(); } }
  class StaticRange extends AbstractRange {
    constructor(init) {
      return Object.assign(Object.create(StaticRange.prototype), { startContainer: init.startContainer, startOffset: init.startOffset, endContainer: init.endContainer, endOffset: init.endOffset, collapsed: init.startContainer === init.endContainer && init.startOffset === init.endOffset });
    }
  }
  const len = (n) => (n.nodeType === 3 || n.nodeType === 8 ? n.data.length : n.childNodes.length);
  class Range extends AbstractRange {
    constructor() {
      return Object.assign(Object.create(Range.prototype), { startContainer: $.document, startOffset: 0, endContainer: $.document, endOffset: 0 });
    }
    get collapsed() { return this.startContainer === this.endContainer && this.startOffset === this.endOffset; }
    get commonAncestorContainer() {
      for (let n = this.startContainer; n; n = n.parentNode) if (n === this.endContainer || n.contains(this.endContainer)) return n;
      return $.document;
    }
    setStart(n, o) {
      if (o > len(n)) throw domError('IndexSizeError', "Failed to execute 'setStart' on 'Range': The offset " + o + ' is larger than the node\'s length.');
      this.startContainer = n;
      this.startOffset = o;
      if (this.endContainer.getRootNode() !== n.getRootNode() || n.compareDocumentPosition(this.endContainer) & 2 || (n === this.endContainer && this.endOffset < o)) this.collapse(true);
    }
    setEnd(n, o) {
      if (o > len(n)) throw domError('IndexSizeError', "Failed to execute 'setEnd' on 'Range': The offset " + o + ' is larger than the node\'s length.');
      this.endContainer = n;
      this.endOffset = o;
      if (this.startContainer.getRootNode() !== n.getRootNode() || n.compareDocumentPosition(this.startContainer) & 4 || (n === this.startContainer && this.startOffset > o)) this.collapse(false);
    }
    _index(n) { return Array.prototype.indexOf.call(n.parentNode.childNodes, n); }
    setStartBefore(n) { this.setStart(n.parentNode, this._index(n)); }
    setStartAfter(n) { this.setStart(n.parentNode, this._index(n) + 1); }
    setEndBefore(n) { this.setEnd(n.parentNode, this._index(n)); }
    setEndAfter(n) { this.setEnd(n.parentNode, this._index(n) + 1); }
    selectNode(n) { const i = this._index(n); this.setStart(n.parentNode, i); this.setEnd(n.parentNode, i + 1); }
    selectNodeContents(n) { this.setStart(n, 0); this.setEnd(n, len(n)); }
    collapse(toStart) {
      if (toStart) { this.endContainer = this.startContainer; this.endOffset = this.startOffset; } else { this.startContainer = this.endContainer; this.startOffset = this.endOffset; }
    }
    cloneRange() { return Object.assign(new Range(), { startContainer: this.startContainer, startOffset: this.startOffset, endContainer: this.endContainer, endOffset: this.endOffset }); }
    detach() {}
    // The nodes wholly inside the range, in tree order, when its ends share a parent.
    _children() {
      const c = this.startContainer;
      if (c !== this.endContainer || c.nodeType === 3) return null;
      return Array.prototype.slice.call(c.childNodes, this.startOffset, this.endOffset);
    }
    deleteContents() {
      const c = this.startContainer;
      if (c === this.endContainer && (c.nodeType === 3 || c.nodeType === 8)) {
        c.deleteData(this.startOffset, this.endOffset - this.startOffset);
      } else {
        const kids = this._children();
        if (kids) kids.forEach((k) => k.remove());
      }
      this.collapse(true);
    }
    extractContents() {
      const f = $.document.createDocumentFragment();
      const c = this.startContainer;
      if (c === this.endContainer && (c.nodeType === 3 || c.nodeType === 8)) {
        f.appendChild($.document.createTextNode(c.data.slice(this.startOffset, this.endOffset)));
        c.deleteData(this.startOffset, this.endOffset - this.startOffset);
      } else {
        const kids = this._children();
        if (kids) kids.forEach((k) => f.appendChild(k));
      }
      this.collapse(true);
      return f;
    }
    cloneContents() {
      const f = $.document.createDocumentFragment();
      const c = this.startContainer;
      if (c === this.endContainer && (c.nodeType === 3 || c.nodeType === 8)) f.appendChild($.document.createTextNode(c.data.slice(this.startOffset, this.endOffset)));
      else (this._children() || []).forEach((k) => f.appendChild(k.cloneNode(true)));
      return f;
    }
    insertNode(n) {
      const c = this.startContainer;
      if (c.nodeType === 3) {
        const rest = c.splitText(this.startOffset);
        c.parentNode.insertBefore(n, rest);
      } else {
        c.insertBefore(n, c.childNodes[this.startOffset] || null);
      }
    }
    surroundContents(n) {
      n.appendChild(this.extractContents());
      this.insertNode(n);
      this.selectNode(n);
    }
    createContextualFragment(html) {
      const t = $.document.createElement('template');
      t.innerHTML = String(html);
      const f = t.content;
      for (const id of __tagged(f[$.ID], 1)) $.started.delete(id);
      return f;
    }
    compareBoundaryPoints(how, other) {
      const pos = (a, ao, b, bo) => (a === b ? Math.sign(ao - bo) : a.compareDocumentPosition(b) & 4 ? -1 : 1);
      const [a, ao, b, bo] = [
        [this.startContainer, this.startOffset, other.startContainer, other.startOffset],
        [this.endContainer, this.endOffset, other.startContainer, other.startOffset],
        [this.endContainer, this.endOffset, other.endContainer, other.endOffset],
        [this.startContainer, this.startOffset, other.endContainer, other.endOffset],
      ][how];
      return pos(a, ao, b, bo);
    }
    comparePoint(n, o) { return n === this.startContainer && o < this.startOffset ? -1 : n === this.endContainer && o > this.endOffset ? 1 : 0; }
    isPointInRange(n, o) { return this.comparePoint(n, o) === 0; }
    intersectsNode(n) { const c = this.commonAncestorContainer; return c === n || c.contains(n) || n.contains(c); }
    getBoundingClientRect() {
      const c = this.startContainer;
      return c.nodeType === 1 ? c.getBoundingClientRect() : c.parentElement ? c.parentElement.getBoundingClientRect() : new g.DOMRect();
    }
    getClientRects() { const r = this.getBoundingClientRect(); return r.width || r.height ? [r] : []; }
    toString() {
      const c = this.startContainer;
      if (c === this.endContainer && (c.nodeType === 3 || c.nodeType === 8)) return c.data.slice(this.startOffset, this.endOffset);
      return (this._children() || []).map((k) => k.textContent).join('');
    }
  }
  Object.assign(Range, { START_TO_START: 0, START_TO_END: 1, END_TO_END: 2, END_TO_START: 3 });

  class Selection {
    constructor() { illegal(); }
    get rangeCount() { return this._ranges.length; }
    get isCollapsed() { return !this._ranges.length || this._ranges[0].collapsed; }
    get type() { return !this._ranges.length ? 'None' : this.isCollapsed ? 'Caret' : 'Range'; }
    get anchorNode() { return this._ranges.length ? this._ranges[0].startContainer : null; }
    get anchorOffset() { return this._ranges.length ? this._ranges[0].startOffset : 0; }
    get focusNode() { return this._ranges.length ? this._ranges[0].endContainer : null; }
    get focusOffset() { return this._ranges.length ? this._ranges[0].endOffset : 0; }
    getRangeAt(i) {
      if (!this._ranges[i]) throw domError('IndexSizeError', "Failed to execute 'getRangeAt' on 'Selection': " + i + ' is not a valid index.');
      return this._ranges[i];
    }
    addRange(r) { this._ranges = [r]; }
    removeRange() { this._ranges = []; }
    removeAllRanges() { this._ranges = []; }
    empty() { this._ranges = []; }
    collapse(n, o = 0) { if (!n) { this._ranges = []; return; } const r = new Range(); r.setStart(n, o); this._ranges = [r]; }
    setPosition(n, o) { this.collapse(n, o); }
    collapseToStart() { if (this._ranges.length) this._ranges[0].collapse(true); }
    collapseToEnd() { if (this._ranges.length) this._ranges[0].collapse(false); }
    selectAllChildren(n) { const r = new Range(); r.selectNodeContents(n); this._ranges = [r]; }
    extend() {}
    modify() {}
    containsNode() { return false; }
    deleteFromDocument() { if (this._ranges.length) this._ranges[0].deleteContents(); }
    toString() { return this._ranges.length ? this._ranges[0].toString() : ''; }
  }
  const selection = Object.create(Selection.prototype);
  selection._ranges = [];

  class DOMImplementation {
    constructor() { illegal(); }
    createHTMLDocument(title) { return $.newDocument(true, title); }
    createDocument(ns, qualifiedName) {
      const d = $.newDocument(false);
      if (qualifiedName) d.appendChild(d.createElementNS(ns, qualifiedName));
      return d;
    }
    createDocumentType(name, publicId = '', systemId = '') {
      const t = Object.create(g.DocumentType.prototype);
      def(t, { name: String(name), publicId: String(publicId), systemId: String(systemId) });
      return t;
    }
    hasFeature() { return true; }
  }
  $.implementation = Object.create(DOMImplementation.prototype);

  class DOMParser {
    parseFromString(markup, type) {
      const d = $.newDocument(false);
      const root = d.appendChild(d.createElement('html'));
      $.parseInto(root, String(markup));
      if (!/^(application\/(xhtml\+)?xml|text\/xml|image\/svg\+xml)$/.test(String(type))) {
        const head = root.querySelector(':scope > head') || root.insertBefore(d.createElement('head'), root.firstChild);
        if (!root.querySelector(':scope > body')) {
          const body = d.createElement('body');
          for (const k of Array.from(root.childNodes)) if (k !== head) body.appendChild(k);
          root.appendChild(body);
        }
      }
      return d;
    }
  }
  class XMLSerializer {
    serializeToString(n) { return n.nodeType === 9 || n.nodeType === 11 ? Array.from(n.childNodes, (c) => this.serializeToString(c)).join('') : n.nodeType === 1 ? n.outerHTML : n.nodeType === 8 ? '<!--' + n.data + '-->' : n.textContent; }
  }

  def(g.Document.prototype, {
    createTreeWalker(root, what, filter) { return walker(TreeWalker, root, what, filter); },
    createNodeIterator(root, what, filter) {
      const it = Object.create(NodeIterator.prototype);
      it._w = walker(TreeWalker, root, what, filter);
      def(it, { root, whatToShow: it._w.whatToShow, filter: filter || null });
      it._before = true;
      return it;
    },
    createRange() { return new Range(); },
    getSelection() { return selection; },
  });
  def(g, { NodeFilter, TreeWalker, NodeIterator, Range, StaticRange, AbstractRange, Selection, DOMParser, XMLSerializer, DOMImplementation });
})
