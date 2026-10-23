// Speed-over-time graph (and its tiny sparkline form). Scrolls continuously while live,
// eases its vertical scale, idles otherwise.
//
//   const g = new SpeedGraph(canvas, {source: () => store.speedHistory(id), seconds: 60, axis: true});
//   g.setLive(true)

import { register, wake, fitCanvas, readColors, reducedMotion } from './loop.js';
import { formatSpeedShort } from './format.js';
import { withAlpha } from './segbar.js';

export class SpeedGraph {
  constructor(canvas, opts = {}) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.opts = { seconds: 60, axis: false, color: 'graph', ...opts };
    this.source = opts.source || (() => []);
    this.live = false;
    this.max = 0;
    this.dirty = true;
    this.colors = null;
    this.unregister = register(this);
    if (typeof ResizeObserver !== 'undefined') {
      this.ro = new ResizeObserver(() => this.poke());
      this.ro.observe(canvas);
    }
  }
  destroy() {
    this.unregister();
    this.ro?.disconnect();
  }
  setLive(v) {
    this.live = v;
    this.poke();
  }
  poke() {
    this.dirty = true;
    wake();
  }
  frame(t) {
    if (!this.canvas.isConnected) return false;
    const animate = this.live && !reducedMotion.matches;
    if (!animate && !this.dirty) return false;
    this.dirty = false;
    this.draw(t);
    return animate;
  }
  draw(t) {
    const fit = fitCanvas(this.canvas);
    if (!fit) return;
    const { w, h, dpr } = fit;
    const ctx = this.ctx;
    if (!this.colors) this.colors = readColors(this.canvas, [this.opts.color, 'grid', 'text-3']);
    const color = this.colors[this.opts.color];
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const pts = this.source();
    // Lag one update behind "now" so the leading edge glides instead of stepping.
    const now = (this.live && !reducedMotion.matches ? t : pts.at(-1)?.t ?? t) - (this.live ? 250 : 0);
    // `fit`: a young history fills the width instead of hugging the right edge.
    let span = this.opts.seconds * 1000;
    if (this.opts.fit && pts.length) span = Math.min(span, Math.max(8000, now - pts[0].t));
    this.span = this.span ? this.span + (span - this.span) * 0.1 : span;
    span = this.span;
    const visible = pts.filter((p) => p.t >= now - span - 1000);
    const peak = visible.reduce((m, p) => Math.max(m, p.v), 0);
    const target = niceMax(peak * 1.12);
    this.max = this.max ? this.max + (target - this.max) * (reducedMotion.matches ? 1 : 0.12) : target;
    if (Math.abs(this.max - target) / (target || 1) > 0.002) this.dirty = true;
    const max = this.max || 1;
    const top = this.opts.axis ? 14 : 2;
    const bottom = this.opts.axis ? 2 : 1;
    const ph = h - top - bottom;
    const X = (tt) => w - ((now - tt) / span) * w;
    const Y = (v) => top + ph - (Math.min(v, max) / max) * ph;

    if (this.opts.axis) {
      ctx.strokeStyle = this.colors.grid;
      ctx.lineWidth = 1 / dpr;
      ctx.fillStyle = this.colors['text-3'];
      ctx.font = `500 10px ui-monospace, "SF Mono", Menlo, monospace`;
      for (const f of [1, 0.5]) {
        const y = Math.round(Y(max * f)) + 0.5 / dpr;
        ctx.beginPath();
        ctx.moveTo(0, y);
        ctx.lineTo(w, y);
        ctx.stroke();
      }
      ctx.textBaseline = 'bottom';
      ctx.fillText(formatSpeedShort(max), 2, top - 2);
      ctx.textAlign = 'right';
      ctx.fillText(spanLabel(span), w - 2, top - 2);
      ctx.textAlign = 'left';
      ctx.beginPath();
      ctx.moveTo(0, top + ph + 0.5);
      ctx.lineTo(w, top + ph + 0.5);
      ctx.stroke();
    }
    if (visible.length < 2) return;

    // Smooth path (monotone-ish midpoint curve).
    const xy = visible.map((p) => [X(p.t), Y(p.v)]);
    const last = xy.at(-1);
    if (last[0] < w) xy.push([w, last[1]]);
    ctx.beginPath();
    ctx.moveTo(xy[0][0], xy[0][1]);
    for (let i = 1; i < xy.length; i++) {
      const [x0, y0] = xy[i - 1];
      const [x1, y1] = xy[i];
      const mx = (x0 + x1) / 2;
      ctx.bezierCurveTo(mx, y0, mx, y1, x1, y1);
    }
    ctx.save();
    ctx.lineTo(w, top + ph);
    ctx.lineTo(xy[0][0], top + ph);
    ctx.closePath();
    const g = ctx.createLinearGradient(0, top, 0, top + ph);
    g.addColorStop(0, withAlpha(color, this.opts.axis ? 0.3 : 0.2));
    g.addColorStop(1, withAlpha(color, 0));
    ctx.fillStyle = g;
    ctx.fill();
    ctx.restore();
    ctx.beginPath();
    ctx.moveTo(xy[0][0], xy[0][1]);
    for (let i = 1; i < xy.length; i++) {
      const [x0, y0] = xy[i - 1];
      const [x1, y1] = xy[i];
      const mx = (x0 + x1) / 2;
      ctx.bezierCurveTo(mx, y0, mx, y1, x1, y1);
    }
    ctx.strokeStyle = color;
    ctx.lineWidth = this.opts.axis ? 1.5 : 1.25;
    ctx.lineJoin = 'round';
    ctx.stroke();
    if (this.live) {
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.arc(Math.min(w - 2, xy.at(-1)[0] - 0.5), xy.at(-1)[1], this.opts.axis ? 2.5 : 1.75, 0, Math.PI * 2);
      ctx.fill();
    }
  }
}

/** Round a scale maximum up to 1/2/2.5/5 × 10^n so the axis label reads cleanly. */
export function niceMax(v) {
  if (!(v > 0)) return 1e6;
  const e = 10 ** Math.floor(Math.log10(v));
  const m = v / e;
  const step = m <= 1 ? 1 : m <= 2 ? 2 : m <= 2.5 ? 2.5 : m <= 5 ? 5 : 10;
  return step * e;
}

function spanLabel(ms) {
  const s = Math.round(ms / 1000);
  return s >= 120 ? `last ${Math.round(s / 60)} min` : `last ${s} s`;
}
