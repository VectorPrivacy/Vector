/**
 * Vector Widescreen — layout-mode controller.
 *
 * Owns `body.ws` (and its modifiers), the rail's collapsed state, and the
 * draggable list width. Everything visual lives in widescreen.css; this file
 * only decides *when* widescreen applies and re-homes the two DOM nodes that
 * have to physically move between layouts.
 *
 * The single source of truth for the breakpoint is WS_MIN_W below. CSS keys
 * off `body.ws` rather than its own media query, so the class and the layout
 * can never disagree mid-resize.
 */

/** Narrowest viewport that gets the multi-pane layout. */
const WS_MIN_W = 900;
/** Below this the rail auto-collapses to icons, whatever the user's pref. */
const WS_RAIL_AUTO_COLLAPSE_W = 1080;

const WS_KEY_LIST_W = 'ws_list_width';
const WS_KEY_RAIL = 'ws_rail_collapsed';

/** True while `body.ws` is applied — used by main.js to skip viewport math that
 *  assumes a pane IS the viewport (see adjustSize). */
function wsActive() {
    return document.body.classList.contains('ws');
}

/** Widescreen needs the width AND a live session: the login screen is
 *  deliberately single-pane, and pre-login there is no account chip to dock. */
function wsShouldApply() {
    return window.innerWidth >= WS_MIN_W && !fInit;
}

function wsReadListWidth() {
    const stored = parseInt(localStorage.getItem(WS_KEY_LIST_W), 10);
    return Number.isFinite(stored) ? stored : 240;
}

/** Clamp to the CSS bounds and to what the window can actually spare, so a
 *  restored width from a bigger monitor can't crush the conversation pane.
 *  Read off `body`, not the navbar: this runs before `body.ws` is applied, when
 *  the rail is still the full-width mobile tab bar, and the collapsed override
 *  of --ws-rail-w lives on the body class. */
function wsClampListWidth(px) {
    const css = getComputedStyle(document.body);
    const min = parseInt(css.getPropertyValue('--ws-list-min'), 10) || 200;
    const max = parseInt(css.getPropertyValue('--ws-list-max'), 10) || 480;
    const rail = parseInt(css.getPropertyValue('--ws-rail-w'), 10) || 200;
    const roomForConversation = 380;
    const fits = window.innerWidth - rail - roomForConversation;
    return Math.round(Math.max(min, Math.min(px, max, Math.max(min, fits))));
}

function wsApplyListWidth(px) {
    document.documentElement.style.setProperty('--ws-list-w', wsClampListWidth(px) + 'px');
}

/** Effective collapse = the user's pref OR too little width to afford labels. */
function wsApplyRailState() {
    const pref = localStorage.getItem(WS_KEY_RAIL) === 'true';
    const forced = window.innerWidth < WS_RAIL_AUTO_COLLAPSE_W;
    document.body.classList.toggle('ws-rail-collapsed', pref || forced);
    // Too narrow to expand: the toggle has nothing to offer and fades away.
    document.body.classList.toggle('ws-rail-locked', forced);
    // Resize ticks land here; a flag write flushes, so only a change writes.
    const st = VectorSvelte.shellState();
    if (st.railCollapsed !== (pref || forced)) VectorSvelte.setShellFlag('railCollapsed', pref || forced);
    if (st.railLocked !== forced) VectorSvelte.setShellFlag('railLocked', forced);
}

/* ---- The list pane's two modes ----------------------------------------------
 * Widescreen splits navigation the way Discord does: the rail holds communities
 * and DM shortcuts, and the pane beside it is EITHER one community's channels or
 * the DM list. The narrow layout has no rail, so its list stays the single index
 * of everything — every rule here is gated on widescreen.
 */

/** The community whose channels the pane is showing, or null for the DM list.
 *  Derived from the open chat, never stored: a mode that can disagree with what's
 *  on screen is a mode that eventually will. */
function wsListCommunityId() {
    if (!wsActive()) return null;
    return communityIdOfChat(arrChats.find(c => c.id === strOpenChat)) || null;
}

/** Where you were last, per community (kept across restarts, per account), and your
 *  last DM. Discord returns you to the channel you left rather than dumping you in
 *  the default one. Loaded on first use: the account is known only after login. */
let wsLastChannel = null;
let wsLastDm = null;

function wsLastChannels() {
    if (wsLastChannel) return wsLastChannel;
    try { wsLastChannel = new Map(Object.entries(JSON.parse(localStorage.getItem(`ws_last_channel:${strPubkey}`)) || {})); }
    catch (_) { wsLastChannel = new Map(); }
    return wsLastChannel;
}

function wsRememberOpenChat() {
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return;
    const communityId = communityIdOfChat(chat);
    if (!communityId) {
        if (chat.chat_type !== 'Community') wsLastDm = chat.id;
        return;
    }
    const last = wsLastChannels();
    if (last.get(communityId) === chat.id) return;
    last.set(communityId, chat.id);
    // Communities since left go with the write.
    for (const id of last.keys()) if (!arrChats.some(c => communityIdOfChat(c) === id)) last.delete(id);
    try { localStorage.setItem(`ws_last_channel:${strPubkey}`, JSON.stringify(Object.fromEntries(last))); } catch (_) { /* this session only */ }
    // Its list row previews the channel you were last in.
    VectorSvelte.touchCommunity(communityId);
}

/** The channel a community's rail shortcut and list row open: where you left it, else
 *  its primary — which is also the fallback when that channel has since gone. */
function wsChannelForCommunity(communityId) {
    const last = wsLastChannels().get(communityId);
    // Only a channel still visible: the one left in may since have been revoked.
    const visible = communityChannelsCache.get(communityId);
    if (last && arrChats.some(c => c.id === last) && (!visible || visible.some(c => c.id === last))) return last;
    return arrChats.find(c => communityIdOfChat(c) === communityId && isPrimaryChannelChat(c))?.id || null;
}

/** The logo is the way home to DMs: reopen the last one, which flips the pane
 *  back to the DM list with it. With no DMs yet, land on the list itself. */
function wsOpenDmHome() {
    const known = wsLastDm && arrChats.some(c => c.id === wsLastDm);
    const target = known ? wsLastDm : arrChats.find(c =>
        c.chat_type !== 'Community' && c.id !== strPubkey && c.messages?.length)?.id;
    if (target) openChat(target);
    else closeChat();
}

/** Mirror the pane's mode onto the body, for the chrome that belongs to one of
 *  them: New Chat / Group Chat start DMs, so they have no place over a
 *  community's channels. */
function wsSyncPaneMode() {
    document.body.classList.toggle('ws-community-pane', !!wsListCommunityId());
}

/**
 * Reflect the open chat into the shell: which column shows, and which list row
 * reads as selected. Called from openChat/closeChat (and after a list render,
 * which rebuilds the rows from scratch).
 */
function wsSyncOpenChat() {
    document.body.classList.toggle('ws-chat-open', !!strOpenChat);
    wsRememberOpenChat();
    ccFollowOpenChat();
    syncRequestBar();
    wsSyncPaneMode();
    // The pane's mode follows the open chat (widescreen), and the active row and rail
    // shortcut derive from it.
    openChatChanged();
    wsSyncMembersPane();
    wsMarkActiveRow();
}

/** The open chat changed: the rows and the rail shortcut derive their active marks from the signal. */
function wsMarkActiveRow() {
    markRailShortcutActive();
}

/** Enter or leave widescreen. Idempotent; safe to call on every resize tick. */
function wsUpdate() {
    const want = wsShouldApply();
    const have = wsActive();
    // Login lands here in the slim layout as well, with nothing to switch.
    if (!want) ccSyncBody();

    if (want) {
        wsApplyRailState();
        wsApplyListWidth(wsReadListWidth());
    }
    if (want === have) return;

    document.body.classList.toggle('ws', want);
    // The account row renders in the rail or the list from this flag.
    VectorSvelte.setShellFlag('ws', want);
    ccSyncBody();
    paneChanged();
    wsSyncPaneMode();
    if (want) {
        // adjustSize() leaves an inline max-height sized against the viewport;
        // in widescreen the list is a flex child and must not stay clamped.
        const list = VectorSvelte.shellElements().chatList;
        if (list) list.style.maxHeight = '';
        // A roster opened narrow replaced its conversation. Here it is a column
        // beside one, so bring that community's channel back and let the sync
        // dock the roster next to it rather than landing on an empty pane.
        const rosterFor = VectorSvelte.paneShown('groupOverview') && VectorSvelte.overviewState().groupId;
        if (!strOpenChat && rosterFor) {
            const target = wsChannelForCommunity(rosterFor);
            if (target) openChat(target);
        }
        wsSyncOpenChat();
        // The rail only exists in this mode, and the list must re-derive
        renderRailShortcuts();
    } else {
        document.body.classList.remove('ws-chat-open', 'ws-rail-collapsed');
        // The roster is a docked column here and a whole screen there. Narrow, both
        // it and the conversation are simply visible, so they stack — the roster
        // sitting on top of the chat rather than replacing it. The conversation is
        // where the user actually is, so the roster is what gives way.
        //
        // The PREFERENCE is deliberately untouched: this is the layout running out
        // of room, not the user closing anything, and widening re-derives the pane
        // from it through wsSyncOpenChat.
        if (VectorSvelte.paneShown('groupOverview')) wsCloseDetails();
        // A roster opened narrow hid every other pane as a whole screen, and widening
        // may have closed it (no chat beside it). Widescreen forced the panes visible
        // regardless; narrow honours their inline none, so land where the user was.
        const anyShown = ['chat', 'chats', 'profile', 'settings', 'groupOverview'].some(p => VectorSvelte.paneShown(p));
        if (!anyShown) {
            if (strOpenChat) openChat(strOpenChat);
            else openChatlist();
        }
        wsMarkActiveRow();
        adjustSize();
    }
}

/** The details pane docks as the 4th column: mirror its visibility onto the body. */
function wsInitDetailsPane() {
    const sync = () => {
        document.body.classList.toggle('ws-details', VectorSvelte.paneShown('groupOverview'));
        };
    VectorSvelte.onPaneChange(sync);
    sync();
}


/**
 * Header tap on a Community. Single-pane swaps the conversation out for the
 * overview; widescreen docks the overview as the details column beside a still
 * open conversation, and a second tap dismisses it.
 *
 * The sidebar can only ever belong to the open chat (openChat hides the pane, and
 * the observer above follows), so "already open" always means "open for this
 * chat" and a plain toggle is correct.
 */
/* ---- The member list's open/closed preference -------------------------------
 * One app-wide answer to "do I want to see who's here", not one per community:
 * it lives in the SQL settings KV rather than localStorage so it travels with
 * the account's database like every other quiet preference.
 */

let wsMembersOpen = true;   // Discord's default, until the stored value lands.

async function wsLoadMembersPref() {
    try {
        const stored = await invoke('get_sql_setting', { key: 'ws_members_open' });
        if (stored === 'true' || stored === 'false') wsMembersOpen = stored === 'true';
    } catch (_) { /* first run, or no account yet: the default stands */ }
    wsSyncMembersPane();
}

/** Only a deliberate toggle writes the preference — never the sync below, which
 *  would turn "this community has no details to show" into "you closed it". */
function wsSetMembersOpen(open) {
    wsMembersOpen = open;
    invoke('set_sql_setting', { key: 'ws_members_open', value: open ? 'true' : 'false' }).catch(() => {});
}

function wsCloseDetails() {
    popBack('group-overview');
    VectorSvelte.showPane('groupOverview', false);
    VectorSvelte.setOverviewGroup(null);
}

/** Bring the pane in line with the preference for whatever is open now: members
 *  beside a community, nothing beside a DM. */
function wsSyncMembersPane() {
    if (!wsActive()) return;
    const chat = arrChats.find(c => c.id === strOpenChat);
    const inCommunity = !!communityIdOfChat(chat);
    // The pane itself, not the body class: `ws-details` is synced from the pane
    // change, so it still reads "open" during the same tick that
    // openChat hid the pane. Reading it here made the sync believe the panel was
    // already up and decline to restore it, so every channel click closed the
    // member list for good — which then looked like a per-community memory.
    const shown = VectorSvelte.paneShown('groupOverview');
    if (inCommunity && wsMembersOpen) {
        // Up already: only another community's channel re-renders it, in place. A channel
        // of the same community keeps the roster and re-scopes it: a Private Channel lists
        // only who may read it.
        const communityId = communityIdOfChat(chat);
        if (!shown || VectorSvelte.overviewState().groupId !== communityId) openGroupOverview(chat);
        else VectorSvelte.overviewRoster()?.setChannel(rosterChannelFor(communityId, chat.id));
    } else if (shown) {
        wsCloseDetails();
    }
}

function openCommunityDetails(chat) {
    if (!wsActive()) {
        // A community channel stays open beneath its details, which slide over it and away.
        if (ccLayerChat()) return ccOpenDetails(chat);
        // Narrow: the member list IS the screen, and Back returns to the channel.
        closeChat();
        openGroupOverview(chat);
        return;
    }
    if (document.body.classList.contains('ws-details')) {
        wsCloseDetails();
        wsSetMembersOpen(false);
        return;
    }
    openGroupOverview(chat);
    wsSetMembersOpen(true);
}

function wsInitResizer() {
    let startX = 0;
    let startW = 0;
    let unlock = null;
    const onMove = (ev) => {
        wsApplyListWidth(startW + (ev.clientX - startX));
    };
    const onUp = () => {
        document.removeEventListener('pointermove', onMove);
        document.removeEventListener('pointerup', onUp);
        document.body.classList.remove('ws-resizing');
        unlock?.();
        const applied = parseInt(getComputedStyle(document.documentElement).getPropertyValue('--ws-list-w'), 10);
        if (Number.isFinite(applied)) localStorage.setItem(WS_KEY_LIST_W, String(applied));
    };
    VectorSvelte.mergeShellHandlers({
        listResizeStart: (ev) => {
            if (ev.button !== 0) return;
            ev.preventDefault();
            startX = ev.clientX;
            startW = VectorSvelte.shellElements().chats?.getBoundingClientRect().width || 0;
            document.body.classList.add('ws-resizing');
            unlock = VectorSvelte.lockSelection();
            document.addEventListener('pointermove', onMove);
            document.addEventListener('pointerup', onUp);
        },
        // Double-click restores the default, the usual escape hatch for a pane dragged
        // somewhere unusable.
        listResizeReset: () => {
            localStorage.removeItem(WS_KEY_LIST_W);
            wsApplyListWidth(wsReadListWidth());
        },
    });
}

function wsInitRail() {
    VectorSvelte.mergeShellHandlers({
        openDmHome: () => wsOpenDmHome(),
        // Only ever writes the user's PREFERENCE; the width-forced collapse below the
        // auto threshold is re-derived on every apply.
        toggleRailCollapse: () => {
            const pref = localStorage.getItem(WS_KEY_RAIL) === 'true';
            localStorage.setItem(WS_KEY_RAIL, String(!pref));
            wsApplyRailState();
        },
    });
}

/* ---- The slim layout: the Centre Console --------------------------------------
 * Below widescreen the list pane is a header over a community rail and the pane
 * beside it: the DMs (Messages | Requests) or the community the rail picked. Folding
 * the rail away puts communities back in the list, behind All | Messages |
 * Communities | Requests.
 */

const CC_KEY_RAIL = 'cc_rail_open';

function ccActive() {
    return !wsActive() && !fInit;
}

/** The community whose channels the pane shows (beside the rail, or in the list's place
 *  while it's folded), or null. One since left reads as none, rather than a pane with
 *  nothing in it. */
function ccSpace() {
    const cc = VectorSvelte.consoleState();
    if (!ccActive() || !cc.space) return null;
    return arrChats.some(c => communityIdOfChat(c) === cc.space) ? cc.space : null;
}

function ccSyncBody() {
    const on = ccActive();
    // adjustSize may have clamped the list before login; here flex sizes it.
    if (on && !document.body.classList.contains('cc')) {
        const list = VectorSvelte.shellElements().chatList;
        if (list) list.style.maxHeight = '';
    }
    document.body.classList.toggle('cc', on);
    VectorSvelte.setConsoleActive(on);
    document.body.classList.toggle('cc-community', on && !!ccSpace());
    ccSyncLayer();
}

function ccScrollTop() {
    VectorSvelte.shellElements().chatList?.scrollTo({ top: 0 });
}

// Where the chats list was scrolled, kept while a community's channels have its scroller.
let ccListTop = 0;
function ccKeepListTop() {
    if (!ccSpace()) ccListTop = VectorSvelte.shellElements().chatList?.scrollTop || 0;
}

/** A community was picked (or the Messages door, null): the pane follows it. Folded,
 *  its channels stand in for the list, and Back returns to it, where it was left. */
function ccShowSpace(communityId) {
    const back = !communityId && !!ccSpace();
    if (communityId) ccKeepListTop();
    VectorSvelte.setConsoleSpace(communityId);
    VectorSvelte.setConsoleQuery('');
    paneChanged();
    ccSyncBack();
    VectorSvelte.flushSync();
    VectorSvelte.shellElements().chatList?.scrollTo({ top: back ? ccListTop : 0 });
}

function ccSyncBack() {
    if (ccSpace() && !VectorSvelte.consoleState().railOpen) pushBack('cc-space', () => ccShowSpace(null));
    else popBack('cc-space');
}

function ccSetRail(open) {
    try { localStorage.setItem(CC_KEY_RAIL, String(open)); } catch (_) { /* per-device nicety only */ }
    VectorSvelte.setConsoleRail(open);
    paneChanged();
    ccSyncBack();
    ccScrollTop();
}

/** Whatever opened a chat, Back lands on the pane that holds it: its community's
 *  channels, or the DMs. A search has done its job once something opens from it. */
function ccFollowOpenChat() {
    ccSyncLayer();
    if (!ccActive()) return;
    // Back to the channels a chat was opened from, and from them to the list.
    if (!strOpenChat) return ccSyncBack();
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat || chat.id === strPubkey) return;
    // A flat chat's drawer is the whole list.
    const space = ccFlatChat(chat) ? null : communityIdOfChat(chat);
    const moved = space !== ccSpace();
    VectorSvelte.setConsoleSpace(space);
    VectorSvelte.setConsoleQuery('');
    // A community's channels start at the top; the list comes back where it was left.
    if (moved) {
        VectorSvelte.flushSync();
        VectorSvelte.shellElements().chatList?.scrollTo({ top: space ? 0 : ccListTop });
    }
}

/* The drawer (lib/drawer.js): with a chat on screen the list pane is its drawer, away until
   pulled out. A community of many channels rests it beside the channel, out when the community
   was just picked: Back from the channel brings it out, and Back from it leaves the community.
   A flat chat (a DM, or a community of one channel) has no list of its own, so its drawer is the
   whole list, and comes out only to leave for it. */
let ccDrawerOpen = false;
let ccLayerChatId = null;

/** The chat on screen in the console, or null. */
function ccLayerChat() {
    if (!ccActive() || !strOpenChat || !VectorSvelte.paneShown('chat')) return null;
    // A DM with nothing in it yet isn't in the list.
    return arrChats.find(c => c.id === strOpenChat) || { id: strOpenChat };
}

/** A chat with no drawer of its own: a DM, or a community of one channel. */
function ccFlatChat(chat) {
    const communityId = communityIdOfChat(chat);
    return !communityId || communityIsSingleChannel(communityId);
}

function ccSyncFlat() {
    const chat = ccLayerChat();
    document.body.classList.toggle('cc-flat', !!chat && ccFlatChat(chat));
}

function ccSyncLayer() {
    const chat = ccLayerChat();
    document.body.classList.toggle('cc-layer', !!chat);
    VectorSvelte.setConsoleLayer(!!chat);
    if (!chat) {
        ccLayerChatId = null;
        ccDrawerOpen = false;
        popBack('cc-drawer');
        VectorSvelte.drawerTo(false, true);
        ccSyncFlat();
        return;
    }
    // Only on arrival: a re-sync must not lift Back past whatever opened over the channel.
    if (ccLayerChatId === chat.id) return;
    ccLayerChatId = chat.id;
    // A drawer left out goes away before a flat chat takes its peek.
    if (ccDrawerOpen && ccFlatChat(chat)) {
        ccDrawerOpen = false;
        VectorSvelte.drawerTo(false, false, ccSyncFlat);
    } else {
        ccSyncFlat();
    }
    ccSyncDrawerBack(chat);
}

function ccSyncDrawerBack(chat) {
    pushBack('cc-drawer', ccDrawerOpen || ccFlatChat(chat) ? ccLeaveChat : () => ccSetDrawer(true));
}

/** Bring the drawer out or put it away; `instant` for a screen that opens with it out. */
function ccSetDrawer(open, instant = false) {
    const chat = ccLayerChat();
    if (!chat) return;
    if (open && ccFlatChat(chat)) return ccLeaveChat();
    ccDrawerOpen = !!open;
    // The keyboard belongs to the conversation going under.
    if (open) document.activeElement?.blur?.();
    VectorSvelte.drawerTo(open, instant);
    ccSyncDrawerBack(chat);
}

/** The Messages door, or the folded list's Back: from a channel's drawer that's out of the
 *  community, which is the only way back to the tab bar where nothing has a Back button. */
function ccHome() {
    if (ccLayerChat()) ccLeaveChat();
    else ccShowSpace(null);
}

/** Search spans every chat, so it starts from the list: out of a community, or off its channels. */
function ccSearch() {
    if (ccLayerChat()) ccLeaveChat();
    else if (ccSpace()) ccShowSpace(null);
}

// Bumped by each widening of the drawer into the list or back, so only the latest one lands.
let ccHomeSeq = 0;

/** Out of the chat on screen, to the list it was picked from. */
function ccLeaveChat() {
    // Cleared first, so the list never shows the community's channels on the way.
    if (ccSpace()) ccShowSpace(null);
    const chat = ccLayerChat();
    if (!chat || !(ccDrawerOpen || ccFlatChat(chat))) return closeChat();
    // The list comes the rest of the way over the chat and widens into the screen, the tab bar rising with it.
    const id = strOpenChat;
    const seq = ++ccHomeSeq;
    if (!document.activeElement?.closest?.('.cc-search')) document.activeElement?.blur?.();
    VectorSvelte.showPane('chats', true);
    VectorSvelte.showPane('navbar', true);
    VectorSvelte.setShellFlag('settingsTab', true);
    VectorSvelte.drawerTo(true);
    VectorSvelte.drawerHome(1, {
        done: () => {
            if (seq === ccHomeSeq && strOpenChat === id) closeChat();
            VectorSvelte.drawerHome(0, { instant: true });
        },
    });
}

/** Arrived from the list: it narrows into the drawer, or slides off a flat chat, as the tab bar sinks. */
function ccCameFromList() {
    const seq = ++ccHomeSeq;
    VectorSvelte.showPane('navbar', true);
    VectorSvelte.setShellFlag('settingsTab', true);
    VectorSvelte.drawerHome(0, {
        from: 1,
        done: () => {
            if (seq !== ccHomeSeq || !ccLayerChat()) return;
            VectorSvelte.showPane('navbar', false);
            VectorSvelte.setShellFlag('settingsTab', false);
        },
    });
}

/** Into a flat chat from the list, which slides away to the left: leaving it, backwards. */
function ccSlideInto(id) {
    // Opening the chat takes the list out of the layout for a moment, which forgets its scroll.
    const list = VectorSvelte.shellElements().chatList;
    const top = list?.scrollTop || 0;
    openChat(id, { fade: false });
    if (!ccLayerChat()) return VectorSvelte.revealPane('chat', 'fadein-anim');
    if (list) list.scrollTop = top;
    VectorSvelte.drawerTo(true, true);
    VectorSvelte.drawerTo(false);
    ccCameFromList();
}

/** A community channel's details (lib/sheet.js), slid in over the channel, which stays open. */
function ccOpenDetails(chat) {
    document.activeElement?.blur?.();
    VectorSvelte.sheetTo(true, { instant: true });
    openGroupOverview(chat, true);
    VectorSvelte.sheetTo(false);
}

/** Slide the details away to the channel beneath. */
function ccCloseDetails() {
    popBack('group-overview');
    VectorSvelte.sheetTo(true, {
        done: () => {
            VectorSvelte.showPane('groupOverview', false);
            VectorSvelte.setOverviewGroup(null);
        },
    });
}

/** The chat header's Back: a channel's drawer, or out of a flat chat. False off the console. */
function ccChatBack() {
    const chat = ccLayerChat();
    if (!chat) return false;
    if (!ccDrawerOpen && !ccFlatChat(chat)) ccSetDrawer(true);
    else ccLeaveChat();
    return true;
}

/** A community picked from the rail or the list: its last channel opens under its drawer,
 *  or on its own when it has just the one. Channels not loaded yet: just its channels. */
function ccOpenCommunity(communityId, fallbackChatId) {
    ccKeepListTop();
    const target = wsChannelForCommunity(communityId) || fallbackChatId;
    if (!target) return ccShowSpace(communityId);
    // Picked from inside the drawer it eases; from the list the screen opens with it out.
    const instant = !ccLayerChat();
    const drawer = !communityIsSingleChannel(communityId);
    if (instant && !drawer) return ccSlideInto(target);
    if (!chatOnScreen(target)) openChat(target);
    ccSetDrawer(drawer, instant);
    if (instant && drawer && ccLayerChat()) ccCameFromList();
}

/** The header's New menu, hung from the + under its right edge. */
function ccOpenNewMenu(anchor) {
    const r = anchor.getBoundingClientRect();
    const items = [
        { label: 'New Chat', icon: 'mail', lead: true, onClick: () => openNewChat() },
        { label: 'Add Friend', icon: 'add-user', lead: true, onClick: () => openQrOverlay(`https://vectorapp.io/profile/${strPubkey}`) },
        { label: 'Bookmark', icon: 'bookmark', lead: true, onClick: () => openChat(strPubkey) },
    ];
    // The rail's own + tile is folded away with it.
    if (!VectorSvelte.consoleState().railOpen) items.push({ label: 'New Community', icon: 'users-multi', lead: true, onClick: () => openCreateGroup() });
    // The phone's tab bar has no Mini Apps tab: the Nexus is reached from here and the composer.
    items.push({ label: 'Nexus', icon: 'nexus', lead: true, onClick: () => showMarketplacePanel() });
    showContextMenu({ x: r.right, y: r.bottom + 6, items });
}

function ccInit() {
    let open = true;
    try { open = localStorage.getItem(CC_KEY_RAIL) !== 'false'; } catch (_) { /* default stands */ }
    VectorSvelte.setConsoleRail(open);
    // The drawer lives exactly as long as a chat is on screen.
    VectorSvelte.onPaneChange((name) => { if (name === 'chat' || name === null) ccSyncLayer(); });
    VectorSvelte.mergeShellHandlers({
        consoleSwitchAccount: (el) => profileSwitcher.toggle('header', el),
        consoleScan: () => openQrScanner(),
        consoleNewMenu: ccOpenNewMenu,
        consoleSetRail: ccSetRail,
        consoleBack: ccHome,
        consoleDrawer: (open) => ccSetDrawer(open),
        consoleDetails: (gone) => (gone ? ccCloseDetails() : VectorSvelte.sheetTo(false)),
        consoleSearch: ccSearch,
        // The rail's own surface (its background, the Messages door, its buttons) folds it;
        // a community or folder keeps its own menu.
        consoleRailMenu: (node) => attachLongPressContextMenu(
            node,
            (x, y) => showContextMenu({ x, y, items: [{ label: 'Collapse', icon: 'chevron-double-left', onClick: () => ccSetRail(false) }] }),
            (target) => !target.closest('.ws-rail-item, .ws-rail-folder'),
        ),
    });
}

window.addEventListener('DOMContentLoaded', () => {
    ccInit();
    wsInitResizer();
    wsInitRail();
    wsInitDetailsPane();
    wsUpdate();
    wsLoadMembersPref();
});

window.addEventListener('resize', wsUpdate);
