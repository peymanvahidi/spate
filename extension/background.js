// Spate service worker: download capture, context menu, media detection, badge.
// All listeners are registered synchronously at top level (MV3 workers can be suspended);
// state that must survive suspension lives in chrome.storage.session.

import {
  addNetworkEntry,
  captureOutcome,
  classifyMediaResponse,
  consumeBypass,
  decideCapture,
  errorCode,
  isHttpUrl,
  itemSize,
  mergeMedia,
} from './lib/logic.js';
import { loadSettings, saveSettings, sendAdd, tabKey } from './lib/native.js';

const MENU_ID = 'spate-download';
const ACCENT = '#0C7A64';

// ---------------------------------------------------------------- install

chrome.runtime.onInstalled.addListener(async () => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: MENU_ID,
      title: 'Download with Spate',
      contexts: ['link', 'video', 'audio'],
    });
  });
  // Persist defaults merged with whatever the user already had.
  await saveSettings(await loadSettings());
});

// ---------------------------------------------------------------- Option-click bypass

let bypassCache = null;

async function bypassRecords() {
  if (!bypassCache) {
    const { bypass } = await chrome.storage.session.get('bypass');
    bypassCache = Array.isArray(bypass) ? bypass : [];
  }
  return bypassCache;
}

async function setBypassRecords(list) {
  bypassCache = list;
  await chrome.storage.session.set({ bypass: list });
}

async function recordBypass(tabId, href) {
  const now = Date.now();
  const list = (await bypassRecords()).filter((r) => now - r.ts < 10_000);
  list.push({ tabId, href: href || null, ts: now });
  await setBypassRecords(list.slice(-10));
}

async function takeBypass(item) {
  const { bypass, remaining } = consumeBypass(await bypassRecords(), item, Date.now());
  await setBypassRecords(remaining);
  return bypass;
}

// ---------------------------------------------------------------- download capture

chrome.downloads.onCreated.addListener((item) => {
  handleDownload(item).catch((e) => console.warn('Spate: capture failed', e));
});

async function handleDownload(item) {
  if (item.byExtensionId && item.byExtensionId === chrome.runtime.id) return;
  const settings = await loadSettings();
  const bypass = await takeBypass(item);
  const decision = decideCapture(item, settings, { selfId: chrome.runtime.id, bypass });
  if (!decision.capture) return;

  try {
    await chrome.downloads.pause(item.id);
  } catch {
    return; // already finished or gone: nothing to take over
  }

  const reply = await sendAdd({
    url: item.finalUrl || item.url,
    filename: item.filename,
    referrer: item.referrer,
    size: itemSize(item),
    mime: item.mime,
  });

  if (captureOutcome(reply) === 'handoff') {
    try {
      await chrome.downloads.cancel(item.id);
    } catch { /* ignore */ }
    try {
      await chrome.downloads.erase({ id: item.id });
    } catch { /* ignore */ }
  } else {
    try {
      await chrome.downloads.resume(item.id);
    } catch (e) {
      console.warn('Spate: could not resume download', e);
    }
    console.info('Spate: left download in Brave:', errorCode(reply) || 'no answer');
  }
}

// ---------------------------------------------------------------- context menu

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId !== MENU_ID) return;
  const url = info.linkUrl || info.srcUrl;
  if (!url) return;
  menuDownload(url, info.frameUrl || info.pageUrl || (tab && tab.url)).catch((e) =>
    console.warn('Spate: menu download failed', e),
  );
});

async function menuDownload(url, referrer) {
  if (isHttpUrl(url)) {
    const reply = await sendAdd({ url, referrer });
    if (reply.ok) return;
    console.info('Spate: menu download falls back to Brave:', errorCode(reply));
  }
  // Nothing is ever lost: Spate unavailable (or blob:/data: source) → Brave downloads it.
  // Downloads we start carry byExtensionId, so onCreated skips them.
  await chrome.downloads.download({ url });
}

// ---------------------------------------------------------------- media detection

const tabQueues = new Map();

/** Serializes read-modify-write of a tab's media record in storage.session. */
function withTab(tabId, fn) {
  const prev = tabQueues.get(tabId) || Promise.resolve();
  const next = prev
    .then(async () => {
      const key = tabKey(tabId);
      const got = await chrome.storage.session.get(key);
      const rec = got[key] || { network: [], frames: {}, title: '' };
      const updated = await fn(rec);
      if (updated === null) {
        await chrome.storage.session.remove(key);
        await updateBadge(tabId, null);
      } else if (updated) {
        await chrome.storage.session.set({ [key]: updated });
        await updateBadge(tabId, updated);
      }
    })
    .catch((e) => console.warn('Spate: media state', e));
  tabQueues.set(tabId, next);
  next.finally(() => {
    if (tabQueues.get(tabId) === next) tabQueues.delete(tabId);
  });
  return next;
}

async function updateBadge(tabId, rec) {
  let count = 0;
  if (rec) {
    const dom = Object.values(rec.frames || {}).flat();
    count = mergeMedia(rec.network, dom, rec.title).items.length;
  }
  try {
    await chrome.action.setBadgeText({ tabId, text: count ? String(count) : '' });
    if (count) {
      await chrome.action.setBadgeBackgroundColor({ tabId, color: ACCENT });
      if (chrome.action.setBadgeTextColor) await chrome.action.setBadgeTextColor({ tabId, color: '#FFFFFF' });
    }
  } catch {
    /* tab closed */
  }
}

chrome.webRequest.onHeadersReceived.addListener(
  (details) => {
    const entry = classifyMediaResponse(details);
    if (!entry) return;
    withTab(details.tabId, (rec) => ({ ...rec, network: addNetworkEntry(rec.network, entry) }));
  },
  { urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'object', 'other'] },
  ['responseHeaders'],
);

// A new top-level navigation starts a fresh list for the tab.
chrome.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (details.tabId >= 0) withTab(details.tabId, () => null);
  },
  { urls: ['<all_urls>'], types: ['main_frame'] },
);

chrome.tabs.onRemoved.addListener((tabId) => {
  withTab(tabId, () => null);
});

// ---------------------------------------------------------------- messages from content scripts

chrome.runtime.onMessage.addListener((msg, sender) => {
  const tabId = sender.tab && sender.tab.id;
  if (!msg || tabId == null || tabId < 0) return;
  if (msg.type === 'alt-click') {
    recordBypass(tabId, msg.href);
  } else if (msg.type === 'media-dom') {
    const frameId = sender.frameId || 0;
    const videos = Array.isArray(msg.videos) ? msg.videos.slice(0, 20) : [];
    withTab(tabId, (rec) => {
      const frames = { ...(rec.frames || {}) };
      if (videos.length) frames[frameId] = videos;
      else if (!frames[frameId]) return undefined; // nothing new, no write
      else delete frames[frameId];
      return { ...rec, frames, title: frameId === 0 && msg.title ? msg.title : rec.title };
    });
  }
});
