import {
  bestVariant,
  describeError,
  errorCode,
  estimateSize,
  formatBytes,
  formatDuration,
  hlsMediaDuration,
  mediaFilename,
  mergeMedia,
  parseDashMpd,
  parseHlsMaster,
  qualityLabel,
} from '../lib/logic.js';
import { sendAdd, sendNative, tabKey } from '../lib/native.js';

const $ = (id) => document.getElementById(id);
const manifestInfo = new Map(); // item.id → { width, height, bitrate, duration }
const rowMessages = new Map(); // item.id → { text, ok }
const pending = new Set(); // item ids being sent
let tab = null;

document.getElementById('options').addEventListener('click', () => chrome.runtime.openOptionsPage());

async function checkStatus() {
  // The host may launch the app and wait up to 1.5 s for it.
  const reply = await sendNative({ type: 'ping' }, 3000);
  const dot = document.querySelector('#status .dot');
  const text = $('status-text');
  if (reply.ok) {
    dot.className = 'dot ok';
    text.textContent = `Connected${reply.version ? ` · v${reply.version}` : ''}`;
    return;
  }
  dot.className = 'dot bad';
  const code = errorCode(reply);
  text.textContent = code === 'app_not_running' ? 'Not running' : code === 'host_missing' ? 'Not installed' : 'Offline';
  const notice = $('notice');
  notice.textContent = describeError(reply) + ' Downloads stay in Brave until then.';
  notice.hidden = false;
}

async function fetchText(url) {
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), 4000);
  try {
    const r = await fetch(url, { credentials: 'include', signal: ctrl.signal });
    return r.ok ? await r.text() : null;
  } catch {
    return null;
  } finally {
    clearTimeout(t);
  }
}

/** Reads an HLS/DASH manifest for best quality, bitrate and duration. */
async function enrich(item) {
  if (!item.stream || manifestInfo.has(item.id)) return;
  manifestInfo.set(item.id, {});
  const text = await fetchText(item.url);
  if (!text) return;
  let info = {};
  if (item.format === 'HLS') {
    const best = bestVariant(parseHlsMaster(text));
    if (best) info = { width: best.width, height: best.height, bitrate: best.bandwidth };
    const d = hlsMediaDuration(text); // media playlist (no variants)
    if (d) info.duration = d;
  } else {
    const mpd = parseDashMpd(text);
    const best = bestVariant(mpd.variants);
    info = { duration: mpd.duration, ...(best ? { width: best.width, height: best.height, bitrate: best.bandwidth } : {}) };
  }
  manifestInfo.set(item.id, info);
  render();
}

function metaLine(item) {
  const m = manifestInfo.get(item.id) || {};
  const width = item.width || m.width;
  const height = item.height || m.height;
  const duration = item.duration || m.duration;
  const parts = [];
  const q = qualityLabel(width, height);
  if (q) parts.push(width && height ? `${q} (${width}×${height})` : q);
  if (duration) parts.push(formatDuration(duration));
  const est = estimateSize({ size: item.size, bitrate: m.bitrate, duration });
  if (est) parts.push(`${est.exact ? '' : '≈ '}${formatBytes(est.bytes)}`);
  return parts;
}

let lastRec = null;

function render(rec = lastRec) {
  lastRec = rec;
  const dom = rec ? Object.values(rec.frames || {}).flat() : [];
  const { items, drm } = rec ? mergeMedia(rec.network, dom, rec.title) : { items: [], drm: false };
  const list = $('list');
  list.textContent = '';
  $('drm').hidden = !drm;
  $('empty').hidden = drm || items.length > 0;

  for (const item of items) {
    enrich(item);
    const row = $('row').content.firstElementChild.cloneNode(true);
    const img = row.querySelector('img');
    if (item.thumb) {
      img.addEventListener('load', () => img.classList.add('loaded'));
      img.src = item.thumb;
    }
    row.querySelector('.playing').hidden = !item.playing;
    const title = row.querySelector('.title');
    title.textContent = item.title;
    title.title = item.title;

    const meta = row.querySelector('.meta');
    if (item.format) {
      const tag = document.createElement('span');
      tag.className = 'tag';
      tag.textContent = item.format;
      meta.append(tag);
    }
    meta.append(metaLine(item).join(' · '));

    const msg = row.querySelector('.msg');
    const saved = rowMessages.get(item.id);
    if (saved) {
      msg.hidden = false;
      msg.textContent = saved.text;
      msg.classList.toggle('ok', saved.ok);
    }

    const btn = row.querySelector('.dl');
    if (pending.has(item.id)) {
      btn.disabled = true;
      btn.textContent = 'Sending…';
    }
    btn.addEventListener('click', async () => {
      pending.add(item.id);
      render();
      const reply = await sendAdd({
        url: item.url,
        filename: mediaFilename(item.title, item.format),
        referrer: tab && tab.url,
        size: item.size,
        mime: item.mime,
        kind: 'media',
        timeoutMs: 3000,
      });
      pending.delete(item.id);
      rowMessages.set(item.id, reply.ok ? { text: 'Sent to Spate.', ok: true } : { text: describeError(reply), ok: false });
      render();
    });
    list.append(row);
  }
}

async function main() {
  checkStatus();
  [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab) return render(null);
  const key = tabKey(tab.id);
  const load = async () => render((await chrome.storage.session.get(key))[key] || null);
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area === 'session' && changes[key]) render(changes[key].newValue || null);
  });
  await load();
  // Ask every frame for a fresh scan (playing state, frame thumbnails); updates arrive via storage.
  chrome.tabs.sendMessage(tab.id, { type: 'scan' }).catch(() => {});
}

main();
