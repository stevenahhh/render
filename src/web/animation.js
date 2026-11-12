// Web Animations: element.animate() runs on the timer clock. Nothing moves in
// between; when an animation ends, one that fills forwards holds its last
// keyframe on the element, after the inline style, until it is cancelled.
(function ($) {
  'use strict';
  const { g, ID, def, domError, events, dispatch } = $;
  const kebab = (k) => (k === 'cssFloat' ? 'float' : k.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase()));
  const TIMING = new Set(['offset', 'easing', 'composite']);
  // The animations of each element, in the order they were made.
  const byElement = new Map();
  // Running animations, which one timer ends together frame by frame, as a
  // browser updates them: promises settle first, then the events go out.
  const running = new Set();
  const FRAME = 16;
  let timer = null;
  const schedule = () => {
    if (timer !== null) g.clearTimeout(timer);
    timer = null;
    let due = Infinity;
    for (const a of running) due = Math.min(due, a._due);
    if (due !== Infinity) timer = g.setTimeout(tick, Math.max(0, due - g.performance.now()));
  };
  const tick = () => {
    timer = null;
    const now = g.performance.now();
    const done = [...running].filter((a) => a._due <= now + FRAME);
    for (const a of done) a._finish();
    if (done.length) g.setTimeout(() => done.forEach((a) => a._announce()), 0);
    schedule();
  };

  // The declarations of the keyframe an animation ends on.
  const endFrame = (keyframes, first) => {
    if (keyframes === null || keyframes === undefined) return '';
    let frame = {};
    if (Array.isArray(keyframes)) {
      frame = keyframes.length ? keyframes[first ? 0 : keyframes.length - 1] : {};
    } else {
      for (const k of Object.keys(keyframes)) {
        const v = keyframes[k];
        frame[k] = Array.isArray(v) ? v[first ? 0 : v.length - 1] : v;
      }
    }
    return Object.keys(frame).filter((k) => !TIMING.has(k) && frame[k] !== undefined).map((k) => kebab(k) + ':' + frame[k]).join(';');
  };

  const held = (el) => {
    const list = byElement.get(el) || [];
    __animated(el[ID], list.filter((a) => a._holds).map((a) => a._end).join(';'));
  };

  class KeyframeEffect {
    constructor(target, keyframes, options) {
      const o = typeof options === 'number' ? { duration: options } : options || {};
      this.target = target;
      this._keyframes = keyframes;
      this._timing = {
        delay: +o.delay || 0,
        endDelay: +o.endDelay || 0,
        duration: o.duration === undefined || o.duration === 'auto' ? 0 : +o.duration,
        iterations: o.iterations === undefined ? 1 : +o.iterations,
        fill: o.fill || 'auto',
        direction: o.direction || 'normal',
        easing: o.easing || 'linear',
      };
    }
    getTiming() { return Object.assign({}, this._timing); }
    getComputedTiming() {
      const t = this._timing;
      return Object.assign(this.getTiming(), { activeDuration: t.duration * t.iterations, endTime: this._endTime() });
    }
    getKeyframes() { return Array.isArray(this._keyframes) ? this._keyframes.slice() : [this._keyframes]; }
    _endTime() { const t = this._timing; return Math.max(t.delay + t.duration * t.iterations + t.endDelay, 0); }
  }

  class Animation extends EventTarget {
    constructor(effect = null) {
      super();
      this.effect = effect;
      this.id = '';
      this.playbackRate = 1;
      this._state = 'idle';
      this._time = null;
      this._newFinished();
      this.ready = Promise.resolve(this);
    }
    get playState() { return this._state; }
    get pending() { return false; }
    get currentTime() { return this._state === 'running' ? Math.min(g.performance.now() - this._start, this.effect._endTime()) : this._time; }
    get startTime() { return this._state === 'running' ? this._start : null; }
    _newFinished() {
      this.finished = new Promise((resolve, reject) => { this._resolve = resolve; this._reject = reject; });
      this.finished.catch(() => {});
    }
    play() {
      if (this._state === 'running') return;
      if (this._state === 'finished' || this._state === 'idle') this._time = 0;
      this._state = 'running';
      this._start = g.performance.now() - (this._time || 0);
      this._release();
      this._due = this._start + this.effect._endTime();
      running.add(this);
      schedule();
    }
    pause() {
      if (this._state !== 'running') return;
      this._time = this.currentTime;
      running.delete(this);
      this._state = 'paused';
    }
    finish() {
      if (this.effect._endTime() === Infinity) throw domError('InvalidStateError', 'Cannot finish an animation that runs forever.');
      this._finish();
      g.setTimeout(() => this._announce(), 0);
    }
    cancel() {
      if (this._state === 'idle') return;
      running.delete(this);
      this._state = 'idle';
      this._time = null;
      this._release();
      this._reject(domError('AbortError', 'The animation was cancelled.'));
      this._newFinished();
      g.setTimeout(() => dispatch(this, new events.AnimationPlaybackEvent('cancel', { currentTime: null })), 0);
    }
    reverse() {
      this.playbackRate = -this.playbackRate;
      this._time = 0;
      this._state = 'idle';
      this.play();
    }
    updatePlaybackRate(rate) { this.playbackRate = +rate; }
    persist() {}
    commitStyles() {
      const el = this.effect && this.effect.target;
      if (el && el.style) for (const d of this._endState().split(';')) if (d) el.style.setProperty(d.slice(0, d.indexOf(':')), d.slice(d.indexOf(':') + 1));
    }
    // The keyframe this animation ends on: the last, or with a reversed run the first.
    _endState() {
      const t = this.effect._timing;
      const back = (t.direction === 'reverse' || t.direction === 'alternate-reverse') !== ((t.direction === 'alternate' || t.direction === 'alternate-reverse') && t.iterations % 2 === 0);
      return endFrame(this.effect._keyframes, back !== this.playbackRate < 0);
    }
    _finish() {
      running.delete(this);
      this._state = 'finished';
      this._time = this.effect._endTime();
      const fill = this.effect._timing.fill;
      this._holds = fill === 'forwards' || fill === 'both';
      this._end = this._endState();
      if (this._holds && this.effect.target) held(this.effect.target);
      this._resolve(this);
    }
    _announce() { dispatch(this, new events.AnimationPlaybackEvent('finish', { currentTime: this._time, timelineTime: g.performance.now() })); }
    _release() {
      if (!this._holds) return;
      this._holds = false;
      if (this.effect.target) held(this.effect.target);
    }
  }

  // Running and paused animations, and finished ones that still fill.
  const relevant = (a) => a._state === 'running' || a._state === 'paused' || (a._state === 'finished' && a._holds);

  def(g.Element.prototype, {
    animate(keyframes, options) {
      const a = new Animation(new KeyframeEffect(this, keyframes, options));
      if (options && options.id !== undefined) a.id = String(options.id);
      if (!byElement.has(this)) byElement.set(this, []);
      byElement.get(this).push(a);
      a.play();
      return a;
    },
    getAnimations() { return (byElement.get(this) || []).filter(relevant); },
  });
  def(g.Document.prototype, {
    getAnimations() { return [...byElement.values()].flat().filter((a) => relevant(a) && a.effect.target.isConnected); },
  });
  def(g, { Animation, KeyframeEffect, AnimationEffect: KeyframeEffect });
})
