// MessageChannel, MessagePort and BroadcastChannel within the page.
(function ($) {
  'use strict';
  const { g, def, illegal, addTimer, fire } = $;
  const clone = (data) => {
    if (data === null || typeof data !== 'object') return data;
    $.need('encoding');
    return g.structuredClone(data);
  };

  class MessagePort extends g.EventTarget {
    constructor() { super(); illegal(); }
    postMessage(data) {
      const to = this._other;
      if (!to || this._closed) return;
      const copy = clone(data);
      addTimer(() => { if (!to._closed) fire(to, 'message', { data: copy }, g.MessageEvent); }, 0, false, []);
    }
    start() {}
    close() { this._closed = true; }
  }
  const port = () => {
    const p = Object.create(MessagePort.prototype);
    p._other = null;
    p._closed = false;
    p.onmessage = null;
    p.onmessageerror = null;
    return p;
  };
  class MessageChannel {
    constructor() {
      this.port1 = port();
      this.port2 = port();
      this.port1._other = this.port2;
      this.port2._other = this.port1;
    }
  }

  const channels = new Map();
  class BroadcastChannel extends g.EventTarget {
    constructor(name) {
      super();
      this.name = String(name);
      this.onmessage = null;
      this._closed = false;
      if (!channels.has(this.name)) channels.set(this.name, new Set());
      channels.get(this.name).add(this);
    }
    postMessage(data) {
      if (this._closed) throw $.domError('InvalidStateError', 'Channel is closed');
      for (const c of channels.get(this.name)) {
        if (c === this) continue;
        const copy = clone(data);
        addTimer(() => { if (!c._closed) fire(c, 'message', { data: copy }, g.MessageEvent); }, 0, false, []);
      }
    }
    close() {
      this._closed = true;
      channels.get(this.name).delete(this);
    }
  }

  $.interfaces({ MessageChannel, MessagePort, BroadcastChannel });
})
