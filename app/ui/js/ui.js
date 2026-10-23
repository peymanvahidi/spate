// Small shared DOM helpers: element builder, status lines, ⋯ menu, inline confirm, toast,
// keyboard handling for selectable lists.

import * as api from './api.js';
import { formatProgress, formatSpeed, formatEta, formatBytes, formatAgo, fileKind, looksLikeUrl } from './format.js';
import { icon, fileIcon, extOf } from './icons.js';
import * as store from './store.js';

/** Mark the page as running outside the shell (desktop backdrop, fake chrome). */
export function boot() {
  if (!api.isShell) document.documentElement.classList.add('preview');
  const p = new URLSearchParams(location.search);
  if (p.get('theme') === 'light') document.documentElement.classList.add('light');
}

/** h('div.row.active', {onclick, 'aria-label': …}, child, 'text', …) */
export function h(tag, attrs, ...kids) {
  const [name, ...classes] = tag.split('.');
  const el = document.createElement(name || 'div');
  if (classes.length) el.className = classes.join(' ');
  if (attrs && (typeof attrs !== 'object' || attrs instanceof Node || Array.isArray(attrs))) {
    kids.unshift(attrs);
    attrs = null;
  }
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'html') el.innerHTML = v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const k of kids.flat()) if (k != null && k !== false) el.append(k instanceof Node ? k : String(k));
  return el;
}

export function svg(markup) {
  const t = document.createElement('template');
  t.innerHTML = markup.trim();
  return t.content.firstChild;
}

export function iconButton(name, label, onclick, cls = '') {
  return h(`button.ib${cls ? '.' + cls : ''}`, { type: 'button', 'aria-label': label, title: label, onclick, html: icon[name] });
}

export function jobIcon(job, size = 28) {
  return svg(fileIcon(fileKind(job.filename), size, extOf(job.filename)));
}

const cap = (s) => (s ? s[0].toUpperCase() + s.slice(1) : s);

/** One-line status under a job's name. */
export function statusText(job, { short = false } = {}) {
  switch (job.state) {
    case 'running': {
      const parts = [formatProgress(job.downloaded, job.total), formatSpeed(job.speed_bps)];
      const eta = formatEta(job.eta_secs);
      if (eta) parts.push(`${eta} left`);
      return parts.join(' · ');
    }
    case 'queued':
      return `Queued${job.total ? ' · ' + formatBytes(job.total) : ''}`;
    case 'paused':
      return `Paused · ${formatProgress(job.downloaded, job.total)}`;
    case 'failed':
      if (job.error === 'cancelled') return 'Cancelled';
      return short ? cap(job.error || 'Failed') : `${cap(job.error || 'Failed')} · ${formatProgress(job.downloaded, job.total)}`;
    case 'done':
      return `${formatBytes(job.total ?? job.downloaded)} · ${formatAgo(job.finished_at)}`;
    default:
      return '';
  }
}

/** Pause a running/queued job, resume a paused/failed one. */
export function toggle(job) {
  if (!job) return;
  if (job.state === 'running' || job.state === 'queued') return api.pause(job.id);
  if (job.state === 'paused' || job.state === 'failed') return api.resume(job.id);
}

/** Pause/resume button that updates in place. */
export function toggleButton(job) {
  const b = h('button.ib.round.toggle', { type: 'button', onclick: (e) => (e.stopPropagation(), toggle(store.get(b.dataset.id))) });
  b.dataset.id = job.id;
  updateToggle(b, job);
  return b;
}
export function updateToggle(b, job) {
  const kind = job.state === 'running' || job.state === 'queued' ? 'pause' : job.state === 'failed' ? 'retry' : 'play';
  if (b.dataset.kind === kind) return;
  b.dataset.kind = kind;
  b.innerHTML = icon[kind];
  const label = kind === 'pause' ? 'Pause' : kind === 'retry' ? 'Retry' : 'Resume';
  b.setAttribute('aria-label', `${label} ${job.filename}`);
  b.title = label;
  b.hidden = job.state === 'done';
}

let openMenuEl = null;
export function closeMenu() {
  openMenuEl?.remove();
  openMenuEl = null;
}

/** Show a popup menu under `anchor`. items: [{label, icon, kbd, danger, run} | '-'] */
export function showMenu(anchor, items) {
  closeMenu();
  const menu = h('div.menu', { role: 'menu' });
  for (const it of items) {
    if (it === '-') {
      menu.append(h('hr'));
      continue;
    }
    menu.append(
      h(
        `button${it.danger ? '.danger' : ''}`,
        {
          type: 'button',
          role: 'menuitem',
          onclick: (e) => {
            e.stopPropagation();
            closeMenu();
            it.run();
          },
        },
        it.icon ? svg(icon[it.icon]) : null,
        h('span', it.label),
        it.kbd ? h('kbd', it.kbd) : null,
      ),
    );
  }
  document.body.append(menu);
  const r = anchor.getBoundingClientRect();
  const mw = menu.offsetWidth;
  const mh = menu.offsetHeight;
  let x = r.right - mw;
  let y = r.bottom + 4;
  if (y + mh > innerHeight - 6) y = r.top - mh - 4;
  menu.style.left = `${Math.max(6, x)}px`;
  menu.style.top = `${Math.max(6, y)}px`;
  openMenuEl = menu;
  menu.querySelector('button')?.focus();
  menu.addEventListener('keydown', (e) => {
    const btns = [...menu.querySelectorAll('button')];
    const i = btns.indexOf(document.activeElement);
    if (e.key === 'ArrowDown') btns[(i + 1) % btns.length].focus();
    else if (e.key === 'ArrowUp') btns[(i - 1 + btns.length) % btns.length].focus();
    else if (e.key === 'Escape') {
      closeMenu();
      anchor.focus();
    } else return;
    e.preventDefault();
    e.stopPropagation();
  });
  return menu;
}
if (typeof document !== 'undefined') {
  document.addEventListener('mousedown', (e) => {
    if (openMenuEl && !openMenuEl.contains(e.target)) closeMenu();
  });
  window.addEventListener('blur', closeMenu);
}

/** The standard ⋯ menu for a job. */
export function jobMenu(anchor, job, { onCancel } = {}) {
  const items = [
    { label: 'Show in Finder', icon: 'folder', run: () => api.reveal(job.id) },
    { label: 'Copy link', icon: 'link', run: () => api.copyText(job.url).then(() => toast('Link copied')) },
  ];
  if (job.state === 'done') {
    items.unshift({ label: 'Open', icon: 'doc', run: () => api.open(job.id) });
    items.push('-', { label: 'Remove from list', icon: 'trash', run: () => removeJob(job) });
  } else if (!(job.state === 'failed' && job.error === 'cancelled')) {
    items.push('-', { label: 'Cancel download', icon: 'close', kbd: '⌘⌫', danger: true, run: () => onCancel?.(job) });
  } else {
    items.push('-', { label: 'Remove from list', icon: 'trash', run: () => removeJob(job) });
  }
  showMenu(anchor, items);
}

export async function removeJob(job) {
  await api.remove(job.id);
  store.forget(job.id);
}

/** Inline "Cancel download?" confirm inside `host`. Resolves true if confirmed. */
export function inlineConfirm(host, text, okLabel = 'Cancel download') {
  host.querySelector(':scope > .confirm')?.remove();
  return new Promise((resolve) => {
    const done = (v) => {
      box.remove();
      resolve(v);
    };
    const keep = h('button.btn.plain', { type: 'button', onclick: (e) => (e.stopPropagation(), done(false)) }, 'Keep');
    const ok = h('button.btn.danger', { type: 'button', onclick: (e) => (e.stopPropagation(), done(true)) }, okLabel);
    const box = h('div.confirm', { role: 'alertdialog', 'aria-label': text, onclick: (e) => e.stopPropagation() }, h('span', text), keep, ok);
    box.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        done(false);
      }
      if (e.key === 'Enter') {
        e.preventDefault();
        e.stopPropagation();
        done(true);
      }
    });
    host.append(box);
    ok.focus();
  });
}

let toastTimer = 0;
export function toast(text) {
  document.querySelector('.toast')?.remove();
  const t = h('div.toast', { role: 'status' }, text);
  document.body.append(t);
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.remove(), 1600);
}

/** Add a pasted link. Returns the job id or null. */
export async function addLink(text) {
  const url = String(text || '').trim();
  if (!looksLikeUrl(url)) return null;
  const r = await api.add(url, { source: 'paste' });
  return r?.job_id ?? null;
}

/**
 * Keyboard for a selectable list:
 * ↑/↓ select, space = pause/resume, ⌘⌫ = cancel (inline confirm), Return = onEnter.
 */
export function listKeys({ ids, getSelected, select, onCancel, onEnter }) {
  document.addEventListener('keydown', (e) => {
    const tag = document.activeElement?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA') return;
    if (document.activeElement?.closest?.('.confirm, .menu')) return;
    const list = ids();
    const cur = getSelected();
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      if (!list.length) return;
      const i = list.indexOf(cur);
      const n = e.key === 'ArrowDown' ? Math.min(list.length - 1, i + 1) : Math.max(0, i < 0 ? 0 : i - 1);
      select(list[n], true);
      e.preventDefault();
    } else if (e.key === ' ' && cur) {
      toggle(store.get(cur));
      e.preventDefault();
    } else if (e.key === 'Backspace' && e.metaKey && cur) {
      onCancel(store.get(cur));
      e.preventDefault();
    } else if (e.key === 'Enter' && cur && onEnter) {
      onEnter(store.get(cur), e);
      e.preventDefault();
    }
  });
}
