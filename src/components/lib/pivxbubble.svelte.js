// A PIVX payment bubble's claim state per gift code: the balance check and a claim
// write here and every bubble for that code re-derives. Keyed by gift code, not
// message, so a re-render or a reopened chat keeps what the check already learned.
import { SvelteMap } from 'svelte/reactivity';

const map = new SvelteMap();   // giftCode → { phase, hint }

/** phase: 'syncing' | 'claimable' | 'claiming' | 'claimed' | 'failed' */
export function pivxBubble(giftCode) { return map.get(giftCode) ?? null; }
export function setPivxBubble(giftCode, patch) {
    map.set(giftCode, { ...(map.get(giftCode) || { phase: 'claimable', hint: '' }), ...patch });
}
