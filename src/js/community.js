// Communities: invites and joining, member counts, the overview and its actions,
// and the invite panel. One global scope: loads after main.js and shares its globals.

/**
 * Load pending Community invites from the backend. The private bundle carries the name
 * (no icon/description), so we parse it out for display.
 */
async function loadCommunityInvites() {
    try {
        const invites = await invoke('list_community_invites');
        arrCommunityInvites = (invites || []).map(inv => {
            let name = 'Community';
            let channels = [];
            let icon = null;
            // The bundle carries name + the channel id(s) + keys; we pull the ids so Accept can
            // render the real community row optimistically (same channel id → no swap on reconcile).
            // It also carries the community icon (encrypted ref) so the invite card shows the real logo.
            try {
                const b = JSON.parse(inv.bundle_json);
                name = b.name || name;
                channels = (b.channels || []).map(c => ({ id: c.id, name: c.name }));
                icon = b.icon || null;
            } catch (_) {}
            return { community_id: inv.community_id, name, inviter_npub: inv.inviter_npub, channels, icon };
        });
    } catch (e) {
        console.error('Failed to load community invites:', e);
    }
}

/**
 * Add/refresh a Community's channel chats in the running session from a backend
 * CommunitySummary (so a freshly-joined/created Community appears without a reload).
 * Returns the first channel id (to navigate to), or null.
 */
async function surfaceCommunitySummary(summary) {
    if (!summary) return null;
    let firstChannel = null;
    let firstSync = null;
    let fallbackChannel = null;
    let fallbackSync = null;
    for (const ch of summary.channels || []) {
        const chat = getOrCreateChat(ch.channel_id, 'Community');
        chat.metadata = chat.metadata || {};
        chat.metadata.custom_fields = chat.metadata.custom_fields || {};
        chat.metadata.custom_fields.name = summary.name;
        chat.metadata.custom_fields.description = summary.description || '';
        chat.metadata.custom_fields.community_id = summary.community_id;
        // Same stamps the backend persists, or the in-memory graft disagrees with the
        // DB until the next reboot: without `primary_channel`, EVERY channel row
        // passes the render fallback and the community appears once per channel.
        chat.metadata.custom_fields.channel_name = ch.name || '';
        if (summary.primary_channel) chat.metadata.custom_fields.primary_channel = summary.primary_channel;
        chat.metadata.custom_fields.is_owner = summary.is_owner ? 'true' : 'false';
        chat.metadata.custom_fields.dissolved = summary.dissolved ? 'true' : 'false';
        // Protocol stack (1 = v1, 2 = v2) gates v2-only affordances (e.g. the
        // Self-Destruct Timer). Never downgrade — mirrors upsert_community_chat:
        // a dual-stack community identified as v2 stays v2.
        const curProto = parseInt(chat.metadata.custom_fields.proto_version, 10) || 0;
        if ((summary.proto_version || 0) > curProto) {
            chat.metadata.custom_fields.proto_version = String(summary.proto_version);
        }
        // Stamp the join moment so an empty community sorts to the top right away. Reloads
        // re-source this from the persisted DB created_at via upsert_community_chat.
        if (!chat.metadata.custom_fields.created_at) {
            chat.metadata.custom_fields.created_at = String(Date.now());
        }
        // Proven owner (verified attestation) → drives the crown + hoist + in-chat Owner tag.
        if (summary.owner_npub) chat.metadata.custom_fields.owner_npub = summary.owner_npub;
        else delete chat.metadata.custom_fields.owner_npub;
        if (summary.has_icon) chat.metadata.custom_fields.icon = '1';
        // The page-1 sync pulls existing history (e.g. the owner's welcome message) so the
        // channel isn't empty on open. Backend anti-stampede dedups a later open.
        const p = invoke('sync_community_channel', { channelId: ch.channel_id, beforeMs: null }).catch(() => {});
        // Navigate to a channel we can actually READ. A private channel we hold no
        // key for renders empty and refuses every send, so landing there reads as
        // the community being broken. Falls back to the first channel when nothing
        // is readable, so a community never becomes unopenable.
        if (ch.readable !== false && !firstChannel) { firstChannel = ch.channel_id; firstSync = p; }
        if (!fallbackChannel) { fallbackChannel = ch.channel_id; fallbackSync = p; }
    }
    loadCommunityRoles(summary.community_id);
    resolveCommunityAvatars();
    listChanged();
    // If a warmed preload was promoted on Accept, the chat is ALREADY populated (its messages were
    // emitted by the backend), so open immediately — the first sync trues it up in the background.
    // Only await the sync when NOT preloaded (a cold join would otherwise open to an empty chat).
    const openChannel = firstChannel || fallbackChannel;
    const openSync = firstChannel ? firstSync : fallbackSync;
    if (openSync && !summary.preloaded) await openSync;
    communityChanged(summary.community_id);
    return openChannel;
}

/**
 * Preload a community's admin roster into its channel chats' metadata. Message rendering reads
 * `metadata.admins` to show the admin tag + chip @everyone from admin senders; without this the
 * roster only loaded when Group Info opened, so admin tags + @everyone were dead until then.
 * Owner status comes from `owner_npub` (already on the chat); this fills the non-owner admin set.
 */
async function loadCommunityRoles(communityId) {
    if (!communityId) return;
    let adminNpubs;
    try { adminNpubs = await invoke('get_community_admins', { communityId }); } catch (_) { return; }
    applyCommunityAdmins(communityId, adminNpubs);
}

/**
 * Cache a community's admin set onto its channel chats AND repaint the `admin` badges already
 * rendered in the open channel. The badge is baked in at row-build time, so a promote/demote
 * otherwise stayed visible until the chat was re-opened; a surgical pass keeps scroll position,
 * which a full re-render would throw away mid-conversation.
 */
function applyCommunityAdmins(communityId, adminNpubs) {
    for (const c of arrChats) {
        if (c.chat_type === 'Community' && c.metadata?.custom_fields?.community_id === communityId) {
            c.metadata = c.metadata || {};
            c.metadata.admins = adminNpubs.slice();
        }
    }
    const open = arrChats.find(c => c.id === strOpenChat);
    if (!open || open.metadata?.custom_fields?.community_id !== communityId) return;
    const adminSet = new Set(adminNpubs);
    // Rows derive their badges from the community signal.
    communityChanged(communityId);
}

/**
 * Accept a pending Community invite → join + open it. Optimistic removal from the list.
 */
async function acceptCommunityInvite(communityId) {
    const snapshot = arrCommunityInvites;
    const invite = arrCommunityInvites.find(i => i.community_id === communityId);
    arrCommunityInvites = arrCommunityInvites.filter(i => i.community_id !== communityId);

    // Optimistic row: the bundle carries the channel id, so we render the real community row INSTANTLY
    // (locked, "Joining…") instead of leaving a dead zone. Same channel id means surfaceCommunitySummary
    // reconciles this exact chat later — no swap/flicker. It unlocks once read/writeable (control-fold/sync
    // resolves, or a message streams in — see the message_new handler).
    const optimisticChannelId = invite?.channels?.[0]?.id || null;
    if (optimisticChannelId) {
        const chat = getOrCreateChat(optimisticChannelId, 'Community');
        chat.metadata = chat.metadata || {};
        chat.metadata.custom_fields = chat.metadata.custom_fields || {};
        chat.metadata.custom_fields.name = invite.name || 'Community';
        chat.metadata.custom_fields.community_id = communityId;
        if (!chat.metadata.custom_fields.created_at) chat.metadata.custom_fields.created_at = String(Date.now());
        chat._joining = true; // renders locked
        // Re-sort so the fresh created_at floats the joining row to the TOP (renderChatlist itself
        // renders arrChats in order; the new chat was pushed to the end).
        sortChats();
    }
    listChanged();
    invitesChanged();
    adjustSize();

    try {
        const summary = await invoke('accept_community_invite', { communityId });
        await loadCommunityInvites();
        // surfaceCommunitySummary awaits the page-1 sync = control-folded + read/writeable.
        const channelId = await surfaceCommunitySummary(summary);
        clearCommunityJoining(communityId);
        adjustSize();
        if (channelId && arrChats.some(c => c.id === channelId)) {
            openChat(channelId);
        } else {
            // The community was torn down during the join (kicked/banned before it landed, so the
            // chat row is gone or never materialized). Surface a short notice rather than silently
            // bailing the open and leaving the user wondering why the join did nothing.
            showToast('You were removed from this community by an admin');
            listChanged();
        }
    } catch (e) {
        console.error('Failed to accept community invite:', e);
        // Roll back the optimistic row + restore the invite.
        if (optimisticChannelId) {
            const idx = arrChats.findIndex(c => c.id === optimisticChannelId);
            if (idx !== -1) arrChats.splice(idx, 1);
        }
        arrCommunityInvites = snapshot;
        listChanged();
        invitesChanged();
        adjustSize();
        popupConfirm('Error', 'Failed to join Community: ' + escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

/**
 * Release the "Joining…" lock on a community's channel rows (read/writeable now). Idempotent;
 * re-renders only if a locked row actually flipped, so the message_new early-unlock is cheap.
 */
function clearCommunityJoining(communityId) {
    let changed = false;
    for (const c of arrChats) {
        if (c._joining && c.chat_type === 'Community' && c.metadata?.custom_fields?.community_id === communityId) {
            c._joining = false;
            chatChanged(c);
        }
    }
}

/**
 * Decline a pending Community invite (drops the parked bundle locally).
 */
async function declineCommunityInvite(communityId) {
    const snapshot = arrCommunityInvites;
    arrCommunityInvites = arrCommunityInvites.filter(i => i.community_id !== communityId);
    invitesChanged();
    adjustSize();
    try {
        await invoke('decline_community_invite', { communityId });
    } catch (e) {
        // Roll back the optimistic removal — otherwise the invite is gone from the UI but still
        // parked in the backend, and silently reappears on the next invite refresh.
        console.error('Failed to decline community invite:', e);
        arrCommunityInvites = snapshot;
        invitesChanged();
        adjustSize();
        popupConfirm('Error', 'Failed to decline invite: ' + escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

/**
 * Preview a public invite URL (or fragment) and offer to join. Shows the community name +
 * description with Join / Ignore. On Join, accepts and navigates into the new channel.
 */
let _communityJoinInFlight = false;
async function previewAndJoinCommunityLink(url) {
    // Re-entrancy guard: a double-paste, deep-link-while-pasting, or double-tap must not fire
    // two concurrent joins (which race surfaceCommunitySummary and hijack the shared popup).
    if (_communityJoinInFlight) return;
    _communityJoinInFlight = true;
    try {
        let preview;
        // Fetching the encrypted bundle off the relays can take several seconds — a PERSISTENT
        // toast (not the auto-timeout one) keeps feedback up for the whole await.
        showToast('Loading community invite…', true);
        try {
            preview = await invoke('preview_public_invite', { url });
        } catch (e) {
            hideToast();
            popupConfirm('Invalid Invite', 'This invite link could not be loaded.<br><br>' + escapeHtml(String(e)), true, '', 'vector_warning.svg');
            return;
        }
        // Already a member: opening an invite is a navigation intent, not a join request — take them
        // to the community instead of asking them to join a room they're standing in.
        const joined = findCommunityChat(preview.community_id);
        if (joined) {
            hideToast();
            openChat(joined.id);
            return;
        }
        const descHtml = preview.description ? `<br><br><span style="opacity:0.8;">${escapeHtml(preview.description)}</span>` : '';
        // Show the community's own logo when it has one, else the same placeholder the chat list
        // uses for logo-less communities. Bare filename: popupConfirm prefixes `./icons/` itself.
        let iconSrc = 'group-placeholder.svg';
        if (preview.icon) {
            try {
                const path = await invoke('cache_invite_logo', { image: preview.icon });
                if (path) iconSrc = convertFileSrc(path);
            } catch (e) { console.debug('invite logo decrypt failed, using placeholder', e); }
        }
        hideToast();
        const confirmed = await popupConfirm(
            `Join ${escapeHtml(preview.name)}?`,
            `You've been invited to join <b>${escapeHtml(preview.name)}</b>.${descHtml}`,
            false, '', iconSrc, '', null, true
        );
        if (!confirmed) return;
        showToast(`Joining ${preview.name}…`, true);
        try {
            const summary = await invoke('accept_public_invite', { url });
            // Await the first-page sync so the channel opens with its history (not empty) and lands
            // in the right chat-list slot instead of at the bottom.
            const channelId = await surfaceCommunitySummary(summary);
            hideToast();
            // Only auto-open if the user is still parked on the chat list (no chat open) — don't
            // yank them out of a DM they opened while the multi-second join was in flight.
            if (channelId && !strOpenChat) openChat(channelId);
        } catch (e) {
            hideToast();
            popupConfirm('Failed to Join', escapeHtml(String(e)), true, '', 'vector_warning.svg');
        }
    } finally {
        _communityJoinInFlight = false;
    }
}

/** Detect a Vector community invite URL (or bare payload) in pasted/typed text.
 *  Covers the v1 fragment form (vectorapp.io only), the v2 naddr form on ANY
 *  host (`…/invite/naddr1…#<frag>` — the naddr+fragment is the whole payload
 *  and self-authenticates, so armada.buzz links join natively), and the
 *  bare-payload equivalents of both. */
function isCommunityInviteUrl(text) {
    if (typeof text !== 'string' || !text.includes('#')) return false;
    const t = text.trim();
    return /vectorapp\.io\/invite\/?#/i.test(t)
        || /\/invite\/naddr1[a-z0-9]{20,}#/i.test(t)
        || /^(?:nostr:)?naddr1[a-z0-9]{20,}#[A-Za-z0-9_-]{20,}$/i.test(t)
        || /^#?[A-Za-z0-9_-]{40,}$/.test(t);
}

// ============================================================================
// In-chat Community Invite cards
// ============================================================================

// Matches a shareable Community invite link in either form (https share URL or vector://
// deep link), v1 or v2. v1 is fragment-only and vectorapp.io-specific (`/invite#<frag>`);
// v2 carries the bundle coordinate as an naddr in the path (`/invite/<naddr>#<frag>`) and
// is accepted from ANY host — the naddr+fragment self-authenticates (the domain is never
// contacted), so Armada-minted links render + join natively. The invite KEY —
// `<naddr>#<frag>` for v2, bare `<frag>` for v1 — is the whole payload: it keys the
// preview cache and reconstructs a canonical URL for the backend.
const COMMUNITY_INVITE_URL_REGEX = /(?:https?:\/\/(?:www\.)?vectorapp\.io\/invite\/?|vector:\/\/invite\/?|https?:\/\/[^\s#]+?\/invite\/(?=naddr1))(naddr1[a-z0-9]{20,})?#([A-Za-z0-9_-]{20,})/gi;

/** Canonical share URL from an invite key (a v2 key carries its naddr locator). */
function communityInviteUrlFromKey(inviteKey) {
    return inviteKey.includes('#')
        ? `https://vectorapp.io/invite/${inviteKey}`
        : `https://vectorapp.io/invite#${inviteKey}`;
}

/** Strip Community invite links from `text` — the invite card carries the affordance. */
function stripCommunityInviteUrls(text) {
    if (!text) return text;
    COMMUNITY_INVITE_URL_REGEX.lastIndex = 0;
    if (!COMMUNITY_INVITE_URL_REGEX.test(text)) return text;
    return text
        .replace(COMMUNITY_INVITE_URL_REGEX, '')
        .replace(/[ \t]{2,}/g, ' ')
        .replace(/\n{3,}/g, '\n\n')
        .trim();
}

/** Chat-list / reply snippets: swap raw invite URLs for a friendly tag. */
function replaceCommunityInviteUrlsForPreview(text) {
    if (!text) return text;
    COMMUNITY_INVITE_URL_REGEX.lastIndex = 0;
    return text.replace(COMMUNITY_INVITE_URL_REGEX, 'Community Invite');
}

// Resolved previews keyed by invite key (v1: fragment; v2: naddr#fragment):
// { state: 'ok', info, iconSrc, ts } or { state: 'err', error, ts } or
// { state: 'loading', promise }. Errors expire so a chat
// reopen after a slow relay retries instead of inheriting a stale "Invite Unavailable".
// Ok entries expire too (non-members track metadata edits via the backend's live fold) —
// EXCEPT when the community is joined: then the entry only supplies the immutable
// token→community mapping and the card reads display data live from the chat itself.
const _invitePreviewCache = new Map();
const INVITE_PREVIEW_ERR_TTL_MS = 10_000;
const INVITE_PREVIEW_OK_TTL_MS = 300_000;
const INVITE_PREVIEW_CACHE_MAX = 64;
// Each distinct card costs a relay fetch — cap per message.
const INVITE_CARDS_PER_MSG = 3;

/**
 * Find every Community invite link inside `text` and append a Join/Open card per unique
 * invite under `target`. Cards build instantly in a skeleton state and fill when the
 * backend preview resolves — which also warms the join preload cache, so an eventual
 * Join opens populated (same path the deep-link flow uses).
 */
/** The distinct invite keys in a message body, capped at what a message may show as cards. */
function communityInviteKeys(text) {
    const keys = [];
    if (!text) return keys;
    COMMUNITY_INVITE_URL_REGEX.lastIndex = 0;
    let match;
    while ((match = COMMUNITY_INVITE_URL_REGEX.exec(text)) !== null) {
        const inviteKey = match[1] ? `${match[1]}#${match[2]}` : match[2];
        if (keys.includes(inviteKey)) continue;
        if (keys.length >= INVITE_CARDS_PER_MSG) break;
        keys.push(inviteKey);
    }
    return keys;
}

function _resolveCommunityInvitePreview(inviteKey) {
    const cached = _invitePreviewCache.get(inviteKey);
    if (cached) {
        if (cached.state === 'loading') return cached.promise;
        if (cached.state === 'ok') {
            const joined = cached.info.community_id
                && arrChats.some(c => c.metadata?.custom_fields?.community_id === cached.info.community_id);
            if (joined || (Date.now() - cached.ts) < INVITE_PREVIEW_OK_TTL_MS) return Promise.resolve(cached);
        } else if (Date.now() - cached.ts < INVITE_PREVIEW_ERR_TTL_MS) {
            return Promise.resolve(cached);
        }
    }
    const prior = (cached && cached.state === 'ok') ? cached : null;
    const promise = (async () => {
        let entry;
        try {
            const info = await invoke('preview_public_invite', { url: communityInviteUrlFromKey(inviteKey) });
            // Decrypt + disk-cache the logo up-front; the card binds the local asset path
            // (the frontend never fetches the remote blob itself).
            let iconSrc = '';
            if (info.icon) {
                try {
                    const path = await invoke('cache_invite_logo', { image: info.icon });
                    if (path) iconSrc = convertFileSrc(path);
                } catch (e) { console.debug('invite logo decrypt failed, using placeholder', e); }
            }
            entry = { state: 'ok', info, iconSrc, ts: Date.now() };
        } catch (e) {
            // A failed refresh must not downgrade a previously-good preview.
            entry = prior ? { ...prior, ts: Date.now() } : { state: 'err', error: String(e), ts: Date.now() };
        }
        if (_invitePreviewCache.size >= INVITE_PREVIEW_CACHE_MAX) {
            const oldest = _invitePreviewCache.keys().next().value;
            if (oldest !== undefined) _invitePreviewCache.delete(oldest);
        }
        _invitePreviewCache.set(inviteKey, entry);
        return entry;
    })();
    _invitePreviewCache.set(inviteKey, { state: 'loading', promise });
    return promise;
}

/** The chat row of a community we're already in, or undefined. */
function findCommunityChat(communityId) {
    if (!communityId) return undefined;
    return arrChats.find(c => c.chat_type === 'Community' && c.metadata?.custom_fields?.community_id === communityId);
}

/** Join from an invite card; lands in the community on success, and surfaces the error itself. */
async function _joinCommunityFromCard(inviteKey, communityId) {
    // Shares the deep-link flow's guard: one join at a time, app-wide.
    if (_communityJoinInFlight) return;
    _communityJoinInFlight = true;
    try {
        const summary = await invoke('accept_public_invite', { url: communityInviteUrlFromKey(inviteKey) });
        // Await the first-page sync so the chat lands populated + in the right list slot.
        const channelId = await surfaceCommunitySummary(summary);
        // Joining IS the navigation intent — land in the new community, same as hitting Open.
        if (channelId) openChat(channelId);
    } catch (e) {
        popupConfirm('Failed to Join', escapeHtml(String(e)), true, '', 'vector_warning.svg');
    } finally {
        _communityJoinInFlight = false;
    }
}

// Track pending status hide timeout
// Cached member count per community id, for the chat-header subtext + overview status. Membership is
// derived from observed activity (best-effort), so this is refreshed live as people join/speak.
const communityMemberCounts = new Map();
// Full roster per community id ([{npub, last_active}]) — the @mention pool reads this
// synchronously, so anyone the Member List shows is taggable (not just RAM-loaded senders).
const communityMembersCache = new Map();
/** A community's role graph (roles + grants), cached so the roster paints grouped on reopen. */
const communityRoleGraphCache = new Map();
const _communityCountLastFetch = new Map();
const _communityCountInFlight = new Set();

/** Render text for a community's member count, or '' if not yet known. */
function communityMemberSubtext(communityId) {
    const n = communityMemberCounts.get(communityId);
    return (n == null) ? '' : `${n} Member${n === 1 ? '' : 's'}`;
}

/**
 * Refresh a community's cached member count and live-update the header/overview if open. Throttled to
 * one fetch per 2s per community (pass force=true to bypass, e.g. on a join/leave/control change).
 */
async function refreshCommunityMemberCount(communityId, force = false) {
    if (!communityId || _communityCountInFlight.has(communityId)) return;
    if (!force && Date.now() - (_communityCountLastFetch.get(communityId) || 0) < 2000) return;
    _communityCountInFlight.add(communityId);
    let members;
    try {
        members = await invoke('get_community_members', { communityId });
    } catch (_) {
        _communityCountInFlight.delete(communityId);
        return;
    }
    _communityCountInFlight.delete(communityId);
    _communityCountLastFetch.set(communityId, Date.now());
    communityMembersCache.set(communityId, members);
    if (communityMemberCounts.get(communityId) === members.length) return; // unchanged, no re-render
    communityMemberCounts.set(communityId, members.length);
    // Live-update the open channel's header (its chat carries this community_id) + the overview status.
    const openChat = strOpenChat ? arrChats.find(c => c.id === strOpenChat) : null;
    if (openChat && openChat.metadata?.custom_fields?.community_id === communityId) {
        updateChatHeaderSubtext(openChat);
    }
    // The channel pane's head derives its count from the community signal.
    VectorSvelte.touchCommunity(communityId);
    if (VectorSvelte.overviewState().groupId === communityId) {
        // The member SET changed while the overview is open — re-render the rows live so a
        // join/leave/new-speaker appears without closing and reopening (preserve any active search).
        if (domGroupOverview.style.display !== 'none') {
            const chat = arrChats.find(c => c.metadata?.custom_fields?.community_id === communityId);
            if (chat) renderCommunityOverview(chat, true);
        }
    }
}

/** The header subtext (status, typing, member count) derives from the chat's signal. */
function updateChatHeaderSubtext(chat) {
    if (!chat) return;
    touchChatRow(chat);
    const communityId = chat.metadata?.custom_fields?.community_id;
    if (communityId) refreshCommunityMemberCount(communityId);
}

/** Leave a community you don't own, from the chat-list context menu. Confirms,
 *  calls leave_community, then drops its channels locally and repaints — mirrors
 *  the Group Overview leave path's teardown. */
async function leaveCommunityFromList(chat) {
    const cf = chat?.metadata?.custom_fields || {};
    const communityId = cf.community_id;
    if (!communityId) return;
    const name = cf.name || 'this community';
    const confirmed = await popupConfirm('Leave Community', `Leave "<b>${escapeHtml(name)}</b>"? You'll need a new invite to rejoin.`, false, '', 'vector_warning.svg');
    if (!confirmed) return;
    try {
        await invoke('leave_community', { communityId });
        if (arrChats.some(c => c.metadata?.custom_fields?.community_id === communityId && c.id === strOpenChat)) {
            await closeChat();
        }
        arrChats = arrChats.filter(c => c.metadata?.custom_fields?.community_id !== communityId);
        listChanged();
    } catch (e) {
        await popupConfirm('Failed to Leave', escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

/**
 * Open the Group Overview view for a specific group chat
 * @param {Chat} chat - The group chat object
 */
async function openGroupOverview(chat) {
    if (!chat || !chatIsGroup(chat)) return;

    pushBack('group-overview', () => {
        VectorSvelte.showPane('groupOverview', false);
        VectorSvelte.setOverviewGroup(null);
        // Widescreen docks the roster beside a conversation that never closed, so
        // dismissing it is the user closing the ROSTER — record that, and don't
        // re-enter a chat that was already open.
        if (wsActive()) {
            wsSetMembersOpen(false);
            return;
        }
        openChat(chat.id);
    });

    navbarSelect('chat-btn');
    VectorSvelte.showPane('settings', false);
    VectorSvelte.showPane('invites', false);
    if (fProfileEditMode) exitProfileEditMode(true);
    VectorSvelte.showPane('profile', false);
    // Narrow, the roster IS the screen and everything else gets out of its way.
    // Widescreen docks it as a fourth column beside panes that must stay up — and
    // hiding them there only LOOKS right because `body.ws` forces them visible with
    // !important. The inline none survives underneath, so narrowing the window past
    // the threshold hands it the win and the whole app paints black.
    if (!wsActive()) {
        VectorSvelte.showPane('navbar', false);
        VectorSvelte.showPane('chats', false);
        VectorSvelte.showPane('chat', false);
    }

    // Store which group is being viewed
    VectorSvelte.setOverviewGroup(chat.id);

    // Show the shell BEFORE rendering: the renderer's header/avatar paint
    // synchronously and its awaited fetches (members can be network-bound) fill
    // in while visible. Every other pane is already hidden above, so awaiting
    // first would leave the app fully black for the whole fetch — and a render
    // throw would strand it there.
    if (domGroupOverview.style.display !== '') {
        domGroupOverview.classList.add('fadein-subtle-anim');
        domGroupOverview.addEventListener('animationend', () => domGroupOverview.classList.remove('fadein-subtle-anim'), { once: true });
        VectorSvelte.showPane('groupOverview', true);
    }

    // Only Communities are group-like now (chatIsGroup gate above), so render the
    // Community overview (no member roster; invite by link/npub).
    try {
        await renderCommunityOverview(chat);
    } catch (e) {
        console.error('Community overview render failed:', e);
    }
}


/**
 * Render the overview panel for a Community channel. Reuses the group-overview DOM, but:
 * editable name/avatar/description (owner only) via the Community commands, NO member
 * roster (membership is hidden), and an invite panel offering a shareable link + by-npub.
 * @param {Chat} chat - The Community channel chat
 */
/**
 * Silently tear the local UI down for a community that's gone (the involuntary KICK path; the backend has
 * already dropped its keys + DB rows). Closes the open channel if it belongs to the community, removes its
 * channels from the chat list + overview, and re-renders. Voluntary leave keeps its own inline teardown.
 */
async function removeCommunityFromUI(communityId) {
    const ids = new Set(
        arrChats.filter(c => c.metadata?.custom_fields?.community_id === communityId).map(c => c.id)
    );
    const wasViewing = ids.has(strOpenChat);
    if (wasViewing) {
        await closeChat();
        VectorSvelte.showPane('groupOverview', false);
        VectorSvelte.setOverviewGroup(null);
    }
    arrChats = arrChats.filter(c => c.metadata?.custom_fields?.community_id !== communityId);
    listChanged();
    if (wasViewing) openChatlist();
}

/**
 * Render the owner-only "Upgrade to Concord v2" migration row from the backend status.
 * Timelock-gated: before the unlock the button is disabled and shows a countdown; once
 * unlocked, a confirm dialog arms the irreversible wizard. Hidden entirely for members,
 * v2 communities, and dissolved/migrated/ineligible ones.
 */
/** Surface the v2 upgrade row where an action or a countdown is meaningful: owner-only, v1-only. */
async function loadMigrationStatus(communityId) {
    let status;
    try {
        status = await invoke('migration_status', { communityId });
    } catch (_) { return; }
    if (VectorSvelte.overviewState().communityId !== communityId) return;
    const shown = status && (status.state === 'ready' || status.state === 'locked' || status.state === 'in_progress');
    VectorSvelte.setOverview({ migration: shown ? status : null });
}

/** The type-to-confirm, irreversible upgrade wizard. Lands the owner back inside the room. */
async function runCommunityMigration() {
    const { communityId, name, chatId } = VectorSvelte.overviewState();
    const ok = await popupConfirm(
        'Upgrade to Concord v2?',
        `This upgrades "<b>${escapeHtml(name)}</b>" to the newer, more private Concord v2.<br><br>Everyone here moves over automatically, keeping their history. Your old invite links will stop working, so you'll need to share new ones. This cannot be undone.`,
        false, '', 'concord_v2.svg');
    if (!ok) return;
    // Lock the app behind the unclosable ring modal for the whole wizard (rekey contract): the owner
    // closing the app mid-wizard is the worst case. Registered before the invoke so no phase is missed.
    const modal = await showRekeyProgressModal('Upgrading to Concord v2', 'community_migration_progress');
    try {
        await invoke('migrate_community', { communityId });
        await modal.finish('Upgrade complete!');
        modal.close();
        // openGroupOverview hid every other pane, so a bare hide paints black: this is the back-entry's
        // close path. The v2 twin reuses the primary channel id, so the same chat id opens the migrated room.
        popBack('group-overview');
        VectorSvelte.showPane('groupOverview', false);
        VectorSvelte.setOverviewGroup(null);
        openChat(chatId);
    } catch (e) {
        modal.close();
        await popupConfirm('Upgrade Failed', escapeHtml(String(e)), true, '', 'vector_warning.svg');
        // A partially-progressed wizard must come back as "Resume upgrade".
        loadMigrationStatus(communityId);
    }
}

let fCommunityOverviewMounted = false;
function mountCommunityOverview() {
    fCommunityOverviewMounted = true;
    const cur = () => {
        const { chatId } = VectorSvelte.overviewState();
        return arrChats.find(c => c.id === chatId) || null;
    };
    VectorSvelte.mountCommunityOverview(document.getElementById('group-overview-scroll'), {
        h: {
            memberSubtext: communityMemberSubtext,
            createAvatarImg,
            toggleMute: async () => {
                const chat = cur();
                if (!chat) return;
                VectorSvelte.setOverview({ muted: await invoke('toggle_chat_mute', { chatId: chat.id }) });
            },
            pickIcon: () => pickCommunityIcon(cur()),
            rename: async (newName) => {
                const chat = cur();
                const cf = chat?.metadata?.custom_fields;
                if (!cf) return;
                const { communityId } = VectorSvelte.overviewState();
                const prev = cf.name;
                cf.name = newName;
                VectorSvelte.setOverview({ name: newName });
                try {
                    await invoke('update_community_metadata', { communityId, name: newName, description: null });
                    communityChanged(communityId);
                } catch (e) {
                    console.error('Failed to rename community:', e);
                    cf.name = prev;
                    VectorSvelte.setOverview({ name: prev });
                    showToast('Failed to update the name');
                }
            },
            setDescription: async (newDesc) => {
                const chat = cur();
                const cf = chat?.metadata?.custom_fields;
                if (!cf) return;
                const { communityId } = VectorSvelte.overviewState();
                const prev = cf.description || '';
                cf.description = newDesc;
                VectorSvelte.setOverview({ description: newDesc });
                try {
                    await invoke('update_community_metadata', { communityId, name: null, description: newDesc });
                } catch (e) {
                    console.error('Failed to update community description:', e);
                    cf.description = prev;
                    VectorSvelte.setOverview({ description: prev });
                    showToast('Failed to update the description');
                }
            },
            invite: () => { const chat = cur(); if (chat) openCommunityInvitePanel(chat); },
            moderate: () => openModerationPanel(VectorSvelte.overviewState().communityId),
            // The flows live in `communityLeaveOrDelete`: the widescreen header menu offers the same action.
            leaveOrDelete: async () => { const chat = cur(); if (chat) await communityLeaveOrDelete(chat); },
            migrate: runCommunityMigration,
        },
    });
}

/** The overview header's back button. Widescreen: the roster's own close button goes through
 *  the same pair as the header's Members toggle, or the preference keeps reading "open". */
function closeGroupOverviewFromHeader() {
    if (wsActive()) {
        wsCloseDetails();
        wsSetMembersOpen(false);
        return;
    }
    const { chatId } = VectorSvelte.overviewState();
    popBack('group-overview');
    VectorSvelte.showPane('groupOverview', false);
    VectorSvelte.setOverviewGroup(null);
    openChat(chatId);
}

VectorSvelte.setOverviewHeadHandlers({
    back: closeGroupOverviewFromHeader,
    memberSubtext: communityMemberSubtext,
    placeholderAvatar: () => createPlaceholderAvatar(true, 22),
});

/** Pick and upload a new community icon; the pencil becomes a progress ring meanwhile. */
async function pickCommunityIcon(chat) {
    const cf = chat?.metadata?.custom_fields;
    if (!cf) return;
    const communityId = cf.community_id;
    const { open } = window.__TAURI__.dialog;
    const selected = await open({ multiple: false, filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp'] }] });
    const filePath = typeof selected === 'string' ? selected : selected?.path;
    if (!filePath) return;
    VectorSvelte.setOverview({ upload: { progress: 5 } });
    let unlisten = null;
    try {
        unlisten = await window.__TAURI__.event.listen('community_image_upload_progress', (e) => {
            if (e.payload?.community_id === communityId && !e.payload?.is_banner) {
                VectorSvelte.setOverview({ upload: { progress: e.payload.progress || 0 } });
            }
        });
        await invoke('set_community_image', { communityId, filepath: filePath, isBanner: false });
        cf.icon = '1';
        const cachedPath = await invoke('cache_community_image', { communityId, isBanner: false });
        if (cachedPath) chat.metadata.avatar_cached = cachedPath;
    } catch (err) {
        console.error('Failed to set community image:', err);
        showToast('Failed to update the image');
        VectorSvelte.setOverview({ upload: null });
        return;
    } finally {
        if (unlisten) unlisten();
    }
    await renderCommunityOverview(chat);
    communityChanged(communityId);
}

/** The mounted member-roster island and the community it shows (one per overview open). */
let groupRoster = null;
let groupRosterCommunityId = null;

async function renderCommunityOverview(chat, preserveSearch = false) {
    const cf = chat.metadata?.custom_fields || {};
    const communityId = cf.community_id;
    // Role-engine capabilities (NOT an owner check — the owner is just the top role). Each management
    // affordance gates on the matching bit. Falls back to no-caps on error (hide everything management).
    let caps = {};
    try { caps = await invoke('get_community_capabilities', { communityId }); } catch (_) {}
    // Tag the overview with its community so the realtime `community_refreshed` listener knows to re-render
    // it when a control change (ban/role/metadata/mode) lands live.
    VectorSvelte.setOverviewGroup(communityId);
    if (!fCommunityOverviewMounted) mountCommunityOverview();
    VectorSvelte.setOverview({
        chatId: chat.id,
        communityId,
        name: cf.name || `Community ${chat.id.substring(0, 10)}...`,
        description: cf.description || '',
        avatarSrc: chat.metadata?.avatar_cached ? convertFileSrc(chat.metadata.avatar_cached) : null,
        muted: !!chat.muted,
        isOwner: cf.is_owner === 'true',
        isV2: cf.proto_version === '2',
        caps,
        raid: null,
        migration: null,
        upload: null,
    });
    // The narrow layout has no community header to hang a pip on, so the Moderate button carries the alarm.
    if (caps.ban) {
        invoke('check_community_raid', { communityId }).then(v => {
            if (v?.detected && VectorSvelte.overviewState().communityId === communityId) VectorSvelte.setOverview({ raid: { suspects: v.suspects } });
        }).catch(() => {});
    }
    loadMigrationStatus(communityId);

    // Member list = observed participants (best-effort): everyone who has posted across the
    // Community's channels. Lurkers and link-joiners who haven't spoken don't appear (membership
    // isn't authoritative). Join announcements (presence) surface here too once that ships.
    if (groupMembersEl()) {
        const searchEl = groupSearchEl();
        const myNpub = arrProfiles.find(p => p.mine)?.id;
        const ownerNpub = cf.owner_npub || null; // PROVEN owner (verified attestation), or null
        // Cache-first: the last known roster paints instantly (no "Loading members…" flash
        // on reopen); the authoritative fetches run AFTER that first paint and land through
        // setRoster only on a real change.
        const hadCache = communityMembersCache.has(communityId);
        let memberList = communityMembersCache.get(communityId) || [];
        // Admins ride the community's channel-chat metadata (applyCommunityAdmins) — the
        // same session cache the in-chat tags read.
        let adminNpubs = (chat.metadata?.admins || []).slice();
        let bannedList = [];
        // The role hierarchy, once it lands. Sections fall back to Admin/Members until then.
        let roleGraph = communityRoleGraphCache.get(communityId) || null;
        // On a live refresh (preserveSearch), keep the active filter; on a fresh open, start clean.
        if (searchEl && !preserveSearch) searchEl.value = '';

        // The roster island (src/components/people/MemberRoster.svelte) owns the member
        // DOM; this side seeds it, feeds it the authoritative lists, and mirrors the
        // member-driven changes it reports back into the session caches. A live refresh
        // feeds the mounted island; a fresh open or another community remounts.
        if (!groupRoster || groupRosterCommunityId !== communityId || !preserveSearch) {
            if (groupRoster) VectorSvelte.unmountComponent(groupRoster);
            groupRosterCommunityId = communityId;
            groupRoster = VectorSvelte.mountMemberRoster(groupMembersEl(), {
                communityId, myNpub, ownerNpub, caps,
                profiles: [...arrProfiles],
                members: memberList, admins: adminNpubs, banned: bannedList, roleGraph,
                loading: !hadCache,
                h: {
                    invoke, popupConfirm, escapeHtml, showToast, showContextMenu,
                    attachLongPressContextMenu, showMiniProfile, getProfileAvatarSrc, getProfile,
                    createPlaceholderAvatar, twemojify, renderCustomEmojiShortcodes, showGlobalTooltip, hideGlobalTooltip,
                    applyCommunityAdmins, dmsgClearDeleteMetaCache, refreshCommunityMemberCount,
                    memberSectionClosed, setMemberSectionClosed,
                },
                onChange: ({ members }) => {
                    communityMembersCache.set(communityId, members);
                    communityMemberCounts.set(communityId, members.length);
                    VectorSvelte.touchCommunity(communityId);
                },
            });
            if (searchEl) groupRoster.setFilter(searchEl.value || '');
        }
        const roster = groupRoster;

        // Authoritative fetches — AFTER the cached paint, so the panel opens fully rendered.
        // The island re-derives only when the roster/admins/banlist/graph actually differ.
        const rosterPrint = () => JSON.stringify([
            memberList.map(m => m.npub).sort(),
            [...adminNpubs].sort(),
            [...bannedList].sort(),
            roleGraph,
        ]);
        const cachedPrint = rosterPrint();
        try { memberList = await invoke('get_community_members', { communityId }); } catch (_) {}
        // Cache the count for the header/overview subtext (the overview's own authoritative fetch).
        communityMembersCache.set(communityId, memberList);
        communityMemberCounts.set(communityId, memberList.length);
        _communityCountLastFetch.set(communityId, Date.now());
        try { adminNpubs = await invoke('get_community_admins', { communityId }); } catch (_) {}
        try {
            roleGraph = await invoke('get_community_role_graph', { communityId });
            communityRoleGraphCache.set(communityId, roleGraph);
        } catch (_) {}
        // Cache admins onto this community's channel chats so message rendering can chip
        // @everyone from admin senders (owner is handled separately via owner_npub).
        applyCommunityAdmins(communityId, adminNpubs);
        // The banlist (for the unban list), shown to anyone who can BAN.
        if (caps.ban) { try { bannedList = await invoke('get_community_banlist', { communityId }); } catch (_) {} }
        // The user may have switched to another community's overview mid-fetch — don't
        // paint this one's roster (or subtext) over it. The panel carries the COMMUNITY
        // id (re-tagged right after open for the realtime listener), never chat.id here.
        if (VectorSvelte.overviewState().groupId !== communityId || groupRoster !== roster) return;
        VectorSvelte.touchCommunity(communityId);
        if (!hadCache || rosterPrint() !== cachedPrint) {
            roster.setRoster({ members: memberList, admins: adminNpubs, banned: bannedList, roleGraph });
        }

        // Resolve unknown member + banned-member profiles (name/avatar), then push the
        // new snapshot once.
        const unknowns = [...memberList.map(m => m.npub), ...bannedList].filter(np => !arrProfiles.some(p => p.id === np) && !strangerProfileRequested.has(np));
        unknowns.forEach(np => strangerProfileRequested.add(np));
        if (unknowns.length) {
            Promise.allSettled(unknowns.map(np => invoke('load_profile', { npub: np }))).then(() => {
                if (groupRoster === roster) roster.setProfiles([...arrProfiles]);
            });
        }

        if (searchEl) {
            // Hide the whole search row (icon + input), not just the input — else the magnifying glass
            // hovers orphaned above an empty member list.
            const searchContainer = searchEl.parentElement;
            if (searchContainer) searchContainer.style.display = memberList.length ? '' : 'none';
            searchEl.oninput = () => roster.setFilter(searchEl.value || '');
        }
    }

}

/**
 * The Community invite panel: generate/copy/revoke shareable links, and invite by npub.
 * @param {Chat} chat - The Community channel chat
 */
/**
 * A non-interactable, unclosable progress modal that guides the user through a multi-second rekey
 * (privatize / private-ban). Listens to `community_rekey_progress` (emitted per phase by the backend:
 * reroll → per-member key prep → send → per-edition repost → finalize) and fills a determinate ring.
 * Awaits the listener registration before returning so early phases aren't missed. Returns { finish, close }.
 */
async function showRekeyProgressModal(title, eventName = 'community_rekey_progress') {
    VectorSvelte.setRekey({ open: true, title: title || 'Updating community keys', pct: 0, step: 'Starting...' });
    const setProgress = (pct, label) => {
        const patch = { pct: Math.max(0, Math.min(100, pct | 0)) };   // the ring's transition sweeps to it
        if (label) patch.step = label;
        VectorSvelte.setRekey(patch);
    };
    // Register BEFORE the caller invokes the op, so we don't miss the opening phases.
    const unlisten = await listen(eventName, (evt) => {
        const { pct, label } = evt.payload || {};
        setProgress(typeof pct === 'number' ? pct : 0, label);
    });
    return {
        // Fill to 100% with a closing label and hold so the sweep and count-up finish before we close.
        finish: async (label) => { setProgress(100, label || 'Done!'); await new Promise(r => setTimeout(r, 700)); },
        close: () => { unlisten(); VectorSvelte.setRekey({ open: false }); },
    };
}

/**
 * Drop a community from this client: close it if open, forget its channel rows,
 * and land back on the list. Local only — the network side is the caller's
 * (leave_community / delete_community), which must have succeeded first.
 */
async function tearDownCommunityLocally(communityId) {
    const goneChannelIds = new Set(
        arrChats.filter(c => c.metadata?.custom_fields?.community_id === communityId).map(c => c.id)
    );
    if (goneChannelIds.has(strOpenChat)) await closeChat();
    arrChats = arrChats.filter(c => c.metadata?.custom_fields?.community_id !== communityId);
    VectorSvelte.showPane('groupOverview', false);
    VectorSvelte.setOverviewGroup(null);
    listChanged();
    openChatlist();
}

/**
 * Leave a community, or — as its owner — end it for everyone. Top-level because
 * two surfaces offer it: the community's header menu in widescreen, and the
 * details pane's button in the narrow layout, which has no such header. One
 * implementation, so the confirm wording and the teardown can't diverge between
 * them.
 */
async function communityLeaveOrDelete(chat) {
    const cf = chat?.metadata?.custom_fields || {};
    const communityId = cf.community_id;
    const name = cf.name || 'this community';
    if (!communityId) return;

    if (cf.is_owner === 'true') {
        // Type-to-confirm: it ends the community for everyone, irreversibly, so it
        // must not be reachable by a mis-click.
        const typed = await popupConfirm(
            'Delete this community?',
            `This permanently ends "<b>${escapeHtml(name)}</b>" for everyone, including you. No new messages can be sent and no one can rejoin. People can still delete their own past messages. This cannot be undone.<br><br>Type the community name to confirm:`,
            false, name, 'vector_warning.svg');
        if (typed === false) return;
        if (String(typed).trim() !== name) {
            await popupConfirm('Not Deleted', 'The name did not match, so nothing was changed.', true, '', 'vector_warning.svg');
            return;
        }
        try {
            await invoke('delete_community', { communityId });
            await tearDownCommunityLocally(communityId);
        } catch (e) {
            await popupConfirm('Failed to Delete', escapeHtml(String(e)), true, '', 'vector_warning.svg');
        }
        return;
    }

    const confirmed = await popupConfirm('Leave Community', `Leave "<b>${escapeHtml(name)}</b>"? You'll need a new invite to rejoin.`, false, '', 'vector_warning.svg');
    if (!confirmed) return;
    try {
        await invoke('leave_community', { communityId });
        await tearDownCommunityLocally(communityId);
    } catch (e) {
        await popupConfirm('Failed to Leave', escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

async function openCommunityInvitePanel(chat) {
    const communityId = chat.metadata?.custom_fields?.community_id;
    if (!communityId) return;

    let busy = false; // a critical op (link create / revoke+rekey / direct invite) is in flight: lock the panel
    let unlistenRefresh = null; // community_refreshed subscription, torn down on dismiss
    const dismiss = () => {
        if (unlistenRefresh) { unlistenRefresh(); unlistenRefresh = null; }
        popBack('community-invite');
        VectorSvelte.setInviteModal({ open: false });
    };

    let statusTimer = null;
    const setStatus = (msg, isError) => {
        clearTimeout(statusTimer);   // a new message cancels any pending auto-dismiss
        VectorSvelte.setInviteModalStatus(msg, isError);
    };
    // Lock the ENTIRE panel during a critical op (the controls disable and the backdrop stops
    // dismissing), so a link create / revoke (which re-keys) / direct invite can't be raced or
    // interrupted half-applied.
    const setBusy = (on) => {
        busy = on;
        VectorSvelte.setInviteModal({ busy: on });
        VectorSvelte.ilSetBusy(on);
    };

    // Track the GLOBAL link state (across every creator, §10) so the create/revoke handlers know when a
    // click crosses the Public⇄Private boundary. The mode is the folded registry, NOT just my own links —
    // another admin's live link keeps the community Public even when I hold none.
    let currentLinkCount = 0;     // MY own links — LOCAL DB, never lags a fresh create/revoke
    let otherCreatorLinkCount = 0; // OTHER creators' links per the folded registry (the remote part)
    let communityIsPublic = false; // the folded mode itself — the ONLY thing the Public⇄Private confirm may gate on
    const renderLinks = async () => {
        let links = [];
        try { links = await invoke('list_public_invites', { communityId }); } catch (_) {}
        currentLinkCount = links.length;
        // §10 computed mode + per-creator breakdown from the folded registry (the authoritative source).
        let summary = { is_public: links.length > 0, creators: [] };
        try { summary = await invoke('get_community_invite_summary', { communityId }); } catch (_) {}
        otherCreatorLinkCount = (summary.creators || [])
            .filter(c => c.npub !== strPubkey)
            .reduce((n, c) => n + (c.count || 0), 0);
        communityIsPublic = !!summary.is_public;
        VectorSvelte.ilSet(links, summary);
    };

    const revokeLink = async (link) => {
        // Revoking the last GLOBAL link (across every creator) flips the community back to Private —
        // a re-founding rekey that cuts off link-joined lurkers. Confirm + warn (it's slow). If
        // another creator still has a link, this revoke is a quiet, instant edit (mode stays Public).
        // Mirror the backend's would_empty_aggregate: my-last-link is LOCAL truth (currentLinkCount,
        // never lags a fresh create), others' links are the folded-registry remote part — predicting
        // off the registry's count of MY OWN links would miss the modal when the fold lags my create.
        const wouldPrivatize = currentLinkCount === 1 && otherCreatorLinkCount === 0;
        if (wouldPrivatize) {
            const ok = await popupConfirm('Make community private?',
                'Revoking the last invite link makes this community <b>private</b> again. This can take a few seconds.',
                false, '', 'vector_warning.svg', '', 'Make private');
            if (!ok) return;
        }
        setBusy(true); // lock the whole panel — revoking the last link re-keys, a critical op
        VectorSvelte.ilSetRevoking(link.token);
        // Privatizing re-keys (multi-second): show the guided progress ring. A plain revoke is quick.
        const prog = wouldPrivatize ? await showRekeyProgressModal('Making community private') : null;
        if (!wouldPrivatize) setStatus('Revoking…');
        try {
            await invoke('revoke_public_invite', { communityId, token: link.token });
            if (prog) await prog.finish('Community is now private');
            setStatus('');
            setBusy(false);
            if (prog) prog.close();
            await renderLinks();
        } catch (e) {
            setBusy(false);
            if (prog) prog.close();
            setStatus('');
            if (wouldPrivatize) {
                // Privatizing re-keys; on a bunker account that fails with a long explanation —
                // show it as a persistent notice rather than a one-line status that scrolls away.
                await popupConfirm("Couldn't make private", escapeHtml(String(e)), true, '', 'vector_warning.svg');
            } else {
                setStatus(String(e), true);
            }
        } finally {
            VectorSvelte.ilSetRevoking(null);
        }
    };

    const createLink = async () => {
        // The FIRST link ANYWHERE (across all creators) flips a private community to Public (anyone with the
        // link can join). Confirm that boundary crossing; if it's already Public (someone holds a link), a
        // new link doesn't change the mode, so skip the warning. Gate on the folded MODE, never on a derived
        // count: the per-creator breakdown drops any creator whose npub won't parse, so it can read 0 while
        // the community is genuinely Public — and then this warns on a boundary that isn't being crossed.
        if (!communityIsPublic) {
            const ok = await popupConfirm('Make community public?',
                'Creating an invite link makes this community <b>public</b>: anyone with the link can join. You can make it private again later by revoking every link.',
                false, '', 'vector_warning.svg', '', 'Make public');
            if (!ok) return;
        }
        // Optional label — the attribution bucket ("Reddit", "Conf"). Shows up as "joined via <label>".
        const labelInput = await popupConfirm('Label this link', 'Optional. A label lets you see which link people join through (e.g. "Reddit", "Twitter"). Leave blank to skip.', false, 'Label (optional)', '', '', 'Create link');
        if (labelInput === false) return; // cancelled
        const label = (typeof labelInput === 'string' && labelInput.trim()) ? labelInput.trim() : null;
        setBusy(true); // lock the panel — the FIRST link flips public + the publish is a critical op
        VectorSvelte.ilSetCreating(true); setStatus('Creating link...');
        try { await invoke('create_public_invite', { communityId, expiresInSecs: null, label }); setStatus(''); }
        catch (err) { setStatus(String(err), true); }
        finally { setBusy(false); VectorSvelte.ilSetCreating(false); await renderLinks(); }
    };

    VectorSvelte.ilReset();
    await renderLinks();
    // Live-refresh when a control change folds in (a remote create/revoke by another admin, or our own
    // privatize re-founding), so the mode pill + per-creator counts update without a manual close/reopen.
    // Skipped while a local critical op is in flight (its own handler re-renders on completion).
    unlistenRefresh = await listen('community_refreshed', (evt) => {
        const cid = evt.payload?.community_id || evt.payload;
        if (cid === communityId && !busy) renderLinks();
    });

    // ── Direct Invites: a multi-select contact list (DM contacts) with paste-to-add ──
    const myNpub = arrProfiles.find(p => p.mine)?.id;
    // Banned npubs can't be invited (§7) — hide them from the picker (the backend also refuses). Empty if
    // we lack the ban permission to read the list (then the backend refusal is the only guard).
    let bannedSet = new Set();
    try { bannedSet = new Set(await invoke('get_community_banlist', { communityId })); } catch (_) {}
    // Existing members can't be invited again — hide them (they're already inside). Roster = observed
    // members ∪ owner ∪ admins (owner/admins may predate activity-based membership).
    const memberSet = new Set();
    try {
        for (const m of await invoke('get_community_members', { communityId })) memberSet.add(m.npub);
    } catch (_) {}
    const ownerNpub = chat.metadata?.custom_fields?.owner_npub;
    if (ownerNpub) memberSet.add(ownerNpub);
    for (const a of (chat.metadata?.admins || [])) memberSet.add(a);

    // The contact picker (people/ContactPicker) owns its dialog-local state; this side drives it
    // through the instance's exports (setFilter / addStranger / select / setProfiles / reset /
    // getSelection) and morphs the footer CTA from the onSelectionChange callback.
    const cl = () => VectorSvelte.inviteModalPicker();
    const contactProps = {
        profiles: arrProfiles,
        getProfile: (npub) => getProfile(npub),
        myNpub,
        banned: [...bannedSet],
        members: [...memberSet],
        dmNpubs: await fetchDmContacts(),
        chatTsById: new Map(arrChats.map(c => [c.id, getChatSortTimestamp(c)])),
        avatarSrc: (p) => (p ? getProfileAvatarSrc(p) : null) || null,
        makePlaceholder: () => createPlaceholderAvatar(false, 25),
        twemojify: (el) => twemojify(el),
        showTooltip: (text, el) => showGlobalTooltip(text, el),
        hideTooltip: () => hideGlobalTooltip(),
        // Plain hex+alpha gradient (like Create Group's mouseenter handler) — a cheap cached
        // layer. color-mix() here re-rasterized every frame under the opacity fade -> avatar flicker.
        hoverBg: 'rgba(255, 255, 255, 0.085)',
        // The footer CTA morphs with the selection: gray "Done" (dismiss) when nothing's
        // picked, accent "Invite N" (send) once contacts are selected.
        onSelectionChange: (sel) => VectorSvelte.setInviteModal({ selected: sel.size }),
    };

    // Typing filters; a pasted/typed valid npub gets added to the list and auto-selected — unless it's
    // me, a banned npub, or someone already in the community (can't invite any of them).
    const searchInput = (value) => {
        const picker = cl();
        if (!picker) return;
        picker.setFilter(value || '');
        const np = extractNpub(value || '');
        if (np && np !== myNpub && !bannedSet.has(np) && !memberSet.has(np)) {
            // Strangers = anyone who isn't an existing DM contact; the contacts loop only
            // renders DM contacts, so a cached-but-never-DM'd profile must ride the stranger
            // path or it shows nowhere. Fetch the profile only when we don't already have it.
            const isDmContact = arrChats.some(c => c.chat_type === 'DirectMessage' && c.id === np);
            if (!isDmContact) {
                picker.addStranger(np);
                if (!arrProfiles.some(p => p.id === np) && !strangerProfileRequested.has(np)) {
                    strangerProfileRequested.add(np);
                    invoke('load_profile', { npub: np }).then(() => cl()?.setProfiles([...arrProfiles])).catch(() => {});
                }
            } else {
                picker.select(np);
            }
        }
    };

    const cta = async () => {
        if (busy) return;
        const targets = [...(cl()?.getSelection() || [])];
        if (!targets.length) {   // "Done" — nothing selected, just close the panel
            dismiss();
            return;
        }
        // "Invite N" — send; the cleared selection then morphs the button back to "Done".
        VectorSvelte.setInviteModal({ ctaBusy: true });
        setStatus(`Inviting ${targets.length} ${targets.length === 1 ? 'person' : 'people'}…`);
        let ok = 0, fail = 0;
        for (const np of targets) {
            try { await invoke('invite_to_community', { communityId, inviteeNpub: np }); ok++; }
            catch (_) { fail++; }
        }
        VectorSvelte.setInviteModal({ ctaBusy: false });
        if (fail === 0) {
            setStatus(`Invited ${ok} ${ok === 1 ? 'person' : 'people'}!`);
            cl()?.reset();
            VectorSvelte.setInviteModal({ search: '' });
            statusTimer = setTimeout(() => setStatus(''), 3000);   // success toast collapses itself after 3s
        } else {
            setStatus(`Invited ${ok}, ${fail} failed.`, ok === 0);
        }
    };

    VectorSvelte.setInviteModalHandlers({
        close: () => { if (!busy) dismiss(); },
        cta,
        searchInput,
        links: { myNpub: strPubkey, name: systemEventName, create: createLink, revoke: revokeLink },
        contactProps,
    });
    // Every local read is in: open in one paint.
    VectorSvelte.setInviteModal({ open: true, busy: false, ctaBusy: false, name: chat.metadata.custom_fields.name || 'Community', status: { text: '', error: false }, selected: 0, search: '' });
    pushBack('community-invite', dismiss);
}
