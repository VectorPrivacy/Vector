// The guided progress ring over a community re-key (privatize, v2 migration).
const r = $state({ open: false, title: '', pct: 0, step: '' });
export function rekeyState() { return r; }
export function setRekey(patch) { Object.assign(r, patch); }
