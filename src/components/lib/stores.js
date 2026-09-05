/**
 * Shared store layer (SVELTE_MIGRATION_PLAN.md).
 *
 * Per-entity invalidation lives in signals.svelte.js. What remains here is the one
 * genuinely global input: the clock. State stays RAW at this boundary on purpose —
 * chat objects are shared by reference with eventCache and the legacy array
 * globals, so nothing may deep-proxy them ($state.raw discipline — see plan §5).
 */
import { writable } from 'svelte/store';

/**
 * Coarse clock tick for relative timestamps ("5m ago") and presence-dot recency, which
 * drift with wall time instead of data. Rows re-derive their strings on tick; only
 * changed strings patch the DOM.
 */
export const timeTickVersion = writable(0);

export function bumpTimeTick() {
    timeTickVersion.update((n) => n + 1);
}
