// Entry for the Svelte island bundle. esbuild compiles this (+ every .svelte it pulls in)
// into src/components.bundle.js as an IIFE exposing the `VectorSvelte` global, so the vanilla
// one-global-scope frontend can call VectorSvelte.mountX(target, props) directly.
import { mount, unmount } from 'svelte';

import ContactList from './ContactList.svelte';

/**
 * Mount the community-invite contact picker into `target`. The component owns its
 * dialog-local state ($state runes); the vanilla side drives it through the methods
 * the returned instance exports (setFilter / addStranger / select / setProfiles /
 * reset / getSelection) and receives selection changes through the `onSelectionChange`
 * callback prop. No bridge stores — the chatlist island's props-in/events-out
 * convention (SVELTE_MIGRATION_PLAN.md §3.4). Pass the return value to
 * `unmountComponent(instance)` on dialog close.
 */
export function mountContactList(target, props = {}) {
    target.replaceChildren();
    return mount(ContactList, { target, props });
}

/** Tear down a mounted island (call on dialog close / element removal). */
export function unmountComponent(instance) {
    return unmount(instance);
}

// Shared store layer (Phase 0 of the Svelte migration — see SVELTE_MIGRATION_PLAN.md).
export { chatlistVersion, invalidateChatlist, timeTickVersion, bumpTimeTick } from './stores.js';

import Chatlist from './Chatlist.svelte';

/**
 * Mount the chat-list island into `target` (#chat-list). The vanilla side supplies
 * every helper it still owns (list.js, row.js, channels.js, main.js globals) plus a
 * snapshot() of the raw state arrays — the bundle is an IIFE and cannot see the page's
 * global lexical bindings. The island owns #chat-list's children exclusively; its keyed
 * {#each} reuses row nodes so single-chat changes patch single rows.
 */
export function mountChatlist(target, { h, snapshot }) {
    target.replaceChildren();
    return mount(Chatlist, { target, props: { h, snapshot } });
}
