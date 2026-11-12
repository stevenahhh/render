// Style sheets as objects: `<style>` and `<link>` sheets, document.styleSheets,
// and constructed sheets that documents adopt.
(function ($) {
  'use strict';
  const { g, TAG, def, illegal, staticList } = $;

  // The top-level rules of a style sheet's text, comments left out.
  const split = (css) => {
    const out = [];
    let depth = 0, start = 0, quote = '';
    css = css.replace(/\/\*[\s\S]*?\*\//g, '');
    for (let i = 0; i < css.length; i++) {
      const c = css[i];
      if (quote) {
        if (c === '\\') i++;
        else if (c === quote) quote = '';
      } else if (c === '"' || c === "'") quote = c;
      else if (c === '{') depth++;
      else if (c === '}' && --depth === 0) {
        out.push(css.slice(start, i + 1).trim());
        start = i + 1;
      } else if (c === ';' && depth === 0) {
        if (css.slice(start, i).trim()) out.push(css.slice(start, i + 1).trim());
        start = i + 1;
      }
    }
    return out.filter(Boolean);
  };

  class CSSRule {
    constructor() { illegal(); }
    get cssText() { return this._text; }
    get type() { return this._text[0] === '@' ? (/^@media/.test(this._text) ? 4 : /^@font-face/.test(this._text) ? 5 : /^@keyframes/.test(this._text) ? 7 : 0) : 1; }
    get parentStyleSheet() { return this._sheet; }
  }
  class CSSStyleRule extends CSSRule {
    get selectorText() { return this._text.slice(0, this._text.indexOf('{')).trim(); }
    get style() { return { cssText: this._text.slice(this._text.indexOf('{') + 1, -1).trim(), getPropertyValue: () => '' }; }
  }
  const rule = (sheet, text) => {
    const r = Object.create(text[0] === '@' ? CSSRule.prototype : CSSStyleRule.prototype);
    r._text = text;
    r._sheet = sheet;
    return r;
  };

  class StyleSheet { constructor() { illegal(); } }
  class CSSStyleSheet extends StyleSheet {
    constructor(options = {}) {
      return Object.assign(Object.create(CSSStyleSheet.prototype), { _rules: [], ownerNode: null, href: null, title: null, disabled: !!options.disabled, media: { mediaText: String(options.media || '') }, type: 'text/css' });
    }
    get cssRules() { return staticList(this._read().map((t) => rule(this, t))); }
    get rules() { return this.cssRules; }
    _read() { return this.ownerNode && this.ownerNode[TAG] === 'style' ? split(this.ownerNode.textContent) : this._rules; }
    _write(list) {
      if (this.ownerNode) {
        if (this.ownerNode[TAG] === 'style') this.ownerNode.textContent = list.join('\n');
      } else {
        this._rules = list;
        adopted.changed(this);
      }
    }
    insertRule(text, index = 0) {
      const list = this._read();
      if (index > list.length) throw $.domError('IndexSizeError', "Failed to execute 'insertRule' on 'CSSStyleSheet': The index provided (" + index + ') is larger than the maximum index (' + list.length + ').');
      list.splice(index, 0, String(text).trim());
      this._write(list);
      return index;
    }
    deleteRule(index) {
      const list = this._read();
      list.splice(index, 1);
      this._write(list);
    }
    addRule(selector, style, index) { return this.insertRule(selector + '{' + style + '}', index === undefined ? this._read().length : index); }
    removeRule(index) { this.deleteRule(index || 0); }
    replaceSync(text) { this._write(split(String(text))); }
    replace(text) { this.replaceSync(text); return Promise.resolve(this); }
  }

  const sheets = new WeakMap();
  $.sheetOf = (el) => {
    if (el[TAG] === 'link' && !/(^|\s)stylesheet(\s|$)/i.test(el.getAttribute('rel') || '')) return null;
    if (!el.isConnected) return null;
    let s = sheets.get(el);
    if (!s) {
      s = new CSSStyleSheet();
      s.ownerNode = el;
      s.href = el[TAG] === 'link' ? el.href : null;
      sheets.set(el, s);
    }
    return s;
  };
  class StyleSheetList {
    constructor() { illegal(); }
    item(i) { return this[i] || null; }
  }
  $.styleSheets = (doc) => {
    const list = Object.create(StyleSheetList.prototype);
    const found = Array.prototype.map.call(doc.querySelectorAll('style, link[rel~=stylesheet]'), $.sheetOf).filter(Boolean);
    found.forEach((s, i) => { list[i] = s; });
    def(list, { length: found.length });
    list[Symbol.iterator] = function* () { for (let i = 0; i < found.length; i++) yield found[i]; };
    return list;
  };

  // Sheets the document adopted, applied after its own.
  // Shadow roots adopt theirs for their own tree.
  const text = (list) => list.filter((s) => !s.disabled).map((s) => s._rules.join('\n')).join('\n');
  const roots = new Set();
  const adopted = {
    list: [],
    changed(sheet) {
      if (this.list.includes(sheet)) this.apply();
      for (const r of roots) if (r[ADOPTED].includes(sheet)) __adopt(text(r[ADOPTED]), r[$.ID]);
    },
    apply() { __adopt(text(this.list)); },
  };
  const ADOPTED = Symbol('adopted');
  const adoptedProp = {
    configurable: true,
    get() { return this === $.document ? adopted.list : this[ADOPTED] || []; },
    set(v) {
      const list = Array.from(v);
      for (const s of list) if (!(s instanceof CSSStyleSheet) || s.ownerNode) throw new TypeError("Failed to set the 'adoptedStyleSheets' property: Sheets must be constructed.");
      if (this === $.document) {
        adopted.list = list;
        adopted.apply();
      } else {
        Object.defineProperty(this, ADOPTED, { value: list, configurable: true });
        roots.add(this);
        __adopt(text(list), this[$.ID]);
      }
    },
  };
  Object.defineProperty(g.Document.prototype, 'adoptedStyleSheets', adoptedProp);
  Object.defineProperty(g.ShadowRoot.prototype, 'adoptedStyleSheets', adoptedProp);

  class CSSRuleList { constructor() { illegal(); } }
  class CSSStyleDeclaration { constructor() { illegal(); } }
  def(g, { CSSStyleSheet, StyleSheet, StyleSheetList, CSSRule, CSSStyleRule, CSSRuleList, CSSStyleDeclaration });
})
