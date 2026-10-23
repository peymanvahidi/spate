// Inline SVG icons in the spirit of SF Symbols (1.5 px strokes on a 16 px grid, round caps),
// and file-type document icons. All use currentColor unless noted.

const S = (body, vb = 16) =>
  `<svg viewBox="0 0 ${vb} ${vb}" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">${body}</svg>`;

export const icon = {
  pause: S('<rect x="4" y="3" width="2.6" height="10" rx="0.9" fill="currentColor" stroke="none"/><rect x="9.4" y="3" width="2.6" height="10" rx="0.9" fill="currentColor" stroke="none"/>'),
  play: S('<path d="M5 3.2v9.6a.6.6 0 0 0 .9.5l7.6-4.8a.6.6 0 0 0 0-1L5.9 2.7a.6.6 0 0 0-.9.5z" fill="currentColor" stroke="none"/>'),
  retry: S('<path d="M13 8a5 5 0 1 1-1.6-3.7"/><path d="M13 2.5v3h-3"/>'),
  more: S('<circle cx="3.5" cy="8" r="1.15" fill="currentColor" stroke="none"/><circle cx="8" cy="8" r="1.15" fill="currentColor" stroke="none"/><circle cx="12.5" cy="8" r="1.15" fill="currentColor" stroke="none"/>'),
  close: S('<path d="M4.5 4.5l7 7M11.5 4.5l-7 7"/>'),
  search: S('<circle cx="7" cy="7" r="4.2"/><path d="M10.2 10.2L13.5 13.5"/>'),
  plus: S('<path d="M8 3v10M3 8h10"/>'),
  gear: S('<circle cx="8" cy="8" r="2.1"/><path d="M8 1.8v1.6M8 12.6v1.6M14.2 8h-1.6M3.4 8H1.8M12.4 3.6l-1.1 1.1M4.7 11.3l-1.1 1.1M12.4 12.4l-1.1-1.1M4.7 4.7L3.6 3.6"/><circle cx="8" cy="8" r="4.6"/>'),
  window: S('<rect x="1.8" y="3" width="12.4" height="10" rx="2"/><path d="M1.8 6h12.4"/>'),
  folder: S('<path d="M1.8 4.6a1.4 1.4 0 0 1 1.4-1.4h3l1.4 1.5h5.2a1.4 1.4 0 0 1 1.4 1.4v5.8a1.4 1.4 0 0 1-1.4 1.4H3.2a1.4 1.4 0 0 1-1.4-1.4z"/>'),
  link: S('<path d="M6.8 9.2a2.6 2.6 0 0 0 3.7 0l2.2-2.2a2.6 2.6 0 0 0-3.7-3.7l-.7.7"/><path d="M9.2 6.8a2.6 2.6 0 0 0-3.7 0L3.3 9a2.6 2.6 0 0 0 3.7 3.7l.7-.7"/>'),
  check: S('<path d="M3.5 8.4l3 3 6-6.6"/>'),
  checkCircle: S('<circle cx="8" cy="8" r="6.2"/><path d="M5.3 8.2l1.9 1.9 3.6-4"/>'),
  warn: S('<path d="M7.1 2.6L1.9 11.8a1 1 0 0 0 .9 1.5h10.4a1 1 0 0 0 .9-1.5L8.9 2.6a1 1 0 0 0-1.8 0z"/><path d="M8 6.2v3"/><circle cx="8" cy="11.1" r=".55" fill="currentColor"/>'),
  down: S('<path d="M8 2.5v9M4 7.8l4 4 4-4"/><path d="M3 14h10"/>'),
  downCircle: S('<circle cx="8" cy="8" r="6.2"/><path d="M8 4.8v6M5.4 8.4L8 11l2.6-2.6"/>'),
  clock: S('<circle cx="8" cy="8" r="6.2"/><path d="M8 4.6V8l2.3 1.5"/>'),
  tray: S('<path d="M2 9.5l1.6-5.3a1 1 0 0 1 1-.7h6.8a1 1 0 0 1 1 .7L14 9.5v3a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z"/><path d="M2 9.5h3.4l.8 1.6h3.6l.8-1.6H14"/>'),
  film: S('<rect x="2" y="2.5" width="12" height="11" rx="1.6"/><path d="M5 2.5v11M11 2.5v11M2 6h3M2 10h3M11 6h3M11 10h3"/>'),
  music: S('<path d="M6 12V3.6l7-1.4v8.4"/><circle cx="4.3" cy="12" r="1.8"/><circle cx="11.3" cy="10.6" r="1.8"/>'),
  archive: S('<rect x="1.8" y="2.6" width="12.4" height="3.2" rx="1"/><path d="M2.8 5.8v6.6a1 1 0 0 0 1 1h8.4a1 1 0 0 0 1-1V5.8"/><path d="M6.4 8.4h3.2"/>'),
  app: S('<rect x="2.2" y="2.2" width="11.6" height="11.6" rx="3"/><path d="M5.8 10.6L8 5l2.2 5.6M6.6 8.8h2.8"/>'),
  doc: S('<path d="M4 1.8h5l3.2 3.2v8a1.2 1.2 0 0 1-1.2 1.2H4A1.2 1.2 0 0 1 2.8 13V3A1.2 1.2 0 0 1 4 1.8z"/><path d="M9 1.8V5h3.2M5.3 8.2h5.4M5.3 10.8h3.8"/>'),
  photo: S('<rect x="1.8" y="2.8" width="12.4" height="10.4" rx="1.8"/><circle cx="5.6" cy="6.2" r="1.2"/><path d="M1.8 11.4l3.6-3.2 2.8 2.4 2.2-1.8 3.8 3"/>'),
  paste: S('<rect x="3" y="2.8" width="10" height="11.4" rx="1.6"/><path d="M6 2.8V2.2a.8.8 0 0 1 .8-.8h2.4a.8.8 0 0 1 .8.8v.6"/><path d="M5.8 7.2h4.4M5.8 9.8h3"/>'),
  chevron: S('<path d="M6 3.8l4.2 4.2L6 12.2"/>'),
  stop: S('<rect x="3.6" y="3.6" width="8.8" height="8.8" rx="1.6" fill="currentColor" stroke="none"/>'),
  trash: S('<path d="M2.8 4.2h10.4M6.2 4.2V2.8h3.6v1.4M4 4.2l.7 8.6a1.2 1.2 0 0 0 1.2 1.1h4.2a1.2 1.2 0 0 0 1.2-1.1l.7-8.6"/>'),
  bolt: S('<path d="M9 1.8L3.6 9h4l-.8 5.2L12.4 7h-4z" fill="currentColor" stroke="none"/>'),
};

export const kindIcon = {
  Video: icon.film,
  Audio: icon.music,
  Archive: icon.archive,
  App: icon.app,
  Document: icon.doc,
  Image: icon.photo,
};

// File-type document icon: a page with a folded corner, tinted per kind, with a glyph.
const TINT = {
  Video: ['#7B5BA6', '#B394DE'],
  Audio: ['#C2410C', '#F08A5D'],
  Archive: ['#8A6A1F', '#E3B65C'],
  App: ['#2F6FA8', '#6BA8E0'],
  Document: ['#B4372F', '#EE7B70'],
  Image: ['#0C7A64', '#3CC4A2'],
  null: ['#6E6E73', '#A1A1A6'],
};

const GLYPH = {
  Video: '<path d="M13 15.6v6.8l5.6-3.4z" fill="var(--fi-ink)"/>',
  Audio: '<path d="M14.6 21.4v-6.6l5-1v6" fill="none" stroke="var(--fi-ink)" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/><circle cx="13.2" cy="21.4" r="1.5" fill="var(--fi-ink)"/><circle cx="18.2" cy="19.8" r="1.5" fill="var(--fi-ink)"/>',
  Archive: '<path d="M16 9.5v1.4m0 1.4v1.4m0 1.4v1.4" stroke="var(--fi-ink)" stroke-width="1.6"/><rect x="14.3" y="17.2" width="3.4" height="4.4" rx="1" fill="none" stroke="var(--fi-ink)" stroke-width="1.3"/>',
  App: '<rect x="11.6" y="13.4" width="8.8" height="8.8" rx="2.2" fill="none" stroke="var(--fi-ink)" stroke-width="1.4"/><path d="M14 19.8l2-4.6 2 4.6" fill="none" stroke="var(--fi-ink)" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>',
  Document: '<path d="M11.5 14.5h9M11.5 17.2h9M11.5 19.9h6" stroke="var(--fi-ink)" stroke-width="1.3" stroke-linecap="round"/>',
  Image: '<circle cx="13.6" cy="15" r="1.4" fill="var(--fi-ink)"/><path d="M10.8 22l3.6-3.6 2.4 2 2-1.6 2.6 3.2z" fill="var(--fi-ink)"/>',
  null: '<path d="M11.5 15h9M11.5 18h9" stroke="var(--fi-ink)" stroke-width="1.3" stroke-linecap="round"/>',
};

const LABEL = { Video: 'Video', Audio: 'Audio', Archive: 'Archive', App: 'App', Document: 'Document', Image: 'Image', null: 'File' };

/** Document icon for a file kind (from fileKind()). Size in CSS px. */
export function fileIcon(kind, size = 28, ext = '') {
  const k = kind in TINT ? kind : 'null';
  const [light, dark] = TINT[k];
  const label = (ext || '').slice(0, 4).toUpperCase();
  return `<svg class="file-icon" viewBox="0 0 32 32" width="${size}" height="${size}" role="img" aria-label="${LABEL[k]} file" style="--fi-light:${light};--fi-dark:${dark}">
  <path d="M8 2.5h11.5L26 9v19a1.5 1.5 0 0 1-1.5 1.5h-16A1.5 1.5 0 0 1 7 28V4a1.5 1.5 0 0 1 1-1.5z" fill="var(--fi-paper)" stroke="var(--fi-edge)" stroke-width="1"/>
  <path d="M19.5 2.5V7.5a1.5 1.5 0 0 0 1.5 1.5H26" fill="var(--fi-fold)" stroke="var(--fi-edge)" stroke-width="1" stroke-linejoin="round"/>
  <g style="--fi-ink:var(--fi-tint)">${GLYPH[k]}</g>
  ${label && size >= 28 ? `<text x="16.5" y="27" text-anchor="middle" font-size="5.2" font-weight="700" letter-spacing=".3" fill="var(--fi-tint)" font-family="-apple-system, system-ui, sans-serif">${label}</text>` : ''}
</svg>`;
}

/** Spate mark: four connection-coloured pieces flowing into a down arrow. */
export const brandMark = `<svg viewBox="0 0 18 18" width="18" height="18" aria-hidden="true">
  <rect x="1" y="2.5" width="3.4" height="7" rx="1.2" fill="var(--seg-1)"/>
  <rect x="5.2" y="2.5" width="3.4" height="7" rx="1.2" fill="var(--seg-2)"/>
  <rect x="9.4" y="2.5" width="3.4" height="7" rx="1.2" fill="var(--seg-3)"/>
  <rect x="13.6" y="2.5" width="3.4" height="7" rx="1.2" fill="var(--seg-4)"/>
  <path d="M5 12l4 3.6 4-3.6" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
</svg>`;

/** Menu-bar template glyph (monochrome). */
export const trayGlyph = `<svg viewBox="0 0 18 18" width="16" height="16" aria-hidden="true" fill="currentColor">
  <rect x="2" y="2.5" width="3" height="7" rx="1"/><rect x="6" y="2.5" width="3" height="7" rx="1"/><rect x="10" y="2.5" width="3" height="7" rx="1" opacity=".55"/><rect x="14" y="2.5" width="2.2" height="7" rx="1" opacity=".3"/>
  <path d="M5 12l4 3.5 4-3.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
</svg>`;

/** Extension of a file name, without the dot ("tar.gz" → "gz"). */
export function extOf(name) {
  const m = /\.([^.]+)$/.exec(name || '');
  return m ? m[1] : '';
}
