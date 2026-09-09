/**
 * Chat list orchestration: the Svelte island's mount host and legacy satellites.
 *
 * - `mountChatlist()` — mounts the island (src/components/chatlist/) once at boot.
 * - the mutators (`chatChanged`, `communityChanged`, `profileChanged`, `listChanged`,
 *   `invitesChanged`, `openChatChanged`, `paneChanged`) — the vanilla side names WHAT
 *   changed; each touches the matching signal and the island re-derives only the
 *   DOM that depends on it. Nothing calls "render" any more.
 * - `updateChatlistPreview` / `updateChatlistTimestamps` — legacy call-site shims.
 * - unread counting (`computeRowBadgeCount` & co.) — shared by the island via the
 *   mount-time `h` helper bundle and by every unread indicator outside the list.
 */

/**
 * Whether a Community chat is the one row its community gets in the list.
 *
 * Every channel is registered and synced, but this release renders a community as a
 * SINGLE row: its primary channel ("general", else the first one). The backend stamps
 * `primary_channel` onto every channel row, so a sibling is any row whose own id isn't
 * that value. Rows written before the stamp existed fall back to rendering — better a
 * duplicate row than a community that silently disappears from the list.
 */
function isPrimaryChannelChat(chat) {
    const cf = chat?.metadata?.custom_fields;
    if (!cf) return true;
    if (cf.primary_channel) return cf.primary_channel === chat.id;
    // Unstamped rows (grafted before the backend stamped them): the community's
    // primary is its "general", else its first row. One head, never one per channel.
    const communityId = cf.community_id;
    if (!communityId) return true;
    const rows = arrChats.filter(c => c.metadata?.custom_fields?.community_id === communityId);
    const head = rows.find(c => (c.metadata.custom_fields.channel_name || '').toLowerCase() === 'general') || rows[0];
    return !head || head.id === chat.id;
}

/**
 * Whether a chat gets a row in the chat list — i.e. whether the user can SEE it.
 *
 * The single source of truth for "does this chat exist in the UI", shared by the row
 * builder and by every unread indicator. Anything invisible here must not be counted
 * anywhere, or an indicator lights for a chat the user cannot open to clear: a blocked
 * DM or a sibling channel has no row, so its unread is unreachable.
 *
 * Keep this as the ONLY definition. The back-chevron dot previously had its own copy of
 * these rules and drifted out of sync with the rows.
 */
function chatIsVisibleInList(chat) {
    if (!chat) return false;
    const isGroup = chatIsGroup(chat);
    // Own profile lives in Bookmarks/Notes, not the list.
    if (chat.id === strPubkey) return false;
    if (isGroup) {
        // A Community row with no owning community is a bare persistence anchor.
        if (!chat.metadata?.custom_fields?.community_id) return false;
        // Sibling channels stay synced and addressable but get no row of their own.
        if (!isPrimaryChannelChat(chat)) return false;
        return true;
    }
    // DMs appear once they have content, and blocked senders never appear.
    if (chat.messages.length === 0) return false;
    if (getProfile(chat.id)?.is_blocked) return false;
    return true;
}

/**
 * The raw page state the island re-derives from. The bundle is an IIFE with its own
 * scope, so it can't see the classic scripts' global lexical bindings (arrChats is
 * REASSIGNED wholesale on init/hot-reload) — it re-pulls through this closure on every
 * store bump instead.
 */
function chatlistSnapshot() {
    return {
        chats: arrChats,
        // Copy: invites are spliced in place, and an {#each} source needs a fresh
        // reference per invalidation or additions/removals never re-diff.
        invites: [...arrCommunityInvites],
        pinned: arrPinnedChats,
    };
}

/**
 * Every page helper the island calls, handed over once at mount. Keeping the bundle
 * prop-driven (rather than reaching for window.X) preserves one seam to audit when
 * the island takes over more of the render tree.
 */
function chatlistHelpers() {
    return {
        // membership + row policy
        chatIsVisibleInList,
        chatIsGroup,
        isPrimaryChannelChat,
        getProfile,
        getName,
        chatPinKey,
        computeListRowBadgeCount,
        computeRowBadgeCount,
        generateChatPreviewText,
        // avatars + text finishing
        convertFileSrc,
        getProfileAvatarSrc,
        twemojify,
        timeAgo,
        renderCustomEmojiShortcodes,
        // row chrome
        attachLongPressContextMenu,
        showChatRowContextMenu: _showChatRowContextMenu,
        showGlobalTooltip,
        hideGlobalTooltip,
        // community channel lists
        communityMemberSubtext,
        getChannels: getCommunityChannels,
        communityHasChannelList,
        channelsShown: communityChannelsShown,
        communityChannelsShown,
        canAddChannels: communityCanAddChannels,
        toggleCommunityExpanded,
        sectionClosed: channelSectionClosed,
        toggleSection: toggleChannelSection,
        chatById: (id) => arrChats.find(c => c.id === id) || null,
        countPingMessages,
        isPrimaryChannelId: (id) => arrChats.some(c => c.id === id && c.metadata?.custom_fields?.primary_channel === id),
        openChannel: openCommunityChannel,
        // A tap that dismissed an open context menu must only close it, not also open the
        // chat behind it; the trailing tap a long-press synthesises is swallowed the same way.
        // An invite row and a community still joining (locked until it is readable) stay put.
        rowClick: (vm) => {
            if (wasContextMenuJustDismissed()) return;
            if (Date.now() - (window._chatRowMenuAt || 0) < 500) { window._chatRowMenuAt = 0; return; }
            if (vm.joining) return;
            openChat(vm.chat.id);
        },
        createChannel: promptCreateChannel,
        deleteChannel: promptDeleteChannel,
        // invite rows
        cacheInviteLogo: (image) => invoke('cache_invite_logo', { image }).then(path => path ? convertFileSrc(path) : null),
        acceptInvite: acceptCommunityInvite,
        declineInvite: declineCommunityInvite,
        // empty state
        newChat: () => openNewChat(),
        openHub: () => openUrl('https://vectorapp.io/hub'),
        bindViktor,
        // widescreen
        wsMarkActiveRow,
        // side-effectful loads
        ensureCommunityPreviewActivity,
    };
}

/** The mounted island instance (one per page life; session swaps reload the page). */
let chatlistIsland = null;

/**
 * Mount the chat-list island once boot has the state ready. From here on nothing
 * "renders" the list: the mutators below name what changed and the island
 * re-derives exactly the rows that depend on it.
 */
function mountChatlist() {
    if (chatlistIsland || fInit) return;
    ensureListSignals();
    sortChats();
    VectorSvelte.setScreen('chatlist', { h: chatlistHelpers(), snapshot: chatlistSnapshot });
    chatlistIsland = true;
    VectorSvelte.setScreen('communityHead', {
            h: {
                primaryChat: (id) => arrChats.find(c => communityIdOfChat(c) === id && isPrimaryChannelChat(c))
                    || arrChats.find(c => communityIdOfChat(c) === id),
                convertFileSrc,
                twemojify,
                communityMemberSubtext,
                raidAlert: (id) => { const v = communityRaidAlerts.get(id); return v && (v.detected || v.suspects > 0) ? v : null; },
                refreshMemberCount: (id) => refreshCommunityMemberCount(id),
                refreshRaidAlert: (id) => refreshCommunityRaidAlert(id),
                openCommunityMenu: (chat, e) => openCommunityMenu(chat, e),
            },
    });
    paneChanged();
    VectorSvelte.setOpenChat(strOpenChat);
    renderRailShortcuts();
}

// ── mutators: the vanilla side names WHAT changed ──

/**
 * One chat changed in place (a message, its unread, a typer, its name, its mute):
 * its row re-derives and the order re-diffs if it moved. A community channel also
 * touches its community, whose single row aggregates every channel.
 */
function chatChanged(chatOrId) {
    const chat = typeof chatOrId === 'string' ? arrChats.find(c => c.id === chatOrId) : chatOrId;
    if (!chat) return;
    touchChatRow(chat);
    reorderChatlist();
}

/** A community's identity, channels, caps or expansion changed: its row and every channel row. */
function communityChanged(communityId) {
    if (!communityId) return;
    VectorSvelte.touchCommunity(communityId);
    for (const c of arrChats) {
        if (c.metadata?.custom_fields?.community_id === communityId) VectorSvelte.touchChat(c.id);
    }
    reorderChatlist();
}

/** Something that feeds every community's badge changed (a sender mute or block). */
function communitiesChanged() {
    const seen = new Set();
    for (const c of arrChats) {
        const id = c.metadata?.custom_fields?.community_id;
        if (id && !seen.has(id)) { seen.add(id); VectorSvelte.touchCommunity(id); }
    }
    reorderChatlist();
}

/**
 * A profile changed (name, avatar, block flag): its DM row re-derives, the list
 * re-diffs in case the block flag changed its membership, and community badges
 * re-count since blocked authors are excluded.
 */
function profileChanged(npub) {
    if (!npub) return;
    VectorSvelte.touchProfile(npub);
    communitiesChanged();
}

/** Chats were added, removed, pinned or unpinned: the list's shape re-diffs unconditionally. */
function listChanged() {
    if (fInit) return;
    ensureListSignals();
    sortChats();
    VectorSvelte.reorderChatlist();
    renderRailShortcuts();
}

/** Pending community invites arrived, were accepted, declined or purged. */
function invitesChanged() {
    VectorSvelte.touchInvites();
}

/** The open chat changed: the active row, the rail's shortcut and the pane mode derive from it. */
function openChatChanged() {
    VectorSvelte.setOpenChat(strOpenChat);
    paneChanged();
}

/** The list pane's mode changed (widescreen entered/left, or the open community changed). */
function paneChanged() {
    const communityId = typeof wsListCommunityId === 'function' ? wsListCommunityId() : null;
    const dmsOnly = !communityId && typeof wsActive === 'function' && wsActive();
    VectorSvelte.setPane(communityId, dmsOnly);
}

/**
 * One chat changed in place: re-derive its row only (no order check). Prefer
 * `chatChanged` unless you know the change cannot move the chat.
 */
function touchChatRow(chat) {
    if (!chat) return;
    VectorSvelte.touchChat(chat.id);
    const communityId = chat.metadata?.custom_fields?.community_id;
    if (communityId) VectorSvelte.touchCommunity(communityId);
}

/**
 * Re-sort and re-diff the list's order and membership without re-deriving any row.
 * Most changes land in a chat that is already where it belongs (the top one, for a
 * live conversation): nothing moved, so the touched row's own repaint was the whole
 * job and the list keeps its derivation.
 */
function reorderChatlist() {
    if (fInit) return;
    ensureListSignals();
    const before = listShapeKey();
    sortChats();
    renderRailShortcuts();
    if (listShapeKey() === before) return;
    VectorSvelte.reorderChatlist();
}

/**
 * Register every chat, DM profile and community key with the signal layer before a
 * row can read it: a key that is first read inside a derived would not be tracked.
 */
function ensureListSignals() {
    const chatIds = [];
    const profileIds = [];
    const communityIds = new Set();
    for (const c of arrChats) {
        chatIds.push(c.id);
        const cid = c.metadata?.custom_fields?.community_id;
        if (cid) communityIds.add(cid);
        else profileIds.push(c.id);
    }
    VectorSvelte.ensureSignals({ chats: chatIds, profiles: profileIds, communities: [...communityIds] });
}

/** The list's order and membership as one string — what a reorder can change. */
function listShapeKey() {
    let key = '';
    for (const c of arrChats) if (chatIsVisibleInList(c)) key += c.id + ',';
    return key;
}

/** Viktor greets on the first paint after login; page-lifetime latch. */
let fViktorGreeted = false;

const VIKTOR_SMILE = '/icons/viktor-smile.gif';

/**
 * Idle pose, rasterised once from the smile clip's first frame — drawImage
 * of an animated image always takes frame one, so no separate still ships
 * and the idle can never drift from the clip it pauses.
 */
let viktorIdleSrc = null;
const viktorIdleReady = (() => {
    const probe = new Image();
    probe.src = VIKTOR_SMILE;
    return probe.decode().then(() => {
        const c = document.createElement('canvas');
        c.width = probe.naturalWidth;
        c.height = probe.naturalHeight;
        c.getContext('2d').drawImage(probe, 0, 0);
        viktorIdleSrc = c.toDataURL('image/png');
    }).catch(() => { viktorIdleSrc = VIKTOR_SMILE; });
})();

/**
 * Viktor's little state machine. GIFs can't be paused, so every state is a
 * file swap: idle = the smile's first frame, hover = the smile loop, click =
 * one exclamation. Leaving mid-smile lets the loop in progress finish rather
 * than cutting him off, and both clips share the idle frame at their seams.
 */
function bindViktor(img) {
    const SMILE = VIKTOR_SMILE;
    const EXCLAIM = '/icons/viktor-exclaim.gif';
    const SMILE_MS = 1500;
    const EXCLAIM_MS = 1600;
    let mode = 'idle';
    let hovering = false;
    let smileStart = 0;
    let timer = null;

    // WebKit animates GIFs on a shared document-wide clock: re-assigning the
    // same URL joins the cycle mid-flight instead of starting at frame one,
    // which reads as a snap against the still. A unique query per play forces
    // a genuine restart; the file is a local asset, so the refetch is free.
    let playSeq = 0;
    const fresh = (url) => `${url}?play=${++playSeq}`;

    const toIdle = () => { mode = 'idle'; if (viktorIdleSrc) img.src = viktorIdleSrc; };
    const smile = () => {
        clearTimeout(timer);
        mode = 'smile';
        smileStart = Date.now();
        img.src = fresh(SMILE);
    };
    const exclaim = () => {
        clearTimeout(timer);
        mode = 'exclaim';
        img.src = fresh(EXCLAIM);
        timer = setTimeout(() => (hovering ? smile() : toIdle()), EXCLAIM_MS);
    };

    // The still rasterises async on first use; paint it as soon as it lands.
    if (viktorIdleSrc) toIdle();
    else viktorIdleReady.then(() => { if (mode === 'idle') toIdle(); });

    img.addEventListener('pointerenter', (e) => {
        if (e.pointerType !== 'mouse') return;
        hovering = true;
        if (mode === 'idle') smile();
        else if (mode === 'smile') clearTimeout(timer);
    });
    img.addEventListener('pointerleave', (e) => {
        if (e.pointerType !== 'mouse') return;
        hovering = false;
        if (mode !== 'smile') return;
        // Let the loop in progress run to its end before settling to idle.
        const remainder = SMILE_MS - ((Date.now() - smileStart) % SMILE_MS);
        clearTimeout(timer);
        timer = setTimeout(() => {
            if (mode === 'smile' && !hovering) toIdle();
        }, remainder);
    });
    img.addEventListener('click', exclaim);

    // Boot greeting: one exclamation as the login fade-in lands.
    if (!fViktorGreeted) {
        fViktorGreeted = true;
        const kick = () => setTimeout(exclaim, 150);
        if (VectorSvelte.revealPending('chatList')) {
            VectorSvelte.revealPane('chatList', 'intro-anim').then(kick);
        } else {
            setTimeout(kick, 400);
        }
    }
}

/**
 * Single-row preview refresh: the row re-derives, the order re-diffs (a new last
 * message can move the chat), every other row keeps its derivation.
 * @param {string} chatId
 */
function updateChatlistPreview(chatId) {
    chatChanged(chatId);
}

/**
 * Whether a sender's DM chat is muted: a muted person is silent in every
 * chat, so their community messages don't badge either. DM ids ARE npubs.
 */
function senderIsMuted(npub) {
    return arrChats.some(c => c.muted && c.id === npub);
}

/**
 * Count the quantity of unread messages
 * @param {Chat} chat - The Chat we're checking
 * @returns {number} - The amount of unread messages, if any
 */
function countUnreadMessages(chat) {
    // If no messages, return 0
    if (!chat.messages || !chat.messages.length) return 0;

    // Walk backwards from the end to count unread messages
    // Stop when we hit: 1) our own message, or 2) the last_read message
    let unreadCount = 0;

    for (let i = chat.messages.length - 1; i >= 0; i--) {
        const msg = chat.messages[i];

        // System events (wallpaper changes, member joined/left, etc.) are
        // state notifications, not conversation — skip them entirely so they
        // can't drive the unread badge or block the walk-back from hitting a
        // real read marker.
        if (msg.system_event) {
            continue;
        }

        // If we hit our own message, stop - we clearly read everything before it
        if (msg.mine) {
            break;
        }

        // If we hit the last_read message, stop - everything at and before this is read
        if (chat.last_read && msg.id === chat.last_read) {
            break;
        }

        // Skip messages from blocked or muted users in group chats
        if (chatIsGroup(chat) && msg.npub) {
            const authorProfile = getProfile(msg.npub);
            if (authorProfile?.is_blocked) continue;
            if (senderIsMuted(msg.npub)) continue;
        }

        // Count this message as unread
        unreadCount++;
    }

    return unreadCount;
}

/**
 * Count messages in `chat` that ping the user (a direct @-mention of our
 * npub, or an @everyone from a group admin). Walks the same window as
 * `countUnreadMessages` (back to last_read or our own latest message).
 * Used for muted group rows so the badge reflects "things you'd want to
 * see" rather than the full unread count.
 */
/**
 * Resolve the badge count for a chat row:
 *  - Muted DM/single-user chat: 0 (silenced entirely).
 *  - Muted group: count only pings (mentions of us / admin @everyone).
 *  - Anything else: full unread count.
 */
function computeRowBadgeCount(chat) {
    if (chat.muted) {
        return chatIsGroup(chat) ? countPingMessages(chat) : 0;
    }
    // DB-sourced count (set by refreshUnreadCounts) is authoritative across restarts, when only
    // the last message per chat is in RAM. Fall back to the in-memory walk before the first
    // refresh lands (or if it ever failed).
    return (typeof chat.unread === 'number') ? chat.unread : countUnreadMessages(chat);
}

/**
 * The badge for a chat-list ROW. Identical to `computeRowBadgeCount` except on a
 * community's row, which represents every channel: its badge is the community total, so
 * unread in a collapsed secondary channel still flags the community.
 */
function computeListRowBadgeCount(chat) {
    const communityId = chatIsGroup(chat) && isPrimaryChannelChat(chat)
        ? chat.metadata?.custom_fields?.community_id
        : null;
    if (!communityId) return computeRowBadgeCount(chat);
    let total = 0;
    for (const c of arrChats) {
        if (c.chat_type !== 'Community') continue;
        if (c.metadata?.custom_fields?.community_id !== communityId) continue;
        total += computeRowBadgeCount(c);
    }
    return total;
}

/**
 * A community's pings across every channel it owns — the same fan-out
 * `computeListRowBadgeCount` does for unread, because a ping in a collapsed
 * channel is still someone calling your name.
 */
function computeCommunityPingCount(chat) {
    const communityId = chatIsGroup(chat) && isPrimaryChannelChat(chat)
        ? chat.metadata?.custom_fields?.community_id
        : null;
    if (!communityId) return countPingMessages(chat);
    let total = 0;
    for (const c of arrChats) {
        if (c.chat_type !== 'Community') continue;
        if (c.metadata?.custom_fields?.community_id !== communityId) continue;
        total += countPingMessages(c);
    }
    return total;
}

function countPingMessages(chat) {
    if (!chat.messages || !chat.messages.length) return 0;
    const isGroup = chatIsGroup(chat);
    const admins = chat.metadata?.admins;
    let pings = 0;
    for (let i = chat.messages.length - 1; i >= 0; i--) {
        const msg = chat.messages[i];
        if (msg.system_event) continue; // not a conversation message
        if (msg.mine) break;
        if (chat.last_read && msg.id === chat.last_read) break;
        if (isGroup && msg.npub) {
            const authorProfile = getProfile(msg.npub);
            if (authorProfile?.is_blocked) continue;
            // A muted sender never pings, even through an unmuted channel.
            if (senderIsMuted(msg.npub)) continue;
        }
        if (!msg.content) continue;
        const mentionedMe = strPubkey && msg.content.includes('@' + strPubkey);
        // Authorized @everyone = owner or admin (owner isn't in the admins list — it's its own tier).
        const everyoneAuthor = msg.npub || '';
        const mentionedEveryone = isGroup
            && /@everyone\b/.test(msg.content)
            && (admins?.includes(everyoneAuthor) || chat.metadata?.custom_fields?.owner_npub === everyoneAuthor);
        if (mentionedMe || mentionedEveryone) pings++;
    }
    return pings;
}

/**
 * Periodic tick for relative timestamps ("5m ago") and presence-dot recency, which
 * drift with wall time instead of data. Bumps the clock store the island's rows
 * already depend on — only strings that actually changed patch the DOM.
 */
function updateChatlistTimestamps() {
    VectorSvelte.bumpTimeTick();
}
