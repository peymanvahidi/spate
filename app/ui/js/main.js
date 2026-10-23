// Main window: sidebar filters, searchable list with expandable rows, toolbar.
import * as api from './api.js';
import * as store from './store.js';
import { SegBar, segVar } from './segbar.js';
import { SpeedGraph } from './graph.js';
import { formatSpeed, formatEta, formatBytes, formatAgo, shortPath, fileKind, KIND_NAMES, looksLikeUrl } from './format.js';
import { icon, kindIcon } from './icons.js';
import { boot, h, svg, iconButton, jobIcon, statusText, toggle, toggleButton, updateToggle, jobMenu, inlineConfirm, addLink, listKeys, toast } from './ui.js';

boot();
const $ = (id) => document.getElementById(id);

const STATES = [
  { id: 'all', label: 'All', icon: 'tray', title: 'All Downloads', test: () => true },
  { id: 'active', label: 'Active', icon: 'downCircle', title: 'Active', test: (j) => j.state === 'running' || j.state === 'paused' },
  { id: 'queued', label: 'Queued', icon: 'clock', title: 'Queued', test: (j) => j.state === 'queued' },
  { id: 'finished', label: 'Finished', icon: 'checkCircle', title: 'Finished', test: (j) => j.state === 'done' },
  { id: 'failed', label: 'Failed', icon: 'warn', title: 'Failed', test: (j) => j.state === 'failed' },
];
const KINDS = KIND_NAMES.map((k) => ({ id: `kind:${k}`, label: k, title: k, svg: kindIcon[k], test: (j) => fileKind(j.filename) === k }));
const FILTERS = [...STATES, ...KINDS];

let filter = 'all';
let query = '';
let selected = null;
const open = new Set();
const items = new Map(); // id -> {el, row parts, bar, detail?}

// ---------------------------------------------------------------- sidebar
const navButtons = new Map();
function buildNav(container, list) {
  for (const f of list) {
    const b = h(
      'button',
      { type: 'button', onclick: () => setFilter(f.id), 'aria-current': String(f.id === filter) },
      svg(f.svg || icon[f.icon]),
      h('span', f.label),
      h('span.count', '0'),
    );
    navButtons.set(f.id, b);
    container.append(b);
  }
}
buildNav($('nav-state'), STATES);
buildNav($('nav-kind'), KINDS);

function setFilter(id) {
  filter = id;
  for (const [fid, b] of navButtons) b.setAttribute('aria-current', String(fid === id));
  render();
}

const sideGraph = new SpeedGraph($('side-graph'), { source: store.totalHistory, seconds: 60, fit: true });

// ---------------------------------------------------------------- toolbar
$('search-icon').innerHTML = icon.search;
$('addbar-icon').innerHTML = icon.link;
const tool = (name, label, onclick) =>
  h('button.tb', { type: 'button', onclick, 'aria-label': label, title: label }, svg(icon[name]), h('span', label));
$('tools').append(
  tool('plus', 'Add link', () => showAddBar()),
  tool('pause', 'Pause all', () => store.all().forEach((j) => (j.state === 'running' || j.state === 'queued') && api.pause(j.id))),
  tool('play', 'Resume all', () => store.all().forEach((j) => j.state === 'paused' && api.resume(j.id))),
);
$('search').addEventListener('input', (e) => {
  query = e.target.value.trim().toLowerCase();
  render();
});
$('search').addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    e.target.value = '';
    query = '';
    render();
    e.target.blur();
  }
});

function showAddBar(text = '') {
  const bar = $('addbar');
  bar.hidden = false;
  $('add-url').value = text;
  $('add-url').focus();
}
function hideAddBar() {
  $('addbar').hidden = true;
}
$('add-cancel').addEventListener('click', hideAddBar);
$('add-url').addEventListener('keydown', (e) => e.key === 'Escape' && hideAddBar());
$('addbar').addEventListener('submit', async (e) => {
  e.preventDefault();
  const v = $('add-url').value;
  if (!looksLikeUrl(v)) {
    $('add-url').classList.add('invalid');
    setTimeout(() => $('add-url').classList.remove('invalid'), 900);
    return;
  }
  const id = await addLink(v);
  hideAddBar();
  if (id) {
    toast('Added to downloads');
    selected = id;
  }
});
document.addEventListener('paste', (e) => {
  if (['INPUT', 'TEXTAREA'].includes(document.activeElement?.tagName)) return;
  const text = e.clipboardData?.getData('text')?.trim() ?? '';
  if (!text) return;
  e.preventDefault();
  if (looksLikeUrl(text)) {
    addLink(text).then((id) => id && (toast('Added to downloads'), (selected = id)));
  } else showAddBar(text);
});
document.addEventListener('keydown', (e) => {
  if (e.metaKey && e.key.toLowerCase() === 'f') {
    $('search').focus();
    e.preventDefault();
  }
  if (e.metaKey && e.key.toLowerCase() === 'n') {
    showAddBar();
    e.preventDefault();
  }
});

// ---------------------------------------------------------------- rows
function makeItem(job) {
  const id = job.id;
  const chev = h('button.chev', { type: 'button', tabindex: '-1', 'aria-label': 'Show details', html: icon.chevron, onclick: (e) => (e.stopPropagation(), toggleOpen(id)) });
  const name = h('div.name.ellipsis');
  const status = h('div.status.ellipsis');
  const canvas = h('canvas.segbar', { 'aria-hidden': 'true' });
  const sp = h('div.sp.num');
  const eta = h('div.eta.num');
  const tg = toggleButton(job);
  const more = iconButton('more', `More actions for ${job.filename}`, (e) => {
    e.stopPropagation();
    jobMenu(more, store.get(id), { onCancel: askCancel });
  });
  const row = h('div.mrow', chev, jobIcon(job, 28), h('div.nm', name, status), canvas, sp, eta, h('div.acts', tg, more));
  const el = h('div.item', { role: 'option', tabindex: '-1', 'aria-selected': 'false', 'aria-expanded': 'false', dataset: { id } }, row);
  row.addEventListener('click', () => {
    select(id);
    toggleOpen(id);
  });
  row.addEventListener('dblclick', () => store.get(id)?.state === 'done' && api.open(id));
  return { el, row, name, status, sp, eta, tg, bar: new SegBar(canvas), job: null, detail: null };
}

function updateItem(it, job) {
  if (it.job === job) return;
  it.job = job;
  it.name.textContent = job.filename;
  it.name.title = job.filename;
  const failed = job.state === 'failed';
  it.status.textContent =
    job.state === 'running' ? `${Math.floor((job.downloaded / (job.total || 1)) * 100)}% · ${formatBytes(job.downloaded)} of ${formatBytes(job.total)} · ${job.connections} conn.` : statusText(job, { short: true });
  it.status.classList.toggle('err', failed && job.error !== 'cancelled');
  it.sp.textContent = job.state === 'running' ? formatSpeed(job.speed_bps) : '';
  it.eta.textContent = job.state === 'running' ? formatEta(job.eta_secs) : job.state === 'done' ? formatAgo(job.finished_at) : '';
  it.el.setAttribute('aria-label', `${job.filename}, ${statusText(job)}`);
  updateToggle(it.tg, job);
  it.bar.update(job);
  if (it.detail) updateDetail(it, job);
}

function toggleOpen(id) {
  const it = items.get(id);
  if (!it) return;
  if (open.has(id)) {
    open.delete(id);
    it.detail?.destroy();
    it.detail = null;
  } else {
    open.add(id);
    it.detail = makeDetail(id);
    it.el.append(it.detail.el);
    updateDetail(it, store.get(id));
  }
  it.el.classList.toggle('open', open.has(id));
  it.el.setAttribute('aria-expanded', String(open.has(id)));
}

function makeDetail(id) {
  const job = store.get(id);
  const canvas = h('canvas.segbar.big-bar', { 'aria-hidden': 'true' });
  const legend = h('div.legend', { 'aria-label': 'Connections' });
  const gcanvas = h('canvas.graph.big-graph', { 'aria-label': 'Speed over time' });
  const fact = (label, value, ...extra) => [h('dt', label), h('dd', value, ...extra)];
  const src = h('span.v', { title: job.url }, job.url);
  const path = h('span.v.mono', { title: job.path || '' }, shortPath(job.path) || '—');
  const resume = h('span.v');
  const added = h('span.v');
  const conns = h('span.v');
  const facts = h(
    'dl.facts',
    fact('Source', src, iconButton('link', 'Copy link', (e) => (e.stopPropagation(), api.copyText(job.url).then(() => toast('Link copied'))))),
    fact('File', path, iconButton('folder', 'Show in Finder', (e) => (e.stopPropagation(), api.reveal(id)))),
    fact('Resume', resume),
    fact('Added', added),
    fact('Pieces', conns),
  );
  const el = h(
    'div.detail',
    { onclick: (e) => e.stopPropagation() },
    canvas,
    legend,
    h('div.dgrid', h('div.graph-box', h('div.gt', 'Speed'), gcanvas), facts),
  );
  const bar = new SegBar(canvas, { radius: 5, gap: 2 });
  const graph = new SpeedGraph(gcanvas, { source: () => store.speedHistory(id), seconds: 120, axis: true, fit: true });
  return {
    el, bar, graph, legend, resume, added, conns,
    destroy() {
      bar.destroy();
      graph.destroy();
      el.remove();
    },
  };
}

function updateDetail(it, job) {
  const d = it.detail;
  d.bar.update(job);
  d.graph.setLive(job.state === 'running');
  // Legend: one chip per active connection, coloured like its piece on the bar.
  d.legend.replaceChildren();
  const segs = job.segments || [];
  const doneCount = segs.filter((s) => s.pos >= s.end).length;
  if (job.state === 'running') {
    store.connectionSpeeds(job.id).forEach((c, i) => {
      const ci = d.bar.tracker.colorOf(c.start);
      d.legend.append(h('span.chip', h('span.sw', { style: `background:${segVar(ci)}` }), `#${i + 1}`, h('b', formatSpeed(c.bps))));
    });
    d.legend.append(h('span.chip.total-chip', `${job.connections} connections · ${formatSpeed(job.speed_bps)}`));
  } else if (job.state === 'done') {
    d.legend.append(h('span.chip', h('span.sw', { style: 'background:var(--seg-done)' }), `Complete · ${formatBytes(job.total ?? job.downloaded)}`));
  } else {
    d.legend.append(h('span.chip', `${segs.length} pieces · ${doneCount} complete · no connections open`));
  }
  d.resume.textContent = resumeInfo(job, doneCount);
  d.resume.title = d.resume.textContent;
  d.added.textContent = new Date(job.created_at * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
  d.conns.textContent = segs.length ? `${segs.length} pieces (${doneCount} complete), min 4 MB` : '—';
}

function resumeInfo(job, doneCount) {
  const state = `${job.filename}.spate-part.json`;
  switch (job.state) {
    case 'running':
      return `Progress saved every second (${state})`;
    case 'paused':
      return `Resumes where it stopped (${formatBytes(job.downloaded)} kept)`;
    case 'queued':
      return 'Waiting for a free slot';
    case 'failed':
      if (job.error === 'cancelled') return 'Cancelled; partial data discarded';
      if (/expired/i.test(job.error || '')) return 'Retry asks Brave for a fresh link; finished pieces are kept';
      return `Retry resumes from ${formatBytes(job.downloaded)}`;
    case 'done': {
      const took = job.finished_at && job.created_at ? job.finished_at - job.created_at : null;
      const avg = took ? (job.total ?? job.downloaded) / took : null;
      return took ? `Finished in ${formatEta(took)} · average ${formatSpeed(avg)}` : 'Finished';
    }
  }
  return '';
}

function visibleJobs() {
  const f = FILTERS.find((x) => x.id === filter) || FILTERS[0];
  return store
    .all()
    .filter((j) => f.test(j))
    .filter((j) => !query || j.filename.toLowerCase().includes(query) || j.url.toLowerCase().includes(query))
    .sort(store.byStatus);
}

function render() {
  const all = store.all();
  for (const f of FILTERS) {
    const n = all.filter(f.test).length;
    const b = navButtons.get(f.id);
    b.querySelector('.count').textContent = String(n);
    b.classList.toggle('zero', n === 0);
  }
  const jobs = visibleJobs();
  const list = $('list');
  const keep = new Set(jobs.map((j) => j.id));
  for (const [id, it] of items) {
    if (!keep.has(id)) {
      it.bar.destroy();
      it.detail?.destroy();
      it.el.remove();
      items.delete(id);
      open.delete(id);
    }
  }
  jobs.forEach((job, i) => {
    let it = items.get(job.id);
    if (!it) items.set(job.id, (it = makeItem(job)));
    updateItem(it, job);
    if (list.children[i] !== it.el) list.insertBefore(it.el, list.children[i] || null);
  });
  for (const [id, it] of items) it.el.setAttribute('aria-selected', String(id === selected));

  const f = FILTERS.find((x) => x.id === filter);
  $('title').textContent = f.title;
  const running = all.filter((j) => j.state === 'running');
  const total = store.totalSpeed();
  $('subtitle').textContent = running.length
    ? `${running.length} downloading · ${formatSpeed(total)} · ${jobs.length} shown`
    : `${jobs.length} item${jobs.length === 1 ? '' : 's'}`;
  $('side-speed').textContent = running.length ? `↓ ${formatSpeed(total)}` : 'Idle';
  sideGraph.setLive(running.length > 0);

  const empty = $('empty');
  empty.hidden = jobs.length > 0;
  if (!jobs.length) {
    empty.replaceChildren(svg(icon[query ? 'search' : 'tray']), h('div', query ? `No results for “${query}”` : 'Nothing here'), h('div.faint', query ? '' : 'Paste a link (⌘V) to add a download.'));
  }
}

function select(id, focus = false) {
  selected = id;
  for (const [iid, it] of items) it.el.setAttribute('aria-selected', String(iid === id));
  if (focus) {
    const el = items.get(id)?.el;
    el?.focus();
    el?.scrollIntoView({ block: 'nearest' });
  }
}

async function askCancel(job) {
  if (!job || job.state === 'done') return;
  const it = items.get(job.id);
  if (!it) return;
  select(job.id);
  if (await inlineConfirm(it.el, `Cancel “${job.filename}”? Downloaded data is discarded.`)) await api.cancel(job.id);
  it.el.focus();
}

listKeys({
  ids: () => visibleJobs().map((j) => j.id),
  getSelected: () => selected,
  select,
  onCancel: askCancel,
  onEnter: (job) => job && toggleOpen(job.id),
});

store.init().then(() => {
  // Open the first running download so the window greets you with the full picture.
  const first = visibleJobs()
    .filter((j) => j.state === 'running')
    .sort((a, b) => (b.total || 0) - (a.total || 0))[0];
  render();
  if (first) {
    select(first.id);
    toggleOpen(first.id);
  }
});
store.subscribe(render);
// Removed (maybe in another window): close its detail and drop the selection; render() drops the row.
store.onRemoved((id) => {
  const it = items.get(id);
  if (it) {
    it.bar.destroy();
    it.detail?.destroy();
    it.el.remove();
    items.delete(id);
  }
  open.delete(id);
  if (selected === id) selected = null;
});
