// The pack creator's view: name, logo, the emoji cells, and per-cell transient state
// (busy ring during a save, broken once a probe or the cache says the file is gone).
const c = $state({ name: '', logo: { blobUrl: '', url: '', dead: false }, max: 30, busy: {}, broken: {}, saving: false, focusTick: 0 });
let emojis = $state.raw([]);   // [{ shortcode, url, blobUrl, dead }]

export function creatorState() { return c; }
export function creatorEmojis() { return emojis; }
export function setCreator({ name, logo, emojis: list, max }) {
    c.name = name || '';
    c.logo = logo;
    if (max) c.max = max;
    emojis = list;
    c.busy = {};
    c.broken = {};
}
export function setCreatorBusy(idx, state) {
    if (state) c.busy[idx] = state;
    else delete c.busy[idx];
}
export function clearCreatorBusy() { c.busy = {}; }
export function markCreatorBroken(idx, message) { c.broken[idx] = message; }
// A publish in flight: the head's controls lock and the pencil spins.
export function setCreatorSaving(on) { c.saving = !!on; }
// The name field takes focus (desktop only; the app decides).
export function focusCreatorName() { c.focusTick++; }
