// Badge drops: a badge offered for a window of time, claimed from its card right after login.
// A simulation for now: the claim lands on the profile at once and waits as pending; the durable
// badge event that confirms it replaces submitDropClaim() when the backend has one.

/** V for Vector, 2026 edition: 24 hours from v0.5.0's release on the 5th of November. */
const BADGE_DROPS = [{
    id: 'vector-2026',
    opensAt: Date.UTC(2026, 10, 5),            // midnight UTC until the release's own hour is set here
    hours: 24,
    minVersion: [0, 5, 0],
    card: {
        title: 'V for Vector Badge',
        subtitle: '2026 Edition',
        html: '<span class="badge-card-line">Remember, Remember... the&nbsp;Fifth&nbsp;of&nbsp;November.</span>“People should not be afraid of their governments. Governments should be afraid of their people.”',
        svg: 'vforvector2026.svg',
        access: 'Partial Premium Access',
    },
}];

/** A claim, per drop and account: `{ at, synced }`, pending until the badge event is sent. */
const dropKey = (id) => `badge_drop:${id}:${strPubkey}`;

function readDropClaim(id) {
    try { return JSON.parse(localStorage.getItem(dropKey(id)) || 'null'); } catch { return null; }
}

function writeDropClaim(id, claim) {
    try {
        if (claim) localStorage.setItem(dropKey(id), JSON.stringify(claim));
        else localStorage.removeItem(dropKey(id));
    } catch { /* the claim still shows this session */ }
}

/** On Vector's clock, not the device's: a wound-back clock can't reopen a drop. */
const dropOpen = (drop) => {
    const now = VectorSvelte.now();
    return now >= drop.opensAt && now < drop.opensAt + drop.hours * 3600000;
};

/** The release the drop asks for or later: a preview of it ("0.5.0-5") comes before it. */
async function meetsDropVersion(min) {
    let version = '';
    try { version = await window.__TAURI__.app.getVersion(); } catch { return false; }
    const m = /^(\d+)\.(\d+)\.(\d+)(-\d+)?$/.exec(version);
    if (!m || m[4]) return false;
    const got = [+m[1], +m[2], +m[3]];
    for (let i = 0; i < 3; i++) if (got[i] !== min[i]) return got[i] > min[i];
    return true;
}

/** After login: this account's claims onto its profile, any still pending sent again, then the
 *  first drop that is open, unclaimed and meant for this version, offered on its card. */
async function offerBadgeDrops() {
    for (const drop of BADGE_DROPS) {
        const claim = readDropClaim(drop.id);
        VectorSvelte.setDropClaimed(drop.id, !!claim);
        if (claim && !claim.synced) submitDropClaim(drop);
    }
    const drop = BADGE_DROPS.find((d) => !readDropClaim(d.id) && dropOpen(d));
    if (drop && await meetsDropVersion(drop.minVersion)) showDropOffer(drop);
}

function showDropOffer(drop) {
    // Locked: the offer is always taken; Claim is the only way out of it.
    showBadgeCard({ ...drop.card, locked: true, action: { label: 'Claim', run: () => claimDrop(drop) } });
}

/** Optimistic: the badge is on the profile the moment it's claimed, and the profile opens on it. */
function claimDrop(drop) {
    writeDropClaim(drop.id, { at: VectorSvelte.now(), synced: false });
    VectorSvelte.setDropClaimed(drop.id, true);
    invoke('play_claim_sound').catch(() => {});
    hideBadgeCard();
    submitDropClaim(drop);
    // The card fades out before the profile takes the screen.
    setTimeout(() => {
        openProfile();
        setTimeout(() => VectorSvelte.celebrateDrop(drop.id), 250);
    }, 220);
}

/** Stand-in for the durable badge event: a moment's "network", then the claim is marked sent.
 *  Anything that stops it leaves the claim pending, and the next login sends it again. */
async function submitDropClaim(drop) {
    const npub = strPubkey;
    try {
        await new Promise((resolve) => setTimeout(resolve, 1200));
        if (npub !== strPubkey) return;
        const claim = readDropClaim(drop.id);
        if (claim && !claim.synced) writeDropClaim(drop.id, { ...claim, synced: true });
    } catch (e) {
        console.warn('[Badge drop] claim not sent yet:', e);
    }
}

/** A claimed drop's card, from the profile. */
function showDropBadgeCard(id) {
    const drop = BADGE_DROPS.find((d) => d.id === id);
    if (drop) showBadgeCard(drop.card);
}

// Development: offer a drop outside its window, or forget this account's claim of it.
function previewBadgeDrop(id = BADGE_DROPS[0].id) {
    const drop = BADGE_DROPS.find((d) => d.id === id);
    if (drop) showDropOffer(drop);
}
function resetBadgeDrop(id = BADGE_DROPS[0].id) {
    writeDropClaim(id, null);
    VectorSvelte.setDropClaimed(id, false);
}
