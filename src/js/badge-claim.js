// Badge drops: a badge offered for a window of time, claimed from its card right after login.
// The claim is optimistic: the badge lands on the profile at once and is held as pending here
// until the backend's claim event is accepted, then the backend holds it. The window and its
// test mode come from the backend (`get_my_badges`), which also refuses a claim outside it.

/** V for Vector, 2026 edition: 24 hours from v0.5.0's release on the 5th of November. */
const BADGE_DROPS = [{
    id: 'vector-2026',
    minVersion: [0, 5, 0],
    /** This drop as our own badges see it: its window, and whether this account holds it. */
    state: (badges) => ({ win: badges?.drop_2026 || null, held: !!badges?.vector_2026 }),
    claim: () => invoke('claim_vector_2026_badge'),
    /** A claim this account published elsewhere; found, the backend holds it from now on. */
    claimedElsewhere: () => invoke('check_vector_2026_badge', { npub: strPubkey }),
    card: {
        title: 'V for Vector Badge',
        subtitle: '2026 Edition',
        html: '<span class="badge-card-line">Remember, Remember... the&nbsp;Fifth&nbsp;of&nbsp;November.</span>“People should not be afraid of their governments. Governments should be afraid of their people.”',
        svg: 'vforvector2026.svg',
        access: 'Partial Premium Access',
    },
}];

/** A pending claim, per drop, window and account: a test window's claim never stands for the
 *  launch one. Cleared once the backend holds the badge. */
const dropKey = (drop, win) => `badge_drop:${drop.id}:${win?.opens_at || 0}:${strPubkey}`;

function readDropClaim(drop, win) {
    try { return JSON.parse(localStorage.getItem(dropKey(drop, win)) || 'null'); } catch { return null; }
}

function writeDropClaim(drop, win, claim) {
    try {
        if (claim) localStorage.setItem(dropKey(drop, win), JSON.stringify(claim));
        else localStorage.removeItem(dropKey(drop, win));
    } catch { /* the claim still shows this session */ }
}

/** On Vector's clock, not the device's: a wound-back clock can't reopen a drop. */
const dropOpen = (win) => {
    const now = VectorSvelte.now();
    return !!win && now >= win.opens_at && now < win.closes_at;
};

/** The release the drop asks for or later: a preview of it ("0.5.0-5") comes before it. A test
 *  window takes any build, so the claim can be tried before the release ships. */
async function meetsDropVersion(min, win) {
    if (win?.test) return true;
    let version = '';
    try { version = await window.__TAURI__.app.getVersion(); } catch { return false; }
    const m = /^(\d+)\.(\d+)\.(\d+)(-\d+)?$/.exec(version);
    if (!m || m[4]) return false;
    const got = [+m[1], +m[2], +m[3]];
    for (let i = 0; i < 3; i++) if (got[i] !== min[i]) return got[i] > min[i];
    return true;
}

/** Each drop's badge on the profile: held by the backend, or claimed and still pending. */
function syncDropBadges(badges) {
    for (const drop of BADGE_DROPS) {
        const { win, held } = drop.state(badges);
        if (held) writeDropClaim(drop, win, null);
        VectorSvelte.setDropClaimed(drop.id, held || !!readDropClaim(drop, win));
    }
}

/** After login: this account's badges onto its profile, a pending claim sent again, then the
 *  first drop that is open, unclaimed and meant for this build, offered on its card. */
async function offerBadgeDrops() {
    let badges = null;
    try { badges = await invoke('get_my_badges'); } catch { return; }
    _myBadges = badges;
    syncDropBadges(badges);
    for (const drop of BADGE_DROPS) {
        const { win, held } = drop.state(badges);
        const pending = readDropClaim(drop, win);
        if (pending && !held) submitDropClaim(drop, win);
        if (!held && !pending && dropOpen(win) && await meetsDropVersion(drop.minVersion, win)) {
            // Another device or a reinstall may already hold it: only this device's cache is empty.
            const npub = strPubkey;
            if (await drop.claimedElsewhere().catch(() => false)) {
                if (npub === strPubkey) VectorSvelte.setDropClaimed(drop.id, true);
                continue;
            }
            if (npub !== strPubkey) return;
            showDropOffer(drop, win);
            return;
        }
    }
}

function showDropOffer(drop, win) {
    // Locked: the offer is always taken; Claim is the only way out of it.
    showBadgeCard({ ...drop.card, locked: true, action: { label: 'Claim', run: () => claimDrop(drop, win) } });
}

/** Optimistic: the badge is on the profile the moment it's claimed, and the profile opens on it. */
function claimDrop(drop, win) {
    writeDropClaim(drop, win, { at: VectorSvelte.now() });
    VectorSvelte.setDropClaimed(drop.id, true);
    invoke('play_claim_sound').catch(() => {});
    hideBadgeCard();
    submitDropClaim(drop, win);
    // The card fades out before the profile takes the screen.
    setTimeout(() => {
        openProfile();
        setTimeout(() => VectorSvelte.celebrateDrop(drop.id), 250);
    }, 220);
}

/** Send the claim. Accepted, the backend holds the badge (badges_updated says so). A closed
 *  window takes the badge back; anything else leaves it pending for the next login. */
async function submitDropClaim(drop, win) {
    const npub = strPubkey;
    try {
        await drop.claim();
        if (npub === strPubkey) writeDropClaim(drop, win, null);
    } catch (e) {
        if (npub !== strPubkey) return;
        if (/^closed/.test(String(e))) {
            writeDropClaim(drop, win, null);
            VectorSvelte.setDropClaimed(drop.id, false);
        }
        console.warn('[Badge drop] claim not sent yet:', e);
    }
}

/** A claimed drop's card, from the profile. */
function showDropBadgeCard(id) {
    const drop = BADGE_DROPS.find((d) => d.id === id);
    if (drop) showBadgeCard(drop.card);
}

// Others' V for Vector 2026, looked up once a session.
const _v2026BadgeCache = new Map();

/** Whether someone else holds V for Vector 2026. Our own reads the profile's drop state. */
async function resolveV2026Badge(npub) {
    if (_v2026BadgeCache.has(npub)) return _v2026BadgeCache.get(npub);
    try {
        const has = await invoke('check_vector_2026_badge', { npub });
        _v2026BadgeCache.set(npub, has);
        return has;
    } catch { return false; }
}

// Development: offer a drop outside its window, or forget this account's pending claim of it.
async function previewBadgeDrop(id = BADGE_DROPS[0].id) {
    const drop = BADGE_DROPS.find((d) => d.id === id);
    if (!drop) return;
    const badges = await invoke('get_my_badges').catch(() => null);
    showDropOffer(drop, drop.state(badges).win);
}
async function resetBadgeDrop(id = BADGE_DROPS[0].id) {
    const drop = BADGE_DROPS.find((d) => d.id === id);
    if (!drop) return;
    const badges = await invoke('get_my_badges').catch(() => null);
    writeDropClaim(drop, drop.state(badges).win, null);
    syncDropBadges(badges);
}
