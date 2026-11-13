// TextEncoder, TextDecoder, structuredClone, and a small Intl for an engine built without one.
(function ($) {
  'use strict';
  const { g, def, domError } = $;

  class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(s = '') { return __encode(String(s)); }
    encodeInto(s, dest) {
      const bytes = __encode(String(s));
      const n = Math.min(bytes.length, dest.length);
      dest.set(bytes.subarray(0, n));
      return { read: n === bytes.length ? String(s).length : new TextDecoder().decode(bytes.subarray(0, n)).length, written: n };
    }
  }

  const LATIN1 = /^(latin1|iso-8859-1|iso8859-1|l1|ascii|us-ascii|windows-1252|cp1252|x-cp1252)$/i;
  class TextDecoder {
    constructor(label = 'utf-8', options = {}) {
      const l = String(label).trim().toLowerCase();
      if (!/^(utf-?8|unicode-1-1-utf-8)$/.test(l) && !LATIN1.test(l) && !/^utf-16(le)?$/.test(l)) throw new RangeError("Failed to construct 'TextDecoder': The encoding label provided ('" + label + "') is invalid.");
      this._kind = LATIN1.test(l) ? 'windows-1252' : /^utf-16/.test(l) ? 'utf-16le' : 'utf-8';
      this.fatal = !!options.fatal;
      this.ignoreBOM = !!options.ignoreBOM;
    }
    get encoding() { return this._kind; }
    // With `{ stream: true }` a UTF-8 sequence cut at the end of `input` waits for the next call.
    decode(input, options) {
      if (this._kind === 'utf-8' && (this._held || (options && options.stream))) {
        const got = input === undefined ? new Uint8Array(0) : ArrayBuffer.isView(input) ? new Uint8Array(input.buffer, input.byteOffset, input.byteLength) : new Uint8Array(input);
        let bytes = got;
        if (this._held) { bytes = new Uint8Array(this._held.length + got.length); bytes.set(this._held); bytes.set(got, this._held.length); }
        let cut = bytes.length;
        if (options && options.stream) {
          for (let i = bytes.length - 1, back = 1; i >= 0 && back <= 3; i--, back++) {
            const b = bytes[i];
            if ((b & 0xc0) === 0x80) continue;
            const need = b >= 0xf0 ? 4 : b >= 0xe0 ? 3 : b >= 0xc0 ? 2 : 1;
            if (need > back) cut = i;
            break;
          }
        }
        this._held = cut < bytes.length ? bytes.slice(cut) : null;
        return __decode(bytes.subarray(0, cut));
      }
      if (input === undefined) return '';
      if (this._kind === 'utf-8') return __decode(input);
      const bytes = ArrayBuffer.isView(input) ? new Uint8Array(input.buffer, input.byteOffset, input.byteLength) : new Uint8Array(input);
      let out = '';
      if (this._kind === 'utf-16le') {
        for (let i = 0; i + 1 < bytes.length; i += 2) out += String.fromCharCode(bytes[i] | (bytes[i + 1] << 8));
      } else {
        for (let i = 0; i < bytes.length; i += 4096) out += String.fromCharCode.apply(null, bytes.subarray(i, i + 4096));
      }
      return out;
    }
  }

  // The structured clone algorithm: plain data, keeping shared and cyclic references.
  const structuredClone = (value, options) => {
    const seen = new Map();
    const transfer = options && options.transfer ? Array.from(options.transfer) : [];
    const copy = (v) => {
      if (v === null || typeof v !== 'object') {
        if (typeof v === 'function' || typeof v === 'symbol') throw domError('DataCloneError', "Failed to execute 'structuredClone' on 'Window': " + String(v) + ' could not be cloned.');
        return v;
      }
      if (seen.has(v)) return seen.get(v);
      let out;
      const tag = Object.prototype.toString.call(v);
      if (v instanceof Date) out = new Date(v.getTime());
      else if (v instanceof RegExp) out = new RegExp(v.source, v.flags);
      else if (v instanceof ArrayBuffer) out = transfer.includes(v) ? v : v.slice(0);
      else if (ArrayBuffer.isView(v)) {
        const buf = copy(v.buffer);
        out = v instanceof DataView ? new DataView(buf, v.byteOffset, v.byteLength) : new v.constructor(buf, v.byteOffset, v.length);
      } else if (v instanceof Map) {
        out = new Map();
        seen.set(v, out);
        for (const [k, x] of v) out.set(copy(k), copy(x));
        return out;
      } else if (v instanceof Set) {
        out = new Set();
        seen.set(v, out);
        for (const x of v) out.add(copy(x));
        return out;
      } else if (v instanceof Error) {
        out = new (g[v.name] || Error)(v.message);
        if (v.stack !== undefined) out.stack = v.stack;
      } else if (tag === '[object Boolean]' || tag === '[object Number]' || tag === '[object String]') out = Object(v.valueOf());
      else if (g.Blob && v instanceof g.Blob) out = v;
      else if (v[$.ID] !== undefined || typeof v.then === 'function' && !Array.isArray(v) && Object.getPrototypeOf(v) !== Object.prototype || v instanceof WeakMap || v instanceof WeakSet) {
        throw domError('DataCloneError', "Failed to execute 'structuredClone' on 'Window': " + tag.slice(8, -1) + ' object could not be cloned.');
      } else {
        out = Array.isArray(v) ? new Array(v.length) : {};
        seen.set(v, out);
        for (const k of Object.keys(v)) out[k] = copy(v[k]);
        return out;
      }
      seen.set(v, out);
      return out;
    };
    return copy(value);
  };

  const globals = { TextEncoder, TextDecoder, structuredClone };
  const own = Object.getOwnPropertyDescriptor(g, 'Intl');
  if (own && own.get) globals.Intl = intl();
  def(g, globals);

  // Formatting in US English only: enough for pages that format numbers, dates and plurals.
  function intl() {
    const locales = (l) => [].concat(l === undefined ? [] : l).map(String);
    const supported = { supportedLocalesOf: (l) => locales(l).filter((x) => /^en\b/i.test(x)) };
    const resolved = (o) => ({ locale: 'en-US', numberingSystem: 'latn', ...o });
    const group = (s) => s.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
    class NumberFormat {
      constructor(l, o = {}) { this._o = o; }
      format(n) {
        n = Number(n);
        const o = this._o;
        if (!isFinite(n)) return isNaN(n) ? 'NaN' : (n < 0 ? '-' : '') + '∞';
        if (o.notation === 'compact') {
          const a = Math.abs(n);
          const [d, s] = a >= 1e12 ? [1e12, 'T'] : a >= 1e9 ? [1e9, 'B'] : a >= 1e6 ? [1e6, 'M'] : a >= 1e3 ? [1e3, 'K'] : [1, ''];
          const v = n / d;
          return (Math.abs(v) < 10 && d > 1 ? Math.round(v * 10) / 10 : Math.round(v)) + s;
        }
        const min = o.minimumFractionDigits ?? (o.style === 'currency' ? 2 : 0);
        const max = Math.max(min, o.maximumFractionDigits ?? (o.style === 'percent' ? 0 : o.style === 'currency' ? 2 : 3));
        const v = o.style === 'percent' ? n * 100 : n;
        let s = Math.abs(v).toFixed(max);
        if (max > min) s = s.replace(new RegExp('0{1,' + (max - min) + '}$'), '').replace(/\.$/, '');
        const [i, f] = s.split('.');
        s = (o.useGrouping === false ? i : group(i)) + (f ? '.' + f : '');
        if (o.style === 'percent') s += '%';
        if (o.style === 'currency') s = (o.currency === 'USD' || !o.currency ? '$' : o.currency + ' ') + s;
        return (v < 0 ? '-' : '') + s;
      }
      formatToParts(n) { return [{ type: 'literal', value: this.format(n) }]; }
      resolvedOptions() { return resolved({ style: 'decimal', ...this._o }); }
    }
    const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
    const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
    class DateTimeFormat {
      constructor(l, o = {}) { this._o = o; }
      format(d = Date.now()) {
        d = new Date(d);
        const o = this._o;
        const parts = [];
        if (o.weekday) parts.push(o.weekday === 'long' ? DAYS[d.getDay()] : DAYS[d.getDay()].slice(0, 3));
        if (o.month === 'long' || o.month === 'short') {
          const m = o.month === 'long' ? MONTHS[d.getMonth()] : MONTHS[d.getMonth()].slice(0, 3);
          parts.push(o.day ? m + ' ' + d.getDate() + (o.year ? ',' : '') : m);
          if (o.year) parts.push(String(d.getFullYear()));
        } else if (o.year || o.month || o.day || (!o.hour && !o.minute)) {
          parts.push((d.getMonth() + 1) + '/' + d.getDate() + '/' + d.getFullYear());
        }
        if (o.hour || o.minute) {
          const h = d.getHours() % 12 || 12, m = String(d.getMinutes()).padStart(2, '0');
          parts.push(h + ':' + m + ' ' + (d.getHours() < 12 ? 'AM' : 'PM'));
        }
        return parts.join(o.weekday ? ', ' : ' ');
      }
      formatToParts(d) { return [{ type: 'literal', value: this.format(d) }]; }
      formatRange(a, b) { return this.format(a) + ' – ' + this.format(b); }
      resolvedOptions() { return resolved({ calendar: 'gregory', timeZone: 'UTC', ...this._o }); }
    }
    class PluralRules {
      constructor(l, o = {}) { this._o = o; }
      select(n) {
        if (this._o.type === 'ordinal') {
          const t = n % 10, h = n % 100;
          return t === 1 && h !== 11 ? 'one' : t === 2 && h !== 12 ? 'two' : t === 3 && h !== 13 ? 'few' : 'other';
        }
        return n === 1 ? 'one' : 'other';
      }
      resolvedOptions() { return resolved({ type: 'cardinal', pluralCategories: ['one', 'other'], ...this._o }); }
    }
    class Collator {
      constructor(l, o = {}) { this._o = o; }
      compare(a, b) {
        a = String(a);
        b = String(b);
        if (this._o.sensitivity === 'base' || this._o.sensitivity === 'accent') { a = a.toLowerCase(); b = b.toLowerCase(); }
        if (this._o.numeric) return a.localeCompare(b, undefined, { numeric: true });
        return a < b ? -1 : a > b ? 1 : 0;
      }
      resolvedOptions() { return resolved(this._o); }
    }
    class RelativeTimeFormat {
      constructor(l, o = {}) { this._o = o; }
      format(v, unit) {
        unit = String(unit).replace(/s$/, '');
        const n = Math.abs(v), u = unit + (n === 1 ? '' : 's');
        if (this._o.numeric === 'auto' && unit === 'day' && Math.abs(v) <= 1) return v === 0 ? 'today' : v > 0 ? 'tomorrow' : 'yesterday';
        return v < 0 || Object.is(v, -0) ? n + ' ' + u + ' ago' : 'in ' + n + ' ' + u;
      }
      formatToParts(v, u) { return [{ type: 'literal', value: this.format(v, u) }]; }
      resolvedOptions() { return resolved(this._o); }
    }
    class ListFormat {
      constructor(l, o = {}) { this._o = o; }
      format(list) {
        const a = Array.from(list, String), word = this._o.type === 'disjunction' ? 'or' : 'and';
        return a.length < 3 ? a.join(' ' + word + ' ') : a.slice(0, -1).join(', ') + ', ' + word + ' ' + a[a.length - 1];
      }
    }
    class Segmenter {
      constructor(l, o = {}) { this._o = o; }
      segment(s) {
        const by = this._o.granularity === 'word' ? s.split(/(\s+)/).filter(Boolean) : Array.from(String(s));
        let index = 0;
        return by.map((segment) => { const r = { segment, index, input: s, isWordLike: /\S/.test(segment) }; index += segment.length; return r; });
      }
    }
    class Locale {
      constructor(tag) { this.baseName = String(tag); this.language = this.baseName.split('-')[0]; }
      toString() { return this.baseName; }
      maximize() { return this; }
      minimize() { return this; }
    }
    // ECMA-402 lets the three oldest constructors be called without `new`.
    const callable = (C) => {
      const F = { [C.name]: function (...a) { return Reflect.construct(C, a, new.target || F); } }[C.name];
      F.prototype = C.prototype;
      Object.defineProperty(C.prototype, 'constructor', { configurable: true, writable: true, value: F });
      return F;
    };
    const api = { NumberFormat: callable(NumberFormat), DateTimeFormat: callable(DateTimeFormat), PluralRules, Collator: callable(Collator), RelativeTimeFormat, ListFormat, Segmenter, Locale };
    for (const C of Object.values(api)) Object.assign(C, supported);
    return { ...api, getCanonicalLocales: locales, supportedValuesOf: () => [] };
  }
})
