/**
 * Widescreen rail shortcuts: unread DMs over communities, sitting between the
 * logo and the nav tabs.
 *
 * The strip itself is a Svelte island (src/components/shell/RailShortcuts.svelte); this module mounts it,
 * keeps the scroll fade and the Chat tab's badge (chrome outside the island's target),
 * and forwards the open chat as a signal.
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
    return VectorSvelte.railEls().spacesRows;
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

/** The strip (src/components/shell/RailShortcuts.svelte) is registered once per page life. */
let railIsland = false;

/**
 * Mount the rail island once the rail exists. From then on the strip derives from
 * the chat list's signals; the legacy callers of this function are no-ops.
 */
function renderRailShortcuts() {
    if (railIsland || !wsActive()) return;
    railIsland = true;
    /**
     * RailHelpers: the widescreen rail's shortcut strip and its items.
     * @typedef {Object} RailHelpers
     * @property {(chat: object) => boolean} chatIsGroup
     * @property {(chat: object) => boolean} isPrimaryChannelChat
     * @property {(chat: object) => string|null} communityIdOfChat
     * @property {(npub: string) => object|null} getProfile
     * @property {(profileOrNpub: object|string) => string} getName
     * @property {(profile: object|null) => string|null} getProfileAvatarSrc
     * @property {(path: string) => string} convertFileSrc
     * @property {(el: Element) => void} twemojify
     * @property {(chat: object) => number} computeRowBadgeCount
     * @property {(chat: object) => number} computeRowUnreadCount
     * @property {(chat: object) => number} computeListRowBadgeCount
     * @property {(chat: object) => number} computeListRowUnreadCount
     * @property {(chat: object) => number} computeCommunityPingCount
     * @property {(chatId: string) => void} openChat
     * @property {() => void} openDmHome
     * @property {(communityId: string) => string|null} wsChannelForCommunity
     * @property {(el: Element, onMenu: (x: number, y: number) => void) => void} attachLongPressContextMenu
     * @property {(chat: object, x: number, y: number) => void} openCommunityMenu
     * @property {(chat: object, isGroup: boolean, unread: number, x: number, y: number) => void} showChatRowContextMenu
     * @property {() => void} syncRailFade
     * @property {(unreadDms: number) => void} onUnreadDms
     * @property {(source: object, target: object, live: string[]) => void} railDrop
     * @property {(folder: object, x: number, y: number) => void} openFolderMenu
     * @property {(node: HTMLElement) => void} closeMenu
     */
    VectorSvelte.setScreen('rail', {
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
            computeRowUnreadCount,
            computeListRowBadgeCount,
            computeListRowUnreadCount,
            computeCommunityPingCount,
            openChat: (id) => { if (!chatOnScreen(id)) openChat(id); },
            openDmHome: wsOpenDmHome,
            wsChannelForCommunity,
            attachLongPressContextMenu,
            // A shortcut stands for the whole community, so it opens the community's menu
            // rather than its anchoring channel's row menu.
            openCommunityMenu: (chat, x, y) => openCommunityMenu(chat, null, { x, y }),
            showChatRowContextMenu: _showChatRowContextMenu,
            syncRailFade,
            onUnreadDms: syncChatTabBadge,
            railDrop,
            openFolderMenu,
            closeMenu: hideContextMenu,
        },
        snapshot: () => ({ chats: arrChats, myNpub: strPubkey }),
    });
    VectorSvelte.setOpenChat(strOpenChat);
    loadRailLayout();
}

/**
 * The account's rail arrangement, from the local mirror so the first paint is
 * already in its order. Cleared first, so a swap never shows the last
 * account's folders while the next one's load.
 */
async function loadRailLayout() {
    VectorSvelte.setRailLayout(null);
    try {
        VectorSvelte.setRailLayout(await invoke('get_rail_layout'));
    } catch (e) {
        console.warn('[Rail] layout load failed:', e);
    }
}

/**
 * A drag on the rail let go. The backend answers with the new arrangement, which
 * also arrives as `rail_layout_updated`; setting it here saves the event's hop.
 */
async function railDrop(source, target, live) {
    try {
        VectorSvelte.setRailLayout(await invoke('rail_apply_drop', { source, target, live }));
    } catch (e) {
        showToast(String(e));
    }
}

/** Folder colours: neutral grey by default, then a spread around the wheel. */
const RAIL_FOLDER_HUES = [
    ['Red', 0], ['Orange', 28], ['Yellow', 50], ['Green', 140],
    ['Teal', 175], ['Blue', 212], ['Purple', 265], ['Pink', 320],
];

function openFolderMenu(folder, x, y) {
    const edit = async (cmd, args) => {
        try { VectorSvelte.setRailLayout(await invoke(cmd, { folderId: folder.id, ...args })); }
        catch (e) { showToast(String(e)); }
    };
    showContextMenu({
        x, y,
        items: [
            {
                label: 'Rename Folder',
                icon: 'edit',
                onClick: async () => {
                    const name = await popupConfirm('Rename Folder', '', false, folder.name || 'Folder Name');
                    if (name === false || name == null) return;
                    edit('rail_rename_folder', { name: String(name) });
                },
            },
            {
                label: 'Colour',
                icon: 'palette',
                submenu: [
                    {
                        label: 'Grey',
                        swatch: '#9a9a9a',
                        checked: folder.hue == null,
                        onClick: () => edit('rail_set_folder_hue', { hue: null }),
                    },
                    ...RAIL_FOLDER_HUES.map(([label, hue]) => ({
                        label,
                        swatch: `hsl(${hue} 65% 62%)`,
                        checked: folder.hue === hue,
                        onClick: () => edit('rail_set_folder_hue', { hue }),
                    })),
                ],
            },
            { divider: true },
            {
                label: 'Ungroup',
                icon: 'folder',
                onClick: () => edit('rail_dissolve_folder', {}),
            },
        ],
    });
}

/**
 * Everything waiting behind the Chat tab: unread DMs plus unanswered invites.
 *
 * The strip shows three unread rows, so this is the only thing that can say there
 * are more — and invites render nowhere but the DM list, so inside a community
 * they'd otherwise be out of sight with nothing pointing back at them.
 *
 * Wears the shortcut rows' own badge class, so it's a count beside the label
 * expanded and a corner dot collapsed without a second set of rules.
 */
function syncChatTabBadge(nUnreadDms) {
    // Invites stay silent while the DM list is the pane on screen, where they are
    // in view already. Unread DMs count wherever you are.
    const away = !!wsListCommunityId();
    const count = (nUnreadDms || 0) + (away ? arrCommunityInvites.length : 0);
    VectorSvelte.setChatBadge(!count ? '' : count > 99 ? '99+' : String(count));
}

/** The open chat changed: the rail's active shortcut derives from the signal. */
function markRailShortcutActive() {
    VectorSvelte.setOpenChat(strOpenChat);
}
