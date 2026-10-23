// Mock backend: simulates the daemon with realistic jobs so the UI can be built and
// tested in a plain browser. It mirrors spate-core's segmentation exactly:
//   - starting connections: clamp(floor(log2(size_MiB / 4)), 1, 8), capped by 4 MiB pieces
//   - initial pieces: size / n rounded up to 64 KiB
//   - when a connection's piece finishes it first resumes any inactive unfinished piece,
//     else splits the active piece with the most *time* left (remaining / speed) in half
//     (midpoint rounded up to 64 KiB) and takes the back half; only pieces with at least
//     2 × 4 MiB remaining are split, so a piece never drops below 4 MB.
// Same interface as the real backend in api.js. No DOM access at import time (node tests).

export const MIB = 1024 * 1024;
export const MIN_PIECE = 4 * MIB;
export const ALIGN = 64 * 1024;
export const TICK_MS = 250;

/** Deterministic PRNG (mulberry32), so tests and the demo are repeatable. */
export function rng(seed = 1) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** spate-core `initial_connections`. */
export function initialConnections(size, max = 8) {
  if (size == null) return 1;
  const quarter = size / MIB / 4;
  const n = quarter < 2 ? 1 : Math.floor(Math.log2(quarter));
  const pieces = Math.max(1, Math.floor(size / MIN_PIECE));
  return Math.min(Math.max(n, 1), 8, Math.max(1, max), pieces);
}

/** spate-core `split_point`: midpoint of [pos, end) rounded up to ALIGN, if strictly inside. */
export function splitPoint(pos, end) {
  const mid = pos + Math.floor((end - pos) / 2);
  const at = Math.ceil(mid / ALIGN) * ALIGN;
  return at > pos && at < end ? at : null;
}

/** Initial pieces for a fresh download. */
export function initialSegments(size, n) {
  const piece = Math.ceil(size / n / ALIGN) * ALIGN;
  const out = [];
  for (let i = 0; i < n; i++) {
    const a = i * piece;
    const b = Math.min((i + 1) * piece, size);
    if (a < b) out.push({ start: a, end: b, pos: a, active: false, speed: 0 });
  }
  return out;
}

/**
 * spate-core `pick`: what a freed connection does.
 * Returns {resume: i} | {split: i, at} | null. `segs` are sorted by start.
 */
export function pick(segs, minPiece = MIN_PIECE) {
  const r = segs.findIndex((s) => s.pos < s.end && !s.active);
  if (r >= 0) return { resume: r };
  let best = -1;
  let bestT = -1;
  segs.forEach((s, i) => {
    if (!s.active || s.pos >= s.end) return;
    const remaining = s.end - s.pos;
    if (remaining < 2 * minPiece) return;
    const t = remaining / Math.max(s.speed, 1);
    if (t > bestT) {
      bestT = t;
      best = i;
    }
  });
  if (best < 0) return null;
  const at = splitPoint(segs[best].pos, segs[best].end);
  return at == null ? null : { split: best, at };
}

/** Apply a pick to `segs` (mutates, keeps them sorted). Returns the segment the connection now owns. */
export function applyPick(segs, p) {
  if (!p) return null;
  if (p.resume != null) {
    segs[p.resume].active = true;
    return segs[p.resume];
  }
  const s = segs[p.split];
  const fresh = { start: p.at, end: s.end, pos: p.at, active: true, speed: 0, born: true };
  s.end = p.at;
  segs.splice(p.split + 1, 0, fresh);
  return fresh;
}

/**
 * Advance one running job by `dt` seconds. A job is
 * {total, segs, conns: [{seg, base, rate}], connections, speed, ...}; mutates it.
 * Returns the list of pieces created by splits in this step.
 */
export function step(job, dt, rand) {
  const created = [];
  const net = job.net ?? 1; // whole-link congestion factor, slowly wandering
  for (const c of job.conns) {
    if (!c.seg) continue;
    // Per-connection speed wanders around its base rate (servers are uneven).
    const target = c.base * net * (0.75 + rand() * 0.5);
    c.rate = c.rate ? c.rate * 0.8 + target * 0.2 : target;
    const s = c.seg;
    const n = Math.min(s.end - s.pos, c.rate * dt);
    s.pos += n;
    s.speed = s.speed ? s.speed * 0.7 + (n / dt) * 0.3 : n / dt;
  }
  for (const c of job.conns) {
    if (!c.seg || c.seg.pos < c.seg.end) continue;
    c.seg.active = false;
    c.seg.speed = 0;
    c.seg = null;
    const p = pick(job.segs);
    const owned = applyPick(job.segs, p);
    if (owned) {
      c.seg = owned;
      if (p.split != null) created.push({ ...owned });
    }
  }
  // Connections with nothing left to do close.
  job.conns = job.conns.filter((c) => c.seg);
  job.connections = job.conns.length;
  const inst = job.segs.reduce((a, s) => a + (s.active ? s.speed : 0), 0);
  job.speed = job.speed ? job.speed * 0.75 + inst * 0.25 : inst;
  return created;
}

/** Add one connection (adaptive ramp-up): it takes whatever `pick` gives it. */
export function addConnection(job, base) {
  const p = pick(job.segs);
  const seg = applyPick(job.segs, p);
  if (!seg) return false;
  job.conns.push({ seg, base, rate: 0 });
  job.connections = job.conns.length;
  return true;
}

export function downloaded(job) {
  return job.segs.reduce((a, s) => a + (s.pos - s.start), 0);
}

export function isComplete(job) {
  return job.segs.length > 0 && job.segs.every((s) => s.pos >= s.end);
}

/** Start connections on a fresh or resumed job. */
export function startConnections(job, perConnBps, rand) {
  const remaining = job.segs.reduce((a, s) => a + (s.end - s.pos), 0);
  const want = initialConnections(remaining, 8);
  job.conns = [];
  for (const s of job.segs) s.active = false;
  for (let i = 0; i < want; i++) {
    const base = perConnBps * (0.7 + rand() * 0.6);
    if (!addConnection(job, base)) break;
  }
  job.lastAdd = 0;
}

// ---------------------------------------------------------------------------
// Sample jobs.

const HOME = '/Users/me/Downloads/Spate/';
const NOW = () => Math.floor(Date.now() / 1000);

function uuid(n) {
  return `5a7e0000-0000-4000-8000-${String(n).padStart(12, '0')}`;
}

/** Build a running job and fast-forward it until `until` bytes are in. */
function warmed({ id, url, filename, total, perConn, until, seed, created, prefill }) {
  const rand = rng(seed);
  const job = {
    id, url, filename, total, path: HOME + filename, state: 'running', error: null,
    segs: initialSegments(total, initialConnections(total)), conns: [], speed: 0,
    created_at: created, finished_at: null, perConn, rand, elapsed: 0, net: 1,
  };
  // `prefill`: progress per piece from an earlier, paused session (so pieces finish unevenly).
  if (prefill) job.segs.forEach((s, i) => (s.pos = s.start + Math.floor((s.end - s.start) * (prefill[i] ?? 0))));
  startConnections(job, perConn, rand);
  while (downloaded(job) < until && !isComplete(job)) {
    step(job, TICK_MS / 1000, rand);
    job.elapsed += TICK_MS / 1000;
  }
  return job;
}

/** A frozen job (paused/failed): pieces partly filled, nothing active. */
function frozen({ id, url, filename, total, state, error, fill, created, finished, seed }) {
  const rand = rng(seed);
  const n = initialConnections(total);
  const segs = initialSegments(total, n);
  for (const s of segs) s.pos = s.start + Math.floor((s.end - s.start) * fill * (0.6 + rand() * 0.8));
  for (const s of segs) s.pos = Math.min(s.pos, s.end);
  return {
    id, url, filename, total, path: HOME + filename, state, error: error ?? null, segs, conns: [],
    speed: 0, created_at: created, finished_at: finished ?? null, perConn: 3e6, rand, elapsed: 0, net: 1,
  };
}

function finished({ id, url, filename, total, created, finished: fin }) {
  return {
    id, url, filename, total, path: HOME + filename, state: 'done', error: null,
    segs: [{ start: 0, end: total, pos: total, active: false, speed: 0 }], conns: [],
    speed: 0, created_at: created, finished_at: fin, perConn: 0, rand: rng(1), elapsed: 0, net: 1,
  };
}

export function sampleJobs(now = NOW()) {
  return [
    warmed({
      id: uuid(1),
      url: 'https://cdimage.ubuntu.com/releases/24.04/release/ubuntu-24.04.1-desktop-arm64.iso',
      filename: 'ubuntu-24.04.1-desktop-arm64.iso',
      total: 4_102_029_312,
      perConn: 4.8e6,
      until: 1.6e9,
      seed: 7,
      created: now - 3600,
      // Resumed after a pause: uneven pieces, so splits are already happening.
      prefill: [0.96, 0.04, 0.22, 0.9, 0.0, 0.1, 0.5, 0.06],
    }),
    warmed({
      id: uuid(2),
      url: 'https://arxiv.org/pdf/2410.01234v2',
      filename: 'Scaling-Laws-for-Download-Managers.pdf',
      total: 24_117_248,
      perConn: 0.48e6,
      until: 7.5e6,
      seed: 3,
      created: now - 9,
    }),
    frozen({
      id: uuid(3),
      url: 'https://download.blender.org/release/Blender4.2/blender-4.2.3-macos-arm64.dmg',
      filename: 'blender-4.2.3-macos-arm64.dmg',
      total: 418_316_288,
      state: 'paused',
      fill: 0.37,
      created: now - 3600 * 2,
      seed: 11,
    }),
    {
      ...frozen({
        id: uuid(4),
        url: 'https://media.example-cdn.net/raw/IMG_4031-4K-HDR.mov',
        filename: 'IMG_4031-4K-HDR.mov',
        total: 2_306_867_200,
        state: 'queued',
        fill: 0,
        created: now - 5,
        seed: 5,
      }),
      perConn: 3.1e6,
    },
    frozen({
      id: uuid(5),
      url: 'https://storage.example-datasets.org/shards/train-shard-07.tar.gz?X-Amz-Expires=3600&X-Amz-Signature=9f2c',
      filename: 'train-shard-07.tar.gz',
      total: 6_442_450_944,
      state: 'failed',
      error: 'link expired (HTTP 403)',
      fill: 0.22,
      created: now - 3600 * 5,
      finished: now - 3600 * 4,
      seed: 13,
    }),
    finished({
      id: uuid(6),
      url: 'https://archive.org/download/forest-dawn-chorus/forest-dawn-chorus-96k.flac',
      filename: 'forest-dawn-chorus-96k.flac',
      total: 312_475_648,
      created: now - 60 * 20,
      finished: now - 60 * 14,
    }),
    finished({
      id: uuid(7),
      url: 'https://github.com/rust-lang/rust/archive/refs/tags/1.82.0.zip',
      filename: 'rust-1.82.0.zip',
      total: 287_309_824,
      created: now - 3600 * 3,
      finished: now - 3600 * 3 + 40,
    }),
    finished({
      id: uuid(8),
      url: 'https://images.example-photos.com/originals/aurora-lofoten-8k.heic',
      filename: 'aurora-lofoten-8k.heic',
      total: 46_137_344,
      created: now - 86400 - 600,
      finished: now - 86400 - 560,
    }),
  ];
}

/** The public JobView of an internal job (CONTRACTS.md / spate-proto `JobView`). */
export function view(job) {
  const total = job.total ?? null;
  const dl = downloaded(job);
  const running = job.state === 'running';
  const speed = running ? Math.round(job.speed) : 0;
  return {
    id: job.id,
    url: job.url,
    filename: job.filename,
    path: job.path ?? null,
    state: job.state,
    total,
    downloaded: job.state === 'done' ? total ?? dl : dl,
    speed_bps: speed,
    eta_secs: running && total && speed > 0 ? Math.round((total - dl) / speed) : null,
    connections: running ? job.connections : 0,
    segments: job.segs.map((s) => ({
      start: s.start,
      end: s.end,
      pos: Math.floor(s.pos),
      active: running && s.active,
    })),
    error: job.error ?? null,
    created_at: job.created_at,
    finished_at: job.finished_at ?? null,
  };
}

// ---------------------------------------------------------------------------
// Backend (same surface as the Tauri backend in api.js).

export function createMock({ jobs = sampleJobs(), tickMs = TICK_MS, maxActive = 2, autoTick = true } = {}) {
  const byId = new Map(jobs.map((j) => [j.id, j]));
  const listeners = new Set();
  let timer = null;
  let nextId = 100;

  const emit = (job) => {
    const v = view(job);
    for (const cb of listeners) {
      try {
        cb(v);
      } catch (e) {
        console.error(e);
      }
    }
  };

  const activeCount = () => [...byId.values()].filter((j) => j.state === 'running').length;

  function startJob(job) {
    job.state = 'running';
    job.error = null;
    job.speed = 0;
    job.elapsed = 0;
    if (isComplete(job)) return finish(job);
    startConnections(job, job.perConn || 3e6, job.rand);
  }

  function finish(job) {
    job.state = 'done';
    job.conns = [];
    job.finished_at = NOW();
    for (const s of job.segs) s.active = false;
    emit(job);
    promote();
  }

  function promote() {
    for (const j of byId.values()) {
      if (activeCount() >= maxActive) break;
      if (j.state === 'queued') {
        startJob(j);
        emit(j);
      }
    }
  }

  function tick() {
    const dt = tickMs / 1000;
    for (const job of byId.values()) {
      if (job.state !== 'running') continue;
      job.elapsed += dt;
      // Slowly wandering link quality makes the speed graph look like a real network.
      job.net = Math.max(0.55, Math.min(1.15, (job.net ?? 1) + (job.rand() - 0.5) * 0.06));
      step(job, dt, job.rand);
      // Adaptive ramp-up: one more connection every ~2.5 s early on, cap 8.
      if (job.total >= 100e6 && job.elapsed < 20 && job.connections < 8 && job.elapsed - (job.lastAdd ?? 0) > 2.5) {
        if (addConnection(job, job.perConn * (0.7 + job.rand() * 0.6))) job.lastAdd = job.elapsed;
      }
      if (isComplete(job)) finish(job);
      else emit(job);
    }
  }

  function ensureTimer() {
    if (autoTick && !timer) timer = setInterval(tick, tickMs);
  }

  const get = (id) => {
    const j = byId.get(id);
    if (!j) throw Object.assign(new Error(`no such job: ${id}`), { code: 'not_found' });
    return j;
  };
  const later = (v) => new Promise((r) => setTimeout(() => r(v), 30));

  return {
    tick,
    async list() {
      ensureTimer();
      return [...byId.values()].map(view);
    },
    async add(url, opts = {}) {
      ensureTimer();
      const name = opts.filename || decodeName(url);
      const id = uuid(nextId++);
      const rand = rng(nextId * 31);
      const total = opts.size_hint ?? Math.round((40 + rand() * 900) * 1e6);
      const job = {
        id, url, filename: name, total, path: HOME + name, state: 'queued', error: null,
        segs: initialSegments(total, initialConnections(total)), conns: [], speed: 0,
        created_at: NOW(), finished_at: null, perConn: 2.5e6 + rand() * 3e6, rand, elapsed: 0, net: 1,
      };
      byId.set(id, job);
      emit(job);
      if (!opts.paused) promote();
      return later({ job_id: id });
    },
    async pause(id) {
      const j = get(id);
      if (j.state === 'running' || j.state === 'queued') {
        j.state = 'paused';
        j.conns = [];
        for (const s of j.segs) {
          s.active = false;
          s.speed = 0;
        }
        emit(j);
        promote();
      }
      return later({});
    },
    async resume(id) {
      const j = get(id);
      if (j.state === 'paused' || j.state === 'failed' || j.state === 'queued') {
        ensureTimer();
        if (activeCount() >= maxActive && j.state !== 'queued') {
          j.state = 'queued';
          j.error = null;
          emit(j);
        } else {
          startJob(j);
          emit(j);
        }
      }
      return later({});
    },
    async cancel(id) {
      const j = get(id);
      if (j.state !== 'done') {
        j.state = 'failed';
        j.error = 'cancelled';
        j.conns = [];
        j.finished_at = NOW();
        for (const s of j.segs) s.active = false;
        emit(j);
        promote();
      }
      return later({});
    },
    async remove(id) {
      byId.delete(id);
      return later({});
    },
    async reveal(id) {
      console.info('[mock] reveal in Finder:', get(id).path);
      return later({});
    },
    async open(id) {
      console.info('[mock] open:', get(id).path);
      return later({});
    },
    onJob(cb) {
      ensureTimer();
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    stop() {
      clearInterval(timer);
      timer = null;
    },
  };
}

function decodeName(url) {
  try {
    const u = new URL(url);
    const last = u.pathname.split('/').filter(Boolean).pop();
    return last ? decodeURIComponent(last) : u.hostname;
  } catch {
    return 'download';
  }
}
