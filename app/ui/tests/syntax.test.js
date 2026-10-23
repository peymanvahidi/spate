// `node --check` every JS file of the UI.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const files = ['js', 'tests'].flatMap((d) =>
  readdirSync(join(root, d))
    .filter((f) => f.endsWith('.js'))
    .map((f) => join(root, d, f)),
);

for (const f of files) {
  test(`node --check ${f.slice(root.length + 1)}`, () => {
    const r = spawnSync(process.execPath, ['--check', f], { encoding: 'utf8' });
    assert.equal(r.status, 0, r.stderr);
  });
}
