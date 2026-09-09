// The shell: which top-level panes show, which navbar tab is lit, and the chrome flags
// nav toggles. JS nav writes here and the shell components bind to it. Every write is
// flushed synchronously because callers measure layout (adjustSize) right after.
import { flushSync } from 'svelte';

const panes = $state({
    navbar: false, chats: true, chat: false, profile: false, settings: false,
    invites: false, groupOverview: false, chatNew: false, createGroup: false,
});
const shell = $state({ tab: 'chat-btn', invitesTab: false, settingsTab: true, updateDot: false });
let handlers = {};   // { openProfile, openChatlist, openSettings, openInvites }

export function shellPanes() { return panes; }
export function shellState() { return shell; }
export function shellHandlers() { return handlers; }
export function setShellHandlers(h) { handlers = h || {}; }

export function showPane(name, on) {
    if (!(name in panes)) throw new Error(`showPane: unknown pane '${name}'`);
    panes[name] = !!on;
    flushSync();
}
export function paneShown(name) { return !!panes[name]; }
export function panesSnapshot() { return { ...panes }; }
export function restorePanes(snap) { Object.assign(panes, snap); flushSync(); }

export function setTab(id) { shell.tab = id; flushSync(); }
export function setShellFlag(key, on) { shell[key] = !!on; flushSync(); }
