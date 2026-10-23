// Pure decision logic for the Spate extension. No `chrome.*` here, so it runs under `node --test`.

export const NATIVE_HOST = 'com.spate.host';
export const NATIVE_TIMEOUT_MS = 2000;
export const BYPASS_WINDOW_MS = 2000;
export const MAX_MEDIA_PER_TAB = 50;
export const THRESHOLD_CHOICES_MB = [5, 20, 50, 100];

export const DEFAULT_TYPES = [
  'zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz', 'dmg', 'pkg', 'iso', 'img', 'exe', 'msi',
  'mp4', 'mkv', 'mov', 'avi', 'webm', 'mp3', 'flac', 'wav', 'bin',
];

export const DEFAULT_SETTINGS = Object.freeze({
  enabled: true,
  thresholdMB: 20,
  types: DEFAULT_TYPES,
  neverSites: [],
});

const MB = 1024 * 1024;

// ---------------------------------------------------------------- settings

export function parseList(text) {
  if (Array.isArray(text)) return text;
  return String(text ?? '').split(/[\s,;]+/).filter(Boolean);
}

export function normalizeType(t) {
  return String(t).trim().toLowerCase().replace(/^\*?\./, '');
}

/** Bare host pattern from whatever the user typed ("https://www.x.com/a" → "www.x.com"). */
export function normalizeSite(s) {
  let v = String(s).trim().toLowerCase();
  v = v.replace(/^[a-z][a-z0-9+.-]*:\/\//, '').replace(/^\*\./, '');
  v = v.split(/[/?#]/)[0];
  v = v.replace(/^[^@]*@/, '').replace(/:\d+$/, '').replace(/\.$/, '');
  return v;
}

export function normalizeSettings(raw = {}) {
  const s = { ...DEFAULT_SETTINGS, ...(raw || {}) };
  const threshold = Number(s.thresholdMB);
  const dedupe = (arr) => [...new Set(arr.filter(Boolean))];
  return {
    enabled: s.enabled !== false,
    thresholdMB: Number.isFinite(threshold) && threshold > 0 ? threshold : DEFAULT_SETTINGS.thresholdMB,
    types: dedupe(parseList(s.types).map(normalizeType)),
    neverSites: dedupe(parseList(s.neverSites).map(normalizeSite)),
  };
}

// ---------------------------------------------------------------- urls and names

export function urlScheme(url) {
  const m = /^([a-z][a-z0-9+.-]*):/i.exec(String(url || ''));
  return m ? m[1].toLowerCase() : '';
}

export function isHttpUrl(url) {
  const s = urlScheme(url);
  return s === 'http' || s === 'https';
}

export function hostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return '';
  }
}

/** Last path segment of a URL or a local path, without query/hash. */
export function baseName(nameOrUrl) {
  let s = String(nameOrUrl || '');
  if (isHttpUrl(s)) {
    try {
      s = decodeURIComponent(new URL(s).pathname);
    } catch {
      s = s.split(/[?#]/)[0];
    }
  }
  const parts = s.split(/[/\\]/);
  return parts[parts.length - 1] || '';
}

export function fileExtension(nameOrUrl) {
  const b = baseName(nameOrUrl);
  const i = b.lastIndexOf('.');
  if (i <= 0 || i === b.length - 1) return '';
  const ext = b.slice(i + 1).toLowerCase();
  return /^[a-z0-9]{1,8}$/.test(ext) ? ext : '';
}

const MIME_EXT = {
  'application/zip': 'zip',
  'application/x-zip-compressed': 'zip',
  'application/vnd.rar': 'rar',
  'application/x-rar-compressed': 'rar',
  'application/x-7z-compressed': '7z',
  'application/x-tar': 'tar',
  'application/gzip': 'gz',
  'application/x-gzip': 'gz',
  'application/x-bzip2': 'bz2',
  'application/x-xz': 'xz',
  'application/x-apple-diskimage': 'dmg',
  'application/x-iso9660-image': 'iso',
  'application/x-msdownload': 'exe',
  'application/x-msi': 'msi',
  'video/mp4': 'mp4',
  'video/x-matroska': 'mkv',
  'video/quicktime': 'mov',
  'video/x-msvideo': 'avi',
  'video/webm': 'webm',
  'audio/mpeg': 'mp3',
  'audio/flac': 'flac',
  'audio/wav': 'wav',
  'audio/x-wav': 'wav',
  'audio/webm': 'webm',
  'audio/mp4': 'm4a',
};

export function mimeBase(mime) {
  return String(mime || '').split(';')[0].trim().toLowerCase();
}

export function extFromMime(mime) {
  return MIME_EXT[mimeBase(mime)] || '';
}

/** Extension of a Brave DownloadItem: filename (if known yet), then final URL, URL, MIME. */
export function itemExtension(item) {
  return (
    fileExtension(item.filename) ||
    fileExtension(item.finalUrl) ||
    fileExtension(item.url) ||
    extFromMime(item.mime)
  );
}

export function siteMatches(host, pattern) {
  if (!host || !pattern) return false;
  return host === pattern || host.endsWith('.' + pattern);
}

export function isExcludedSite(urls, neverSites) {
  const hosts = urls.map(hostOf).filter(Boolean);
  return neverSites.some((p) => hosts.some((h) => siteMatches(h, p)));
}

// ---------------------------------------------------------------- capture decision

/** Known size in bytes of a DownloadItem, or null. */
export function itemSize(item) {
  if (item.totalSize > 0) return item.totalSize;
  if (item.fileSize > 0) return item.fileSize;
  return null;
}

/**
 * Should Spate take over this Brave download?
 * @param item     chrome.downloads.DownloadItem (only plain fields are read)
 * @param settings normalized settings
 * @param ctx      { selfId, bypass } — bypass = an Option-click was seen just before
 * @returns {{capture: boolean, reason: string}}
 */
export function decideCapture(item, settings, ctx = {}) {
  const no = (reason) => ({ capture: false, reason });
  if (!settings.enabled) return no('disabled');
  const url = item.finalUrl || item.url;
  if (!isHttpUrl(item.url) || (item.finalUrl && !isHttpUrl(item.finalUrl))) return no('scheme');
  if (item.byExtensionId && item.byExtensionId === ctx.selfId) return no('self');
  if (item.state && item.state !== 'in_progress') return no('state');
  if (item.incognito) return no('incognito');
  if (ctx.bypass) return no('bypass');
  if (isExcludedSite([url, item.url, item.referrer].filter(Boolean), settings.neverSites)) return no('site');

  const ext = itemExtension(item);
  if (ext && settings.types.includes(ext)) return { capture: true, reason: 'type' };
  const size = itemSize(item);
  if (size == null) return no('unknown_size');
  if (size >= settings.thresholdMB * MB) return { capture: true, reason: 'size' };
  return no('small');
}

// ---------------------------------------------------------------- Option-click bypass

/**
 * Records are `{ tabId, href, ts }` of recent Option-clicks. Returns whether this download
 * should stay in Brave, and the records left afterwards (consumed + expired ones removed).
 * DownloadItem has no tabId, so a record from any tab within the window counts;
 * one whose href matches the download URL is preferred.
 */
export function consumeBypass(records, item, now, windowMs = BYPASS_WINDOW_MS) {
  const live = (records || []).filter((r) => now - r.ts >= 0 && now - r.ts <= windowMs);
  if (!live.length) return { bypass: false, remaining: [] };
  const urls = [item.url, item.finalUrl].filter(Boolean);
  let hit = live.find((r) => r.href && urls.includes(r.href));
  if (!hit) hit = live.reduce((a, b) => (b.ts > a.ts ? b : a));
  return { bypass: true, remaining: live.filter((r) => r !== hit) };
}

// ---------------------------------------------------------------- add request

/** Cookie header from chrome.cookies results; first occurrence of a name wins (most specific path). */
export function cookieHeader(cookies) {
  const seen = new Set();
  const parts = [];
  for (const c of cookies || []) {
    if (!c || !c.name || seen.has(c.name)) continue;
    seen.add(c.name);
    parts.push(`${c.name}=${c.value}`);
  }
  return parts.join('; ');
}

export function buildAddRequest({ url, filename, referrer, size, mime, cookies, userAgent, kind = 'file' }) {
  const headers = [];
  const cookie = cookieHeader(cookies);
  if (cookie) headers.push(['Cookie', cookie]);
  if (referrer && isHttpUrl(referrer)) headers.push(['Referer', referrer]);
  if (userAgent) headers.push(['User-Agent', userAgent]);
  const req = { type: 'add', url, headers, source: 'brave', kind };
  const name = baseName(filename);
  if (filename && name) req.filename = name;
  if (referrer && isHttpUrl(referrer)) req.referrer = referrer;
  if (size > 0) req.size_hint = size;
  const m = mimeBase(mime);
  if (m) req.mime = m;
  return req;
}

/** What to do after the native host answered (or didn't). */
export function captureOutcome(reply) {
  if (reply && reply.ok === true) return 'handoff';
  return 'resume';
}

export function errorCode(reply) {
  const e = reply && typeof reply.error === 'string' ? reply.error : '';
  return e.split(':')[0].trim();
}

/** Short user-facing text for a failed native reply. */
export function describeError(reply) {
  switch (errorCode(reply)) {
    case 'not_supported':
      return 'Video download arrives in a later version.';
    case 'app_not_running':
      return "Spate isn't running. Open the Spate app and try again.";
    case 'host_missing':
      return 'Spate helper not installed. Run scripts/install-native-host.sh.';
    case 'timeout':
      return 'Spate did not answer in time.';
    default:
      return (reply && reply.error) || 'Something went wrong.';
  }
}

// ---------------------------------------------------------------- media detection

const STREAM_MIMES = {
  'application/vnd.apple.mpegurl': 'HLS',
  'application/x-mpegurl': 'HLS',
  'audio/mpegurl': 'HLS',
  'audio/x-mpegurl': 'HLS',
  'application/dash+xml': 'DASH',
};

const SEGMENT_MIMES = new Set(['video/mp2t', 'video/iso.segment', 'audio/iso.segment']);
const SEGMENT_EXTS = new Set(['ts', 'm4s', 'cmfv', 'cmfa', 'aac', 'vtt', 'webvtt']);
const RANGE_PARAMS = ['range', 'rn', 'rbuf', 'bytestart', 'byteend', 'ump', 'srfvp'];
const MIN_MEDIA_BYTES = 256 * 1024;

export function formatOf(url, mime) {
  const m = mimeBase(mime);
  if (STREAM_MIMES[m]) return STREAM_MIMES[m];
  const ext = fileExtension(url);
  if (ext === 'm3u8') return 'HLS';
  if (ext === 'mpd') return 'DASH';
  const byMime = {
    'video/mp4': 'MP4', 'video/webm': 'WebM', 'video/quicktime': 'MOV', 'video/x-matroska': 'MKV',
    'video/ogg': 'OGG', 'audio/mpeg': 'MP3', 'audio/mp4': 'M4A', 'audio/webm': 'WebM',
    'audio/ogg': 'OGG', 'audio/flac': 'FLAC', 'audio/wav': 'WAV', 'audio/x-wav': 'WAV',
  }[m];
  if (byMime) return byMime;
  if (ext === 'webm') return 'WebM';
  if (ext) return ext.toUpperCase();
  if (m.startsWith('video/') || m.startsWith('audio/')) return m.split('/')[1].toUpperCase();
  return '';
}

export function isStreamFormat(format) {
  return format === 'HLS' || format === 'DASH';
}

/** Stable identity for a media URL: byte-range query parameters removed, hash dropped. */
export function mediaKey(url) {
  try {
    const u = new URL(url);
    for (const p of RANGE_PARAMS) u.searchParams.delete(p);
    u.hash = '';
    return u.toString();
  } catch {
    return String(url);
  }
}

/** Total size from "bytes 0-99/12345" (null for "*"). */
export function contentRangeTotal(value) {
  const m = /\/\s*(\d+)\s*$/.exec(String(value || ''));
  return m ? Number(m[1]) : null;
}

export function headerValue(headers, name) {
  const n = name.toLowerCase();
  const h = (headers || []).find((x) => x.name && x.name.toLowerCase() === n);
  return h ? h.value : undefined;
}

/**
 * Turns a webRequest.onHeadersReceived details object into a media entry, or null.
 * Entry: { key, url, mime, format, size, stream, ts }
 */
export function classifyMediaResponse(details) {
  if (!details || details.tabId == null || details.tabId < 0) return null;
  if (!isHttpUrl(details.url)) return null;
  if (details.statusCode && (details.statusCode < 200 || details.statusCode >= 300)) return null;
  const mime = mimeBase(headerValue(details.responseHeaders, 'content-type'));
  const ext = fileExtension(details.url);
  if (SEGMENT_MIMES.has(mime) || SEGMENT_EXTS.has(ext)) return null;

  const format = formatOf(details.url, mime);
  const stream = isStreamFormat(format);
  const isMedia = stream || mime.startsWith('video/') || mime.startsWith('audio/');
  if (!isMedia) return null;

  let size = null;
  const range = headerValue(details.responseHeaders, 'content-range');
  if (range) size = contentRangeTotal(range);
  if (size == null && details.statusCode !== 206) {
    const len = Number(headerValue(details.responseHeaders, 'content-length'));
    if (Number.isFinite(len) && len > 0) size = len;
  }
  if (!stream && size != null && size < MIN_MEDIA_BYTES) return null; // UI sounds, previews
  return { key: mediaKey(details.url), url: details.url, mime, format, size, stream, ts: details.timeStamp || 0 };
}

/** Adds or refreshes an entry in a tab's network list (newest kept, capped). */
export function addNetworkEntry(list, entry, max = MAX_MEDIA_PER_TAB) {
  const out = (list || []).slice();
  const i = out.findIndex((e) => e.key === entry.key);
  if (i >= 0) {
    const prev = out[i];
    out[i] = { ...prev, size: Math.max(prev.size || 0, entry.size || 0) || null, mime: prev.mime || entry.mime };
    return out;
  }
  out.push(entry);
  return out.length > max ? out.slice(out.length - max) : out;
}

/**
 * Combines what the page's DOM reports (`dom`: list of video descriptions from the content
 * script) with what the network showed (`network`: entries from classifyMediaResponse).
 * Returns { items, drm }. DRM (EME active on any video) hides everything for the tab.
 */
export function mergeMedia(network, dom, pageTitle = '') {
  const videos = dom || [];
  if (videos.some((v) => v.drm)) return { items: [], drm: true };
  const net = (network || []).slice();
  const claimed = new Set();
  const byKey = new Map(net.map((e) => [e.key, e]));
  const items = [];

  for (const v of videos) {
    const candidates = [v.src, ...(v.sources || [])].filter(isHttpUrl);
    let entry = null;
    let url = null;
    for (const c of candidates) {
      const e = byKey.get(mediaKey(c));
      if (e) { entry = e; url = c; break; }
    }
    if (!entry && candidates.length) url = candidates[0];
    if (!url) {
      // blob:/MSE player — attach the first stream manifest the tab loaded, if any.
      entry = net.find((e) => e.stream && !claimed.has(e.key));
      if (!entry) continue;
      url = entry.url;
    }
    if (entry) claimed.add(entry.key);
    const format = entry ? entry.format : formatOf(url, v.type);
    items.push({
      id: mediaKey(url),
      url,
      title: v.title || pageTitle || baseName(url),
      width: v.width || null,
      height: v.height || null,
      duration: Number.isFinite(v.duration) && v.duration > 0 ? v.duration : null,
      playing: !!v.playing,
      thumb: v.thumb || v.poster || null,
      size: entry ? entry.size : null,
      mime: entry ? entry.mime : v.type || '',
      format,
      stream: isStreamFormat(format),
    });
  }

  // Leftover network hits. For streams keep only the first manifest per format
  // (typically the master playlist; later ones are its variants).
  const streamSeen = new Set(items.filter((i) => i.stream).map((i) => i.format));
  for (const e of net) {
    if (claimed.has(e.key)) continue;
    if (e.stream) {
      if (streamSeen.has(e.format)) continue;
      streamSeen.add(e.format);
    }
    items.push({
      id: e.key, url: e.url, title: pageTitle || baseName(e.url), width: null, height: null,
      duration: null, playing: false, thumb: null, size: e.size, mime: e.mime,
      format: e.format, stream: e.stream,
    });
  }

  const seen = new Set();
  const unique = items.filter((i) => (seen.has(i.id) ? false : seen.add(i.id)));
  unique.sort((a, b) => Number(b.playing) - Number(a.playing));
  return { items: unique, drm: false };
}

// ---------------------------------------------------------------- manifests

function attr(line, name) {
  const m = new RegExp(`(?:^|[,:])${name}=("([^"]*)"|[^,]*)`, 'i').exec(line);
  return m ? (m[2] !== undefined ? m[2] : m[1]) : undefined;
}

/** Variants of an HLS master playlist: [{bandwidth, width, height}]. Empty for media playlists. */
export function parseHlsMaster(text) {
  const out = [];
  for (const line of String(text || '').split(/\r?\n/)) {
    if (!line.startsWith('#EXT-X-STREAM-INF')) continue;
    const bw = Number(attr(line, 'AVERAGE-BANDWIDTH') || attr(line, 'BANDWIDTH'));
    const res = attr(line, 'RESOLUTION');
    const [w, h] = res ? res.split('x').map(Number) : [null, null];
    out.push({ bandwidth: Number.isFinite(bw) ? bw : null, width: w || null, height: h || null });
  }
  return out;
}

/** Sum of #EXTINF durations of an HLS media playlist (null if none). */
export function hlsMediaDuration(text) {
  let total = 0;
  let any = false;
  for (const m of String(text || '').matchAll(/#EXTINF:\s*([\d.]+)/g)) {
    total += Number(m[1]);
    any = true;
  }
  return any ? total : null;
}

/** "PT1H2M3.5S" → 3723.5 */
export function parseIsoDuration(s) {
  const m = /^P(?:(\d+(?:\.\d+)?)D)?(?:T(?:(\d+(?:\.\d+)?)H)?(?:(\d+(?:\.\d+)?)M)?(?:(\d+(?:\.\d+)?)S)?)?$/.exec(
    String(s || '').trim(),
  );
  if (!m) return null;
  const [d, h, min, sec] = m.slice(1).map((x) => Number(x || 0));
  return d * 86400 + h * 3600 + min * 60 + sec;
}

/** DASH MPD: { duration, variants: [{bandwidth, width, height}] } for video representations. */
export function parseDashMpd(text) {
  const t = String(text || '');
  const dur = /mediaPresentationDuration="([^"]+)"/.exec(t);
  const variants = [];
  for (const m of t.matchAll(/<Representation\b([^>]*)>/g)) {
    const a = m[1];
    const get = (n) => {
      const r = new RegExp(`\\b${n}="([^"]*)"`).exec(a);
      return r ? Number(r[1]) : null;
    };
    const height = get('height');
    if (!height) continue; // audio-only representation
    variants.push({ bandwidth: get('bandwidth'), width: get('width'), height });
  }
  return { duration: dur ? parseIsoDuration(dur[1]) : null, variants };
}

export function bestVariant(variants) {
  if (!variants || !variants.length) return null;
  return variants.reduce((a, b) =>
    (b.height || 0) > (a.height || 0) || ((b.height || 0) === (a.height || 0) && (b.bandwidth || 0) > (a.bandwidth || 0))
      ? b
      : a,
  );
}

// ---------------------------------------------------------------- display helpers

/** Size in bytes: exact if known, else bitrate (bits/s) × duration. */
export function estimateSize({ size, bitrate, duration }) {
  if (size > 0) return { bytes: size, exact: true };
  if (bitrate > 0 && duration > 0) return { bytes: Math.round((bitrate * duration) / 8), exact: false };
  return null;
}

export function formatBytes(n) {
  if (!(n >= 0)) return '';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = n;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return `${i === 0 ? v : v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`;
}

export function formatDuration(sec) {
  if (!(sec > 0) || !Number.isFinite(sec)) return '';
  const s = Math.round(sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

export function qualityLabel(width, height) {
  if (!height) return '';
  const tiers = [[4320, '8K'], [2160, '4K'], [1440, '1440p'], [1080, '1080p'], [720, '720p'], [480, '480p'], [360, '360p'], [240, '240p']];
  // Portrait videos: the short side decides the tier.
  const short = width ? Math.min(width, height) : height;
  for (const [h, label] of tiers) if (short >= h * 0.95) return label;
  return `${short}p`;
}

/** Filename for a media download: page/video title + extension from the format. */
export function mediaFilename(title, format) {
  const ext = { MP4: 'mp4', WebM: 'webm', MOV: 'mov', MKV: 'mkv', MP3: 'mp3', M4A: 'm4a', HLS: 'mp4', DASH: 'mp4' }[format];
  const clean = String(title || 'video').replace(/[/\\:*?"<>|\u0000-\u001f]+/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 120);
  return ext ? `${clean || 'video'}.${ext}` : clean || 'video';
}
