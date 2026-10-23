// Capture card: shown when Brave hands a download over (UX choice 2). Auto-starts after 2 s,
// then becomes the live progress view and dismisses itself ~4 s after it's running (hover keeps it).
// URL: card.html?job=<id>   (&stay=1 disables auto-dismiss, for screenshots/dev)
import * as api from './api.js';
import * as store from './store.js';
import { SegBar } from './segbar.js';
import { SpeedGraph } from './graph.js';
import { formatBytes, formatProgress, formatSpeed, formatEta, shortPath, dirname } from './format.js';
import { icon } from './icons.js';
import { boot, h, jobIcon, toast } from './ui.js';

boot();
const $ = (id) => document.getElementById(id);
const params = new URLSearchParams(location.search);
const stay = params.get('stay') === '1';
const COUNTDOWN_MS = 2000;
const DISMISS_MS = 4000;

const card = $('card');
$('close').innerHTML = icon.close;
const bar = new SegBar($('bar'));
let jobId = params.get('job');
let phase = 'countdown'; // countdown | running | done | gone
let countdownEnd = 0;
let countdownTimer = 0;
let dismissTimer = 0;
let hovering = false;
let destOverride = null;
let iconSet = false;
let spark = null;

function job() {
  return store.get(jobId);
}

function pickDefault() {
  const all = store.all();
  return (
    all.find((j) => j.state === 'queued') ||
    all.filter((j) => j.state === 'running').sort((a, b) => b.created_at - a.created_at)[0] ||
    all[0]
  )?.id;
}

function render() {
  const j = job();
  if (!j) {
    $('name').textContent = 'Download not found';
    return;
  }
  if (!iconSet) {
    $('icon').replaceChildren(jobIcon(j, 34));
    iconSet = true;
    spark = new SpeedGraph($('spark'), { source: () => store.speedHistory(jobId), seconds: 30, fit: true });
  }
  card.className = `card state-${j.state}${card.classList.contains('leaving') ? ' leaving' : ''}`;
  $('name').textContent = j.filename;
  $('name').title = j.url;
  $('size').textContent = j.total ? formatBytes(j.total) : 'Unknown size';
  $('dest').textContent = destOverride || shortPath(dirname(j.path)) || '~/Downloads/Spate';
  bar.update(j);
  spark.setLive(j.state === 'running');

  const stats = $('stats');
  stats.replaceChildren();
  if (phase === 'countdown' && j.state !== 'running' && j.state !== 'done') {
    const left = Math.max(0, Math.ceil((countdownEnd - performance.now()) / 1000));
    stats.append(h('div', `Starting in ${left} s`), h('div.line2', 'Saving to Spate · ⌥-click a link to keep it in Brave'));
  } else if (j.state === 'running') {
    const eta = formatEta(j.eta_secs);
    stats.append(
      h('div', h('b', formatSpeed(j.speed_bps)), eta ? ` · ${eta} left` : ''),
      h('div.line2', `${formatProgress(j.downloaded, j.total)} · ${j.connections} connection${j.connections === 1 ? '' : 's'}`),
    );
  } else if (j.state === 'done') {
    stats.append(h('div', h('b', 'Finished')), h('div.line2', formatBytes(j.total ?? j.downloaded)));
  } else if (j.state === 'failed') {
    stats.append(h('div', j.error === 'cancelled' ? 'Cancelled' : j.error || 'Failed'));
  } else {
    stats.append(h('div', j.state === 'paused' ? 'Paused' : 'Queued'), h('div.line2', formatProgress(j.downloaded, j.total)));
  }

  const start = $('start');
  if (j.state === 'done') {
    start.hidden = false;
    start.classList.remove('counting', 'started');
    $('start-label').textContent = 'Open';
    start.setAttribute('aria-label', `Open ${j.filename}`);
    $('cancel').textContent = 'Show in Finder';
    if (phase !== 'done') {
      phase = 'done';
      armDismiss();
    }
  } else if (j.state === 'running' && phase === 'countdown') {
    becomeRunning();
  }
}

function startCountdown() {
  const j = job();
  if (!j || j.state === 'running' || j.state === 'done') {
    phase = j?.state === 'done' ? 'done' : 'running';
    $('start').hidden = j?.state !== 'done';
    armDismiss();
    return;
  }
  countdownEnd = performance.now() + COUNTDOWN_MS;
  $('start').classList.add('counting');
  const tick = () => {
    if (phase !== 'countdown') return;
    if (performance.now() >= countdownEnd) return startNow();
    render();
    countdownTimer = setTimeout(tick, 200);
  };
  tick();
}

async function startNow() {
  clearTimeout(countdownTimer);
  if (phase !== 'countdown') return;
  const j = job();
  if (j && (j.state === 'queued' || j.state === 'paused')) await api.resume(j.id);
  becomeRunning();
}

function becomeRunning() {
  if (phase !== 'countdown') return;
  phase = 'running';
  clearTimeout(countdownTimer);
  const s = $('start');
  s.classList.remove('counting');
  s.classList.add('started');
  s.hidden = true;
  armDismiss();
  render();
}

function armDismiss() {
  clearTimeout(dismissTimer);
  if (stay || hovering) return;
  dismissTimer = setTimeout(dismiss, DISMISS_MS);
}

function dismiss() {
  if (hovering || phase === 'gone') return;
  phase = 'gone';
  card.classList.add('leaving');
  setTimeout(() => api.closeWindow(), 320);
}

card.addEventListener('mouseenter', () => {
  hovering = true;
  clearTimeout(dismissTimer);
});
card.addEventListener('mouseleave', () => {
  hovering = false;
  if (phase === 'running' || phase === 'done') {
    clearTimeout(dismissTimer);
    if (!stay) dismissTimer = setTimeout(dismiss, 2000);
  }
});

$('start').addEventListener('click', () => {
  const j = job();
  if (j?.state === 'done') {
    api.open(j.id);
    dismiss();
  } else startNow();
});
$('cancel').addEventListener('click', async () => {
  const j = job();
  if (!j) return;
  if (j.state === 'done') {
    api.reveal(j.id);
    return dismiss();
  }
  clearTimeout(countdownTimer);
  phase = 'running';
  await api.cancel(j.id);
  toast('Download cancelled');
  setTimeout(dismiss, 600);
});
$('change').addEventListener('click', async () => {
  const dir = await api.chooseFolder(jobId);
  if (dir) {
    destOverride = shortPath(dir);
    render();
  }
});
$('close').addEventListener('click', () => {
  hovering = false;
  dismiss();
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') dismiss();
  if (e.key === 'Enter' && phase === 'countdown') startNow();
});
// In a plain browser closeWindow just fires this: leave a hint so the page isn't blank.
document.addEventListener('spate:close', () => {
  document.body.append(h('p.gone-hint', { style: 'position:fixed;top:44px;right:24px;color:var(--text-2);font-size:12px' }, 'Card dismissed — reload to show it again'));
});

store.init().then(() => {
  if (!jobId || !store.get(jobId)) jobId = pickDefault();
  render();
  startCountdown();
});
store.subscribe((ids) => {
  if (ids.has('*') || ids.has(jobId)) render();
});
