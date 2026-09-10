// The My Profile switcher: open state, where it hangs from (the Profile header, or the
// widescreen rail's chip as a drop-up anchored by pixel), the rows and the Add gate.
// accounts.js drives it and answers the row and Add clicks through `handlers`.
import { flushSync } from 'svelte';

const sw = $state({
    open: false, dropup: false, bottomPx: null,
    accounts: [], activeNpub: '',
    addDisabled: false, addLabel: 'Add Profile',
});
let handlers = $state.raw({});   // { close, onPick, onDelete, onAdd, rowHelpers }

export function switcherState() { return sw; }
export function switcherHandlers() { return handlers; }
export function setSwitcherHandlers(h) { handlers = h || {}; }
export function setSwitcherRows(accounts, activeNpub) { sw.accounts = accounts; sw.activeNpub = activeNpub || ''; }
export function setSwitcherAdd(disabled, label) { sw.addDisabled = !!disabled; sw.addLabel = label; }
export function openSwitcher(dropup, bottomPx) {
    sw.dropup = !!dropup; sw.bottomPx = Number.isFinite(bottomPx) ? bottomPx : null; sw.open = true;
    flushSync();
}
export function closeSwitcher() { sw.open = false; sw.dropup = false; sw.bottomPx = null; flushSync(); }
