// The segmented bar: one colour per connection, received part solid, remaining part a faint
// track, thin gaps at piece boundaries, a soft glow at each active write head, smooth
// interpolation between the ~4/s updates. A finished download collapses into one colour.
//
//   const bar = new SegBar(canvas, {gap: 1.5});   bar.update(jobView);   bar.destroy();
//
// Colours come from CSS custom properties on the canvas (--seg-1..4, --seg-track, --seg-done),
// so the same component follows light/dark mode and the always-dark island.

import { register, wake, fitCanvas, readColors, reducedMotion } from './loop.js';

const COLOR_NAMES = ['seg-1', 'seg-2', 'seg-3', 'seg-4', 'seg-track', 'seg-done', 'seg-fail'];
export const PALETTE_SIZE = 4;

/**
 * Stable colour per connection. Segments are keyed by `start`; a new piece (from a split)
 * inherits the colour of the connection that just freed up, since that is who takes it.
 */
export class ColorTracker {
  constructor() {
    this.map = new Map();
    this.wasActive = new Set();
  }
  assign(segments) {
    const fresh = this.map.size === 0;
    const freed = [];
    for (const s of segments) {
      if (this.wasActive.has(s.start) && !s.active && this.map.has(s.start)) freed.push(this.map.get(s.start));
    }
    const live = new Set(segments.map((s) => s.start));
    for (const k of [...this.map.keys()]) if (!live.has(k)) this.map.delete(k);
    segments.forEach((s, i) => {
      if (this.map.has(s.start)) return;
      if (fresh) return this.map.set(s.start, i % PALETTE_SIZE);
      let c = freed.shift();
      if (c == null) {
        // Least used colour among active pieces.
        const use = new Array(PALETTE_SIZE).fill(0);
        for (const t of segments) if (t.active && this.map.has(t.start)) use[this.map.get(t.start)]++;
        c = use.indexOf(Math.min(...use));
      }
      this.map.set(s.start, c);
    });
    this.wasActive = new Set(segments.filter((s) => s.active).map((s) => s.start));
    return this.map;
  }
  colorOf(start) {
    return this.map.get(start) ?? 0;
  }
}

const easeOut = (p) => 1 - (1 - p) ** 3;

export class SegBar {
  constructor(canvas, opts = {}) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.opts = { gap: 1.5, radius: null, glow: true, ...opts };
    this.colors = null;
    this.tracker = new ColorTracker();
    this.segs = new Map(); // start -> {start, end, from, to, t0, dur, born, active}
    this.total = 0;
    this.state = 'queued';
    this.doneAt = null;
    this.lastUpdate = 0;
    this.interval = 250;
    this.dirty = true;
    this.unregister = register(this);
    if (typeof ResizeObserver !== 'undefined') {
      this.ro = new ResizeObserver(() => {
        this.dirty = true;
        wake();
      });
      this.ro.observe(canvas);
    }
  }

  destroy() {
    this.unregister();
    this.ro?.disconnect();
  }

  /** Feed a JobView (only total, state and segments are used). */
  update(job) {
    const t = performance.now();
    const rm = reducedMotion.matches;
    if (this.lastUpdate) this.interval = Math.min(600, Math.max(120, (t - this.lastUpdate) * 0.5 + this.interval * 0.5));
    const first = this.lastUpdate === 0;
    this.lastUpdate = t;
    const segments = job.segments || [];
    this.total = job.total || segments.reduce((m, s) => Math.max(m, s.end), 0) || 0;
    if (job.state === 'done' && this.state !== 'done') this.doneAt = first || rm ? t - 1e4 : t;
    if (job.state !== 'done') this.doneAt = null;
    this.state = job.state;
    this.tracker.assign(segments);
    const next = new Map();
    for (const s of segments) {
      const prev = this.segs.get(s.start);
      const cur = prev ? this.posAt(prev, t) : first ? s.start : s.start;
      const animate = !rm && (!first || job.state !== 'done');
      next.set(s.start, {
        start: s.start,
        end: s.end,
        from: animate ? cur : s.pos,
        to: s.pos,
        t0: t,
        dur: prev ? this.interval * 1.05 : first ? 650 : 300,
        ease: !prev,
        born: prev ? prev.born : first || rm ? t - 1e4 : t,
        active: !!s.active,
      });
    }
    this.segs = next;
    this.dirty = true;
    wake();
  }

  posAt(s, t) {
    if (s.dur <= 0) return s.to;
    let p = Math.min(1, Math.max(0, (t - s.t0) / s.dur));
    if (s.ease) p = easeOut(p);
    return s.from + (s.to - s.from) * p;
  }

  frame(t) {
    const c = this.canvas;
    if (!c.isConnected) return false;
    const animating =
      [...this.segs.values()].some((s) => t - s.t0 < s.dur || t - s.born < 300) ||
      (this.doneAt != null && t - this.doneAt < 700) ||
      (this.state === 'running' && this.opts.glow && !reducedMotion.matches);
    if (!animating && !this.dirty) return false;
    this.dirty = false;
    this.draw(t);
    return animating;
  }

  draw(t) {
    const fit = fitCanvas(this.canvas);
    if (!fit) return;
    const { w, h, dpr } = fit;
    const ctx = this.ctx;
    if (!this.colors) this.colors = readColors(this.canvas, COLOR_NAMES);
    const col = this.colors;
    const pal = [col['seg-1'], col['seg-2'], col['seg-3'], col['seg-4']];
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const r = this.opts.radius ?? h / 2;
    ctx.save();
    roundRect(ctx, 0, 0, w, h, r);
    ctx.clip();

    const doneP = this.doneAt == null ? 0 : Math.min(1, (t - this.doneAt) / 600);
    const total = this.total;
    const segs = [...this.segs.values()];

    if (!total || !segs.length) {
      ctx.fillStyle = col['seg-track'];
      ctx.fillRect(0, 0, w, h);
      if (this.state === 'running' && !reducedMotion.matches) {
        // Unknown size: a travelling sheen.
        const x = ((t / 1400) % 1) * (w + 80) - 40;
        const g = ctx.createLinearGradient(x - 40, 0, x + 40, 0);
        g.addColorStop(0, 'transparent');
        g.addColorStop(0.5, pal[0]);
        g.addColorStop(1, 'transparent');
        ctx.fillStyle = g;
        ctx.fillRect(0, 0, w, h);
      }
      ctx.restore();
      return;
    }

    const X = (b) => (b / total) * w;
    const gapFull = this.opts.gap;
    const failed = this.state === 'failed';
    const dim = this.state === 'paused' || this.state === 'queued' || failed;

    if (doneP < 1) {
      ctx.globalAlpha = 1 - easeOut(doneP);
      for (const s of segs) {
        const ci = this.tracker.colorOf(s.start);
        const color = failed ? col['seg-fail'] : pal[ci];
        const grow = Math.min(1, (t - s.born) / 300);
        const gap = s.start === 0 ? 0 : gapFull * grow;
        const x0 = X(s.start) + gap / 2;
        const x1 = Math.max(x0, X(s.end) - (s.end >= total ? 0 : gapFull / 2));
        const xp = Math.min(x1, Math.max(x0, X(this.posAt(s, t))));
        // Track: neutral base plus a whisper of the owner's colour.
        ctx.fillStyle = col['seg-track'];
        ctx.fillRect(x0, 0, x1 - x0, h);
        ctx.save();
        ctx.globalAlpha *= 0.14;
        ctx.fillStyle = color;
        ctx.fillRect(x0, 0, x1 - x0, h);
        ctx.restore();
        // Received.
        ctx.save();
        if (dim) ctx.globalAlpha *= failed ? 0.6 : 0.7;
        ctx.fillStyle = color;
        ctx.fillRect(x0, 0, xp - x0, h);
        ctx.restore();
        // Write head glow.
        if (s.active && this.state === 'running' && this.opts.glow && xp < x1) {
          const pulse = reducedMotion.matches ? 0.8 : 0.65 + 0.35 * Math.sin(t / 260 + s.start / 7e7);
          const len = Math.max(6, Math.min(18, h * 2.2));
          const g = ctx.createLinearGradient(xp - len, 0, xp, 0);
          g.addColorStop(0, 'rgba(255,255,255,0)');
          g.addColorStop(1, `rgba(255,255,255,${0.55 * pulse})`);
          ctx.fillStyle = g;
          ctx.fillRect(Math.max(x0, xp - len), 0, Math.min(len, xp - x0), h);
          // Halo spilling just past the head.
          const rg = ctx.createRadialGradient(xp, h / 2, 0, xp, h / 2, h * 1.6);
          rg.addColorStop(0, withAlpha(color, 0.55 * pulse));
          rg.addColorStop(1, withAlpha(color, 0));
          ctx.fillStyle = rg;
          ctx.fillRect(xp, 0, Math.min(h * 1.6, x1 - xp), h);
          ctx.fillStyle = `rgba(255,255,255,${0.9 * pulse})`;
          ctx.fillRect(xp - Math.min(1, 1 / dpr * 1.5), 0, Math.min(1.5, Math.max(1, 1.5 / dpr * 2)), h);
        }
      }
      ctx.globalAlpha = 1;
    }
    if (doneP > 0) {
      ctx.globalAlpha = easeOut(doneP);
      ctx.fillStyle = col['seg-done'];
      ctx.fillRect(0, 0, w, h);
      ctx.globalAlpha = 1;
    }
    ctx.restore();
  }
}

function roundRect(ctx, x, y, w, h, r) {
  r = Math.min(r, h / 2, w / 2);
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

/** "#RRGGBB" + alpha → rgba(). Other formats pass through unchanged. */
export function withAlpha(hex, a) {
  const m = /^#([0-9a-f]{6})$/i.exec(hex);
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  return `rgba(${n >> 16},${(n >> 8) & 255},${n & 255},${a})`;
}

/** Colour of connection i (for legends): CSS var name. */
export const segVar = (i) => `var(--seg-${(i % PALETTE_SIZE) + 1})`;
