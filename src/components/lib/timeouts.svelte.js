// Each community's running Timeouts (CORD-04 §8), as the backend folds them: npub →
// { from, until } in unix seconds. One reactive entry per community, replaced whole when it
// changes, so every name showing a Timeout re-derives together.

import { SvelteMap } from 'svelte/reactivity';

const byCommunity = new SvelteMap();

/** A community's running Timeouts, or null before they're read. */
export function communityTimeouts(communityId) {
    return (communityId && byCommunity.get(communityId)) || null;
}

/** When `npub`'s Timeout in a community ends (unix seconds), 0 when they have none. */
export function timedOutUntil(communityId, npub) {
    const until = communityTimeouts(communityId)?.[npub]?.until || 0;
    return until * 1000 > Date.now() ? until : 0;
}

/** Hold a community's running Timeouts. Returns whether anything changed. */
export function setCommunityTimeouts(communityId, running) {
    const next = running || {};
    if (JSON.stringify(byCommunity.get(communityId) || null) === JSON.stringify(next)) return false;
    byCommunity.set(communityId, next);
    return true;
}
