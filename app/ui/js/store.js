// Client-side job store shared by every screen: keeps the latest JobView per id, a speed
// history per job (for graphs) and per-connection speeds derived from segment progress.

import * as api from './api.js';

const jobs = new Map();
const history = new Map(); // id -> [{t (ms), v (bps)}]
const segRates = new Map(); // id -> Map(start -> {pos, t, bps})
const listeners = new Set();
const HISTORY_MS = 5 * 60 * 1000;
let ready = null;
let pending = new Set();
let flushQueued = false;

function now() {
  return typeof performance !== 'undefined' ? performance.now() : Date.now();
}

function ingest(job) {
  const t = now();
  jobs.set(job.id, job);
  if (job.state === 'running') {
    let h = history.get(job.id);
    if (!h) history.set(job.id, (h = []));
    h.push({ t, v: job.speed_bps });
    while (h.length && h[0].t < t - HISTORY_MS) h.shift();
    let m = segRates.get(job.id);
    if (!m) segRates.set(job.id, (m = new Map()));
    const seen = new Set();
    for (const s of job.segments) {
      seen.add(s.start);
      const prev = m.get(s.start);
      if (!s.active) {
        m.set(s.start, { pos: s.pos, t, bps: 0 });
        continue;
      }
      if (prev && t > prev.t) {
        const inst = Math.max(0, (s.pos - prev.pos) / ((t - prev.t) / 1000));
        m.set(s.start, { pos: s.pos, t, bps: prev.bps ? prev.bps * 0.6 + inst * 0.4 : inst });
      } else {
        m.set(s.start, { pos: s.pos, t, bps: 0 });
      }
    }
    for (const k of m.keys()) if (!seen.has(k)) m.delete(k);
  } else {
    // Keep a flat tail so a paused graph stops instead of freezing mid-air.
    const h = history.get(job.id);
    if (h && h.length && h[h.length - 1].v !== 0) h.push({ t, v: 0 });
  }
}

function schedule(id) {
  pending.add(id);
  if (flushQueued) return;
  flushQueued = true;
  queueMicrotask(() => {
    flushQueued = false;
    const ids = pending;
    pending = new Set();
    for (const cb of listeners) cb(ids);
  });
}

/** Load the job list once and start listening. Resolves when the first list is in. */
export function init() {
  if (!ready) {
    ready = api.list().then((list) => {
      for (const j of list) ingest(j);
      api.onJob((j) => {
        ingest(j);
        schedule(j.id);
      });
      schedule('*');
    });
  }
  return ready;
}

/** cb(Set of changed ids; contains "*" for structural changes). Returns an unsubscribe fn. */
export function subscribe(cb) {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

export const get = (id) => jobs.get(id);
export const all = () => [...jobs.values()];
export const speedHistory = (id) => history.get(id) || [];

/** Locally drop a job (after api.remove resolved). */
export function forget(id) {
  jobs.delete(id);
  history.delete(id);
  segRates.delete(id);
  schedule('*');
}

/** Per-connection speeds: [{start, bps}] for active segments, in segment order. */
export function connectionSpeeds(id) {
  const j = jobs.get(id);
  const m = segRates.get(id);
  if (!j || !m) return [];
  return j.segments.filter((s) => s.active).map((s) => ({ start: s.start, bps: m.get(s.start)?.bps ?? 0 }));
}

/** Total speed across running jobs, and a combined history (sum per sample time bucket). */
export function totalSpeed() {
  let s = 0;
  for (const j of jobs.values()) if (j.state === 'running') s += j.speed_bps || 0;
  return s;
}

const totalHist = [];
let lastTotalT = 0;
/** A ~4/s history of the total speed, recorded lazily by whoever asks for it. */
export function totalHistory() {
  const t = now();
  if (t - lastTotalT > 200) {
    totalHist.push({ t, v: totalSpeed() });
    lastTotalT = t;
    while (totalHist.length && totalHist[0].t < t - HISTORY_MS) totalHist.shift();
  }
  return totalHist;
}

export const isActive = (j) => j.state === 'running';
export const isCancelled = (j) => j.state === 'failed' && j.error === 'cancelled';

/** Sort: running first (newest first), then queued, paused, failed; finished by finish time. */
export function byStatus(a, b) {
  const rank = { running: 0, queued: 1, paused: 2, failed: 3, done: 4 };
  const r = rank[a.state] - rank[b.state];
  if (r) return r;
  if (a.state === 'done') return (b.finished_at || 0) - (a.finished_at || 0);
  return (b.created_at || 0) - (a.created_at || 0);
}
