// Each community's running Timeouts (CORD-04 §8), as the backend folds them: npub →
// { from, until } in unix seconds. One reactive entry per community, replaced whole when it
// changes, so every name showing a Timeout re-derives together.

import { SvelteMap } from 'svelte/reactivity';
import { now as clockNow } from './clock.svelte.js';

const byCommunity = new SvelteMap();

/** A community's running Timeouts, or null before they're read. */
export function communityTimeouts(communityId) {
    return (communityId && byCommunity.get(communityId)) || null;
}

/** When `npub`'s Timeout in a community ends (unix seconds), 0 when they have none. */
export function timedOutUntil(communityId, npub) {
    const until = communityTimeouts(communityId)?.[npub]?.until || 0;
    return until * 1000 > clockNow() ? until : 0;
}

/** Hold a community's running Timeouts. Returns whether anything changed. */
export function setCommunityTimeouts(communityId, running) {
    const next = running || {};
    // A first read that finds none changes nothing already painted.
    const changed = JSON.stringify(byCommunity.get(communityId) || {}) !== JSON.stringify(next);
    if (changed || !byCommunity.has(communityId)) byCommunity.set(communityId, next);
    return changed;
}
