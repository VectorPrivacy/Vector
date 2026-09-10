// Per-key invalidation signals: the fine-grained half of the store layer.
//
// Page state stays RAW (chat and profile objects are shared by reference with
// eventCache and the legacy arrays, mutated in place), so reactivity cannot come
// from proxies. Instead every entity has a version signal: a row's view-model reads
// its own chat's version, and the vanilla side bumps exactly that one when it
// mutates the chat. One message arriving re-derives one row; the list's order and
// membership have a signal of their own, so a reorder re-diffs the keyed each
// without touching any row's derivation.

import { SvelteMap } from 'svelte/reactivity';

// One reactive map per entity kind. SvelteMap creates each key's source the way the
// runtime expects: a plain `$state` minted inside the derived that first reads it is
// deliberately NOT tracked by that derived (Svelte's `current_sources` rule), which
// is why the per-key sources cannot be hand-rolled. Keys are pre-registered at the
// list chokepoints (`ensureSignals`) so a first touch never has to invalidate every
// reader of a still-missing key.
const chats = new SvelteMap();
const profiles = new SvelteMap();
const communities = new SvelteMap();

function read(map, key) {
    return map.get(key) ?? 0;
}
function bump(map, key) {
    map.set(key, (map.get(key) ?? 0) + 1);
}

/** Register keys ahead of their first read. Call outside reactions (vanilla side). */
export function ensureSignals({ chats: c = [], profiles: p = [], communities: m = [] } = {}) {
    for (const id of c) if (!chats.has(id)) chats.set(id, 0);
    for (const id of p) if (!profiles.has(id)) profiles.set(id, 0);
    for (const id of m) if (!communities.has(id)) communities.set(id, 0);
}

/** Read: a chat's version (the row re-derives when it changes). */
export function chatVersion(id) {
    return read(chats, id);
}
/** Write: this chat changed in place (message, unread, typing, name, mute). */
export function touchChat(id) {
    bump(chats, id);
}

/** Read/write for a profile: DM rows show its name and avatar. */
export function profileVersion(npub) {
    return read(profiles, npub);
}
export function touchProfile(npub) {
    bump(profiles, npub);
}

/** Read/write for a community: its single row aggregates every channel chat. */
export function communityVersion(id) {
    return read(communities, id);
}
export function touchCommunity(id) {
    bump(communities, id);
}

// The list's own shape: order and membership. Bumped after a sort, never for a
// change inside a row.
const list = $state({ seq: 0 });
export function listVersion() {
    return list.seq;
}
export function reorderChatlist() {
    list.seq++;
}

// Which chat is open: the rail's active shortcut and (later) the list's active row
// derive from it instead of being re-stamped by hand.
const ui = $state({ openChat: null });
export function openChatId() {
    return ui.openChat;
}
export function setOpenChat(id) {
    ui.openChat = id || null;
}

// Pending community invites are spliced into the list above the chats; they have
// their own signal because they are not chats and never touch a chat's row.
const invites = $state({ seq: 0 });
export function invitesVersion() {
    return invites.seq;
}
export function touchInvites() {
    invites.seq++;
}

// The list pane's mode (widescreen): inside a community the list IS that
// community's channel list; outside it, optionally DMs only.
const pane = $state({ communityId: null, dmsOnly: false });
export function paneState() {
    return pane;
}
export function setPane(communityId, dmsOnly) {
    pane.communityId = communityId || null;
    pane.dmsOnly = !!dmsOnly;
}

// The coarse clock for relative timestamps ("5m ago") and presence-dot recency, which drift
// with wall time instead of data. Rows re-derive their strings on tick; only changed strings
// patch the DOM.
const clock = $state({ tick: 0 });
export function clockTick() { return clock.tick; }
export function bumpClockTick() { clock.tick++; }

// Whether the chat list rendered any row: the pane's bottom fadeout softens a scrolling
// list, and over the empty state it would just wash out the intro.
const chatlist = $state({ hasRows: false });
export function listHasRows() { return chatlist.hasRows; }
export function setListHasRows(on) { chatlist.hasRows = !!on; }

// The expanded profile view: which npub it shows, and whether our own profile is
// in Edit Mode (the reconciler leaves the screen alone while it is).
const profileView = $state({ id: '', editing: false });
export function profileViewState() { return profileView; }
export function setOpenProfile(id) { profileView.id = id || ''; }
export function setProfileEditing(on) { profileView.editing = !!on; }
