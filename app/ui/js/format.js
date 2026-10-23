// Formatting helpers. Pure functions, no DOM: unit-tested with `node --test`.
// Sizes use decimal units like Finder (1 GB = 1000^3 bytes).

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

/** Index into UNITS for a byte count. */
export function unitIndex(n) {
  let i = 0;
  let v = Math.abs(n);
  while (v >= 1000 && i < UNITS.length - 1) {
    v /= 1000;
    i++;
  }
  // 999.96 MB rounds to "1000 MB": promote to the next unit instead.
  if (i < UNITS.length - 1 && i > 0 && Number((v).toFixed(digitsFor(i, v))) >= 1000) i++;
  return i;
}

function digitsFor(i, v) {
  if (i <= 1) return 0; // B, KB
  if (i === 2) return v < 100 ? 1 : 0; // MB
  return v < 100 ? 1 : 0; // GB, TB
}

/** Number part of `n` expressed in unit `i`, with the right precision. */
function scaled(n, i, digits) {
  const v = n / 1000 ** i;
  const d = digits ?? digitsFor(i, Math.abs(v));
  return v.toFixed(d);
}

/** "512 B", "34 KB", "24.0 MB", "418 MB", "4.1 GB". Invalid input → "—". */
export function formatBytes(n) {
  if (n == null || !Number.isFinite(n) || n < 0) return '—';
  if (n < 1000) return `${Math.round(n)} B`;
  const i = unitIndex(n);
  return `${scaled(n, i)} ${UNITS[i]}`;
}

/**
 * "1.6 / 4.1 GB" when both fit the total's unit, "512 KB / 4.1 GB" when the
 * downloaded part is tiny, "1.6 GB" when the total is unknown.
 */
export function formatProgress(downloaded, total) {
  if (total == null || !Number.isFinite(total) || total <= 0) return formatBytes(downloaded);
  const i = unitIndex(total);
  if (i === 0) return `${Math.round(downloaded)} / ${Math.round(total)} B`;
  const t = scaled(total, i);
  const digits = t.includes('.') ? 1 : 0;
  // Below 1 % of a unit the shared unit reads as "0.0 / 4.1 GB": show its own unit.
  if (downloaded > 0 && downloaded < 1000 ** i / 10) return `${formatBytes(downloaded)} / ${t} ${UNITS[i]}`;
  return `${scaled(Math.min(downloaded, total), i, digits)} / ${t} ${UNITS[i]}`;
}

/** "38.2 MB/s", "812 KB/s", "0 KB/s". */
export function formatSpeed(bps) {
  if (bps == null || !Number.isFinite(bps) || bps <= 0) return '0 KB/s';
  if (bps < 1000) return `${Math.round(bps)} B/s`;
  const i = unitIndex(bps);
  const v = bps / 1000 ** i;
  const d = i <= 1 ? 0 : v < 100 ? 1 : 0;
  return `${v.toFixed(d)} ${UNITS[i]}/s`;
}

/** Compact speed for tight spots (the island wing): "38 MB/s", "4.2 MB/s". */
export function formatSpeedShort(bps) {
  if (bps == null || !Number.isFinite(bps) || bps <= 0) return '0 KB/s';
  if (bps < 1000) return `${Math.round(bps)} B/s`;
  const i = unitIndex(bps);
  const v = bps / 1000 ** i;
  const d = i >= 2 && v < 10 ? 1 : 0;
  return `${v.toFixed(d)} ${UNITS[i]}/s`;
}

/**
 * Time left: "0:42", "1:07", "1:02:03", "2 d 4 h". Null/unknown → "".
 */
export function formatEta(secs) {
  if (secs == null || !Number.isFinite(secs) || secs < 0) return '';
  const s = Math.round(secs);
  if (s >= 86400 * 2) {
    const d = Math.floor(s / 86400);
    const h = Math.floor((s % 86400) / 3600);
    return h ? `${d} d ${h} h` : `${d} d`;
  }
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (x) => String(x).padStart(2, '0');
  return h ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

/** "Just now", "5 min ago", "2 h ago", "Yesterday", "3 days ago", else a date. Unix seconds. */
export function formatAgo(ts, now = Date.now() / 1000) {
  if (ts == null) return '';
  const d = Math.max(0, now - ts);
  if (d < 60) return 'Just now';
  if (d < 3600) return `${Math.floor(d / 60)} min ago`;
  if (d < 86400) return `${Math.floor(d / 3600)} h ago`;
  if (d < 2 * 86400) return 'Yesterday';
  if (d < 7 * 86400) return `${Math.floor(d / 86400)} days ago`;
  return new Date(ts * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

/** Percentage 0–100 (integer), or null if the total is unknown. */
export function percent(downloaded, total) {
  if (!total) return null;
  return Math.max(0, Math.min(100, Math.floor((downloaded / total) * 100)));
}

/** "/Users/me/Downloads/Spate/x.iso" → "~/Downloads/Spate/x.iso". */
export function shortPath(p) {
  if (!p) return '';
  return String(p).replace(/^\/Users\/[^/]+/, '~');
}

/** Directory part of a path. */
export function dirname(p) {
  if (!p) return '';
  const s = String(p);
  const i = s.lastIndexOf('/');
  return i > 0 ? s.slice(0, i) : s;
}

/** "Finder tag" category for a file name, mirroring spate-core `finder_tag`. */
export function fileKind(name) {
  const m = /\.([^.]+)$/.exec(String(name || '').toLowerCase());
  const ext = m ? m[1] : '';
  return KIND_BY_EXT[ext] || null;
}

const KINDS = {
  Video: 'mp4 mkv mov avi webm m4v flv wmv mpg mpeg ts 3gp',
  Audio: 'mp3 m4a aac flac wav ogg opus wma aiff aif alac',
  Archive: 'zip rar 7z tar gz tgz bz2 xz zst tbz2 txz lz',
  App: 'dmg pkg mpkg app iso',
  Document: 'pdf doc docx xls xlsx ppt pptx txt rtf epub md csv odt ods pages numbers key',
  Image: 'jpg jpeg png gif webp heic heif tif tiff bmp svg avif',
};
const KIND_BY_EXT = {};
for (const [k, list] of Object.entries(KINDS)) for (const e of list.split(' ')) KIND_BY_EXT[e] = k;
export const KIND_NAMES = Object.keys(KINDS);

/** File name from a URL: last path segment, decoded; falls back to the host. */
export function filenameFromUrl(url) {
  try {
    const u = new URL(url);
    const last = u.pathname.split('/').filter(Boolean).pop();
    if (last) return decodeURIComponent(last);
    return u.hostname;
  } catch {
    return 'download';
  }
}

/** True for http(s) URLs worth handing to the daemon. */
export function looksLikeUrl(s) {
  if (!s) return false;
  try {
    const u = new URL(String(s).trim());
    return u.protocol === 'http:' || u.protocol === 'https:';
  } catch {
    return false;
  }
}
