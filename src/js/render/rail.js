/**
 * Widescreen rail shortcuts: recent DMs over recent communities, sitting between the
 * logo and the nav tabs.
 *
 * Derived from the same `arrChats` order the chat list renders (newest-first), so this
 * is rebuilt from `renderChatlist`'s tail and inherits its state-hash gate for free.
 * Communities use the list's community-wide badge, so a shortcut flags unread sitting
 * in a collapsed channel.
 */

/** How many DMs the rail keeps. Communities below it are unbounded (the strip scrolls). */
const WS_RAIL_DM_COUNT = 5;

/** Depth of the strip's bottom fade with a full screen of scroll still below. */
const WS_RAIL_FADE_MAX = 24;

/** Last depth written, so a scroll frame that changes nothing writes nothing. */
let nRailFadeDepth = -1;

/**
 * Give the fade the depth of the scroll that remains: the full ramp mid-strip,
 * nothing once you're resting at the bottom, and 1:1 with the last 24px in
 * between — so the scroll itself is the animation and the final row is never
 * left dimmed.
 */
function syncRailFade() {
    const domRail = document.getElementById('ws-rail-shortcuts');
    if (!domRail) return;
    const nBelow = domRail.scrollHeight - domRail.clientHeight - domRail.scrollTop;
    const nDepth = Math.round(Math.max(0, Math.min(WS_RAIL_FADE_MAX, nBelow)));
    if (nDepth === nRailFadeDepth) return;
    nRailFadeDepth = nDepth;
    domRail.style.setProperty('--ws-rail-fade', nDepth + 'px');
}

// Scroll is the common case; the strip also changes height when the rail
// collapses or the window resizes, which no scroll event reports.
(() => {
    const domRail = document.getElementById('ws-rail-shortcuts');
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
    for (const chat of arrChats) {
        if (chatIsGroup(chat)) {
            if (!chat.metadata?.custom_fields?.community_id) continue;
            if (!isPrimaryChannelChat(chat)) continue;
            communityChats.push(chat);
            continue;
        }
        if (!chat.messages.length || chat.id === strPubkey) continue;
        if (getProfile(chat.id)?.is_blocked) continue;
        if (dmChats.length < WS_RAIL_DM_COUNT) dmChats.push(chat);
    }

    dms.replaceChildren(...dmChats.map(chat => buildRailItem(chat, false)));
    spaces.replaceChildren(...communityChats.map(chat => buildRailItem(chat, true)));
    // The rule between the groups only earns its keep when both sides have rows.
    dms.classList.toggle('has-rows', dmChats.length > 0);
    spaces.classList.toggle('has-rows', communityChats.length > 0);
    // New rows change what's below without moving the strip's own box, which is
    // the one case the ResizeObserver can't see.
    syncRailFade();
    markRailShortcutActive();
    syncRailHeadBadge();
}

/**
 * Badge the home mark while an unanswered invite waits in the DM list. Invites
 * only render there, so inside a community they'd otherwise be out of sight
 * with nothing pointing back at them.
 *
 * Wears the shortcut rows' own badge class, so it's a count beside the wordmark
 * expanded and a corner dot collapsed without a second set of rules.
 */
function syncRailHeadBadge() {
    const head = document.getElementById('ws-rail-head');
    if (!head) return;
    // Silent while the DM list is the pane on screen — the invites are right there.
    const away = typeof wsListCommunityId === 'function' && !!wsListCommunityId();
    const count = away ? arrCommunityInvites.length : 0;
    let badge = document.getElementById('ws-rail-head-badge');
    if (!count) {
        badge?.remove();
        return;
    }
    if (!badge) {
        badge = document.createElement('span');
        badge.id = 'ws-rail-head-badge';
        badge.className = 'ws-rail-item-badge';
        head.appendChild(badge);
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
    if (unread) {
        const badge = document.createElement('span');
        badge.className = 'ws-rail-item-badge' + (chat.muted ? ' muted' : '');
        badge.textContent = unread > 99 ? '99+' : String(unread);
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
