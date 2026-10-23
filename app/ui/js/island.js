// Notch island (UX choice 3). Collapsed: a slim black strip around the notch with a tiny
// segmented bar on the left wing and the total speed on the right. Hover: springs open into
// a ~420×120 panel listing the active downloads. Nothing downloading: collapses fully and asks
// the shell to hide the window.
//
// URL params: nonotch=1 (floating pill under the menu bar), notch=<px> and bar=<px> (notch
// geometry from the shell), expanded=1 (pin open, for screenshots/dev).
import * as api from './api.js';
import * as store from './store.js';
import { SegBar } from './segbar.js';
import { formatSpeed, formatSpeedShort, formatEta } from './format.js';
import { brandMark } from './icons.js';
import { boot, h, jobIcon, toggleButton, updateToggle } from './ui.js';

boot();
const params = new URLSearchParams(location.search);
const root = document.documentElement;
if (params.get('nonotch') === '1') root.classList.add('nonotch');
if (params.get('notch')) root.style.setProperty('--notch-w', `${parseInt(params.get('notch'), 10)}px`);
if (params.get('bar')) root.style.setProperty('--bar-h', `${parseInt(params.get('bar'), 10)}px`);
const pinned = params.get('expanded') === '1';

const $ = (id) => document.getElementById(id);
const island = $('island');
const MAX_ROWS = 3;
const ROW_H = 40;
let expanded = false;
let leaveTimer = 0;
let mode = null;

$('band-l').innerHTML = `${brandMark}<span>Spate</span>`;

// Tiny wing bars (one per active download, up to two).
const wingBars = [];
function wingBar(i) {
  if (!wingBars[i]) {
    const c = h('canvas.segbar', { 'aria-hidden': 'true' });
    $('wing-l').append(c);
    wingBars[i] = new SegBar(c, { gap: 1 });
  }
  return wingBars[i];
}

const rows = new Map();
function makeRow(job) {
  const name = h('div.name.ellipsis');
  const canvas = h('canvas.segbar', { 'aria-hidden': 'true' });
  const spd = h('span.spd.num');
  const eta = h('span.eta.num');
  const tg = toggleButton(job);
  const el = h(
    'div.irow',
    { role: 'listitem', title: 'Open Spate', onclick: () => api.openWindow('main') },
    jobIcon(job, 24),
    name,
    h('div.right', h('div.nums', spd, eta), tg),
    canvas,
  );
  return { el, name, spd, eta, tg, bar: new SegBar(canvas), job: null };
}
function updateRow(r, job) {
  if (r.job === job) return;
  r.job = job;
  r.name.textContent = job.filename;
  r.spd.textContent = job.state === 'running' ? formatSpeed(job.speed_bps) : job.state === 'paused' ? 'Paused' : '';
  const e = formatEta(job.eta_secs);
  r.eta.textContent = job.state === 'running' && e ? `${e} left` : '';
  r.el.setAttribute('aria-label', `${job.filename}, ${r.spd.textContent} ${r.eta.textContent}`);
  updateToggle(r.tg, job);
  r.bar.update(job);
}

function running() {
  return store
    .all()
    .filter((j) => j.state === 'running')
    .sort((a, b) => a.created_at - b.created_at);
}

function render() {
  const act = running();
  // Rows (expanded): running jobs plus anything paused by hand from here (kept until collapse).
  const shown = act.slice(0, MAX_ROWS);
  const keep = new Set(shown.map((j) => j.id));
  if (expanded) for (const [id] of rows) if (!keep.has(id) && store.get(id) && store.get(id).state !== 'done') keep.add(id);
  const list = [...keep].map((id) => store.get(id)).filter(Boolean).slice(0, MAX_ROWS);
  for (const [id, r] of rows) {
    if (!list.find((j) => j.id === id)) {
      r.bar.destroy();
      r.el.remove();
      rows.delete(id);
    }
  }
  const container = $('rows');
  list.forEach((job, i) => {
    let r = rows.get(job.id);
    if (!r) rows.set(job.id, (r = makeRow(job)));
    updateRow(r, job);
    if (container.children[i] !== r.el) container.insertBefore(r.el, container.children[i] || null);
  });
  container.querySelector('.more-line')?.remove();
  if (act.length > MAX_ROWS) container.append(h('div.more-line', `and ${act.length - MAX_ROWS} more…`));
  const n = Math.max(1, list.length) + (act.length > MAX_ROWS ? 0.45 : 0);
  root.style.setProperty('--ex-h', `${Math.round(parseFloat(getComputedStyle(root).getPropertyValue('--bar-h')) + n * ROW_H + 14)}px`);

  // Wings (collapsed).
  const two = act.slice(0, 2);
  $('wing-l').classList.toggle('two', two.length === 2);
  two.forEach((j, i) => wingBar(i).update(j));
  wingBars.forEach((b, i) => (b.canvas.hidden = i >= two.length));
  const total = store.totalSpeed();
  $('wing-r').textContent = act.length ? formatSpeedShort(total) : '';
  $('band-r').textContent = act.length ? `↓ ${formatSpeed(total)}` : '';
  island.setAttribute('aria-label', act.length ? `${act.length} active download${act.length > 1 ? 's' : ''}, ${formatSpeed(total)}` : 'No active downloads');

  const empty = act.length === 0 && !(expanded && list.length);
  island.classList.toggle('empty', empty);
  if (empty && expanded && !pinned) setExpanded(false);
  setMode(empty ? 'hidden' : expanded ? 'expanded' : 'collapsed');
}

function setMode(m) {
  if (m === mode) return;
  mode = m;
  api.setIsland(m);
}

function setExpanded(v) {
  if (island.classList.contains('empty') && v) return;
  expanded = v;
  island.classList.toggle('expanded', v);
  render();
}

island.addEventListener('mouseenter', () => {
  clearTimeout(leaveTimer);
  setExpanded(true);
});
island.addEventListener('mouseleave', () => {
  if (pinned) return;
  clearTimeout(leaveTimer);
  leaveTimer = setTimeout(() => setExpanded(false), 160);
});
island.addEventListener('focusin', () => setExpanded(true));
island.addEventListener('focusout', (e) => !pinned && !island.contains(e.relatedTarget) && setExpanded(false));
island.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !pinned) {
    setExpanded(false);
    island.blur();
  }
});

store.init().then(() => {
  render();
  if (pinned) setExpanded(true);
});
store.subscribe(render);
