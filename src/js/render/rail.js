/**
 * Widescreen rail shortcuts: unread DMs over communities, sitting between the
 * logo and the nav tabs.
 *
 * Derived from the same `arrChats` order the chat list renders (newest-first), so this
 * is rebuilt from `renderChatlist`'s tail and inherits its state-hash gate for free.
 * Communities use the list's community-wide badge, so a shortcut flags unread sitting
 * in a collapsed channel.
 */

/** How many unread DMs the rail surfaces. The mail badge carries the rest, which is
 *  the one thing these rows cannot say. Communities below are unbounded (it scrolls). */
const WS_RAIL_DM_COUNT = 3;

/** Depth of the strip's bottom fade with a full screen of scroll still below. */
const WS_RAIL_FADE_MAX = 24;

/** Last depths written, so a scroll frame that changes nothing writes nothing. */
let nRailFadeDepth = -1;
let nRailFadeTop = -1;

/**
 * Give each fade the depth of the scroll that lies past it: the full ramp
 * mid-strip, nothing once you're resting against that end, and 1:1 with the
 * last 24px in between — so the scroll itself is the animation and neither the
 * first nor the last row is left dimmed once you've reached it.
 */
function railScroller() {
    return document.querySelector('#ws-rail-spaces .ws-rail-rows');
}

function syncRailFade() {
    const domRail = railScroller();
    if (!domRail) return;
    const nBelow = domRail.scrollHeight - domRail.clientHeight - domRail.scrollTop;
    const nDepth = Math.round(Math.max(0, Math.min(WS_RAIL_FADE_MAX, nBelow)));
    const nTop = Math.round(Math.max(0, Math.min(WS_RAIL_FADE_MAX, domRail.scrollTop)));
    if (nDepth === nRailFadeDepth && nTop === nRailFadeTop) return;
    nRailFadeDepth = nDepth;
    nRailFadeTop = nTop;
    domRail.style.setProperty('--ws-rail-fade', nDepth + 'px');
    domRail.style.setProperty('--ws-rail-fade-top', nTop + 'px');
}

// Scroll is the common case; the strip also changes height when the rail
// collapses or the window resizes, which no scroll event reports.
(() => {
    const domRail = railScroller();
    if (!domRail) return;
    domRail.addEventListener('scroll', syncRailFade, { passive: true });
    new ResizeObserver(syncRailFade).observe(domRail);
})();

function renderRailShortcuts() {
    const dms = document.getElementById('ws-rail-dms');
    const spaces = document.getElementById('ws-rail-spaces');
    if (!dms || !spaces || !wsActive()) return;

    const dmChats = [];
    const communityChats = [];
    let nUnreadDms = 0;
    for (const chat of arrChats) {
        if (chatIsGroup(chat)) {
            if (!chat.metadata?.custom_fields?.community_id) continue;
            if (!isPrimaryChannelChat(chat)) continue;
            communityChats.push(chat);
            continue;
        }
        if (!chat.messages.length || chat.id === strPubkey) continue;
        if (getProfile(chat.id)?.is_blocked) continue;
        // Unread only. A muted chat scores 0 here, so one you asked not to hear
        // about never takes one of the three slots.
        if (!computeRowBadgeCount(chat)) continue;
        nUnreadDms++;
        if (dmChats.length < WS_RAIL_DM_COUNT) dmChats.push(chat);
    }

    fillRailGroup(dms, dmChats, false);
    fillRailGroup(spaces, communityChats, true);
    // New rows change what's below without moving the strip's own box, which is
    // the one case the ResizeObserver can't see.
    syncRailFade();
    markRailShortcutActive();
    syncRailMailBadge(nUnreadDms);
}

/**
 * Replace a group's rows while leaving its label alone, and hide the whole group
 * when it has none — which takes the heading with it, so "New Messages" never
 * sits over nothing.
 */
function fillRailGroup(group, chats, isCommunity) {
    // Rows live in their own box so the heading can stay put while they scroll.
    const rows = group.querySelector('.ws-rail-rows') || group;
    rows.replaceChildren(...chats.map(c => buildRailItem(c, isCommunity)));
    group.hidden = chats.length === 0;
}

/**
 * Everything waiting behind the mail button: unread DMs plus unanswered invites.
 *
 * The rows below show three, so this is the only thing that can say there are
 * more — and invites render nowhere but the DM list, so inside a community
 * they'd otherwise be out of sight with nothing pointing back at them.
 *
 * Wears the shortcut rows' own badge class, so it's a count beside the icon
 * expanded and a corner dot collapsed without a second set of rules.
 */
function syncRailMailBadge(nUnreadDms) {
    const mail = document.getElementById('ws-rail-mail');
    if (!mail) return;
    // Invites stay silent while the DM list is the pane on screen — they're
    // right there. Unread DMs count wherever you are.
    const away = typeof wsListCommunityId === 'function' && !!wsListCommunityId();
    const count = (nUnreadDms || 0) + (away ? arrCommunityInvites.length : 0);
    let badge = document.getElementById('ws-rail-mail-badge');
    if (!count) {
        badge?.remove();
        return;
    }
    if (!badge) {
        badge = document.createElement('span');
        badge.id = 'ws-rail-mail-badge';
        badge.className = 'ws-rail-item-badge';
        mail.appendChild(badge);
    }
    badge.textContent = count > 99 ? '99+' : String(count);
}

function buildRailItem(chat, isCommunity) {
    const profile = isCommunity ? null : getProfile(chat.id);
    const name = isCommunity
        ? (chat.metadata?.custom_fields?.name || 'Community')
        : getName(profile || chat.id);

    const item = document.createElement('div');
    item.className = 'ws-rail-item btn' + (isCommunity ? ' is-community' : '');
    item.id = `ws-rail-item-${chat.id}`;
    item.title = name;
    // Selection is stamped by markRailShortcutActive once the strip exists — one
    // owner for the rule, rather than a second copy of it per row.

    const avatarSrc = isCommunity
        ? (chat.metadata?.avatar_cached ? convertFileSrc(chat.metadata.avatar_cached) : null)
        : getProfileAvatarSrc(profile);
    const avatar = avatarSrc
        ? createAvatarImg(avatarSrc, 26, isCommunity)
        : createPlaceholderAvatar(isCommunity, 26);
    avatar.classList.add('ws-rail-item-avatar');
    item.appendChild(avatar);

    const label = document.createElement('span');
    label.className = 'ws-rail-item-name cutoff';
    label.textContent = name;
    if (isCommunity || profile?.nickname || profile?.name) twemojify(label);
    item.appendChild(label);

    // Community shortcuts total their channels, so unread in a collapsed channel still shows.
    const unread = isCommunity ? computeListRowBadgeCount(chat) : computeRowBadgeCount(chat);
    // A community earns a NUMBER only when someone called your name — a ping or
    // an authorised @everyone. Ordinary unread is a dot: it says the room is
    // awake without asking you to act, which is the difference between a place
    // you belong to and a person waiting on you.
    const pings = isCommunity ? computeCommunityPingCount(chat) : unread;
    if (isCommunity && !unread) item.classList.add('is-quiet');
    if (pings || unread) {
        const badge = document.createElement('span');
        const isDot = isCommunity && !pings;
        badge.className = 'ws-rail-item-badge'
            + (isDot ? ' is-dot' : '')
            + (chat.muted && !isDot ? ' muted' : '');
        badge.textContent = isDot ? '' : (pings > 99 ? '99+' : String(pings));
        item.appendChild(badge);
    }

    // A community's shortcut returns you to the channel you left it in, not to its
    // primary every time — the rail row stands for the community, not a channel.
    item.onclick = () => openChat(
        isCommunity ? (wsChannelForCommunity(communityIdOfChat(chat)) || chat.id) : chat.id
    );
    return item;
}

/**
 * The rail row that REPRESENTS the open chat. A community has one shortcut, built
 * from its primary channel, and it stands for the whole community — so any of its
 * channels lights it. Matching the open chat's own id only ever hit `general`.
 */
function railItemIdForOpenChat() {
    if (!strOpenChat) return null;
    const chat = arrChats.find(c => c.id === strOpenChat);
    const communityId = communityIdOfChat(chat);
    if (!communityId) return `ws-rail-item-${strOpenChat}`;
    const primary = arrChats.find(c => communityIdOfChat(c) === communityId && isPrimaryChannelChat(c));
    return `ws-rail-item-${(primary || chat).id}`;
}

/** Re-stamp which shortcut is the open chat, without rebuilding the strip. */
function markRailShortcutActive() {
    const rail = document.getElementById('ws-rail-shortcuts');
    if (!rail) return;
    for (const item of rail.querySelectorAll('.ws-rail-item.active')) item.classList.remove('active');
    const id = railItemIdForOpenChat();
    if (id) document.getElementById(id)?.classList.add('active');
}
