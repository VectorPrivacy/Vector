// The mini profile popup: who it shows, where it sits, and whether a fetch for an
// identity relays have nothing for has been given up on (then it reads "Anon").
// Opened from inside a community, it also carries that community's roles for them.
const closed = () => ({ npub: null, anchor: null, reuse: null, settled: false, communityId: null, roles: null, roleBusy: null });
let mini = $state.raw(closed());

export function miniProfile() { return mini; }
/**
 * Open for `npub`. `anchor` positions it (null = centred); `reuse` = { left, top } | { centered }
 * keeps a replaced popup's spot; `communityId` is the community it was opened within, if any.
 */
export function openMiniProfile(npub, anchor, reuse, communityId = null) {
    mini = { ...closed(), npub, anchor: anchor || null, reuse: reuse || null, communityId: communityId || null };
}
export function settleMiniProfile(npub) {
    if (mini.npub === npub) mini = { ...mini, settled: true };
}
export function closeMiniProfile() {
    if (mini.npub) mini = closed();
}
/** `roles`: { owner, roles: [{ id, name, tint, channel, removable }], addable } | null. */
export function setMiniProfileRoles(npub, roles) {
    if (mini.npub === npub) mini = { ...mini, roles };
}
export function setMiniProfileRoleBusy(id) {
    if (mini.npub) mini = { ...mini, roleBusy: id };
}

// The popup element while open, for the dismiss paths' hit tests.
const els = { popup: null };
export function miniProfileEls() { return els; }
export function bindMiniProfileEl(node) { els.popup = node; return { destroy() { els.popup = null; } }; }
