// The widescreen rail: its scroller element, and the community arrangement it paints.
import { SvelteSet } from 'svelte/reactivity';

const els = { spacesRows: null };
export function railEls() { return els; }
export function bindRailEl(name) { return (node) => { els[name] = node; return { destroy() { els[name] = null; } }; }; }

// The stored arrangement (order + folders) as the backend last reported it. Keys it
// holds for communities this device has not synced are kept and simply not painted.
let layout = $state.raw({ v: 1, nodes: [] });
export function railLayout() { return layout; }
export function setRailLayout(next) { layout = next && Array.isArray(next.nodes) ? next : { v: 1, nodes: [] }; }

// Which folders are open is the screen's business, not the account's: two devices
// would otherwise fold each other's rails shut.
const OPEN_KEY = 'vector_rail_open_folders';
const open = new SvelteSet(readOpen());
function readOpen() {
    try { return JSON.parse(localStorage.getItem(OPEN_KEY) || '[]'); } catch { return []; }
}
function writeOpen() {
    try { localStorage.setItem(OPEN_KEY, JSON.stringify([...open])); } catch { /* per-device nicety only */ }
}
export function railFolderOpen(id) { return open.has(id); }
export function toggleRailFolder(id) {
    if (open.has(id)) open.delete(id); else open.add(id);
    writeOpen();
}
