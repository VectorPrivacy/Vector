// The Mini Apps panel grid: the recent apps (PIVX is a virtual entry, hidden rather than
// absent), the search query, hold-to-edit mode, and per-app icon, update and download state.
// Entries are plain objects replaced on change so every reader moves.
const g = $state({ query: '', editMode: false, empty: false });
let apps = $state.raw([]);

export function gridState() { return g; }
export function gridApps() { return apps; }
export function gridSetApps(list, empty) { apps = list; g.empty = !!empty; }
export function gridSetQuery(q) { g.query = q; }
export function gridSetEditMode(on) { g.editMode = !!on; }
/** Replace one entry's fields by key. */
export function gridPatch(key, fields) {
    apps = apps.map(a => a.key === key ? { ...a, ...fields } : a);
}
export function gridRemove(key) { apps = apps.filter(a => a.key !== key); }
