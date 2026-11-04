// The holographic badge card: what it shows and the tilt the pointer or gyro drives.
// badge-card.js owns the input math and writes `tilt`; the card paints it as CSS vars.
const b = $state({
    open: false, visible: false,
    // `action`: a button's label under the card, when it offers one (a drop's Claim).
    badge: { src: '', title: '', subtitle: '', html: '', tiers: null, access: '', action: '' },
    tilt: { rx: '0deg', ry: '0deg', mx: '50%', my: '50%', holo: '0', idle: true },
});
const els = { card: null };
let handlers = $state.raw({});   // close, pointerMove(e), pointerEnd()
export function badgeCardState() { return b; }
export function badgeCardEls() { return els; }
export function badgeCardHandlers() { return handlers; }
export function setBadgeCardHandlers(h) { handlers = h || {}; }
export function setBadgeCard(patch) { Object.assign(b, patch); }
export function setBadgeTiltVars(tilt) { b.tilt = tilt; }

// Badge drops (js/badge-claim.js): what this account has claimed, shown before the durable event
// confirms it, and a tick the profile replays a badge's arrival on.
const drops = $state({ claimed: {}, celebrate: { id: '', tick: 0 } });
export function badgeDrops() { return drops; }
export function setDropClaimed(id, on) { drops.claimed = { ...drops.claimed, [id]: !!on }; }
export function celebrateDrop(id) { drops.celebrate = { id, tick: drops.celebrate.tick + 1 }; }
