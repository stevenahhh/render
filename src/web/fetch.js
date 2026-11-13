// fetch, Headers, Request, Response, XMLHttpRequest, FormData, Blob, File,
// FileReader and a ReadableStream that hands over a whole body at once.
// Requests go to `__fetch`, which runs them on a worker thread; the host
// reports each result through `$.fetched`.
(function ($) {
  'use strict';
  const { g, def, domError, addTimer, fire, resolveURL } = $;
  const encode = (s) => __encode(String(s));
  const decode = (b) => __decode(b);

  const pending = new Map();
  let nextRequest = 1;
  $.fetched = (id, result) => {
    const done = pending.get(id);
    if (!done) return;
    pending.delete(id);
    done(result);
  };

  const blobs = new Map();
  $.createObjectURL = (blob) => {
    const url = 'blob:' + g.location.origin + '/' + g.crypto.randomUUID();
    blobs.set(url, blob);
    return url;
  };
  $.revokeObjectURL = (url) => blobs.delete(String(url));

  // `data:` and `blob:` URLs are answered here; the rest go to the network.
  const local = (url) => {
    if (url.startsWith('blob:')) {
      const b = blobs.get(url.split('#')[0]);
      return b ? [200, 'OK', url, ['content-type', b.type], b._bytes] : 'blob URL not found';
    }
    const m = /^data:([^,]*?)(;base64)?,(.*)$/s.exec(url);
    if (!m) return 'invalid data URL';
    let bytes;
    try {
      bytes = m[2] ? Uint8Array.from(atob(decodeURIComponent(m[3])), (c) => c.charCodeAt(0)) : encode(decodeURIComponent(m[3]));
    } catch (e) {
      return 'invalid data URL';
    }
    return [200, 'OK', url, ['content-type', m[1] || 'text/plain;charset=US-ASCII'], bytes];
  };

  // Sends a request; `done` gets `[status, statusText, url, headers, body]` or an error message.
  const send = (method, url, headers, body, sync, done) => {
    if (/^(data|blob):/.test(url)) {
      const r = local(url);
      if (sync) done(r); else addTimer(() => done(r), 0, false, []);
      return 0;
    }
    const id = nextRequest++;
    if (sync) {
      done(__fetch(id, method, url, headers, body, true));
      return 0;
    }
    pending.set(id, done);
    __fetch(id, method, url, headers, body, false);
    return id;
  };
  const cancel = (id) => {
    if (pending.delete(id)) __abort_fetch(id);
  };

  // Attributes are accessors of the interface's prototype, as in WebIDL, reading `_<name>`;
  // `setters` maps the writable ones to the conversion their setter applies.
  const attributes = (C, names, setters = {}) => {
    for (const k of names) {
      const slot = '_' + k, set = setters[k];
      Object.defineProperty(C.prototype, k, { configurable: true, enumerable: true, get() { return this[slot]; }, set: set && function (v) { this[slot] = set(v, this[slot]); } });
    }
  };

  class Blob {
    constructor(parts = [], options = {}) {
      const chunks = [];
      for (const p of parts) {
        if (p instanceof Blob) chunks.push(p._bytes);
        else if (p instanceof ArrayBuffer) chunks.push(new Uint8Array(p));
        else if (ArrayBuffer.isView(p)) chunks.push(new Uint8Array(p.buffer, p.byteOffset, p.byteLength));
        else chunks.push(encode(p));
      }
      const size = chunks.reduce((n, c) => n + c.length, 0);
      const bytes = new Uint8Array(size);
      let at = 0;
      for (const c of chunks) {
        bytes.set(c, at);
        at += c.length;
      }
      this._bytes = bytes;
      this._type = String(options.type || '').toLowerCase();
    }
    get size() { return this._bytes.length; }
    get type() { return this._type; }
    slice(start = 0, end = this.size, type = '') {
      const b = new Blob([], { type });
      b._bytes = this._bytes.slice(start < 0 ? Math.max(this.size + start, 0) : start, end < 0 ? this.size + end : end);
      return b;
    }
    text() { return Promise.resolve(decode(this._bytes)); }
    arrayBuffer() { return Promise.resolve(this._bytes.slice().buffer); }
    bytes() { return Promise.resolve(this._bytes.slice()); }
    stream() { return streamOf(this._bytes); }
  }
  class File extends Blob {
    constructor(parts, name, options = {}) {
      super(parts, options);
      this.name = String(name);
      this.lastModified = options.lastModified ?? Date.now();
      this.webkitRelativePath = '';
    }
  }

  class FileReader extends g.EventTarget {
    constructor() {
      super();
      this.readyState = 0;
      this.result = null;
      this.error = null;
      for (const k of ['onloadstart', 'onprogress', 'onload', 'onloadend', 'onerror', 'onabort']) this[k] = null;
    }
    _read(blob, as) {
      if (this.readyState === 1) throw domError('InvalidStateError', 'The object is already busy reading Blobs.');
      this.readyState = 1;
      addTimer(() => {
        if (this.readyState !== 1) return;
        const b = blob._bytes;
        fire(this, 'loadstart', {}, g.ProgressEvent);
        this.result = as === 'text' ? decode(b) : as === 'buffer' ? b.slice().buffer : as === 'binary' ? String.fromCharCode(...b) : 'data:' + (blob.type || 'application/octet-stream') + ';base64,' + btoa(String.fromCharCode(...b));
        this.readyState = 2;
        fire(this, 'load', { lengthComputable: true, loaded: b.length, total: b.length }, g.ProgressEvent);
        fire(this, 'loadend', { lengthComputable: true, loaded: b.length, total: b.length }, g.ProgressEvent);
      }, 0, false, []);
    }
    readAsText(b) { this._read(b, 'text'); }
    readAsArrayBuffer(b) { this._read(b, 'buffer'); }
    readAsBinaryString(b) { this._read(b, 'binary'); }
    readAsDataURL(b) { this._read(b, 'url'); }
    abort() {
      if (this.readyState !== 1) return;
      this.readyState = 2;
      this.result = null;
      fire(this, 'abort', {}, g.ProgressEvent);
      fire(this, 'loadend', {}, g.ProgressEvent);
    }
  }
  Object.assign(FileReader, { EMPTY: 0, LOADING: 1, DONE: 2 });

  // A stream that delivers one chunk; enough for code that reads a body to its end.
  class ReadableStream {
    constructor(source = {}) {
      this._chunks = [];
      this._done = false;
      this._waiting = [];
      const controller = {
        enqueue: (c) => { this._chunks.push(c); this._wake(); },
        close: () => { this._done = true; this._wake(); },
        error: (e) => { this._error = e; this._wake(); },
        desiredSize: 1,
      };
      if (source.start) source.start(controller);
      this._pull = source.pull ? () => source.pull(controller) : null;
      this.locked = false;
    }
    _wake() {
      while (this._waiting.length && (this._chunks.length || this._done || this._error)) {
        const [ok, fail] = this._waiting.shift();
        if (this._error) fail(this._error);
        else if (this._chunks.length) ok({ value: this._chunks.shift(), done: false });
        else ok({ value: undefined, done: true });
      }
    }
    getReader() {
      this.locked = true;
      const stream = this;
      return {
        read() {
          return new Promise((ok, fail) => {
            stream._waiting.push([ok, fail]);
            stream._wake();
            if (stream._waiting.length && stream._pull) Promise.resolve(stream._pull()).then(() => stream._wake());
          });
        },
        releaseLock() { stream.locked = false; },
        cancel() { stream._done = true; stream._chunks = []; stream._wake(); return Promise.resolve(); },
        get closed() { return Promise.resolve(); },
      };
    }
    cancel() { this._done = true; return Promise.resolve(); }
    tee() { return [this, this]; }
    async pipeTo(dest) {
      const r = this.getReader();
      const w = dest.getWriter ? dest.getWriter() : dest;
      try {
        for (;;) {
          const { value, done } = await r.read();
          if (done) break;
          await w.write(value);
        }
      } catch (e) {
        if (w.abort) await w.abort(e);
        throw e;
      }
      if (w.close) await w.close();
    }
    // Chunks go through `transform` while this stream is read; its readable side is returned.
    pipeThrough(transform) {
      this.pipeTo(transform.writable).catch(() => {});
      return transform.readable;
    }
    [Symbol.asyncIterator]() { const r = this.getReader(); return { next: () => r.read(), return: () => r.cancel().then(() => ({ done: true })) }; }
  }
  const streamOf = (bytes) => new ReadableStream({ start(c) { if (bytes.length) c.enqueue(bytes); c.close(); } });

  // A sink written one chunk at a time; each write waits for the one before it.
  class WritableStream {
    constructor(sink = {}) {
      this._sink = sink;
      this._last = Promise.resolve(sink.start ? sink.start({ error: (e) => { this._error = e; } }) : undefined);
      this._closed = false;
      this.locked = false;
    }
    _then(f) {
      this._last = this._last.then(() => {
        if (this._error) throw this._error;
        return f();
      });
      return this._last;
    }
    getWriter() {
      if (this.locked) throw new TypeError('WritableStream is locked');
      this.locked = true;
      const s = this;
      return {
        write: (chunk) => s._then(() => s._sink.write && s._sink.write(chunk)),
        close: () => s._then(() => { s._closed = true; return s._sink.close && s._sink.close(); }),
        abort: (e) => { s._error = e; return Promise.resolve(s._sink.abort && s._sink.abort(e)); },
        releaseLock: () => { s.locked = false; },
        get ready() { return s._last; },
        get closed() { return s._last.then(() => undefined); },
        get desiredSize() { return s._closed ? 0 : 1; },
      };
    }
    close() { return this.getWriter().close(); }
    abort(e) { return this.getWriter().abort(e); }
  }

  // A writable side whose chunks `transformer` turns into those of a readable side.
  class TransformStream {
    constructor(transformer = {}) {
      let out;
      this.readable = new ReadableStream({ start: (c) => { out = c; } });
      const controller = { enqueue: (c) => out.enqueue(c), error: (e) => out.error(e), terminate: () => out.close(), desiredSize: 1 };
      if (transformer.start) transformer.start(controller);
      this.writable = new WritableStream({
        write: (chunk) => (transformer.transform ? transformer.transform(chunk, controller) : controller.enqueue(chunk)),
        close: () => Promise.resolve(transformer.flush && transformer.flush(controller)).then(() => out.close()),
        abort: (e) => out.error(e),
      });
    }
  }
  class TextEncoderStream extends TransformStream {
    constructor() { super({ transform: (s, c) => { const b = encode(s); if (b.length) c.enqueue(b); } }); }
    get encoding() { return 'utf-8'; }
  }
  class TextDecoderStream extends TransformStream {
    constructor(label = 'utf-8', options = {}) {
      const d = new g.TextDecoder(label, options);
      super({
        transform: (b, c) => { const s = d.decode(b, { stream: true }); if (s) c.enqueue(s); },
        flush: (c) => { const s = d.decode(); if (s) c.enqueue(s); },
      });
      this._d = d;
    }
    get encoding() { return this._d.encoding; }
  }

  class Headers {
    constructor(init) {
      this._l = [];
      if (init instanceof Headers) this._l = init._l.map((p) => p.slice());
      else if (init && typeof init === 'object') {
        if (typeof init[Symbol.iterator] === 'function') for (const [k, v] of init) this.append(k, v);
        else for (const k of Object.keys(init)) this.append(k, init[k]);
      }
    }
    append(k, v) {
      k = String(k).toLowerCase();
      if (!/^[!#$%&'*+\-.^_`|~0-9a-z]+$/.test(k)) throw new TypeError("Failed to execute 'append' on 'Headers': Invalid name");
      this._l.push([k, String(v).trim()]);
    }
    set(k, v) { this.delete(k); this.append(k, v); }
    get(k) {
      k = String(k).toLowerCase();
      const v = this._l.filter((p) => p[0] === k).map((p) => p[1]);
      return v.length ? v.join(', ') : null;
    }
    getSetCookie() { return this._l.filter((p) => p[0] === 'set-cookie').map((p) => p[1]); }
    has(k) { k = String(k).toLowerCase(); return this._l.some((p) => p[0] === k); }
    delete(k) { k = String(k).toLowerCase(); this._l = this._l.filter((p) => p[0] !== k); }
    forEach(f, that) { for (const [k, v] of this) f.call(that, v, k, this); }
    *entries() {
      const names = [...new Set(this._l.map((p) => p[0]))].sort();
      for (const k of names) yield [k, this.get(k)];
    }
    *keys() { for (const [k] of this.entries()) yield k; }
    *values() { for (const [, v] of this.entries()) yield v; }
    [Symbol.iterator]() { return this.entries(); }
  }
  const flat = (h) => h._l.flat();

  // What a request or response body is made from: its bytes and the content type it implies.
  const bodyOf = (body) => {
    if (body === undefined || body === null) return [null, null];
    if (typeof body === 'string') return [encode(body), 'text/plain;charset=UTF-8'];
    if (body instanceof Blob) return [body._bytes, body.type || null];
    if (body instanceof ArrayBuffer) return [new Uint8Array(body.slice(0)), null];
    if (ArrayBuffer.isView(body)) return [new Uint8Array(body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength)), null];
    if (body instanceof g.URLSearchParams) return [encode(String(body)), 'application/x-www-form-urlencoded;charset=UTF-8'];
    if (body instanceof FormData) {
      const boundary = '----render' + Math.random().toString(16).slice(2);
      let s = '';
      for (const [k, v] of body._l) s += '--' + boundary + '\r\nContent-Disposition: form-data; name="' + k + '"' + (v instanceof File ? '; filename="' + v.name + '"\r\nContent-Type: ' + (v.type || 'application/octet-stream') : '') + '\r\n\r\n' + (v instanceof Blob ? decode(v._bytes) : v) + '\r\n';
      return [encode(s + '--' + boundary + '--\r\n'), 'multipart/form-data; boundary=' + boundary];
    }
    if (body instanceof ReadableStream) return [new Uint8Array(0), null];
    return [encode(String(body)), 'text/plain;charset=UTF-8'];
  };

  class Body {
    get bodyUsed() { return this._used; }
    get body() { return this._bytes === null ? null : streamOf(this._bytes); }
    _consume() {
      if (this._used) return Promise.reject(new TypeError('Body has already been consumed.'));
      this._used = true;
      return Promise.resolve(this._bytes || new Uint8Array(0));
    }
    text() { return this._consume().then(decode); }
    json() { return this.text().then(JSON.parse); }
    arrayBuffer() { return this._consume().then((b) => b.slice().buffer); }
    bytes() { return this._consume().then((b) => b.slice()); }
    blob() { return this._consume().then((b) => new Blob([b], { type: this.headers.get('content-type') || '' })); }
    formData() { return this.text().then((t) => { const f = new FormData(); for (const [k, v] of new g.URLSearchParams(t)) f.append(k, v); return f; }); }
  }

  class Request extends Body {
    constructor(input, init = {}) {
      super();
      const from = input instanceof Request ? input : null;
      const url = from ? from.url : resolveURL(String(input));
      if (url === null) throw new TypeError("Failed to construct 'Request': Failed to parse URL from " + input);
      this._url = url;
      this._method = String(init.method || (from ? from.method : 'GET')).toUpperCase();
      this._headers = new Headers(init.headers || (from ? from.headers : undefined));
      const [bytes, type] = init.body !== undefined ? bodyOf(init.body) : [from ? from._bytes : null, null];
      if (bytes && (this.method === 'GET' || this.method === 'HEAD')) throw new TypeError("Failed to construct 'Request': Request with GET/HEAD method cannot have body.");
      if (type && !this.headers.has('content-type')) this.headers.set('content-type', type);
      this._bytes = bytes;
      this._used = false;
      this._signal = init.signal || (from ? from.signal : new g.AbortController().signal);
      for (const k of REQUEST_OPTIONS) this['_' + k] = init[k] ?? (from ? from[k] : { mode: 'cors', credentials: 'same-origin', cache: 'default', redirect: 'follow', referrer: 'about:client', referrerPolicy: '', integrity: '', keepalive: false, priority: 'auto' }[k]);
      this._destination = '';
    }
    clone() { return new Request(this); }
  }
  const REQUEST_OPTIONS = ['mode', 'credentials', 'cache', 'redirect', 'referrer', 'referrerPolicy', 'integrity', 'keepalive', 'priority'];
  attributes(Request, ['url', 'method', 'headers', 'signal', 'destination', ...REQUEST_OPTIONS]);

  class Response extends Body {
    constructor(body = null, init = {}) {
      super();
      const [bytes, type] = bodyOf(body);
      this._status = init.status ?? 200;
      this._statusText = init.statusText ?? '';
      this._headers = new Headers(init.headers);
      if (type && !this.headers.has('content-type')) this.headers.set('content-type', type);
      this._bytes = bytes;
      this._used = false;
      this._url = '';
      this._redirected = false;
      this._type = 'default';
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    clone() {
      if (this._used) throw new TypeError("Failed to execute 'clone' on 'Response': Response body is already used");
      const r = new Response(null, this);
      Object.assign(r, { _bytes: this._bytes, _url: this.url, _redirected: this.redirected, _type: this.type });
      return r;
    }
    static error() { const r = new Response(null, { status: 0 }); r._type = 'error'; return r; }
    static redirect(url, status = 302) { return new Response(null, { status, headers: { location: resolveURL(String(url)) } }); }
    static json(data, init = {}) { const r = new Response(JSON.stringify(data), init); r.headers.set('content-type', 'application/json'); return r; }
  }
  attributes(Response, ['status', 'statusText', 'headers', 'url', 'redirected', 'type']);

  const fetch = (input, init) => new Promise((resolve, reject) => {
    let req;
    try {
      req = new Request(input, init);
    } catch (e) {
      reject(e instanceof TypeError ? e : new TypeError(String(e && e.message)));
      return;
    }
    const signal = req.signal;
    if (signal && signal.aborted) {
      reject(signal.reason);
      return;
    }
    if (!/^(https?|file|data|blob):/.test(req.url)) {
      reject(new TypeError('Failed to fetch'));
      return;
    }
    let settled = false;
    const id = send(req.method, req.url, flat(req.headers), req._bytes, false, (r) => {
      if (settled) return;
      settled = true;
      if (typeof r === 'string') {
        reject(new TypeError('Failed to fetch'));
        return;
      }
      const res = new Response(null, { status: r[0], statusText: r[1] });
      for (let i = 0; i < r[3].length; i += 2) res.headers.append(r[3][i], r[3][i + 1]);
      res._bytes = req.method === 'HEAD' ? null : r[4];
      res._url = r[2];
      res._redirected = r[2] !== req.url;
      res._type = 'basic';
      resolve(res);
    });
    if (signal) signal.addEventListener('abort', () => {
      if (settled) return;
      settled = true;
      cancel(id);
      reject(signal.reason);
    });
  });

  class FormData {
    constructor(form) {
      this._l = [];
      if (form) for (const el of form.elements) {
        const name = el.getAttribute('name');
        if (!name || el.disabled) continue;
        const type = (el.getAttribute('type') || '').toLowerCase();
        if ((type === 'checkbox' || type === 'radio') && !el.checked) continue;
        if (type === 'submit' || type === 'button' || type === 'reset' || type === 'image') continue;
        this._l.push([name, el.value]);
      }
    }
    append(k, v, name) { this._l.push([String(k), v instanceof Blob ? (v instanceof File || name === undefined ? v : new File([v], name, { type: v.type })) : String(v)]); }
    set(k, v, name) { this.delete(k); this.append(k, v, name); }
    get(k) { const p = this._l.find((x) => x[0] === String(k)); return p ? p[1] : null; }
    getAll(k) { return this._l.filter((x) => x[0] === String(k)).map((x) => x[1]); }
    has(k) { return this._l.some((x) => x[0] === String(k)); }
    delete(k) { this._l = this._l.filter((x) => x[0] !== String(k)); }
    forEach(f, that) { for (const [k, v] of this._l) f.call(that, v, k, this); }
    entries() { return this._l.map((p) => [p[0], p[1]])[Symbol.iterator](); }
    keys() { return this._l.map((p) => p[0])[Symbol.iterator](); }
    values() { return this._l.map((p) => p[1])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
  }

  class XMLHttpRequestEventTarget extends g.EventTarget {}
  class XMLHttpRequestUpload extends XMLHttpRequestEventTarget {}
  const STATES = { UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 };
  class XMLHttpRequest extends XMLHttpRequestEventTarget {
    constructor() {
      super();
      this._readyState = 0;
      this._status = 0;
      this._statusText = '';
      this._responseURL = '';
      this._responseType = '';
      this._timeout = 0;
      this._withCredentials = false;
      this._upload = new XMLHttpRequestUpload();
      for (const k of ['onreadystatechange', 'onloadstart', 'onprogress', 'onload', 'onloadend', 'onerror', 'onabort', 'ontimeout']) this[k] = null;
      this._headers = [];
      this._got = [];
      this._body = null;
      this._id = 0;
      this._mime = null;
    }
    _state(s) {
      this._readyState = s;
      fire(this, 'readystatechange');
    }
    open(method, url, async = true) {
      const u = resolveURL(String(url));
      if (u === null) throw domError('SyntaxError', "Failed to execute 'open' on 'XMLHttpRequest': Invalid URL");
      this.abort();
      Object.assign(this, { _method: String(method).toUpperCase(), _url: u, _async: async !== false, _headers: [], _got: [], _body: null, _status: 0, _statusText: '', _responseURL: '', _sent: false });
      this._state(1);
    }
    setRequestHeader(k, v) {
      if (this._readyState !== 1 || this._sent) throw domError('InvalidStateError', "Failed to execute 'setRequestHeader' on 'XMLHttpRequest': The object's state must be OPENED.");
      this._headers.push(String(k), String(v));
    }
    overrideMimeType(m) { this._mime = String(m); }
    getResponseHeader(k) {
      if (this._readyState < 2) return null;
      k = String(k).toLowerCase();
      const v = [];
      for (let i = 0; i < this._got.length; i += 2) if (this._got[i] === k) v.push(this._got[i + 1]);
      return v.length ? v.join(', ') : null;
    }
    getAllResponseHeaders() {
      if (this._readyState < 2) return '';
      let s = '';
      for (let i = 0; i < this._got.length; i += 2) s += this._got[i] + ': ' + this._got[i + 1] + '\r\n';
      return s;
    }
    send(body) {
      if (this._readyState !== 1 || this._sent) throw domError('InvalidStateError', "Failed to execute 'send' on 'XMLHttpRequest': The object's state must be OPENED.");
      this._sent = true;
      const [bytes, type] = this._method === 'GET' || this._method === 'HEAD' ? [null, null] : bodyOf(body);
      if (type && !this._headers.some((h, i) => i % 2 === 0 && h.toLowerCase() === 'content-type')) this._headers.push('content-type', type);
      fire(this, 'loadstart', {}, g.ProgressEvent);
      const done = (r) => this._done(r);
      if (!this._async) {
        send(this._method, this._url, this._headers, bytes, true, done);
        return;
      }
      this._id = send(this._method, this._url, this._headers, bytes, false, done);
      if (this._timeout > 0) {
        const id = this._id;
        addTimer(() => {
          if (this._id !== id || this._readyState === 4) return;
          cancel(id);
          this._fail('timeout');
        }, this._timeout, false, []);
      }
    }
    _done(r) {
      this._id = 0;
      if (typeof r === 'string') return this._fail('error');
      [this._status, this._statusText, this._responseURL, this._got, this._bytes] = r;
      this._state(2);
      this._state(3);
      const n = this._bytes.length;
      fire(this, 'progress', { lengthComputable: true, loaded: n, total: n }, g.ProgressEvent);
      this._state(4);
      fire(this, 'load', { lengthComputable: true, loaded: n, total: n }, g.ProgressEvent);
      fire(this, 'loadend', { lengthComputable: true, loaded: n, total: n }, g.ProgressEvent);
    }
    _fail(type) {
      this._status = 0;
      this._bytes = null;
      this._state(4);
      fire(this, type, {}, g.ProgressEvent);
      fire(this, 'loadend', {}, g.ProgressEvent);
    }
    abort() {
      if (this._id) {
        cancel(this._id);
        this._id = 0;
        if (this._readyState > 0 && this._readyState < 4 && this._sent) {
          this._fail('abort');
        }
      }
      this._readyState = 0;
    }
    // Internal reads go to the slots, never through the accessors a page may have replaced.
    get responseText() {
      if (this._responseType !== '' && this._responseType !== 'text') throw domError('InvalidStateError', "Failed to read the 'responseText' property from 'XMLHttpRequest': The value is only accessible if the object's 'responseType' is '' or 'text'.");
      return this._text();
    }
    _text() { return this._readyState < 3 || !this._bytes ? '' : decode(this._bytes); }
    get response() { return this._response(); }
    _response() {
      if (this._readyState < 4 || !this._bytes) return this._responseType === '' || this._responseType === 'text' ? this._text() : null;
      switch (this._responseType) {
        case 'json': try { return JSON.parse(decode(this._bytes)); } catch (e) { return null; }
        case 'arraybuffer': return this._bytes.slice().buffer;
        case 'blob': return new Blob([this._bytes], { type: this._mime || this.getResponseHeader('content-type') || '' });
        case 'document': return new g.DOMParser().parseFromString(decode(this._bytes), 'text/html');
        default: return decode(this._bytes);
      }
    }
    get responseXML() { return this._responseType === 'document' ? this._response() : null; }
  }
  Object.assign(XMLHttpRequest, STATES);
  Object.assign(XMLHttpRequest.prototype, STATES);
  const RESPONSE_TYPES = ['', 'arraybuffer', 'blob', 'document', 'json', 'text'];
  attributes(XMLHttpRequest, ['readyState', 'status', 'statusText', 'responseURL', 'responseType', 'timeout', 'withCredentials', 'upload'], {
    responseType: (v, old) => (RESPONSE_TYPES.includes(String(v)) ? String(v) : old),
    timeout: (v) => Number(v) >>> 0,
    withCredentials: (v) => !!v,
  });

  def(g, { fetch, Headers, Request, Response, XMLHttpRequest, XMLHttpRequestEventTarget, XMLHttpRequestUpload, FormData, Blob, File, FileReader, ReadableStream, WritableStream, TransformStream, TextEncoderStream, TextDecoderStream });
})
