// The credential modal (type choice, PIN, password, or a validating hold) and the
// encryption migration overlay. settings.js opens each and owns the promise; the modal
// answers through `handlers`.
const c = $state({
    open: false, mode: 'pin', title: '', subtitle: '', subtitleGradient: false,
    confirmText: 'Confirm', selectedType: 'pin', password: '', pinSeq: 0,
});
let handlers = null;   // { cancel(), submit(value) }

export function credentialState() { return c; }
export function credentialHandlers() { return handlers; }
export function openCredentialDialog(view, h) {
    Object.assign(c, { mode: 'pin', subtitle: '', subtitleGradient: false, confirmText: 'Confirm', selectedType: 'pin', password: '' }, view, { open: true });
    c.pinSeq++;
    handlers = h;
}
export function closeCredentialDialog() { c.open = false; handlers = null; }

const m = $state({ open: false, title: '', phase: 'Preparing...', pct: 0 });
export function migrationState() { return m; }
export function showMigration(title) { m.title = title; m.phase = 'Preparing...'; m.pct = 0; m.open = true; }
export function hideMigration() { m.open = false; }
export function setMigrationProgress(phase, pct) { m.phase = phase; m.pct = pct; }
