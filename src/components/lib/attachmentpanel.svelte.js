// The attachment panel: which view is up (main, mini apps, PIVX wallet), the platform
// visibility of the main buttons, the mini apps search, and the PIVX balance card.
// `pulse` counters replay the staggered fade-in of a view's items on each open.
const p = $state({
    visible: false, bottom: '',   // the root's `visible` class and offset above the composer
    view: 'main',              // main | miniapps | pivx
    folderShown: false,
    commandsShown: false,
    commandsDisabled: false,
    search: '',
    tick: { main: 0, grid: 0, pivx: 0 },
});
export function attachmentState() { return p; }
// The panel's own root: visible over the composer at a bottom offset the opener measures.
// Every open and close goes through one setter, which the back stack listens to.
const rootEls = $state.raw({ root: null });
let handlers = $state.raw(null);
const visibilityListeners = new Set();
export function attachmentEls() { return rootEls; }
export function setAttachmentRootEl(el) { rootEls.root = el; }
export function attachmentHandlers() { return handlers; }
export function setAttachmentHandlers(h) { handlers = h; }
export function onAttachmentVisibility(fn) { visibilityListeners.add(fn); return () => visibilityListeners.delete(fn); }
export function attachmentVisible() { return !!p.visible; }
export function setAttachmentVisible(on, bottom = '') {
    p.bottom = on ? bottom : '';
    if (!!p.visible === !!on) return;
    p.visible = !!on;
    for (const fn of visibilityListeners) fn(p.visible);
}
export function attachmentSetView(view) { p.view = view; }
export function attachmentPatch(fields) { Object.assign(p, fields); }
/** Replay the fade-in of a view's items: main, grid or pivx. */
export function attachmentPulse(which) { p.tick[which]++; }

// The wallet card: null balance = loading spinner; the deposit button locks above the cap.
const w = $state({ balance: null, fiat: '', depositDisabled: false, depositLoading: false, seq: 0 });
export function pivxWalletState() { return w; }
export function pivxWalletLoading() { w.balance = null; w.fiat = ''; }
export function pivxWalletSet({ balance, fiat, depositDisabled }) {
    w.balance = balance; w.fiat = fiat || ''; w.depositDisabled = !!depositDisabled; w.seq++;
}
export function pivxWalletPatch(fields) { Object.assign(w, fields); }
