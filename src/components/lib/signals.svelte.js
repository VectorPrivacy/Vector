// Per-key invalidation signals: the fine-grained half of the store layer.
//
// Page state stays RAW (chat and profile objects are shared by reference with
// eventCache and the legacy arrays, mutated in place), so reactivity cannot come
// from proxies. Instead every entity has a version signal: a row's view-model reads
// its own chat's version, and the vanilla side bumps exactly that one when it
// mutates the chat. One message arriving re-derives one row; the list's order and
// membership have a signal of their own, so a reorder re-diffs the keyed each
// without touching any row's derivation.
//
// Signals are created on first read (a plain Map write, allowed inside a derived).

const chats = new Map();
const profiles = new Map();
const communities = new Map();

function sig(map, key) {
    let s = map.get(key);
    if (!s) {
        const fresh = $state({ v: 0 });
        map.set(key, fresh);
        s = fresh;
    }
    return s;
}

/** Read: a chat's version (the row re-derives when it changes). */
export function chatVersion(id) {
    return sig(chats, id).v;
}
/** Write: this chat changed in place (message, unread, typing, name, mute). */
export function touchChat(id) {
    sig(chats, id).v++;
}

/** Read/write for a profile: DM rows show its name and avatar. */
export function profileVersion(npub) {
    return sig(profiles, npub).v;
}
export function touchProfile(npub) {
    sig(profiles, npub).v++;
}

/** Read/write for a community: its single row aggregates every channel chat. */
export function communityVersion(id) {
    return sig(communities, id).v;
}
export function touchCommunity(id) {
    sig(communities, id).v++;
}

// The list's own shape: order and membership. Bumped after a sort, never for a
// change inside a row.
const list = $state({ v: 0 });
export function listVersion() {
    return list.v;
}
export function reorderChatlist() {
    list.v++;
}
