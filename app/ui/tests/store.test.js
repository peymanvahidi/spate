// The shared store follows the backend's "removed" event (a job removed in another window).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as api from '../js/api.js';
import * as store from '../js/store.js';

const tick = () => new Promise((r) => setTimeout(r, 0));

test('store drops a job on the removed event and tells its listeners once', async () => {
  try {
    await store.init();
    const done = store.all().find((j) => j.state === 'done');
    assert.ok(done, 'sample data has a finished job');
    const changes = [];
    const unSub = store.subscribe((ids) => changes.push(ids));
    const removed = [];
    const unRem = store.onRemoved((id) => removed.push(id));

    // Removed elsewhere: only the backend event reaches this window.
    await api.remove(done.id);
    await tick();
    assert.equal(store.get(done.id), undefined);
    assert.ok(!store.all().some((j) => j.id === done.id));
    assert.deepEqual(removed, [done.id]);
    assert.ok(changes.some((ids) => ids.has('*')), 'screens get a structural change');

    // This window's own removeJob path calls forget() after the event: no second notification.
    store.forget(done.id);
    assert.deepEqual(removed, [done.id]);
    unSub();
    unRem();
  } finally {
    api.stopMock();
  }
});
