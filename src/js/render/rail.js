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
    if (chat.id === strOpenChat) item.classList.add('active');

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

    item.onclick = () => openChat(chat.id);
    return item;
}

/** Re-stamp which shortcut is the open chat, without rebuilding the strip. */
function markRailShortcutActive() {
    const rail = document.getElementById('ws-rail-shortcuts');
    if (!rail) return;
    for (const item of rail.querySelectorAll('.ws-rail-item.active')) item.classList.remove('active');
    if (strOpenChat) document.getElementById(`ws-rail-item-${strOpenChat}`)?.classList.add('active');
}
