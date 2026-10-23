// Thin wrappers around chrome.* used by the service worker and the popup.

import { NATIVE_HOST, NATIVE_TIMEOUT_MS, buildAddRequest, hostOf, normalizeSettings, siteMatches } from './logic.js';

/** Sends one message to spate-host. Resolves to the reply, or `{ok:false, error}` on failure/timeout. */
export function sendNative(message, timeoutMs = NATIVE_TIMEOUT_MS) {
  return new Promise((resolve) => {
    let done = false;
    const finish = (v) => {
      if (!done) {
        done = true;
        clearTimeout(timer);
        resolve(v);
      }
    };
    const timer = setTimeout(() => finish({ ok: false, error: 'timeout' }), timeoutMs);
    try {
      chrome.runtime.sendNativeMessage(NATIVE_HOST, message, (reply) => {
        const err = chrome.runtime.lastError;
        if (err) {
          const msg = String(err.message || err);
          finish({ ok: false, error: /not found|forbidden/i.test(msg) ? `host_missing: ${msg}` : `native_error: ${msg}` });
        } else {
          finish(reply || { ok: false, error: 'native_error: empty reply' });
        }
      });
    } catch (e) {
      finish({ ok: false, error: `native_error: ${e && e.message}` });
    }
  });
}

/**
 * Cookies that would be sent to `url`, plus partitioned (CHIPS) cookies whose top-level
 * site matches the page the request came from (`topUrl`, usually the referrer).
 */
export async function cookiesFor(url, storeId, topUrl) {
  const base = storeId ? { url, storeId } : { url };
  let cookies = [];
  try {
    cookies = await chrome.cookies.getAll(base);
  } catch {
    cookies = [];
  }
  try {
    // An empty partitionKey asks for cookies from every partition (Chromium 119+).
    const topHost = hostOf(topUrl) || hostOf(url);
    const partitioned = await chrome.cookies.getAll({ ...base, partitionKey: {} });
    for (const c of partitioned || []) {
      const site = c.partitionKey && hostOf(c.partitionKey.topLevelSite);
      if (site && siteMatches(topHost, site)) cookies.push(c);
    }
  } catch {
    /* older Chromium: no partitioned cookie support */
  }
  return cookies;
}

/** Builds and sends an `add` for `url`. Cookies stay in memory only. */
export async function sendAdd({ url, filename, referrer, size, mime, kind = 'file', storeId, timeoutMs }) {
  const cookies = await cookiesFor(url, storeId, referrer);
  const req = buildAddRequest({ url, filename, referrer, size, mime, cookies, userAgent: navigator.userAgent, kind });
  return sendNative(req, timeoutMs);
}

export async function loadSettings() {
  const raw = await chrome.storage.local.get('settings');
  return normalizeSettings(raw.settings);
}

export async function saveSettings(settings) {
  await chrome.storage.local.set({ settings: normalizeSettings(settings) });
}

export const tabKey = (tabId) => `tab:${tabId}`;
