// One shared requestAnimationFrame loop for every animated canvas on the page.
// Items return true from frame() while they still need frames; the loop sleeps when no item
// does, and never runs while the page is hidden.

const items = new Set();
let raf = 0;

export const reducedMotion =
  typeof matchMedia === 'function' ? matchMedia('(prefers-reduced-motion: reduce)') : { matches: false };

export function register(item) {
  items.add(item);
  wake();
  return () => items.delete(item);
}

export function wake() {
  if (raf || typeof document === 'undefined' || document.hidden) return;
  raf = requestAnimationFrame(frame);
}

function frame(t) {
  raf = 0;
  let more = false;
  for (const it of items) {
    try {
      if (it.frame(t)) more = true;
    } catch (e) {
      console.error(e);
    }
  }
  if (more) wake();
}

if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) {
      for (const it of items) it.dirty = true;
      wake();
    }
  });
  // Colour scheme / reduced motion changes: redraw everything once.
  const redraw = () => {
    for (const it of items) {
      it.dirty = true;
      it.colors = null;
    }
    wake();
  };
  if (typeof matchMedia === 'function') {
    matchMedia('(prefers-color-scheme: dark)').addEventListener?.('change', redraw);
    reducedMotion.addEventListener?.('change', redraw);
  }
}

/** Size a canvas for HiDPI. Returns {w, h, dpr} in CSS pixels, or null if not laid out. */
export function fitCanvas(canvas) {
  const r = canvas.getBoundingClientRect();
  if (!r.width || !r.height) return null;
  const dpr = Math.max(1, window.devicePixelRatio || 1);
  const W = Math.round(r.width * dpr);
  const H = Math.round(r.height * dpr);
  if (canvas.width !== W || canvas.height !== H) {
    canvas.width = W;
    canvas.height = H;
  }
  return { w: r.width, h: r.height, dpr };
}

/** Read colour tokens from the element's computed style. */
export function readColors(el, names) {
  const cs = getComputedStyle(el);
  const out = {};
  for (const n of names) out[n] = cs.getPropertyValue(`--${n}`).trim() || '#888';
  return out;
}
