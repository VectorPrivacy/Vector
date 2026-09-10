// Realtime status per Mini App topic (or per attachment for a solo app): whether a
// window is open here and who else is in the session. The app's listener writes it.
import { SvelteMap } from 'svelte/reactivity';
import { fadeDialog } from './dialog-lifecycle.svelte.js';

const status = new SvelteMap();   // key → { active, peerCount, peers: [npub] }

export function miniappStatus(key) { return status.get(key) ?? null; }
export function setMiniappStatus(key, { active, peerCount, peers }) {
    const prev = status.get(key) || { active: false, peerCount: 0, peers: [] };
    status.set(key, { active: !!active, peerCount: peerCount || 0, peers: peers || prev.peers });
}

// The launch dialog (solo, or invite someone), whose handlers are fixed at mount.
export const launchDialog = fadeDialog({ name: '', actionText: 'Play', updateMode: false, icon: null });
let launchHandlers = $state.raw(null);
export function launchDialogHandlers() { return launchHandlers; }
export function setLaunchDialogHandlers(h) { launchHandlers = h; }

