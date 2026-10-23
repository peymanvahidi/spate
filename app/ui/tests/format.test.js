import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  formatBytes, formatProgress, formatSpeed, formatSpeedShort, formatEta, formatAgo, percent, shortPath, fileKind, filenameFromUrl, looksLikeUrl,
} from '../js/format.js';

test('formatBytes uses decimal units like Finder', () => {
  assert.equal(formatBytes(0), '0 B');
  assert.equal(formatBytes(512), '512 B');
  assert.equal(formatBytes(34_400), '34 KB');
  assert.equal(formatBytes(24_117_248), '24.1 MB');
  assert.equal(formatBytes(418_316_288), '418 MB');
  assert.equal(formatBytes(4_102_029_312), '4.1 GB');
  assert.equal(formatBytes(999_960_000), '1.0 GB');
  assert.equal(formatBytes(2.5e12), '2.5 TB');
  assert.equal(formatBytes(null), '—');
  assert.equal(formatBytes(-1), '—');
});

test('formatProgress shares the total unit', () => {
  assert.equal(formatProgress(1.6e9, 4.1e9), '1.6 / 4.1 GB');
  assert.equal(formatProgress(10.2e6, 24.1e6), '10.2 / 24.1 MB');
  assert.equal(formatProgress(168e6, 418e6), '168 / 418 MB');
  assert.equal(formatProgress(0, 4.1e9), '0.0 / 4.1 GB');
  assert.equal(formatProgress(5e6, 4.1e9), '5.0 MB / 4.1 GB');
  assert.equal(formatProgress(1.6e9, null), '1.6 GB');
  assert.equal(formatProgress(5e9, 4.1e9), '4.1 / 4.1 GB');
});

test('formatSpeed', () => {
  assert.equal(formatSpeed(38_200_000), '38.2 MB/s');
  assert.equal(formatSpeed(956_000), '956 KB/s');
  assert.equal(formatSpeed(120_400_000), '120 MB/s');
  assert.equal(formatSpeed(1.25e9), '1.3 GB/s');
  assert.equal(formatSpeed(0), '0 KB/s');
  assert.equal(formatSpeed(NaN), '0 KB/s');
  assert.equal(formatSpeed(800), '800 B/s');
  assert.equal(formatSpeedShort(38_200_000), '38 MB/s');
  assert.equal(formatSpeedShort(4_240_000), '4.2 MB/s');
});

test('formatEta', () => {
  assert.equal(formatEta(67), '1:07');
  assert.equal(formatEta(5), '0:05');
  assert.equal(formatEta(59.6), '1:00');
  assert.equal(formatEta(3723), '1:02:03');
  assert.equal(formatEta(86400 * 3 + 3600 * 4), '3 d 4 h');
  assert.equal(formatEta(null), '');
  assert.equal(formatEta(-1), '');
  assert.equal(formatEta(Infinity), '');
});

test('formatAgo', () => {
  const now = 1_700_000_000;
  assert.equal(formatAgo(now - 10, now), 'Just now');
  assert.equal(formatAgo(now - 14 * 60, now), '14 min ago');
  assert.equal(formatAgo(now - 3 * 3600, now), '3 h ago');
  assert.equal(formatAgo(now - 30 * 3600, now), 'Yesterday');
  assert.equal(formatAgo(now - 3 * 86400, now), '3 days ago');
});

test('misc helpers', () => {
  assert.equal(percent(1, 4), 25);
  assert.equal(percent(5, 0), null);
  assert.equal(shortPath('/Users/me/Downloads/Spate/a.iso'), '~/Downloads/Spate/a.iso');
  assert.equal(fileKind('movie.MKV'), 'Video');
  assert.equal(fileKind('x.tar.gz'), 'Archive');
  assert.equal(fileKind('Installer.dmg'), 'App');
  assert.equal(fileKind('paper.pdf'), 'Document');
  assert.equal(fileKind('pic.heic'), 'Image');
  assert.equal(fileKind('song.flac'), 'Audio');
  assert.equal(fileKind('README'), null);
  assert.equal(filenameFromUrl('https://x.org/a/b%20c.zip?x=1'), 'b c.zip');
  assert.ok(looksLikeUrl(' https://x.org/f.iso '));
  assert.ok(!looksLikeUrl('ftp://x.org/f'));
  assert.ok(!looksLikeUrl('hello'));
});
