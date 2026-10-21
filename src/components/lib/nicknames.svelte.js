// What members are called inside each community (CORD-04 §10), as the backend folds it.
// One reactive entry per community, replaced whole when it changes, so everything showing
// that community's names re-derives together.

import { SvelteMap } from 'svelte/reactivity';

const byCommunity = new SvelteMap();

/** A member's nickname in a community, '' when they have none there. */
export function communityNickname(communityId, npub) {
    return (communityId && byCommunity.get(communityId)?.get(npub)) || '';
}

/** Hold a community's nicknames (npub → nickname). Returns whether anything changed. */
export function setCommunityNicknames(communityId, named) {
    const next = new Map(Object.entries(named || {}));
    const prev = byCommunity.get(communityId);
    if (prev && prev.size === next.size && [...next].every(([k, v]) => prev.get(k) === v)) return false;
    byCommunity.set(communityId, next);
    return true;
}
