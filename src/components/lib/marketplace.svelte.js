import { SvelteMap } from 'svelte/reactivity';
// The Nexus (marketplace) state: the app catalogue, search and category filters, the
// per-app action in flight (install, update, launch, uninstall), resolved icons, and the
// details panel's app and permissions. Apps are replaced as whole objects on change.
const m = $state({ query: '', filters: [], loading: false, error: '', animate: false, detailsId: null,
    panelOpen: false, panelClosing: false, detailsOpen: false, detailsClosing: false });
let handlers = $state.raw(null);
const closeWaiters = new Map();   // 'panel' | 'details' → resolve, settled by the closing animation's end
export function mktHandlers() { return handlers; }
export function setMarketplaceHandlers(h) { handlers = h; }
export function mktOpenPanel() { m.panelClosing = false; m.panelOpen = true; }
export function mktOpenDetailsPanel() { m.detailsClosing = false; m.detailsOpen = true; }
/** Play the closing animation; resolves once it ends (immediately when not open). */
export function mktClosePanel(which) {
    const open = which === 'panel' ? m.panelOpen : m.detailsOpen;
    if (!open) return Promise.resolve();
    return new Promise((resolve) => {
        closeWaiters.set(which, resolve);
        if (which === 'panel') m.panelClosing = true; else m.detailsClosing = true;
        // A frozen or disabled animation never ends; the close must not hang on it.
        setTimeout(() => { if (closeWaiters.get(which) === resolve) mktClosingEnded(which); }, 400);
    });
}
export function mktClosingEnded(which) {
    if (which === 'panel') { m.panelOpen = false; m.panelClosing = false; }
    else { m.detailsOpen = false; m.detailsClosing = false; }
    closeWaiters.get(which)?.();
    closeWaiters.delete(which);
}
let apps = $state.raw([]);
const actions = new SvelteMap();   // per-key reactive: a row re-derives only when its own entry moves
const icons = new SvelteMap();
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
    if (action) actions.set(id, action); else actions.delete(id);
}
/** A resolved icon source, or false when the icon cannot be shown. */
export function mktSetIcon(key, src) {
    if (icons.get(key) === src) return;
    icons.set(key, src);
}
export function mktOpenDetails(id) { m.detailsId = id; perms = null; }
export function mktCloseDetails() { m.detailsId = null; perms = null; }
export function mktSetPerms(next) { perms = next; }
