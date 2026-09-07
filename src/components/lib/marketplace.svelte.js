// The Nexus (marketplace) state: the app catalogue, search and category filters, the
// per-app action in flight (install, update, launch, uninstall), resolved icons, and the
// details panel's app and permissions. Apps are replaced as whole objects on change.
const m = $state({ query: '', filters: [], loading: false, error: '', animate: false, detailsId: null });
let apps = $state.raw([]);
let actions = $state.raw(new Map());
let icons = $state.raw(new Map());
let perms = $state.raw(null);

export function mktState() { return m; }
export function mktApps() { return apps; }
export function mktActions() { return actions; }
export function mktIcons() { return icons; }
export function mktPerms() { return perms; }

export function mktSetApps(list) { apps = list || []; }
export function mktPatchApp(id, fields) { apps = apps.map(a => a.id === id ? { ...a, ...fields } : a); }
export function mktSetQuery(q) { m.query = q || ''; }
export function mktAddFilter(category) {
    const c = category.toLowerCase();
    if (!m.filters.includes(c)) m.filters = [...m.filters, c];
}
export function mktRemoveFilter(category) {
    const c = category.toLowerCase();
    m.filters = m.filters.filter(f => f !== c);
}
export function mktClearFilters() { m.filters = []; m.query = ''; }
export function mktSetLoading(on) { m.loading = !!on; if (on) m.error = ''; }
export function mktSetError(err) { m.error = err ? String(err) : ''; m.loading = false; }
export function mktSetAnimate(on) { m.animate = !!on; }
/** The action running on one app, or null when it is idle. `{ kind, label }`. */
export function mktSetAction(id, action) {
    const next = new Map(actions);
    if (action) next.set(id, action); else next.delete(id);
    actions = next;
}
/** A resolved icon source, or false when the icon cannot be shown. */
export function mktSetIcon(key, src) {
    if (icons.get(key) === src) return;
    const next = new Map(icons); next.set(key, src); icons = next;
}
export function mktOpenDetails(id) { m.detailsId = id; perms = null; }
export function mktCloseDetails() { m.detailsId = null; perms = null; }
export function mktSetPerms(next) { perms = next; }
