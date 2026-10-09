// The warning before a message carrying a secret goes out: what it is (`kind`: nostr_key,
// seed_phrase, wallet_key), where it would land, and whether a Self-Destruct Timer is on,
// on offer, or not available there ('on' | 'offer' | '').
import { popOverlay } from './dialog-lifecycle.svelte.js';

export const secretGuard = popOverlay({ kind: 'nostr_key', community: false, where: '', timer: '' });
