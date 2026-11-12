// document.write, writeln, open and close. Markup a script of the parser
// writes goes in right after that script (or, for one in <head>, into <body>
// once it is not head content), parsed as soon as the elements it opened are
// closed. Written after the document loaded, markup replaces the document.
(function ($) {
  'use strict';
  const { g, ID, def } = $;
  const LEFT_OPEN = 8;
  const HEAD_CONTENT = /^(script|style|link|meta|title|base|noscript|template)$/;

  // What the running parser script wrote: markup still waiting for its end tags,
  // the node the next markup goes after, and the scripts it wrote.
  let state = null;
  let spill = null;
  // Markup written into a document that `open` cleared, until it is parsed.
  let opened = null;

  const fresh = (script, after) => ({ script, after, pending: '', scripts: [] });

  // The node written head content stops before in <body>: what was its first child.
  const bodyPoint = () => {
    const body = $.document.body;
    if (spill === null || spill.parentNode !== body) spill = body ? body.firstChild : null;
    return spill;
  };

  const place = (st, nodes) => {
    for (const n of nodes) {
      let parent = st.after ? st.after.parentNode : $.document.body;
      let before = st.after ? st.after.nextSibling : null;
      const blank = n.nodeType === 3 && !/\S/.test(n.data);
      if (parent && parent === $.document.head && !(n.nodeType === 1 && HEAD_CONTENT.test(n.localName)) && n.nodeType !== 8 && !blank) {
        parent = $.document.body;
        before = bodyPoint();
      }
      if (!parent) return;
      $.insert(parent, n, before);
      st.after = n;
    }
  };

  const flush = (st, force) => {
    if (!st.pending) return;
    const f = $.document.createDocumentFragment();
    const got = $.parseInto(f, st.pending);
    if (got & LEFT_OPEN && !force) return;
    st.pending = '';
    const nodes = Array.from(f.childNodes);
    const scripts = [];
    for (const n of nodes) {
      if (n.nodeType !== 1) continue;
      if (n.localName === 'script') scripts.push(n);
      else scripts.push(...n.getElementsByTagName('script'));
    }
    for (const s of scripts) $.started.add(s[ID]);
    place(st, nodes);
    // Inline scripts run as soon as they are written; from the first external
    // one on, they wait for the script that wrote them to end.
    for (const s of scripts) {
      if (st.scripts.length || __attr(s[ID], 'src') !== null) {
        st.scripts.push(s);
        continue;
      }
      // What the script writes comes before the rest of this markup.
      const end = run(s);
      if (st.after === s && end) st.after = end;
    }
  };

  // Runs a script that markup wrote, as the parser runs its own; returns the
  // last node of what it wrote.
  const run = (s) => {
    const id = s[ID];
    const type = (__attr(id, 'type') || '').trim().toLowerCase();
    if (!(type === '' || type.includes('javascript') || type.includes('ecmascript'))) return;
    const src = __attr(id, 'src');
    let code = __content(id), url = $.docURL(), name = 'inline script';
    if (src !== null) {
      url = $.resolveURL(src);
      const got = url === null ? null : __load(url, false);
      if (got === null) return s.dispatchEvent(new Event('error'));
      code = got[2];
      name = src;
    }
    const end = asParser(s, () => __run(code, url, name));
    if (src !== null) s.dispatchEvent(new Event('load'));
    return end;
  };

  const asParser = (s, f) => {
    const saved = state;
    state = fresh(s, s);
    $.setCurrentScript(s);
    const own = state;
    try {
      f();
    } finally {
      finish(own);
      state = saved;
      $.setCurrentScript(saved ? saved.script : $.parserScript);
    }
    return own.after;
  };

  const finish = (st) => {
    flush(st, true);
    for (const s of st.scripts) run(s);
  };

  // A page script ended: what it wrote is placed and its scripts run.
  $.parsed = () => {
    const st = state;
    state = null;
    if (st) finish(st);
  };

  const parseOpened = () => {
    if (opened === null) return;
    const text = opened;
    opened = null;
    $.parseInto($.document, text, 2);
    for (const s of Array.from($.document.getElementsByTagName('script'))) run(s);
    const body = $.document.body;
    if (body) trailing = fresh(null, body.lastChild);
  };
  // Markup written after `open` and the first parse goes to the end of <body>.
  let trailing = null;

  const write = (doc, text) => {
    if (doc !== $.document) return;
    if (!state && $.parserScript) state = fresh($.parserScript, $.parserScript);
    if (state) {
      state.pending += text;
      flush(state, false);
      return;
    }
    if (opened !== null) {
      opened += text;
      return;
    }
    if (trailing) {
      trailing.pending += text;
      flush(trailing, false);
      return;
    }
    if (doc.readyState !== 'complete') return;
    doc.open();
    opened += text;
  };

  def(g.Document.prototype, {
    open() {
      if (this !== $.document || $.parserScript || state) return this;
      $.parseInto(this, '', 2);
      opened = '';
      trailing = null;
      $.addTimer(parseOpened, 0, false, []);
      return this;
    },
    close() {
      if (this === $.document) {
        parseOpened();
        if (trailing) flush(trailing, true);
        trailing = null;
      }
    },
    write(...a) { write(this, a.join('')); },
    writeln(...a) { write(this, a.join('') + '\n'); },
  });
})
