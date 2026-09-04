/**
 * Chat list orchestration: the Svelte island's mount host and legacy satellites.
 *
 * - `renderChatlist()` — invalidation pump; every mutation path bumps the shared
 *   `chatlistVersion` store (src/components/stores.js). The island (src/components/
 *   Chatlist.svelte) is the store's renderer: a keyed {#each} patches single rows
 *   instead of rebuilding the list.
 * - `renderChatlistNow` — the store's first subscriber: sorts (the one ordering
 *   chokepoint), mounts the island once, then refreshes the store-blind satellites
 *   (rail shortcuts, back-button dot) that later slices will migrate onto stores.
 * - `updateChatlistPreview` / `updateChatlistTimestamps` — legacy call-site shims;
 *   a store bump (full, but row-granular) and a clock tick respectively.
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
    const primary = chat?.metadata?.custom_fields?.primary_channel;
    return !primary || primary === chat.id;
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
    const paneCommunityId = typeof wsListCommunityId === 'function' ? wsListCommunityId() : null;
    return {
        chats: arrChats,
        // Copy: invites are spliced in place, and an {#each} source needs a fresh
        // reference per invalidation or additions/removals never re-diff.
        invites: [...arrCommunityInvites],
        pinned: arrPinnedChats,
        paneCommunityId,
        openChat: strOpenChat,
        dmsOnly: !paneCommunityId && typeof wsActive === 'function' && wsActive(),
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
        createPlaceholderAvatar,
        twemojify,
        timeAgo,
        renderCustomEmojiShortcodes,
        // row chrome
        attachLongPressContextMenu,
        showChatRowContextMenu: _showChatRowContextMenu,
        showGlobalTooltip,
        hideGlobalTooltip,
        // community channel lists (still vanilla builders; later slice)
        renderCommunityChannels,
        renderCommunityListHeader,
        channelStateHashParts,
        communityMemberSubtext,
        getCommunityChannels,
        communityHasChannelList,
        communityChannelsShown,
        communityCanAddChannels,
        toggleCommunityExpanded,
        renderCommunityInviteItem,
        // empty state
        buildChatlistEmptyState,
        buildChatlistIntro,
        // widescreen
        wsMarkActiveRow,
        // side-effectful loads
        ensureCommunityPreviewActivity,
    };
}

/** The mounted island instance (one per page life; session swaps reload the page). */
let chatlistIsland = null;

/**
 * The chatlist render pass. Runs as chatlistVersion's FIRST subscriber (registered
 * below, before the island mounts), so the sort always lands before the island
 * derives its row order.
 */
function renderChatlistNow() {
    if (fInit) return;

    // Pinned first, then newest-first with a creation/join-time fallback for
    // message-less communities — the one chokepoint that guarantees order no
    // matter which path added a chat (create, join, boot, message). Without it a
    // freshly-surfaced chat stays wherever it was appended, and a pin set by any
    // other path is undone by the next render.
    sortChats();

    // The island owns #chat-list's children from here on; data flows through the
    // store, so mounting once is enough for the page's lifetime.
    if (!chatlistIsland) {
        chatlistIsland = VectorSvelte.mountChatlist(domChatList, {
            h: chatlistHelpers(),
            snapshot: chatlistSnapshot,
        });
    }

    // The rail's shortcuts are the same data in a different shape, so they rebuild here
    // until their own slice moves them onto the store.
    renderRailShortcuts();

    // Update the back button notification
    updateChatBackNotification();
}

/**
 * Invalidate the chat list through the shared store: every row re-derives. The
 * subscription below runs synchronously (Svelte stores notify on set), so callers
 * keep the old "DOM is updated when renderChatlist() returns" contract.
 *
 * Reach for `touchChatRow` + `reorderChatlist` instead when you know WHICH chat
 * changed — that path re-derives one row and re-diffs the order, nothing else.
 */
function renderChatlist() {
    VectorSvelte.invalidateChatlist();
}

/**
 * One chat changed in place (a message, its unread, a typer, its name): re-derive
 * its row only. A community channel also touches its community, whose single row
 * aggregates every channel.
 */
function touchChatRow(chat) {
    if (!chat) return;
    VectorSvelte.touchChat(chat.id);
    const communityId = chat.metadata?.custom_fields?.community_id;
    if (communityId) VectorSvelte.touchCommunity(communityId);
}

/**
 * Re-sort and re-diff the list's order and membership without re-deriving any row.
 * Pair it with `touchChatRow` after a change that can move a chat (a new message,
 * a first message making a DM visible).
 */
function reorderChatlist() {
    if (fInit) return;
    const before = listShapeKey();
    sortChats();
    updateChatBackNotification();
    renderRailShortcuts();
    // Most changes land in a chat that is already where it belongs (the top one, for a
    // live conversation): nothing moved, so the touched row's own repaint was the whole
    // job and the list keeps its derivation.
    if (listShapeKey() === before) return;
    VectorSvelte.reorderChatlist();
}

/** The list's order and membership as one string — what a reorder can change. */
function listShapeKey() {
    let key = '';
    for (const c of arrChats) if (chatIsVisibleInList(c)) key += c.id + ',';
    return key;
}

// The first subscriber: every invalidation — from this file, main.js, or any
// js/ module — sorts and mounts before the island's own subscription re-derives
// (subscription order = registration order). The immediate subscribe-time run is
// absorbed by the fInit guard while boot is still in flight.
VectorSvelte.chatlistVersion.subscribe(() => renderChatlistNow());

/**
 * Build the empty-state placeholder shown when the chat list has no
 * chats or invites. Pulls the user toward the New Chat / Group Chat
 * buttons at the top of the screen, plus a one-tap "Share My Contact"
 * button that copies the user's vectorapp.io profile link to the
 * clipboard so they can paste it into another channel and bootstrap
 * their first conversations.
 */
function buildChatlistEmptyState() {
    const wrap = document.createElement('div');
    wrap.className = 'chatlist-get-started btn';
    wrap.setAttribute('role', 'button');
    wrap.innerHTML = `
        <div class="chatlist-get-started-badge">
            <span class="icon icon-add-user"></span>
        </div>
        <div class="chatlist-get-started-text">
            <h4>Get Started</h4>
            <p>Create your first private chat.</p>
        </div>
        <div class="chatlist-get-started-watermark">
            <span class="icon icon-add-user"></span>
        </div>
    `;
    // Rides the New Chat button's own handler, so the two can never diverge.
    wrap.addEventListener('click', () => document.getElementById('new-chat-btn')?.click());
    return wrap;
}

/**
 * Bottom-of-list welcome: Viktor points fresh accounts at the Hub. Rides the
 * list fragment, so the first real chat render sweeps it away with the rest.
 */
function buildChatlistIntro() {
    const wrap = document.createElement('div');
    wrap.className = 'chatlist-intro';
    wrap.innerHTML = `
        <img class="chatlist-intro-viktor" alt="Viktor">
        <div class="chatlist-intro-text">
            <h4>Welcome to Vector!</h4>
            <p>Feel free to <span class="chatlist-intro-link">join the public community</span> to learn more about Vector, discuss privacy, and make some new friends.</p>
        </div>
    `;
    wrap.querySelector('.chatlist-intro-link').addEventListener('click', () => {
        openUrl('https://vectorapp.io/hub');
    });

    bindViktor(wrap.querySelector('.chatlist-intro-viktor'));
    return wrap;
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
        if (domChatList.classList.contains('intro-anim')) {
            domChatList.addEventListener('animationend', kick, { once: true });
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
    const chat = arrChats.find(c => c.id === chatId);
    if (!chat) return;
    touchChatRow(chat);
    reorderChatlist();
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
