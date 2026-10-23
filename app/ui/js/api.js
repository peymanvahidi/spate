// The UI's only door to the outside world. Every screen talks to this module, never to the
// backend directly. In the Tauri shell (`window.__TAURI__` present, `withGlobalTauri: true`)
// it uses commands and events; in a plain browser it uses the in-page mock.
//
// Contract for the shell (phase 3b): see tauriBackend() below for the exact command names,
// argument shapes and the `job` / `removed` events. All JobView objects are spate-proto `JobView`.

import { createMock } from './mock.js';

const T = typeof window !== 'undefined' ? window.__TAURI__ : undefined;

function tauriBackend(t) {
  const invoke = t.core.invoke;
  return {
    /** -> JobView[] */
    list: () => invoke('list'),
    /** opts: Partial<AddRequest> (filename, referrer, size_hint, mime, headers, source). -> {job_id} */
    add: (url, opts = {}) => invoke('add', { request: { url, headers: [], source: 'paste', kind: 'file', ...opts } }),
    pause: (id) => invoke('pause', { jobId: id }),
    resume: (id) => invoke('resume', { jobId: id }),
    cancel: (id) => invoke('cancel', { jobId: id }),
    remove: (id) => invoke('remove', { jobId: id }),
    /** Show the file (or its .spate-part) in Finder. */
    reveal: (id) => invoke('reveal', { jobId: id }),
    /** Open the finished file with its default app. */
    open: (id) => invoke('open', { jobId: id }),
    /** Event "job", payload = JobView, ≈4/s per running job plus every state change. Returns an unsubscribe fn. */
    onJob(cb) {
      let un = null;
      let dead = false;
      t.event.listen('job', (e) => cb(e.payload)).then((u) => (dead ? u() : (un = u)));
      return () => {
        dead = true;
        if (un) un();
      };
    },
    /** Event "removed", payload = job id: the job left the history (removed in any window). Returns an unsubscribe fn. */
    onRemoved(cb) {
      let un = null;
      let dead = false;
      // Payload is the id string; accept {job_id} too, like the socket push.
      t.event.listen('removed', (e) => cb(typeof e.payload === 'string' ? e.payload : e.payload?.job_id)).then((u) => (dead ? u() : (un = u)));
      return () => {
        dead = true;
        if (un) un();
      };
    },
    // Window/shell helpers (not daemon methods).
    openWindow: (name) => invoke('open_window', { name }), // "main" | "settings"
    closeWindow: () => t.window.getCurrentWindow().close(),
    chooseFolder: (id) => invoke('choose_folder', { jobId: id }), // -> new dir or null
    setIsland: (mode) => invoke('set_island', { mode }), // "hidden" | "collapsed" | "expanded"
    copyText: (text) => navigator.clipboard.writeText(text),
  };
}

function mockBackend() {
  const m = createMock();
  return {
    ...m,
    openWindow: async (name) => {
      const url = name === 'main' ? 'main.html' : null;
      if (url) window.open(url, '_blank');
      else console.info('[mock] open window:', name);
    },
    closeWindow: async () => document.dispatchEvent(new CustomEvent('spate:close')),
    chooseFolder: async () => {
      console.info('[mock] choose folder');
      return '/Users/me/Desktop';
    },
    setIsland: async (mode) => document.documentElement.setAttribute('data-island', mode),
    copyText: async (text) => {
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        console.info('[mock] copy:', text);
      }
    },
  };
}

// Swap point: the shell sets window.__TAURI__, everything else is the mock.
const backend = T?.core ? tauriBackend(T) : mockBackend();

export const isShell = !!T?.core;
export const list = () => backend.list();
export const add = (url, opts) => backend.add(url, opts);
export const pause = (id) => backend.pause(id);
export const resume = (id) => backend.resume(id);
export const cancel = (id) => backend.cancel(id);
export const remove = (id) => backend.remove(id);
export const reveal = (id) => backend.reveal(id);
export const open = (id) => backend.open(id);
export const onJob = (cb) => backend.onJob(cb);
export const onRemoved = (cb) => backend.onRemoved(cb);
/** Stop the mock's timer (node tests); no-op in the shell. */
export const stopMock = () => backend.stop?.();
export const openWindow = (name) => backend.openWindow(name);
export const closeWindow = () => backend.closeWindow();
export const chooseFolder = (id) => backend.chooseFolder(id);
export const setIsland = (mode) => backend.setIsland(mode);
export const copyText = (text) => backend.copyText(text);
