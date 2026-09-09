/**
 * Widescreen rail shortcuts: unread DMs over communities, sitting between the
 * logo and the nav tabs.
 *
 * The strip itself is a Svelte island (src/components/rail/); this module mounts it,
 * keeps the scroll fade and the mail badge (chrome outside the island's target), and
 * forwards the open chat as a signal.
 */

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

/** The mounted island (src/components/rail/RailShortcuts.svelte); one per page life. */
let railIsland = null;

/**
 * Mount the rail island once the rail exists. From then on the strip derives from
 * the chat list's signals; the legacy callers of this function are no-ops.
 */
function renderRailShortcuts() {
    if (railIsland) return;
    const target = document.getElementById('ws-rail-shortcuts');
    if (!target || !wsActive()) return;
    railIsland = VectorSvelte.mountRailShortcuts(target, {
        h: {
            chatIsGroup,
            isPrimaryChannelChat,
            communityIdOfChat,
            getProfile,
            getName,
            getProfileAvatarSrc,
            convertFileSrc,
            twemojify,
            computeRowBadgeCount,
            computeListRowBadgeCount,
            computeCommunityPingCount,
            openChat,
            wsChannelForCommunity,
            syncRailFade,
            onUnreadDms: syncRailMailBadge,
        },
        snapshot: () => ({ chats: arrChats, myNpub: strPubkey }),
    });
    VectorSvelte.setOpenChat(strOpenChat);
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
    // Invites stay silent while the DM list is the pane on screen, where they are
    // in view already. Unread DMs count wherever you are.
    const away = !!wsListCommunityId();
    const count = (nUnreadDms || 0) + (away ? arrCommunityInvites.length : 0);
    VectorSvelte.setMailBadge(!count ? '' : count > 99 ? '99+' : String(count));
}

/** The open chat changed: the rail's active shortcut derives from the signal. */
function markRailShortcutActive() {
    VectorSvelte.setOpenChat(strOpenChat);
}
