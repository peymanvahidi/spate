import { DEFAULT_SETTINGS, THRESHOLD_CHOICES_MB, parseList } from '../lib/logic.js';
import { loadSettings, saveSettings } from '../lib/native.js';

const $ = (id) => document.getElementById(id);

for (const mb of THRESHOLD_CHOICES_MB) {
  const o = document.createElement('option');
  o.value = String(mb);
  o.textContent = `${mb} MB`;
  $('threshold').append(o);
}

function show(s) {
  $('enabled').checked = s.enabled;
  if (!THRESHOLD_CHOICES_MB.includes(s.thresholdMB)) {
    const o = document.createElement('option');
    o.value = String(s.thresholdMB);
    o.textContent = `${s.thresholdMB} MB`;
    $('threshold').append(o);
  }
  $('threshold').value = String(s.thresholdMB);
  $('types').value = s.types.join(' ');
  $('sites').value = s.neverSites.join('\n');
}

let savedTimer = null;
async function save() {
  await saveSettings({
    enabled: $('enabled').checked,
    thresholdMB: Number($('threshold').value),
    types: parseList($('types').value),
    neverSites: parseList($('sites').value),
  });
  $('saved').textContent = 'Saved';
  clearTimeout(savedTimer);
  savedTimer = setTimeout(() => ($('saved').textContent = ''), 1500);
}

$('enabled').addEventListener('change', save);
$('threshold').addEventListener('change', save);
// Text areas save when the user leaves them, then show the normalized value.
for (const id of ['types', 'sites']) {
  $(id).addEventListener('change', async () => {
    await save();
    show(await loadSettings());
  });
}
$('reset').addEventListener('click', async () => {
  await saveSettings({ ...DEFAULT_SETTINGS, neverSites: [] });
  show(await loadSettings());
});

loadSettings().then(show);
