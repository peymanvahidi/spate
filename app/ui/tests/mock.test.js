// The mock's segmentation must behave exactly like spate-core's.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  MIB, MIN_PIECE, ALIGN, rng, initialConnections, splitPoint, initialSegments, pick, applyPick, step,
  startConnections, downloaded, isComplete, createMock, view, sampleJobs,
} from '../js/mock.js';

test('initial connections follow clamp(floor(log2(MiB/4)), 1, 8)', () => {
  assert.equal(initialConnections(5 * MIB), 1);
  assert.equal(initialConnections(20 * MIB), 2);
  assert.equal(initialConnections(24 * MIB), 2);
  assert.equal(initialConnections(100 * MIB), 4);
  assert.equal(initialConnections(500 * MIB), 6);
  assert.equal(initialConnections(1024 * MIB), 8);
  assert.equal(initialConnections(4.1e9), 8);
  assert.equal(initialConnections(null), 1);
  assert.equal(initialConnections(100 * MIB, 3), 3);
});

test('split point is the 64 KiB-aligned midpoint, strictly inside', () => {
  assert.equal(splitPoint(0, 100 * MIB), 50 * MIB);
  const at = splitPoint(1, 10 * MIB + 7);
  assert.equal(at % ALIGN, 0);
  assert.ok(at > 1 && at < 10 * MIB + 7);
  assert.equal(splitPoint(0, ALIGN / 2), null);
});

test('initial pieces cover the file exactly', () => {
  const size = 4_102_029_312;
  const segs = initialSegments(size, 8);
  assert.equal(segs.length, 8);
  assert.equal(segs[0].start, 0);
  assert.equal(segs.at(-1).end, size);
  for (let i = 1; i < segs.length; i++) assert.equal(segs[i].start, segs[i - 1].end);
});

test('pick resumes an inactive unfinished piece first', () => {
  const segs = [
    { start: 0, end: 100 * MIB, pos: 10 * MIB, active: true, speed: 1e6 },
    { start: 100 * MIB, end: 200 * MIB, pos: 100 * MIB, active: false, speed: 0 },
  ];
  assert.deepEqual(pick(segs), { resume: 1 });
});

test('pick splits the piece with the most TIME left, not the most bytes', () => {
  const segs = [
    // 80 MiB left at 10 MB/s = 8 s
    { start: 0, end: 100 * MIB, pos: 20 * MIB, active: true, speed: 10e6 },
    // 40 MiB left at 1 MB/s = 42 s  <- slowest finisher
    { start: 100 * MIB, end: 200 * MIB, pos: 160 * MIB, active: true, speed: 1e6 },
    { start: 200 * MIB, end: 300 * MIB, pos: 300 * MIB, active: false, speed: 0 },
  ];
  const p = pick(segs);
  assert.equal(p.split, 1);
  assert.equal(p.at, splitPoint(160 * MIB, 200 * MIB));
  applyPick(segs, p);
  assert.equal(segs.length, 4);
  assert.equal(segs[1].end, p.at);
  assert.deepEqual([segs[2].start, segs[2].end, segs[2].pos, segs[2].active], [p.at, 200 * MIB, p.at, true]);
});

test('pieces with less than 2 × 4 MiB left are never split', () => {
  const segs = [{ start: 0, end: 100 * MIB, pos: 100 * MIB - 2 * MIN_PIECE + 1, active: true, speed: 1 }];
  assert.equal(pick(segs), null);
  segs[0].pos = 100 * MIB - 2 * MIN_PIECE;
  assert.ok(pick(segs).split === 0);
});

function simulate(size, seed, perConn = 4e6) {
  const rand = rng(seed);
  const job = { total: size, segs: initialSegments(size, initialConnections(size)), conns: [], speed: 0 };
  startConnections(job, perConn, rand);
  const created = [];
  let steps = 0;
  while (!isComplete(job) && steps++ < 1e6) created.push(...step(job, 0.25, rand));
  return { job, created, steps };
}

test('a full 4.1 GB download completes with contiguous, non-overlapping pieces', () => {
  const size = 4_102_029_312;
  const { job, created } = simulate(size, 7);
  assert.ok(isComplete(job));
  assert.equal(downloaded(job), size);
  assert.equal(job.segs[0].start, 0);
  assert.equal(job.segs.at(-1).end, size);
  for (let i = 1; i < job.segs.length; i++) assert.equal(job.segs[i].start, job.segs[i - 1].end);
  assert.ok(created.length > 0, 'pieces were split dynamically');
  assert.equal(job.connections, 0, 'all connections closed at the end');
});

test('a split never creates a piece below 4 MB', () => {
  for (const seed of [1, 2, 3, 4, 5]) {
    const { job, created } = simulate(1.2e9, seed, 3e6 + seed * 1e6);
    for (const c of created) assert.ok(c.end - c.start >= 4_000_000, `new piece ${c.end - c.start} B`);
    // Every piece (including the shrunken front halves) stays at least 4 MB long.
    for (const s of job.segs) assert.ok(s.end - s.start >= 4_000_000, `piece ${s.end - s.start} B`);
  }
});

test('connections stay busy until the end (no idle connection while splittable work exists)', () => {
  const rand = rng(9);
  const job = { total: 800e6, segs: initialSegments(800e6, 6), conns: [], speed: 0 };
  startConnections(job, 5e6, rand);
  const n = job.connections;
  assert.equal(n, initialConnections(800e6));
  while (!isComplete(job)) {
    step(job, 0.25, rand);
    const splittable = job.segs.some((s) => s.active && s.end - s.pos >= 2 * MIN_PIECE);
    if (splittable) assert.equal(job.connections, n);
  }
});

test('JobView shape matches CONTRACTS.md / spate-proto', () => {
  const keys = ['id', 'url', 'filename', 'path', 'state', 'total', 'downloaded', 'speed_bps', 'eta_secs', 'connections', 'segments', 'error', 'created_at', 'finished_at'].sort();
  const states = new Set();
  for (const j of sampleJobs(1_700_000_000)) {
    const v = view(j);
    assert.deepEqual(Object.keys(v).sort(), keys);
    states.add(v.state);
    for (const s of v.segments) {
      assert.deepEqual(Object.keys(s).sort(), ['active', 'end', 'pos', 'start']);
      assert.ok(s.start <= s.pos && s.pos <= s.end);
    }
    assert.ok(Number.isInteger(v.created_at));
    if (v.state !== 'running') assert.equal(v.eta_secs, null);
  }
  assert.deepEqual([...states].sort(), ['done', 'failed', 'paused', 'queued', 'running']);
});

test('sample set: 8-connection 4.1 GB ISO, 2-connection 24 MB PDF, paused, done and a link-expired failure', () => {
  const v = sampleJobs().map(view);
  const iso = v.find((j) => j.filename.endsWith('.iso'));
  assert.equal(iso.connections, 8);
  assert.ok(Math.abs(iso.total - 4.1e9) < 0.05e9);
  const pdf = v.find((j) => j.filename.endsWith('.pdf'));
  assert.equal(pdf.connections, 2);
  assert.ok(Math.abs(pdf.total - 24e6) < 0.5e6);
  assert.ok(v.some((j) => j.state === 'paused'));
  assert.ok(v.some((j) => j.state === 'done'));
  assert.ok(v.some((j) => j.state === 'failed' && /link expired/.test(j.error)));
});

test('mock backend: list, events, pause/resume/cancel/remove', async () => {
  const m = createMock({ autoTick: false });
  const list = await m.list();
  const iso = list.find((j) => j.filename.endsWith('.iso'));
  const seen = [];
  const un = m.onJob((j) => seen.push(j));
  m.tick();
  assert.ok(seen.some((j) => j.id === iso.id && j.state === 'running'));
  const last = () => seen.findLast((j) => j.id === iso.id);
  await m.pause(iso.id);
  assert.equal(last().state, 'paused');
  assert.equal(last().connections, 0);
  assert.ok(last().segments.every((s) => !s.active));
  await m.resume(iso.id);
  m.tick();
  assert.ok(['running', 'queued'].includes(last().state));
  await m.cancel(iso.id);
  assert.equal(seen.findLast((j) => j.id === iso.id).error, 'cancelled');
  const { job_id } = await m.add('https://example.com/files/big%20file.zip', { source: 'paste' });
  assert.ok((await m.list()).find((j) => j.id === job_id && j.filename === 'big file.zip'));
  await m.remove(job_id);
  assert.ok(!(await m.list()).find((j) => j.id === job_id));
  un();
  m.stop();
});
