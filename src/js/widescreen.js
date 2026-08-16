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

/** Set once the account row has been re-parented into the rail footer, so the
 *  restore path can put it back exactly where the markup had it. */
let wsAccountHome = null;

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
    return Number.isFinite(stored) ? stored : 330;
}

/** Clamp to the CSS bounds and to what the window can actually spare, so a
 *  restored width from a bigger monitor can't crush the conversation pane.
 *  Read off `body`, not the navbar: this runs before `body.ws` is applied, when
 *  the rail is still the full-width mobile tab bar, and the collapsed override
 *  of --ws-rail-w lives on the body class. */
function wsClampListWidth(px) {
    const css = getComputedStyle(document.body);
    const min = parseInt(css.getPropertyValue('--ws-list-min'), 10) || 260;
    const max = parseInt(css.getPropertyValue('--ws-list-max'), 10) || 520;
    const rail = parseInt(css.getPropertyValue('--ws-rail-w'), 10) || 232;
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
}

/** Dock the account row into the rail footer, or return it to the chat list. */
function wsMoveAccountRow(intoRail) {
    const account = document.getElementById('account');
    const slot = document.getElementById('ws-rail-account');
    if (!account || !slot) return;
    if (intoRail) {
        if (account.parentElement === slot) return;
        if (!wsAccountHome) {
            wsAccountHome = { parent: account.parentElement, next: account.nextElementSibling };
        }
        slot.appendChild(account);
        wsAddAccountCaret(account);
    } else {
        if (!wsAccountHome || account.parentElement !== slot) return;
        account.querySelector('.ws-account-caret')?.remove();
        wsAccountHome.parent.insertBefore(account, wsAccountHome.next);
    }
}

/**
 * The chip's own affordance: name opens the profile and status opens the status
 * editor, so switching accounts needs a target of its own rather than a third
 * meaning for the row. Added on dock and removed on undock — the row belongs to
 * the chat list the rest of the time.
 */
function wsAddAccountCaret(account) {
    if (account.querySelector('.ws-account-caret')) return;
    const caret = document.createElement('div');
    caret.className = 'ws-account-caret btn';
    caret.title = 'Switch account';
    caret.onclick = (e) => {
        e.stopPropagation();
        profileSwitcher.toggle();
    };
    account.appendChild(caret);
}

/**
 * Reflect the open chat into the shell: which column shows, and which list row
 * reads as selected. Called from openChat/closeChat (and after a list render,
 * which rebuilds the rows from scratch).
 */
function wsSyncOpenChat() {
    document.body.classList.toggle('ws-chat-open', !!strOpenChat);
    // Entering a multi-channel community expands its channel list, which is a
    // structural change — the rest is just re-stamping classes.
    if (ensureOpenChannelVisible()) renderChatlist();
    else wsMarkActiveRow();
}

/**
 * Mark the open chat in the list: the contact row (widescreen only, where the list
 * stays beside the conversation) and its channel row (both layouts, since a nested
 * channel list is on screen in both).
 */
function wsMarkActiveRow() {
    const list = document.getElementById('chat-list');
    if (!list) return;
    for (const row of list.querySelectorAll('.chatlist-contact.ws-active')) {
        row.classList.remove('ws-active');
    }
    if (wsActive() && strOpenChat) {
        document.getElementById(`chatlist-${strOpenChat}`)?.classList.add('ws-active');
    }
    for (const row of list.querySelectorAll('.chatlist-channel.active')) {
        row.classList.remove('active');
    }
    if (strOpenChat) {
        document.getElementById(`chatlist-channel-${strOpenChat}`)?.classList.add('active');
    }
    markRailShortcutActive();
}

/** Enter or leave widescreen. Idempotent; safe to call on every resize tick. */
function wsUpdate() {
    const want = wsShouldApply();
    const have = wsActive();

    if (want) {
        wsApplyRailState();
        wsApplyListWidth(wsReadListWidth());
    }
    if (want === have) return;

    document.body.classList.toggle('ws', want);
    wsMoveAccountRow(want);
    if (want) {
        // adjustSize() leaves an inline max-height sized against the viewport;
        // in widescreen the list is a flex child and must not stay clamped.
        document.getElementById('chat-list').style.maxHeight = '';
        wsSyncOpenChat();
        // The rail only exists in this mode, and renderChatlist's hash gate would
        // skip the render that normally fills it.
        renderRailShortcuts();
    } else {
        document.body.classList.remove('ws-chat-open', 'ws-rail-collapsed');
        wsMarkActiveRow();
        adjustSize();
    }
}

/**
 * Track the community details pane so it can dock as the 4th column.
 *
 * Its visibility is written from ~8 places (openGroupOverview, openChat,
 * closeChat, openChatlist, openProfile, the teardown paths…), so this watches
 * the style attribute instead of hooking every one of them: an observer cannot
 * fall out of sync with what is actually on screen, a list of hooks can.
 */
function wsInitDetailsPane() {
    const pane = document.getElementById('group-overview');
    if (!pane) return;
    const sync = () => {
        document.body.classList.toggle('ws-details', pane.style.display !== 'none');
    };
    new MutationObserver(sync).observe(pane, { attributes: true, attributeFilter: ['style'] });
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
function openCommunityDetails(chat) {
    if (!wsActive()) {
        closeChat();
        openGroupOverview(chat);
        return;
    }
    if (document.body.classList.contains('ws-details')) {
        popBack('group-overview');
        domGroupOverview.style.display = 'none';
        domGroupOverview.removeAttribute('data-group-id');
        return;
    }
    openGroupOverview(chat);
}

function wsInitResizer() {
    const handle = document.getElementById('ws-list-resize');
    if (!handle) return;
    let startX = 0;
    let startW = 0;

    const onMove = (ev) => {
        wsApplyListWidth(startW + (ev.clientX - startX));
    };
    const onUp = () => {
        document.removeEventListener('pointermove', onMove);
        document.removeEventListener('pointerup', onUp);
        document.body.classList.remove('ws-resizing');
        const applied = parseInt(getComputedStyle(document.documentElement).getPropertyValue('--ws-list-w'), 10);
        if (Number.isFinite(applied)) localStorage.setItem(WS_KEY_LIST_W, String(applied));
    };

    handle.addEventListener('pointerdown', (ev) => {
        if (ev.button !== 0) return;
        ev.preventDefault();
        startX = ev.clientX;
        startW = document.getElementById('chats').getBoundingClientRect().width;
        document.body.classList.add('ws-resizing');
        document.addEventListener('pointermove', onMove);
        document.addEventListener('pointerup', onUp);
    });

    // Double-click restores the default, the usual escape hatch for a pane
    // dragged somewhere unusable.
    handle.addEventListener('dblclick', () => {
        localStorage.removeItem(WS_KEY_LIST_W);
        wsApplyListWidth(wsReadListWidth());
    });
}

function wsInitRail() {
    const toggle = document.getElementById('ws-rail-collapse');
    if (!toggle) return;
    toggle.addEventListener('click', () => {
        // Only ever writes the user's PREFERENCE; the width-forced collapse
        // below the auto threshold is re-derived on every apply.
        const pref = localStorage.getItem(WS_KEY_RAIL) === 'true';
        localStorage.setItem(WS_KEY_RAIL, String(!pref));
        wsApplyRailState();
    });
}

window.addEventListener('DOMContentLoaded', () => {
    wsInitResizer();
    wsInitRail();
    wsInitDetailsPane();
    wsUpdate();
});

window.addEventListener('resize', wsUpdate);
