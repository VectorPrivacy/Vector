/**
 * Phase 0 shared store layer (SVELTE_MIGRATION_PLAN.md).
 *
 * Mutations converge here instead of scattering manual render calls. The vanilla
 * side bumps a version store through the VectorSvelte global; islands (and, during
 * migration, legacy render functions subscribed here) re-derive from it.
 *
 * State stays RAW at this boundary on purpose: chat objects are shared by reference
 * with eventCache and the legacy array globals, so nothing may deep-proxy them
 * ($state.raw discipline — see plan §5).
 */
import { writable } from 'svelte/store';

/**
 * Chat-list invalidation. Bumped by every path that used to call renderChatlist()
 * directly. Subscribers re-derive; the legacy state-hash gate on the subscriber
 * side keeps no-op bumps cheap until the island replaces it with fine-grained rows.
 */
export const chatlistVersion = writable(0);

/** Bump the chat list. Call after any mutation to chats, invites, pins or pane identity. */
export function invalidateChatlist() {
    chatlistVersion.update((n) => n + 1);
}

/**
 * Coarse clock tick for relative timestamps ("5m ago") and presence-dot recency, which
 * drift with wall time instead of data. Rows re-derive their strings on tick; only
 * changed strings patch the DOM.
 */
export const timeTickVersion = writable(0);

export function bumpTimeTick() {
    timeTickVersion.update((n) => n + 1);
}
