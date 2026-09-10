// Community Moderation console (Concord v2).
//
// A sybil raid arrives as one message each from a hundred fresh npubs, so the
// per-member affordances in the roster — right-click, kick, ban — lose the race by
// design. This panel works at the scale of the wave instead: the backend scores every
// member against cohort evidence, this ticks the survivors, and one action either
// publishes a single banlist edition or rotates the community's keys around them.
//
// Ticked = KEPT. The unticked set is what a rotation cuts, which is why the default
// selection is the thing worth getting right: protected and trusted members start
// ticked, convicted ones start clear.

// The console's state lives in lib/moderation.svelte.js; this side fetches and publishes.
const modCommunityId = () => VectorSvelte.modState().communityId;
const modIntel = () => VectorSvelte.modIntel();
const modKeep = () => VectorSvelte.modKeep();

VectorSvelte.setScreen('modConsole', {
    h: {
        ago: modAgo,
        displayName: modDisplayName,
        avatarSrc: (npub) => { const p = arrProfiles.find(x => x.id === npub); return p ? getProfileAvatarSrc(p) : null; },
        showTab: (which) => modShowTab(which),
        close: () => closeModerationPanel(),
        revoke: () => modRevokeInvites(),
        rotate: () => modRotate(),
        banRotate: () => modBanRotate(),
    },
});

// Two groups that always sum to Everyone, named after what happens to them rather than
// after the machinery. There is deliberately no "Suspects" filter: the verdict starts
// equal to the selection, so it was two chips showing one number.

/** Relative time that stays readable at raid speed (seconds matter here). */
function modAgo(secs) {
    if (!secs || secs < 0) return 'unknown';
    if (secs < 60) return `${Math.floor(secs)}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m`;
    if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
    if (secs < 86400 * 365) return `${Math.floor(secs / 86400)}d`;
    return `${(secs / (86400 * 365)).toFixed(1)}y`;
}

function modShortNpub(npub) {
    return npub.substring(0, 10) + '…' + npub.substring(npub.length - 4);
}

function modDisplayName(npub) {
    const p = arrProfiles.find(p => p.id === npub);
    const name = p ? (p.nickname || p.name || p.display_name || '') : '';
    return name || modShortNpub(npub);
}

/**
 * Open the console for a community. Read-only until an action is pressed, so it is
 * always safe to look.
 */
async function openModerationPanel(communityId) {
    if (!communityId) return;
    VectorSvelte.modOpen(communityId);
    VectorSvelte.polResetChannels();
    VectorSvelte.modOverlay.open({});
    pushBack('mod-overlay', closeModerationPanel);
    await modFetch(communityId);
}

/** Fetch the intel; protected and trusted start ticked, convicted start clear. */
async function modFetch(communityId) {
    try {
        const intel = await invoke('get_moderation_intel', { communityId });
        // A swap or a close while the read was in flight: don't paint over what is shown now.
        if (modCommunityId() !== communityId) return;
        // The designer's per-channel exemptions read from the same payload.
        VectorSvelte.polSetChannels((intel.channels || []).map(c => ({ id: c.id, name: c.name || 'channel' })));
        VectorSvelte.modSetIntel(intel, new Set(intel.report.members.filter(m => m.verdict !== 'suspect').map(m => m.npub)));
    } catch (err) {
        if (modCommunityId() !== communityId) return;
        VectorSvelte.modSetError(err);
    }
}

/// The two faces of the console: who is here, and what the rules are.
function modShowTab(which) {
    VectorSvelte.modSetTab(which);
    const communityId = modCommunityId();
    if (which === 'policies' && communityId && window.openPolicyDesigner) window.openPolicyDesigner(communityId);
}

/// The console is opened DURING a raid, and its intel was a one-shot read: the
/// panel showed the roster from the moment it opened, so accounts that walked in
/// seconds later were invisible in the one tool meant to remove them.
///
/// Driven by arrivals, not a clock. Re-reading on a timer would re-run
/// `policy_console_report` — which judges every member and clusters the message
/// corpus — on a quiet community forever; a raid announces itself, so refresh
/// when it does. Debounced, because a wave lands as a burst and each message
/// would otherwise ask for its own report.
let modRefreshPending = null;
let modRefreshedAt = 0;

/// Coalesce a burst, then leave a floor between reports. Sustained spam is the
/// case that has to stay cheap: `policy_console_report` judges every member and
/// clusters the corpus, so a flood must not be able to ask for one per message.
const MOD_REFRESH_COALESCE_MS = 1500;
const MOD_REFRESH_FLOOR_MS = 6000;

function modStopRefresh() {
    if (modRefreshPending) {
        clearTimeout(modRefreshPending);
        modRefreshPending = null;
    }
}

/// A message landed in a community. Refresh the console if it is the one open.
///
/// A THROTTLE, not a debounce: an arrival while one is already scheduled is
/// dropped rather than pushing the deadline out. Resetting per message is what
/// would let an unrelenting flood — exactly when the console is needed — starve
/// the refresh forever.
function modNoteActivity(communityId) {
    if (!communityId || modCommunityId() !== communityId) return;
    if (modRefreshPending) return; // already coalescing this burst
    const since = Date.now() - modRefreshedAt;
    const wait = Math.max(MOD_REFRESH_COALESCE_MS, MOD_REFRESH_FLOOR_MS - since);
    modRefreshPending = setTimeout(() => {
        modRefreshPending = null;
        modRefreshedAt = Date.now();
        modRefresh(communityId);
    }, wait);
}

/// A rotation or control change landed — the roster it implies is authoritative.
///
/// Separate from `modNoteActivity` on purpose: message bursts are throttled because
/// judging a corpus is expensive and arrives constantly, whereas an epoch moves
/// rarely and changes WHO IS IN THE ROOM. A kicked or banned member lingering in the
/// selector is worse than a slow refresh — an operator would tick a row that no
/// longer exists and act on a roster the network has already replaced.
function modNoteControlChange(communityId) {
    if (!communityId || modCommunityId() !== communityId) return;
    modStopRefresh();
    modRefresh(communityId);
}

async function modRefresh(communityId) {
    const print = (intel) => JSON.stringify((intel?.report?.members || []).map(m => [m.npub, m.verdict]).sort());
    // A close, a swap, or a rotation in flight: nothing to repaint onto.
    if (modCommunityId() !== communityId || VectorSvelte.modState().busy) return;
    let intel;
    try {
        intel = await invoke('get_moderation_intel', { communityId });
    } catch (_) {
        return; // a blip must not blank the panel an operator is working in
    }
    if (modCommunityId() !== communityId || VectorSvelte.modState().busy) return;
    const prev = modIntel();
    if (print(intel) === print(prev)) return;
    // NEW members default to their verdict; everyone the operator has already decided on
    // keeps the tick they were given. A refresh must never move a tick mid-triage.
    const decided = modKeep();
    const seen = new Set((prev?.report?.members || []).map(m => m.npub));
    VectorSvelte.modSetIntel(intel, new Set(
        intel.report.members
            .filter(m => (seen.has(m.npub) ? decided.has(m.npub) : m.verdict !== 'suspect'))
            .map(m => m.npub)
    ));
}

function closeModerationPanel() {
    if (VectorSvelte.modOverlay.closing()) return;
    if (VectorSvelte.modState().busy) return;
    popBack('mod-overlay');
    modStopRefresh();
    VectorSvelte.modOpen(null);
    VectorSvelte.modOverlay.close();
}

/** Paint the header, the raid banner and the list from a freshly-read snapshot. */
function modCutList() {
    const keep = modKeep();
    return modIntel().report.members.filter(m => !keep.has(m.npub)).map(m => m.npub);
}

/** Lock the console for the duration of a publish; these take seconds, not frames. */
function modSetBusy(busy, label) {
    VectorSvelte.modSetBusy(busy, label, busy ? ' Publishing. Leave this open until it finishes.' : '');
}

async function modReload() {
    const communityId = modCommunityId();
    if (!communityId) return;
    // The header pip and the menu entry both cache a verdict; an action just invalidated it.
    clearCommunityRaidAlert(communityId);
    await modFetch(communityId);
}

// The purge publishes one directive per member, so it runs for minutes on a big wave.
// Without a counter the panel looks hung and a moderator kills it half-done.
window.__TAURI__.event.listen('community_purge_progress', (e) => {
    const p = e.payload;
    if (!VectorSvelte.modState().busy || p.community_id !== modCommunityId()) return;
    const pct = p.total ? Math.round((p.done / p.total) * 100) : 0;
    VectorSvelte.modSetProgress(`Removing ${p.done}/${p.total}`, ` ${pct}% \u2014 leave this open until it finishes.`);
});

async function modRevokeInvites() {
    const n = modIntel().invites.length;
    const ok = await popupConfirm(
        'Revoke every invite link',
        `Retire all ${n} public invite link${n === 1 ? '' : 's'}? Anyone holding one can no longer join. Existing members are unaffected.`,
        false, '', 'vector_warning.svg');
    if (!ok) return;
    modSetBusy(true, 'Revoking invite links…');
    try {
        const res = await invoke('revoke_all_public_invites', { communityId: modCommunityId() });
        showToast(res.failed ? `Revoked ${res.revoked}, ${res.failed} failed` : `Revoked ${res.revoked} invite link${res.revoked === 1 ? '' : 's'}`);
        modSetBusy(false);
        await modReload();
    } catch (err) {
        modSetBusy(false);
        await popupConfirm("Couldn't revoke", escapeHtml(String(err)), true, '', 'vector_warning.svg');
    }
}

async function modRotate() {
    const cut = modCutList();
    // A rotation with links still live buys minutes: the same holder walks back in.
    const live = modIntel().invites.length;
    const linkWarning = live
        ? `<br><br><b>${live} invite link${live === 1 ? ' is' : 's are'} still live.</b> Anyone holding one can rejoin straight after this. Revoke them first.`
        : '';
    const ok = await popupConfirm(
        cut.length ? 'Remove members and rotate' : 'Rotate keys',
        (cut.length
            ? `Remove <b>${cut.length}</b> member${cut.length === 1 ? '' : 's'} from the community, then mint a new epoch only the ${modKeep().size} remaining can follow. They are dropped from everyone's member list and lose access, without being banned.<br><br>This publishes one removal per member, so ${cut.length} will take a while.`
            : 'Mint a new epoch for everyone currently in the community. Use this when an invite link leaked but the members are all real.') + linkWarning,
        false, '', 'vector_warning.svg');
    if (!ok) return;
    modSetBusy(true, cut.length ? `Removing 0/${cut.length}` : 'Rotating keys…');
    try {
        // An empty retain rotates without removing anyone; a non-empty one is the keep-list.
        const retain = cut.length ? [...modKeep()] : [];
        const res = await invoke('refound_community', { communityId: modCommunityId(), retain });
        const refused = res?.refused ? ` ${res.refused} refused.` : '';
        showToast(cut.length ? `Removed ${res?.kicked ?? cut.length} and rotated.${refused}` : 'Keys rotated.');
        modSetBusy(false);
        await modReload();
        // The header count, chat-header subtext and roster all cache the member set.
        refreshCommunityMemberCount(modCommunityId(), true);
    } catch (err) {
        modSetBusy(false);
        await popupConfirm("Couldn't complete the removal", escapeHtml(String(err)), true, '', 'vector_warning.svg');
    }
}

async function modBanRotate() {
    const cut = modCutList();
    if (!cut.length) return;
    const ok = await popupConfirm(
        'Ban and rotate',
        `Ban <b>${cut.length}</b> member${cut.length === 1 ? '' : 's'} and rotate the keys around them. They're added to the banlist, stripped of any role, and cannot rejoin until unbanned.`,
        false, '', 'vector_warning.svg');
    if (!ok) return;
    modSetBusy(true, `Banning ${cut.length}…`);
    try {
        await invoke('ban_community_members', { communityId: modCommunityId(), npubs: cut });
    } catch (err) {
        modSetBusy(false);
        await popupConfirm("Couldn't ban", escapeHtml(String(err)), true, '', 'vector_warning.svg');
        return;
    }
    // The ban lands, then the keys move. Banning ALONE stops them posting and
    // leaves them holding the current key — they read everything until an epoch
    // they are not vended arrives. This button promises both, so it does both,
    // and a rotation that fails says so rather than reporting the ban as the
    // whole job.
    try {
        modSetBusy(true, 'Rotating keys…');
        const retain = [...modKeep()];
        await invoke('refound_community', { communityId: modCommunityId(), retain });
        showToast(`Banned ${cut.length} and rotated.`);
    } catch (err) {
        modSetBusy(false);
        await popupConfirm(
            'Banned, but the keys did NOT rotate',
            `The ${cut.length} account${cut.length === 1 ? ' is' : 's are'} banned and cannot post. They still hold the current key and can READ until a rotation lands. Use <b>Rotate keys</b> to finish.<br><br>` + escapeHtml(String(err)),
            true, '', 'vector_warning.svg');
        await modReload();
        return;
    }
    modSetBusy(false);
    await modReload();
    refreshCommunityMemberCount(modCommunityId(), true);
}

