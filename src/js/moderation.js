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

const domModOverlay = document.getElementById('mod-overlay');
const domModCard = domModOverlay.querySelector('.mod-card');
const domModName = document.getElementById('mod-community-name');
const domModEpoch = document.getElementById('mod-epoch');
const domModClose = document.getElementById('mod-close');
const domModAlert = document.getElementById('mod-alert');
const domModAlertTitle = document.getElementById('mod-alert-title');
const domModAlertBody = document.getElementById('mod-alert-body');
const domModSearch = document.getElementById('mod-search-input');
const domModFilters = document.getElementById('mod-filters');
const domModList = document.getElementById('mod-list');
const domModTallyKeep = document.getElementById('mod-tally-keep');
const domModTallyCut = document.getElementById('mod-tally-cut');
const domModBanlist = document.getElementById('mod-banlist');
const domModRevoke = document.getElementById('mod-revoke-invites');
const domModRotate = document.getElementById('mod-rotate');
const domModBanRotate = document.getElementById('mod-ban-rotate');

let modState = {
    communityId: null,
    intel: null,
    /// npubs to carry through a rotation. The panel's whole output.
    keep: new Set(),
    filter: 'all',
    query: '',
    busy: false,
};

// Two groups that always sum to Everyone, named after what happens to them rather than
// after the machinery. There is deliberately no "Suspects" filter: the verdict starts
// equal to the selection, so it was two chips showing one number.
const MOD_FILTERS = [
    { id: 'all', label: 'Everyone' },
    { id: 'cut', label: 'Removing' },
    { id: 'keep', label: 'Staying' },
];

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
    modState = { communityId, intel: null, keep: new Set(), filter: 'all', query: '', busy: false };
    modShowTab('members');
    const tm = document.getElementById('mod-tab-members');
    const tp = document.getElementById('mod-tab-policies');
    if (tm) tm.onclick = () => modShowTab('members');
    if (tp) tp.onclick = () => modShowTab('policies');

    domModSearch.value = '';
    domModName.textContent = '';
    domModEpoch.textContent = '';
    domModAlert.style.display = 'none';
    domModList.innerHTML = '<div class="mod-empty"><span class="icon icon-loading spin"></span></div>';
    modRenderFilters();
    modSetTallies(0, 0);

    domModOverlay.classList.remove('closing');
    domModCard.classList.remove('pop-in');
    domModOverlay.classList.add('active');
    void domModCard.offsetWidth;
    domModCard.classList.add('pop-in');
    document.addEventListener('keydown', modEscape);
    pushBack('mod-overlay', closeModerationPanel);

    try {
        const intel = await invoke('get_moderation_intel', { communityId });
        // A swap or a close while the read was in flight: don't paint over what
        // the user is looking at now.
        if (modState.communityId !== communityId) return;
        // The designer's per-channel exemptions read from the same payload the
        // console already has — one place to ask, one answer.
        window.polChannels = (intel.channels || []).map(c => ({ id: c.id, name: c.name || 'channel' }));
        window.polChannelsLoaded = true;
        window.polChannelsReady?.();
        modState.intel = intel;
        modState.keep = new Set(
            intel.report.members.filter(m => m.verdict !== 'suspect').map(m => m.npub)
        );
        modApplyIntel();
    } catch (err) {
        domModList.innerHTML = '';
        const p = document.createElement('div');
        p.className = 'mod-empty';
        p.textContent = String(err);
        domModList.appendChild(p);
    }
}

/// The two faces of the console: who is here, and what the rules are.
function modShowTab(which) {
    const members = which === 'members';
    document.getElementById('mod-tab-members')?.classList.toggle('active', members);
    document.getElementById('mod-tab-policies')?.classList.toggle('active', !members);
    const pane = document.getElementById('mod-members-pane');
    if (pane) pane.style.display = members ? '' : 'none';
    const foot = document.getElementById('mod-foot');
    // The removal actions belong to the member list; hiding them on the
    // Policies tab keeps "edit a rule" and "remove people" from sharing a
    // footer.
    if (foot) foot.style.display = members ? '' : 'none';
    const pol = document.getElementById('mod-policies-pane');
    if (pol) pol.style.display = members ? 'none' : '';
    const explain = document.querySelector('.mod-explain');
    if (explain) explain.style.display = members ? '' : 'none';
    if (!members && window.openPolicyDesigner) window.openPolicyDesigner(modState.communityId);
}

function closeModerationPanel() {
    if (domModOverlay.classList.contains('closing')) return;
    if (modState.busy) return;
    document.removeEventListener('keydown', modEscape);
    popBack('mod-overlay');
    modState.communityId = null;
    domModOverlay.classList.add('closing');
    domModOverlay._closeTimer = setTimeout(() => {
        domModOverlay.classList.remove('active', 'closing');
        domModCard.classList.remove('pop-in');
    }, 160);
}

function modEscape(e) {
    if (e.key === 'Escape') closeModerationPanel();
}

/** Paint the header, the raid banner and the list from a freshly-read snapshot. */
function modApplyIntel() {
    const intel = modState.intel;
    const r = intel.report;
    domModName.textContent = intel.name || 'Community';
    domModEpoch.textContent = `Epoch ${intel.epoch}`;

    modPaintAlert();

    const used = intel.banlist_count;
    const max = intel.banlist_max;
    domModBanlist.textContent = `banlist ${used}/${max}`;
    domModRevoke.querySelector('.mod-btn-label').textContent =
        intel.invites.length ? `Revoke ${intel.invites.length} invite${intel.invites.length === 1 ? '' : 's'}` : 'No invite links';
    domModRevoke.disabled = intel.invites.length === 0;

    modRenderFilters();
    modRenderList();
}

/** The raid banner, or nothing. Re-run on leaving a busy state, which borrows this slot. */
function modPaintAlert() {
    const r = modState.intel?.report;
    domModAlert.classList.remove('working');
    if (!r || !r.raid_detected) {
        domModAlert.style.display = 'none';
        return;
    }
    // `size` is the true cluster; `members` is only a display sample the backend caps.
    const biggest = r.cohorts[0];
    const burst = r.burst_size >= 2 && r.burst_to_ms > r.burst_from_ms
        ? ` ${r.burst_size} joined within ${modAgo(Math.round((r.burst_to_ms - r.burst_from_ms) / 1000))} of each other.`
        : '';
    domModAlertTitle.textContent = `Raid: ${r.suspects} accounts flagged.`;
    domModAlertBody.textContent = (biggest ? ` ${biggest.size} posted \u201c${biggest.sample.slice(0, 40)}\u201d.` : '')
        + burst
        + ' They start unticked, so they are the ones being removed.';
    domModAlert.style.display = '';
}

function modRenderFilters() {
    domModFilters.innerHTML = '';
    const counts = modState.intel ? modCounts() : {};
    for (const f of MOD_FILTERS) {
        const b = document.createElement('button');
        b.className = 'mod-chip' + (modState.filter === f.id ? ' active' : '');
        b.textContent = f.label;
        if (modState.intel) {
            const n = document.createElement('span');
            n.className = 'mod-chip-count';
            n.textContent = counts[f.id] ?? 0;
            b.appendChild(n);
        }
        b.onclick = () => { modState.filter = f.id; modRenderFilters(); modRenderList(); };
        domModFilters.appendChild(b);
    }
}

function modCounts() {
    const members = modState.intel.report.members;
    const cut = members.filter(m => !modState.keep.has(m.npub)).length;
    return { all: members.length, cut, keep: members.length - cut };
}

function modVisible() {
    const q = modState.query.trim().toLowerCase();
    return modState.intel.report.members.filter(m => {
        if (modState.filter === 'cut' && modState.keep.has(m.npub)) return false;
        if (modState.filter === 'keep' && !modState.keep.has(m.npub)) return false;
        if (q && !(modDisplayName(m.npub) + ' ' + m.npub).toLowerCase().includes(q)) return false;
        return true;
    });
}

/// What the panel says before you read a single row. A moderator opening this
/// mid-raid needs one glance to know whether anything is wrong.
function modRenderStats() {
    const el = document.getElementById('mod-stats');
    if (!el || !modState.intel) return;
    const r = modState.intel.report;
    const flagged = r.members.filter(m => m.verdict === 'suspect').length;
    const cells = [
        { n: r.members.length, label: 'members' },
        { n: r.trusted || 0, label: 'trusted', tone: 'good' },
        { n: r.protected || 0, label: 'staff', tone: 'staff' },
        { n: flagged, label: 'flagged', tone: flagged ? 'bad' : 'quiet' },
    ];
    el.innerHTML = cells.map(c =>
        `<div class="mod-stat mod-stat-${c.tone || 'quiet'}">
            <span class="mod-stat-n">${c.n}</span>
            <span class="mod-stat-l">${c.label}</span>
         </div>`).join('');
}

function modRenderList() {
    const rows = modVisible();
    domModList.innerHTML = '';
    if (!rows.length) {
        const p = document.createElement('div');
        p.className = 'mod-empty';
        p.textContent = 'No members match.';
        domModList.appendChild(p);
        modUpdateTallies();
        return;
    }
    const frag = document.createDocumentFragment();
    for (const m of rows) frag.appendChild(modBuildRow(m));
    domModList.appendChild(frag);
    modRenderStats();
    modUpdateTallies();
}

function modBuildRow(m) {
    const kept = modState.keep.has(m.npub);
    const locked = m.verdict === 'protected';

    const row = document.createElement('div');
    // The rail encodes standing at a glance; the badge only appears where a
    // word adds something the colour cannot.
    const standing = m.verdict === 'protected' ? 'staff'
        : m.verdict === 'suspect' ? 'flagged'
        : m.verdict === 'trusted' ? 'trusted' : 'plain';
    row.className = `mod-row mod-standing-${standing}` + (kept ? '' : ' cutting') + (locked ? ' locked' : '');
    row.dataset.npub = m.npub;

    const box = document.createElement('div');
    box.className = 'mod-check' + (kept ? ' on' : '');
    box.setAttribute('role', 'checkbox');
    box.setAttribute('aria-checked', String(kept));
    if (kept) box.innerHTML = '<span class="icon icon-check"></span>';
    row.appendChild(box);

    const profile = arrProfiles.find(p => p.id === m.npub) || null;
    const avatar = createAvatarImg(profile ? getProfileAvatarSrc(profile) : null, 30, false);
    avatar.className = 'mod-avatar';
    row.appendChild(avatar);

    const body = document.createElement('div');
    body.className = 'mod-body';

    const top = document.createElement('div');
    top.className = 'mod-row-top';
    const name = document.createElement('span');
    name.className = 'mod-name cutoff';
    name.textContent = modDisplayName(m.npub);
    top.appendChild(name);
    const badge = modBadge(m);
    if (badge) top.appendChild(badge);
    body.appendChild(top);

    const meta = document.createElement('div');
    meta.className = 'mod-meta cutoff';
    // Tenure, not the raw Guestbook join: a migration re-seeds every Join at the same
    // moment, so the join date would tell 600 members they arrived on the same day.
    // Numbers are wrapped so they can sit in tabular figures and line up down the
    // column — the difference between reading a list and scanning one.
    const num = (v) => `<span class="mod-num">${v}</span>`;
    const bits = [];
    bits.push(m.tenure_secs ? `here ${num(modAgo(m.tenure_secs))}` : '<span class="mod-unknown">age unknown</span>');
    bits.push(`${num(m.messages)} msg${m.messages === 1 ? '' : 's'}`);
    if (m.distinct > 0) bits.push(`${num(m.distinct)} distinct`);
    if (m.invite_label) bits.push(`via ${modEscapeText(m.invite_label)}`);
    meta.innerHTML = bits.join('<span class="mod-dot">·</span>');
    body.appendChild(meta);

    // A reason that only restates the badge is a third copy of the same fact —
    // the badge names the standing and the tenure above it is the evidence.
    // Cite the line only when it says something neither of those does.
    const why = m.reasons.filter(r => !MOD_RESTATES_BADGE.has(r));
    if (why.length) {
        const el = document.createElement('div');
        el.className = 'mod-why cutoff';
        el.textContent = why.join(' · ');
        body.appendChild(el);
    }
    row.appendChild(body);

    if (!locked) {
        row.onclick = () => modToggle(m.npub);
        row.style.cursor = 'pointer';
    } else {
        row.title = m.reasons[0] || 'Protected';
    }
    return row;
}

const MOD_RESTATES_BADGE = new Set(['Long-standing member', 'Holds a role', 'Community owner']);

/** A badge only where it adds something: standing you can't infer from the group. */
function modBadge(m) {
    let text = null;
    if (m.is_owner) text = 'OWNER';
    else if (m.is_me) text = 'YOU';
    else if (m.is_admin) text = 'STAFF';
    else if (m.verdict === 'trusted') text = 'REGULAR';
    else if (m.verdict === 'neutral' && m.reasons.length) text = 'CHECK';
    if (!text) return null;
    const b = document.createElement('span');
    b.className = `mod-badge mod-badge-${m.verdict}`;
    b.textContent = text;
    return b;
}

/** Flip one member in place — a full re-render would lose the scroll position. */
function modToggle(npub) {
    if (modState.busy) return;
    if (modState.keep.has(npub)) modState.keep.delete(npub);
    else modState.keep.add(npub);
    const m = modState.intel.report.members.find(x => x.npub === npub);
    const old = domModList.querySelector(`.mod-row[data-npub="${CSS.escape(npub)}"]`);
    if (m && old) old.replaceWith(modBuildRow(m));
    modUpdateTallies();
    modRenderFilters();
}

function modSetTallies(keep, cut) {
    domModTallyKeep.textContent = keep;
    domModTallyCut.textContent = cut;
    // A red "0 removing" reads as a standing alarm. It only belongs there once
    // the admin has actually unticked someone.
    domModTallyCut.parentElement.hidden = cut === 0;
}

function modUpdateTallies() {
    if (!modState.intel) return;
    const members = modState.intel.report.members;
    const cut = members.filter(m => !modState.keep.has(m.npub));
    modSetTallies(members.length - cut.length, cut.length);

    const room = modState.intel.banlist_max - modState.intel.banlist_count;
    const overCap = cut.length > room;
    // A bare rotation with nobody cut is legitimate — it's the answer to a leaked link.
    domModRotate.disabled = false;
    domModBanRotate.disabled = cut.length === 0 || overCap;
    domModBanRotate.title = overCap
        ? `The banlist holds ${modState.intel.banlist_max}; only ${room} slots are free. Rotate instead: it has no ceiling.`
        : '';
    domModRotate.querySelector('.mod-btn-label').textContent =
        cut.length ? `Remove ${cut.length} & rotate` : 'Rotate keys';
    domModBanRotate.querySelector('.mod-btn-label').textContent =
        cut.length ? `Ban ${cut.length} & rotate` : 'Ban & rotate';
}

function modCutList() {
    return modState.intel.report.members
        .filter(m => !modState.keep.has(m.npub))
        .map(m => m.npub);
}

/** Lock the console for the duration of a publish; these take seconds, not frames. */
function modSetBusy(busy, label) {
    modState.busy = busy;
    domModCard.classList.toggle('busy', busy);
    for (const b of [domModRevoke, domModRotate, domModBanRotate]) b.disabled = busy;
    domModClose.disabled = busy;
    if (busy && label) {
        domModAlertTitle.textContent = label;
        domModAlertBody.textContent = ' Publishing. Leave this open until it finishes.';
        domModAlert.style.display = '';
        domModAlert.classList.add('working');
    } else {
        modPaintAlert();
    }
}

async function modReload() {
    const communityId = modState.communityId;
    if (!communityId) return;
    // The header pip and the menu entry both cache a verdict; an action just invalidated it.
    clearCommunityRaidAlert(communityId);
    try {
        const intel = await invoke('get_moderation_intel', { communityId });
        if (modState.communityId !== communityId) return;
        modState.intel = intel;
        modState.keep = new Set(intel.report.members.filter(m => m.verdict !== 'suspect').map(m => m.npub));
        modApplyIntel();
    } catch (err) {
        showToast(String(err));
    }
}

// The purge publishes one directive per member, so it runs for minutes on a big wave.
// Without a counter the panel looks hung and a moderator kills it half-done.
window.__TAURI__.event.listen('community_purge_progress', (e) => {
    const p = e.payload;
    if (!modState.busy || p.community_id !== modState.communityId) return;
    const pct = p.total ? Math.round((p.done / p.total) * 100) : 0;
    domModAlertTitle.textContent = `Removing ${p.done}/${p.total}`;
    domModAlertBody.textContent = ` ${pct}% \u2014 leave this open until it finishes.`;
});

domModClose.onclick = closeModerationPanel;
domModOverlay.onclick = (e) => { if (e.target === domModOverlay) closeModerationPanel(); };
domModSearch.oninput = () => { modState.query = domModSearch.value; modRenderList(); };

domModRevoke.onclick = async () => {
    const n = modState.intel.invites.length;
    const ok = await popupConfirm(
        'Revoke every invite link',
        `Retire all ${n} public invite link${n === 1 ? '' : 's'}? Anyone holding one can no longer join. Existing members are unaffected.`,
        false, '', 'vector_warning.svg');
    if (!ok) return;
    modSetBusy(true, 'Revoking invite links…');
    try {
        const res = await invoke('revoke_all_public_invites', { communityId: modState.communityId });
        showToast(res.failed ? `Revoked ${res.revoked}, ${res.failed} failed` : `Revoked ${res.revoked} invite link${res.revoked === 1 ? '' : 's'}`);
        modSetBusy(false);
        await modReload();
    } catch (err) {
        modSetBusy(false);
        await popupConfirm("Couldn't revoke", escapeHtml(String(err)), true, '', 'vector_warning.svg');
    }
};

domModRotate.onclick = async () => {
    const cut = modCutList();
    // A rotation with links still live buys minutes: the same holder walks back in.
    const live = modState.intel.invites.length;
    const linkWarning = live
        ? `<br><br><b>${live} invite link${live === 1 ? ' is' : 's are'} still live.</b> Anyone holding one can rejoin straight after this. Revoke them first.`
        : '';
    const ok = await popupConfirm(
        cut.length ? 'Remove members and rotate' : 'Rotate keys',
        (cut.length
            ? `Remove <b>${cut.length}</b> member${cut.length === 1 ? '' : 's'} from the community, then mint a new epoch only the ${modState.keep.size} remaining can follow. They are dropped from everyone's member list and lose access, without being banned.<br><br>This publishes one removal per member, so ${cut.length} will take a while.`
            : 'Mint a new epoch for everyone currently in the community. Use this when an invite link leaked but the members are all real.') + linkWarning,
        false, '', 'vector_warning.svg');
    if (!ok) return;
    modSetBusy(true, cut.length ? `Removing 0/${cut.length}` : 'Rotating keys…');
    try {
        // An empty retain rotates without removing anyone; a non-empty one is the keep-list.
        const retain = cut.length ? [...modState.keep] : [];
        const res = await invoke('refound_community', { communityId: modState.communityId, retain });
        const refused = res?.refused ? ` ${res.refused} refused.` : '';
        showToast(cut.length ? `Removed ${res?.kicked ?? cut.length} and rotated.${refused}` : 'Keys rotated.');
        modSetBusy(false);
        await modReload();
        // The header count, chat-header subtext and roster all cache the member set.
        refreshCommunityMemberCount(modState.communityId, true);
    } catch (err) {
        modSetBusy(false);
        await popupConfirm("Couldn't complete the removal", escapeHtml(String(err)), true, '', 'vector_warning.svg');
    }
};

domModBanRotate.onclick = async () => {
    const cut = modCutList();
    if (!cut.length) return;
    const ok = await popupConfirm(
        'Ban and rotate',
        `Ban <b>${cut.length}</b> member${cut.length === 1 ? '' : 's'} and rotate the keys around them. They're added to the banlist, stripped of any role, and cannot rejoin until unbanned.`,
        false, '', 'vector_warning.svg');
    if (!ok) return;
    modSetBusy(true, `Banning ${cut.length}…`);
    try {
        await invoke('ban_community_members', { communityId: modState.communityId, npubs: cut });
        showToast(`Banned ${cut.length}.`);
        modSetBusy(false);
        await modReload();
        refreshCommunityMemberCount(modState.communityId, true);
    } catch (err) {
        modSetBusy(false);
        await popupConfirm("Couldn't ban", escapeHtml(String(err)), true, '', 'vector_warning.svg');
    }
};

/// Member-supplied text (an invite label) never reaches innerHTML unescaped.
function modEscapeText(s) {
    return String(s ?? '').replace(/[&<>"']/g, c =>
        ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
