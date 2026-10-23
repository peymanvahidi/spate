// Menu-bar popover.
import * as api from './api.js';
import * as store from './store.js';
import { SegBar } from './segbar.js';
import { SpeedGraph } from './graph.js';
import { formatSpeed, formatBytes, formatAgo, looksLikeUrl } from './format.js';
import { icon, brandMark, trayGlyph } from './icons.js';
import { boot, h, svg, iconButton, jobIcon, statusText, toggleButton, updateToggle, jobMenu, inlineConfirm, addLink, listKeys, toast } from './ui.js';

boot();
const $ = (id) => document.getElementById(id);
$('brand-mark').innerHTML = brandMark;
$('mb-icon').innerHTML = trayGlyph;
$('paste-icon').innerHTML = icon.paste;
$('empty-mark').innerHTML = icon.tray;
$('foot-buttons').append(
  iconButton('window', 'Open Spate window', () => api.openWindow('main')),
  iconButton('gear', 'Settings', () => api.openWindow('settings')),
);

const rows = new Map(); // id -> {el, bar, job, meta, toggle}
const drows = new Map();
let selected = null;
let firstRender = true;
const MAX_DONE = 6;

const headGraph = new SpeedGraph($('head-spark'), { source: store.totalHistory, seconds: 45, fit: true });

function activeJobs() {
  return store
    .all()
    .filter((j) => j.state !== 'done' && !store.isCancelled(j))
    .sort(store.byStatus);
}
function doneJobs() {
  return store
    .all()
    .filter((j) => j.state === 'done')
    .sort(store.byStatus)
    .slice(0, MAX_DONE);
}

function makeRow(job) {
  const name = h('div.name.ellipsis');
  const canvas = h('canvas.segbar', { 'aria-hidden': 'true' });
  const meta = h('div.meta.ellipsis');
  const toggle = toggleButton(job);
  const more = iconButton('more', `More actions for ${job.filename}`, (e) => {
    e.stopPropagation();
    jobMenu(more, store.get(job.id), { onCancel: askCancel });
  });
  const el = h(
    'div.row',
    { role: 'option', tabindex: '-1', 'aria-selected': 'false', dataset: { id: job.id }, onclick: () => select(job.id) },
    jobIcon(job, 30),
    name,
    h('div.acts', toggle, more),
    canvas,
    meta,
  );
  // Grid placement: name / bar / meta stack in column 2.
  name.style.gridColumn = canvas.style.gridColumn = meta.style.gridColumn = '2';
  name.style.gridRow = '1';
  canvas.style.gridRow = '2';
  meta.style.gridRow = '3';
  el.addEventListener('dblclick', () => store.get(job.id)?.state === 'done' && api.open(job.id));
  const r = { el, bar: new SegBar(canvas), name, meta, toggle, job: null };
  if (!firstRender) el.classList.add('entering');
  return r;
}

function updateRow(r, job) {
  if (r.job === job) return;
  r.job = job;
  r.name.textContent = job.filename;
  r.name.title = job.filename;
  r.meta.textContent = statusText(job);
  r.meta.classList.toggle('err', job.state === 'failed');
  r.el.setAttribute('aria-label', `${job.filename}, ${statusText(job)}`);
  updateToggle(r.toggle, job);
  r.bar.update(job);
}

function makeDrow(job) {
  const reveal = iconButton('folder', `Show ${job.filename} in Finder`, (e) => {
    e.stopPropagation();
    api.reveal(job.id);
  });
  const el = h(
    'div.drow',
    {
      role: 'listitem',
      tabindex: '0',
      title: 'Click to open · ⌥-click to show in Finder',
      onclick: (e) => (e.altKey ? api.reveal(job.id) : api.open(job.id)),
      onkeydown: (e) => e.key === 'Enter' && (e.altKey ? api.reveal(job.id) : api.open(job.id)),
    },
    jobIcon(job, 22),
    h('div', h('div.name.ellipsis', job.filename), h('div.meta')),
    h('div', h('span.check', { html: icon.check, 'aria-label': 'Finished' }), reveal),
  );
  if (!firstRender) el.classList.add('entering');
  return { el, job: null };
}

function updateDrow(r, job) {
  r.job = job;
  r.el.querySelector('.meta').textContent = `${formatBytes(job.total ?? job.downloaded)} · ${formatAgo(job.finished_at)}`;
}

function reconcile(container, map, jobs, make, update) {
  const keep = new Set(jobs.map((j) => j.id));
  for (const [id, r] of map) {
    if (!keep.has(id)) {
      r.bar?.destroy();
      r.el.remove();
      map.delete(id);
    }
  }
  jobs.forEach((job, i) => {
    let r = map.get(job.id);
    if (!r) map.set(job.id, (r = make(job)));
    update(r, job);
    if (container.children[i] !== r.el) container.insertBefore(r.el, container.children[i] || null);
  });
}

function render() {
  const act = activeJobs();
  const done = doneJobs();
  reconcile($('active'), rows, act, makeRow, updateRow);
  reconcile($('done'), drows, done, makeDrow, updateDrow);
  $('sec-active').hidden = !act.length;
  $('sec-done').hidden = !done.length;
  $('empty').hidden = !!(act.length || done.length);
  const running = act.filter((j) => j.state === 'running').length;
  $('h-active').innerHTML = '';
  $('h-active').append(h('span', running ? 'Downloading' : 'Downloads'), h('span.count', `${act.length}`));
  if (selected && !rows.has(selected)) selected = null;
  const total = store.totalSpeed();
  const t = $('total');
  t.classList.toggle('idle', !running);
  t.innerHTML = '';
  if (running) t.append(svg(icon.down), formatSpeed(total));
  else t.append('Idle');
  t.setAttribute('aria-label', running ? `Total speed ${formatSpeed(total)}` : 'Idle');
  headGraph.setLive(running > 0);
  firstRender = false;
}

function select(id, focus = false) {
  selected = id;
  for (const [rid, r] of rows) r.el.setAttribute('aria-selected', String(rid === id));
  if (focus) rows.get(id)?.el.focus();
}

async function askCancel(job) {
  if (!job || job.state === 'done') return;
  const r = rows.get(job.id);
  if (!r) return;
  select(job.id);
  if (await inlineConfirm(r.el, "Cancel this download?")) await api.cancel(job.id);
  r.el.focus();
}

listKeys({
  ids: () => activeJobs().map((j) => j.id),
  getSelected: () => selected,
  select,
  onCancel: askCancel,
});

// Paste a link (UX choice 8: paste only, no clipboard watching).
const field = $('paste');
async function submit(text) {
  const label = field.closest('.paste');
  if (!looksLikeUrl(text)) {
    field.classList.add('invalid');
    setTimeout(() => field.classList.remove('invalid'), 900);
    return;
  }
  const id = await addLink(text);
  field.value = '';
  label.classList.add('flash');
  setTimeout(() => label.classList.remove('flash'), 500);
  if (id) toast('Added to downloads');
}
field.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') submit(field.value);
  if (e.key === 'Escape') field.blur();
});
field.addEventListener('paste', (e) => {
  const text = e.clipboardData?.getData('text') ?? '';
  if (looksLikeUrl(text) && !field.value) {
    e.preventDefault();
    submit(text);
  }
});
document.addEventListener('paste', (e) => {
  if (e.target === field) return;
  const text = e.clipboardData?.getData('text') ?? '';
  e.preventDefault();
  field.value = text.trim();
  submit(text);
});

store.init().then(render);
store.subscribe(render);
// Removed (maybe in another window): drop the selection; render() drops the row.
store.onRemoved((id) => {
  if (selected === id) selected = null;
});
