import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  DEFAULT_SETTINGS,
  addNetworkEntry,
  baseName,
  bestVariant,
  buildAddRequest,
  captureOutcome,
  classifyMediaResponse,
  consumeBypass,
  contentRangeTotal,
  cookieHeader,
  decideCapture,
  describeError,
  estimateSize,
  fileExtension,
  formatBytes,
  formatDuration,
  formatOf,
  hlsMediaDuration,
  isExcludedSite,
  itemExtension,
  mediaFilename,
  mediaKey,
  mergeMedia,
  normalizeSettings,
  normalizeSite,
  parseDashMpd,
  parseHlsMaster,
  parseIsoDuration,
  qualityLabel,
} from '../lib/logic.js';

const MB = 1024 * 1024;
const S = normalizeSettings();
const SELF = 'akcgnbcpioinnkkmoobjdgnnmebipnil';
const item = (o) => ({ id: 1, url: 'https://cdn.example.com/file', state: 'in_progress', totalSize: -1, ...o });
const decide = (o, s = S, ctx = {}) => decideCapture(item(o), s, { selfId: SELF, ...ctx });

// ------------------------------------------------------------ settings

test('defaults', () => {
  assert.equal(S.enabled, true);
  assert.equal(S.thresholdMB, 20);
  assert.deepEqual(S.types, DEFAULT_SETTINGS.types);
  assert.ok(S.types.includes('dmg') && S.types.includes('bin'));
});

test('normalizeSettings cleans user input', () => {
  const s = normalizeSettings({
    enabled: false,
    thresholdMB: 'abc',
    types: '.ZIP, *.iso  mp4,zip',
    neverSites: ['https://www.Example.com/path?q', '*.foo.org', 'user@bar.net:8080', ''],
  });
  assert.equal(s.enabled, false);
  assert.equal(s.thresholdMB, 20);
  assert.deepEqual(s.types, ['zip', 'iso', 'mp4']);
  assert.deepEqual(s.neverSites, ['www.example.com', 'foo.org', 'bar.net']);
  assert.equal(normalizeSite('Example.COM.'), 'example.com');
});

// ------------------------------------------------------------ names

test('file extensions from names, urls and mime', () => {
  assert.equal(fileExtension('/Users/me/Downloads/Ubuntu.ISO'), 'iso');
  assert.equal(fileExtension('https://x.com/a/b/archive.tar.gz?token=1#x'), 'gz');
  assert.equal(fileExtension('https://x.com/download?id=3'), '');
  assert.equal(fileExtension('.bashrc'), '');
  assert.equal(fileExtension('https://x.com/a%20b.dmg'), 'dmg');
  assert.equal(baseName('https://x.com/dir/My%20File.zip?x'), 'My File.zip');
  assert.equal(itemExtension({ filename: '', url: 'https://x.com/get?id=1', mime: 'application/x-apple-diskimage' }), 'dmg');
  assert.equal(itemExtension({ filename: '', url: 'https://x.com/get', finalUrl: 'https://cdn.x.com/a.pkg' }), 'pkg');
});

test('site exclusion matches subdomains only on label boundaries', () => {
  assert.ok(isExcludedSite(['https://dl.example.com/a'], ['example.com']));
  assert.ok(isExcludedSite(['https://example.com/a'], ['example.com']));
  assert.ok(!isExcludedSite(['https://notexample.com/a'], ['example.com']));
  assert.ok(!isExcludedSite(['blob:https://example.com/x'], ['other.com']));
});

// ------------------------------------------------------------ capture decision

test('captures files at or above the threshold', () => {
  assert.deepEqual(decide({ totalSize: 20 * MB }), { capture: true, reason: 'size' });
  assert.deepEqual(decide({ totalSize: 20 * MB - 1 }), { capture: false, reason: 'small' });
  assert.deepEqual(decide({ totalSize: 6 * MB }, normalizeSettings({ thresholdMB: 5 })), { capture: true, reason: 'size' });
  assert.deepEqual(decide({ totalSize: -1, fileSize: 30 * MB }), { capture: true, reason: 'size' });
});

test('captures listed types regardless of size', () => {
  assert.deepEqual(decide({ url: 'https://x.com/tiny.zip', totalSize: 1000 }), { capture: true, reason: 'type' });
  assert.deepEqual(decide({ filename: '/Users/me/Downloads/setup.exe', totalSize: 10 }), { capture: true, reason: 'type' });
  assert.deepEqual(decide({ url: 'https://x.com/doc.pdf', totalSize: 1000 }), { capture: false, reason: 'small' });
});

test('unknown size is decided by type', () => {
  assert.deepEqual(decide({ url: 'https://x.com/movie.mkv', totalSize: 0 }), { capture: true, reason: 'type' });
  assert.deepEqual(decide({ url: 'https://x.com/report.pdf', totalSize: 0 }), { capture: false, reason: 'unknown_size' });
  assert.deepEqual(decide({ url: 'https://x.com/dl', totalSize: -1, mime: 'application/zip' }), { capture: true, reason: 'type' });
});

test('never captures blob:, data:, self-started, finished, disabled, bypassed or excluded', () => {
  const big = { totalSize: 500 * MB };
  assert.equal(decide({ ...big, url: 'blob:https://x.com/123' }).reason, 'scheme');
  assert.equal(decide({ ...big, url: 'data:application/zip;base64,AAAA' }).reason, 'scheme');
  assert.equal(decide({ ...big, finalUrl: 'blob:https://x.com/1' }).reason, 'scheme');
  assert.equal(decide({ ...big, url: 'ftp://x.com/a.zip' }).reason, 'scheme');
  assert.equal(decide({ ...big, byExtensionId: SELF }).reason, 'self');
  assert.deepEqual(decide({ ...big, byExtensionId: 'someotherextension' }), { capture: true, reason: 'size' });
  assert.equal(decide({ ...big, state: 'complete' }).reason, 'state');
  assert.equal(decide(big, normalizeSettings({ enabled: false })).reason, 'disabled');
  assert.equal(decide(big, S, { bypass: true }).reason, 'bypass');
  assert.equal(decide({ ...big, referrer: 'https://www.site.com/page' }, normalizeSettings({ neverSites: ['site.com'] })).reason, 'site');
  assert.equal(decide({ ...big, incognito: true }).reason, 'incognito');
});

// ------------------------------------------------------------ Option-click bypass

test('bypass: a recent Option-click keeps the next download in Brave, once', () => {
  const now = 10_000;
  const recs = [{ tabId: 3, href: 'https://x.com/a.zip', ts: now - 500 }];
  const r1 = consumeBypass(recs, { url: 'https://x.com/a.zip' }, now);
  assert.equal(r1.bypass, true);
  assert.deepEqual(r1.remaining, []);
  assert.equal(consumeBypass(r1.remaining, { url: 'https://x.com/a.zip' }, now).bypass, false);
});

test('bypass: expires after the window and prefers a matching href', () => {
  const now = 10_000;
  assert.equal(consumeBypass([{ tabId: 1, href: null, ts: now - 2500 }], { url: 'u' }, now).bypass, false);
  const recs = [
    { tabId: 1, href: 'https://x.com/a.zip', ts: now - 1500 },
    { tabId: 2, href: 'https://y.com/b.zip', ts: now - 100 },
  ];
  const r = consumeBypass(recs, { url: 'https://x.com/a.zip' }, now);
  assert.equal(r.bypass, true);
  assert.deepEqual(r.remaining.map((x) => x.tabId), [2]);
  // No match → the most recent record is used (redirected downloads change URL).
  const r2 = consumeBypass(recs, { url: 'https://cdn.z.com/c.zip' }, now);
  assert.deepEqual(r2.remaining.map((x) => x.tabId), [1]);
});

// ------------------------------------------------------------ add request

test('cookie header keeps the first cookie of each name', () => {
  assert.equal(
    cookieHeader([{ name: 'a', value: '1' }, { name: 'b', value: 'x=y' }, { name: 'a', value: '2' }]),
    'a=1; b=x=y',
  );
  assert.equal(cookieHeader([]), '');
});

test('buildAddRequest matches the AddRequest contract', () => {
  const req = buildAddRequest({
    url: 'https://cdn.x.com/big.iso',
    filename: '/Users/me/Downloads/big.iso',
    referrer: 'https://x.com/downloads',
    size: 700 * MB,
    mime: 'application/x-iso9660-image; charset=binary',
    cookies: [{ name: 'sid', value: 'secret' }],
    userAgent: 'Mozilla/5.0 Brave',
  });
  assert.deepEqual(req, {
    type: 'add',
    url: 'https://cdn.x.com/big.iso',
    headers: [['Cookie', 'sid=secret'], ['Referer', 'https://x.com/downloads'], ['User-Agent', 'Mozilla/5.0 Brave']],
    source: 'brave',
    kind: 'file',
    filename: 'big.iso',
    referrer: 'https://x.com/downloads',
    size_hint: 700 * MB,
    mime: 'application/x-iso9660-image',
  });
  const bare = buildAddRequest({ url: 'https://x.com/v.m3u8', cookies: [], kind: 'media', size: null, referrer: '' });
  assert.deepEqual(bare, { type: 'add', url: 'https://x.com/v.m3u8', headers: [], source: 'brave', kind: 'media' });
});

test('capture outcome: only ok:true hands over', () => {
  assert.equal(captureOutcome({ ok: true, job_id: 'x' }), 'handoff');
  assert.equal(captureOutcome({ ok: false, error: 'app_not_running' }), 'resume');
  assert.equal(captureOutcome({ ok: false, error: 'timeout' }), 'resume');
  assert.equal(captureOutcome(undefined), 'resume');
  assert.equal(describeError({ ok: false, error: 'not_supported: v2' }), 'Video download arrives in a later version.');
  assert.match(describeError({ ok: false, error: 'app_not_running' }), /isn't running/);
});

// ------------------------------------------------------------ media detection

const resp = (url, headers, o = {}) => ({
  tabId: 7,
  url,
  statusCode: 200,
  timeStamp: 1,
  responseHeaders: Object.entries(headers).map(([name, value]) => ({ name, value })),
  ...o,
});

test('classifies media responses', () => {
  const mp4 = classifyMediaResponse(resp('https://v.com/movie.mp4', { 'Content-Type': 'video/mp4', 'Content-Length': String(80 * MB) }));
  assert.equal(mp4.format, 'MP4');
  assert.equal(mp4.size, 80 * MB);
  assert.equal(mp4.stream, false);

  const ranged = classifyMediaResponse(
    resp('https://v.com/m.webm', { 'content-type': 'video/webm', 'content-range': 'bytes 0-1023/52428800', 'content-length': '1024' }, { statusCode: 206 }),
  );
  assert.equal(ranged.size, 52428800);
  assert.equal(ranged.format, 'WebM');

  const hls = classifyMediaResponse(resp('https://v.com/master.m3u8?t=1', { 'content-type': 'application/vnd.apple.mpegurl' }));
  assert.equal(hls.format, 'HLS');
  assert.equal(hls.stream, true);
  const hlsByExt = classifyMediaResponse(resp('https://v.com/index.m3u8', { 'content-type': 'text/plain' }));
  assert.equal(hlsByExt.format, 'HLS');
  const dash = classifyMediaResponse(resp('https://v.com/manifest.mpd', { 'content-type': 'application/dash+xml' }));
  assert.equal(dash.format, 'DASH');
});

test('ignores segments, small sounds, errors, non-media and background requests', () => {
  assert.equal(classifyMediaResponse(resp('https://v.com/seg-1.ts', { 'content-type': 'video/mp2t' })), null);
  assert.equal(classifyMediaResponse(resp('https://v.com/chunk.m4s', { 'content-type': 'video/mp4' })), null);
  assert.equal(classifyMediaResponse(resp('https://v.com/ping.mp3', { 'content-type': 'audio/mpeg', 'content-length': '20000' })), null);
  assert.equal(classifyMediaResponse(resp('https://v.com/a.mp4', { 'content-type': 'video/mp4' }, { statusCode: 404 })), null);
  assert.equal(classifyMediaResponse(resp('https://v.com/page', { 'content-type': 'text/html' })), null);
  assert.equal(classifyMediaResponse(resp('https://v.com/a.mp4', { 'content-type': 'video/mp4' }, { tabId: -1 })), null);
});

test('media keys drop byte-range parameters', () => {
  assert.equal(
    mediaKey('https://r1.googlevideo.com/videoplayback?itag=22&range=0-1000&rn=3#t=1'),
    'https://r1.googlevideo.com/videoplayback?itag=22',
  );
  assert.equal(contentRangeTotal('bytes 0-99/*'), null);
  assert.equal(contentRangeTotal('bytes 0-99/1234'), 1234);
});

test('network list dedupes, keeps the largest size and is capped', () => {
  const e = (url, size) => ({ key: mediaKey(url), url, size, mime: 'video/mp4', format: 'MP4', stream: false });
  let list = addNetworkEntry([], e('https://v.com/a?range=0-1', 100));
  list = addNetworkEntry(list, e('https://v.com/a?range=2-3', 500));
  assert.equal(list.length, 1);
  assert.equal(list[0].size, 500);
  for (let i = 0; i < 60; i++) list = addNetworkEntry(list, e(`https://v.com/${i}`, 1), 50);
  assert.equal(list.length, 50);
  assert.equal(list[49].url, 'https://v.com/59');
});

test('mergeMedia joins DOM videos with network sizes and orders playing first', () => {
  const network = [
    { key: 'https://v.com/a.mp4', url: 'https://v.com/a.mp4', mime: 'video/mp4', format: 'MP4', size: 90 * MB, stream: false },
    { key: 'https://v.com/master.m3u8', url: 'https://v.com/master.m3u8', mime: 'application/vnd.apple.mpegurl', format: 'HLS', size: null, stream: true },
    { key: 'https://v.com/720.m3u8', url: 'https://v.com/720.m3u8', mime: 'application/vnd.apple.mpegurl', format: 'HLS', size: null, stream: true },
  ];
  const dom = [
    { src: 'https://v.com/a.mp4', title: 'Clip A', width: 1280, height: 720, duration: 60, playing: false, poster: 'https://v.com/a.jpg' },
    { src: 'blob:https://v.com/xyz', title: 'Live', width: 1920, height: 1080, duration: 300, playing: true, thumb: 'data:image/jpeg;base64,AA' },
  ];
  const { items, drm } = mergeMedia(network, dom, 'Page');
  assert.equal(drm, false);
  assert.equal(items.length, 2, 'variant playlist is folded into the master');
  assert.equal(items[0].title, 'Live');
  assert.equal(items[0].playing, true);
  assert.equal(items[0].format, 'HLS');
  assert.equal(items[0].url, 'https://v.com/master.m3u8');
  assert.equal(items[0].thumb, 'data:image/jpeg;base64,AA');
  assert.equal(items[1].size, 90 * MB);
  assert.equal(items[1].thumb, 'https://v.com/a.jpg');
  assert.equal(items[1].height, 720);
});

test('mergeMedia lists network-only media and skips blob videos without a source', () => {
  const network = [{ key: 'https://a.com/song.mp3', url: 'https://a.com/song.mp3', mime: 'audio/mpeg', format: 'MP3', size: 5 * MB, stream: false }];
  const { items } = mergeMedia(network, [{ src: 'blob:https://a.com/1', title: 'x' }], 'Song page');
  assert.equal(items.length, 1);
  assert.equal(items[0].title, 'Song page');
  assert.equal(items[0].format, 'MP3');
  assert.deepEqual(mergeMedia([], [], '').items, []);
  // A DOM video with a plain http src and no network hit still shows (size unknown).
  const only = mergeMedia([], [{ src: 'https://a.com/v.webm', title: 'V' }]).items;
  assert.equal(only[0].format, 'WebM');
});

test('mergeMedia hides everything when DRM is active', () => {
  const network = [{ key: 'k', url: 'https://n.com/m.mpd', format: 'DASH', stream: true }];
  assert.deepEqual(mergeMedia(network, [{ src: 'blob:x', drm: true }]), { items: [], drm: true });
});

// ------------------------------------------------------------ manifests

test('HLS master playlist parsing', () => {
  const m3u8 = [
    '#EXTM3U',
    '#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS="avc1.4d401e,mp4a.40.2"',
    '360.m3u8',
    '#EXT-X-STREAM-INF:BANDWIDTH=5000000,AVERAGE-BANDWIDTH=4500000,RESOLUTION=1920x1080',
    '1080.m3u8',
  ].join('\n');
  const v = parseHlsMaster(m3u8);
  assert.equal(v.length, 2);
  assert.deepEqual(bestVariant(v), { bandwidth: 4500000, width: 1920, height: 1080 });
  assert.equal(hlsMediaDuration('#EXTM3U\n#EXTINF:6.0,\na.ts\n#EXTINF:4.5,\nb.ts'), 10.5);
  assert.equal(hlsMediaDuration(m3u8), null);
});

test('DASH MPD parsing', () => {
  const mpd = `<MPD mediaPresentationDuration="PT1H2M3.5S"><Period>
    <AdaptationSet mimeType="video/mp4">
      <Representation id="1" bandwidth="1000000" width="1280" height="720"/>
      <Representation id="2" bandwidth="3000000" width="1920" height="1080"/>
    </AdaptationSet>
    <AdaptationSet mimeType="audio/mp4"><Representation id="a" bandwidth="128000"/></AdaptationSet>
  </Period></MPD>`;
  const d = parseDashMpd(mpd);
  assert.equal(d.duration, 3723.5);
  assert.equal(d.variants.length, 2);
  assert.equal(bestVariant(d.variants).height, 1080);
  assert.equal(parseIsoDuration('PT90S'), 90);
  assert.equal(parseIsoDuration('garbage'), null);
});

// ------------------------------------------------------------ display

test('display helpers', () => {
  assert.deepEqual(estimateSize({ size: 1000 }), { bytes: 1000, exact: true });
  assert.deepEqual(estimateSize({ bitrate: 8_000_000, duration: 10 }), { bytes: 10_000_000, exact: false });
  assert.equal(estimateSize({}), null);
  assert.equal(formatBytes(512), '512 B');
  assert.equal(formatBytes(1_500_000), '1.5 MB');
  assert.equal(formatBytes(734_003_200), '734 MB');
  assert.equal(formatDuration(65), '1:05');
  assert.equal(formatDuration(3723.4), '1:02:03');
  assert.equal(formatDuration(NaN), '');
  assert.equal(qualityLabel(1920, 1080), '1080p');
  assert.equal(qualityLabel(1080, 1920), '1080p');
  assert.equal(qualityLabel(3840, 2160), '4K');
  assert.equal(qualityLabel(1280, 718), '720p');
  assert.equal(qualityLabel(null, null), '');
  assert.equal(formatOf('https://x/a', 'video/quicktime'), 'MOV');
  assert.equal(mediaFilename('My: Video / Part 1?', 'HLS'), 'My Video Part 1.mp4');
  assert.equal(mediaFilename('', 'MP3'), 'video.mp3');
});
