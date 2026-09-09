// The attachment panel: which view is up (main, mini apps, PIVX wallet), the platform
// visibility of the main buttons, the mini apps search, and the PIVX balance card.
// `pulse` counters replay the staggered fade-in of a view's items on each open.
const p = $state({
    view: 'main',              // main | miniapps | pivx
    folderShown: false,
    commandsShown: false,
    commandsDisabled: false,
    search: '',
    pulse: { main: 0, grid: 0, pivx: 0 },
});
export function attachmentState() { return p; }
export function attachmentSetView(view) { p.view = view; }
export function attachmentPatch(fields) { Object.assign(p, fields); }
/** Replay the fade-in of a view's items: main, grid or pivx. */
export function attachmentPulse(which) { p.pulse[which]++; }

// The wallet card: null balance = loading spinner; the deposit button locks above the cap.
const w = $state({ balance: null, fiat: '', depositDisabled: false, depositLoading: false, v: 0 });
export function pivxWalletState() { return w; }
export function pivxWalletLoading() { w.balance = null; w.fiat = ''; }
export function pivxWalletSet({ balance, fiat, depositDisabled }) {
    w.balance = balance; w.fiat = fiat || ''; w.depositDisabled = !!depositDisabled; w.v++;
}
export function pivxWalletPatch(fields) { Object.assign(w, fields); }
