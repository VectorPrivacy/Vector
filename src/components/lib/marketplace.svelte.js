import { SvelteMap } from 'svelte/reactivity';
import { fitsDevice } from './nexus.js';
// The Nexus (marketplace) state: the app catalogue, the page on show (a view, a category or
// one app), the search, the per-app action in flight (install, update, launch, uninstall),
// resolved icons, what you've played, and the open app's permissions. Apps are replaced as
// whole objects on change.
const m = $state({ query: '', view: 'discover', category: '', loading: false, error: '', detailsId: null,
    panelOpen: false, panelClosing: false, tick: 0, fitOnly: true });
// What this device plays with (marketplace.js detects it): touch, a keyboard and mouse, a controller.
const device = $state({ touch: false, keys: true, pad: false, phone: false });
let handlers = $state.raw(null);
const closeWaiters = [];
let closeTimer = 0;
export function mktHandlers() { return handlers; }
export function setMarketplaceHandlers(h) { handlers = h; }
/** Open (or reopen mid-close): a close still waiting is settled, as the panel it awaited is gone. */
export function mktOpenPanel() {
    clearTimeout(closeTimer);
    m.panelClosing = false;
    m.panelOpen = true;
    for (const done of closeWaiters.splice(0)) done();
}
/** Play the closing animation; resolves once it ends (immediately when not open). Asked
 *  again mid-close, it waits on the same one. */
export function mktClosePanel() {
    if (!m.panelOpen) return Promise.resolve();
    return new Promise((resolve) => {
        closeWaiters.push(resolve);
        if (m.panelClosing) return;
        m.panelClosing = true;
        // A frozen or disabled animation never ends; the close must not hang on it.
        clearTimeout(closeTimer);
        closeTimer = setTimeout(() => { if (m.panelClosing) mktClosingEnded(); }, 400);
    });
}
export function mktClosingEnded() {
    clearTimeout(closeTimer);
    m.panelOpen = false;
    m.panelClosing = false;
    m.detailsId = null;
    perms = null;
    for (const done of closeWaiters.splice(0)) done();
}
let apps = $state.raw([]);
const actions = new SvelteMap();   // per-key reactive: a row re-derives only when its own entry moves
const icons = new SvelteMap();
let perms = $state.raw(null);
let played = $state.raw([]);

export function mktState() { return m; }
export function mktDevice() { return device; }
export function mktSetDevice(d) { Object.assign(device, d); }
/** Only what fits this device, or everything. */
export function mktSetFitOnly(on) { m.fitOnly = !!on; }
/** Whether a listing shows: it fits this device, or the filter is off. */
export function mktFits(app) { return !m.fitOnly || fitsDevice(app, device); }
export function mktApps() { return apps; }
export function mktActions() { return actions; }
export function mktIcons() { return icons; }
export function mktPerms() { return perms; }
/** Your play history, newest first: `{ id, at, count }` per Nexus app. */
export function mktPlayed() { return played; }

export function mktSetApps(list) { apps = list || []; }
export function mktPatchApp(id, fields) { apps = apps.map(a => a.id === id ? { ...a, ...fields } : a); }
export function mktSetPlayed(list) { played = list || []; }
export function mktSetQuery(q) { m.query = q || ''; }
/** Show a page: 'discover', 'library', 'all', or a category's apps. Leaves any open app. */
export function mktShow(view, category = '') {
    m.view = view;
    m.category = view === 'category' ? category.toLowerCase() : '';
    m.query = '';
    m.detailsId = null;
    perms = null;
    m.tick++;
}
export function mktReset() { m.view = 'discover'; m.category = ''; m.query = ''; m.detailsId = null; perms = null; m.tick++; }
export function mktSetLoading(on) { m.loading = !!on; if (on) m.error = ''; }
export function mktSetError(err) { m.error = err ? String(err) : ''; m.loading = false; }
/** The action running on one app, or null when it is idle. `{ kind, label, pct? }`. */
export function mktSetAction(id, action) {
    if (action) actions.set(id, action); else actions.delete(id);
}
/** A download's progress, onto the action already showing it. */
export function mktSetProgress(id, pct) {
    const a = actions.get(id);
    if (a && (a.kind === 'installing' || a.kind === 'updating') && a.pct !== pct) actions.set(id, { ...a, pct });
}
/** A resolved icon source, or false when the icon cannot be shown. */
export function mktSetIcon(key, src) {
    if (icons.get(key) === src) return;
    icons.set(key, src);
}
export function mktOpenDetails(id) { m.detailsId = id; perms = null; m.tick++; }
export function mktCloseDetails() { m.detailsId = null; perms = null; m.tick++; }
export function mktSetPerms(next) { perms = next; }

// Play with friends: which app is going out, and to whom it could.
const picker = $state({ open: false, active: false, closing: false, appId: null, tick: 0 });
let pickerTimer = 0;
export function mktPicker() { return picker; }
export function mktOpenPicker(appId) {
    clearTimeout(pickerTimer);
    picker.appId = appId;
    picker.open = true;
    picker.closing = false;
    picker.tick++;
    requestAnimationFrame(() => { picker.active = true; });
}
export function mktClosePicker() {
    if (!picker.open || picker.closing) return;
    picker.closing = true;
    pickerTimer = setTimeout(() => {
        if (!picker.closing) return;
        picker.open = false; picker.active = false; picker.closing = false; picker.appId = null;
    }, 150);
}
