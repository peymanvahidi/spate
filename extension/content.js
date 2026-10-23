// Spate content script (all frames, document_start).
// 1. Option/Alt + mousedown on a link → tell the worker so the next download stays in Brave.
// 2. Scan <video> elements and report title, resolution, duration, poster/frame, playing, DRM.
(() => {
  if (window.__spateContent) return;
  window.__spateContent = true;

  const send = (msg) => {
    try {
      chrome.runtime.sendMessage(msg).catch(() => {});
    } catch {
      /* extension reloaded: this script is orphaned */
    }
  };

  // ------------------------------------------------------------ Option-click bypass
  document.addEventListener(
    'mousedown',
    (e) => {
      if (!e.altKey || e.button !== 0) return;
      const a = e.target && e.target.closest && e.target.closest('a[href]');
      send({ type: 'alt-click', href: a ? a.href : null });
    },
    true,
  );

  // ------------------------------------------------------------ video scan
  const THUMB_W = 160;
  const THUMB_EVERY_MS = 15000;
  const thumbs = new WeakMap(); // video → { data, at }

  function captureFrame(v) {
    const prev = thumbs.get(v);
    if (prev && Date.now() - prev.at < THUMB_EVERY_MS) return prev.data;
    let data = prev ? prev.data : null;
    try {
      if (v.readyState >= 2 && v.videoWidth > 0) {
        const c = document.createElement('canvas');
        c.width = THUMB_W;
        c.height = Math.max(1, Math.round((THUMB_W * v.videoHeight) / v.videoWidth));
        c.getContext('2d').drawImage(v, 0, 0, c.width, c.height);
        data = c.toDataURL('image/jpeg', 0.7); // throws for cross-origin (tainted) video
      }
    } catch {
      /* tainted canvas: no frame thumbnail */
    }
    thumbs.set(v, { data, at: Date.now() });
    return data;
  }

  function titleFor(v) {
    const own = v.getAttribute('title') || v.getAttribute('aria-label');
    if (own) return own.trim();
    const og = document.querySelector('meta[property="og:title"]');
    if (og && og.content) return og.content.trim();
    return (document.title || '').trim();
  }

  function describe(v) {
    const sources = [...v.querySelectorAll('source[src]')].map((s) => s.src);
    const typeEl = v.querySelector('source[type]');
    const src = v.currentSrc || v.src || '';
    const playing = !v.paused && !v.ended && v.readyState > 2;
    const poster = v.poster || null;
    return {
      src,
      sources,
      type: typeEl ? typeEl.type : '',
      title: titleFor(v),
      width: v.videoWidth || null,
      height: v.videoHeight || null,
      duration: Number.isFinite(v.duration) ? v.duration : null,
      playing,
      poster,
      thumb: poster ? null : captureFrame(v),
      // EME: a MediaKeys object is attached → DRM-protected, never offered.
      drm: !!v.mediaKeys,
    };
  }

  let lastSent = '';
  function scan(force = false) {
    if (!document.documentElement) return;
    const videos = [...document.querySelectorAll('video')]
      .filter((v) => v.currentSrc || v.src || v.querySelector('source[src]') || v.mediaKeys)
      // Tiny muted autoplay loops (UI decoration) are not interesting.
      .filter((v) => !(v.videoWidth && v.videoWidth < 120 && v.videoHeight < 120))
      .map(describe);
    const msg = { type: 'media-dom', title: document.title, videos };
    const sig = JSON.stringify(msg);
    if (!force && sig === lastSent) return;
    if (!videos.length && !lastSent) return; // never reported anything: stay quiet
    lastSent = sig;
    send(msg);
  }

  let timer = null;
  const soon = (ms = 400) => {
    clearTimeout(timer);
    timer = setTimeout(() => scan(), ms);
  };

  for (const ev of ['loadedmetadata', 'play', 'playing', 'pause', 'ended', 'emptied', 'encrypted']) {
    document.addEventListener(ev, (e) => e.target instanceof HTMLMediaElement && soon(100), true);
  }

  const start = () => {
    scan();
    new MutationObserver(() => soon(1000)).observe(document.documentElement, { childList: true, subtree: true });
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start, { once: true });
  else start();

  // The popup asks for a fresh scan (current "playing" state, new frame thumbnail).
  chrome.runtime.onMessage.addListener((msg) => {
    if (msg && msg.type === 'scan') scan(true);
  });
})();
