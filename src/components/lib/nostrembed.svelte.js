// Nostr embeds: the expanded post or article (with the trail of quotes opened from it), and
// the download progress of embedded videos.
import { SvelteMap } from 'svelte/reactivity';

let view = $state.raw(null);   // null (closed) | { embed, url, entity, origin: { chatId, msgId }|null }
let trail = $state.raw([]);    // the views a quote was opened from, newest last
const progress = new SvelteMap();   // video url -> percent, -1 while the size is unknown

export function nostrEmbedModal() { return view; }
export function nostrEmbedCanGoBack() { return trail.length > 0; }
export function openNostrEmbedModal(v) { trail = []; view = v; }
/** Open `v` over the current view, which Back returns to. Opens fresh when closed. */
export function pushNostrEmbedModal(v) {
    if (view) {
        trail = [...trail, view];
        // A quote is still in the message the modal was opened from.
        if (v.origin === undefined) v = { ...v, origin: view.origin };
    }
    view = v;
}
/** Step back to the view a quote was opened from; false when there is none. */
export function popNostrEmbedModal() {
    if (!trail.length) return false;
    view = trail[trail.length - 1];
    trail = trail.slice(0, -1);
    return true;
}
export function closeNostrEmbedModal() { trail = []; view = null; }

export function embedVideoProgress(url) { return progress.get(url) ?? null; }
export function embedVideoProgressed(url, pct) {
    if (pct >= 100) progress.delete(url);
    else progress.set(url, pct);
}
