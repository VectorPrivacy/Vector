/**
 * Chat Scroll Management
 * Handles procedural message loading, scroll correction, and navigation
 */

// ===========================================================================
// DOM windowing — bound the rendered chat to a sliding window of rows.
//
// The window is a DOM-only view over the already-in-memory `chat.messages`
// array (aliased to eventCache.getEventsRef(chatId), ASC by .at). We render a
// contiguous slice [startIdx, endIdx) and drop the far end when extending the
// near end, keeping the DOM bounded regardless of how much history is loaded.
//
// Anchors are message IDs (the array mutates under realtime/prepend) — indices
// are re-derived from the ids on each op. Backend loading is unchanged and only
// happens at the oldest edge; scrolling DOWN never hits the backend (newer rows
// are always already in chat.messages).
// ===========================================================================

/** Master flag. Flip to false to fall back to today's accrete-everything render. */
const CHAT_WINDOW_ENABLED = true;

/** Max rows rendered into the DOM at once (a few screens). */
const MAX_WINDOW_ROWS = 80;
/** Rows added/dropped per window step (one batch). */
const WINDOW_STEP = 20;
/** Larger older-fetch batch used while the unread jump pill is active — the user is paging toward a
 *  possibly-deep boundary, so fetch more per round-trip (the rendered window still advances by
 *  WINDOW_STEP, paging the surplus from memory between fetches). */
const UNREAD_SEEK_FETCH_STEP = 50;

// Window anchors — message ids of the top and bottom rendered rows. Re-resolved
// to array indices on each operation since chat.messages mutates underneath.
let windowTopId = null;
let windowBottomId = null;

// True when the window's bottom edge is the live tail (newest message). After a
// seek (jumpToUnread / reply-jump) chat.messages is a bounded slice that does NOT
// reach the tail, so the DOM's last row being the slice's last row no longer means
// "at the newest message". O(1) flag instead of a per-call query; the scroll-return
// button keys off isAtDataBottom() which returns it.
let windowAtTail = true;

// Set while a window slide (extend-newer / drop) is mutating the DOM so that
// updateChat's auto-scroll-to-bottom tail doesn't yank the viewport — the slide
// owns scrollTop itself. Read by updateChat in main.js.
let _windowSuppressAutoScroll = false;

// True for the duration of a jumpToUnread resolve (relay-walk + DB pull). The
// window must NEVER shift until the jump renders, so realtime arrivals during
// the resolve are data-only (no DOM rows, no badge). Cleared in jumpToUnread's
// finally and as a safety net at the top of openChat.
let _unreadJumpResolving = false;

/** The chat.messages array for the open chat (cache ref preferred, falls back to chat.messages). */
function _windowMessages() {
    if (!strOpenChat) return null;
    const ref = eventCache.getEventsRef(strOpenChat);
    if (ref) return ref;
    const chat = arrChats.find(c => c.id === strOpenChat);
    return chat?.messages || null;
}

/** Resolve a message id to its index in chat.messages, or -1. */
function _windowIndexOfId(id) {
    if (!id) return -1;
    const msgs = _windowMessages();
    if (!msgs) return -1;
    return msgs.findIndex(m => m.id === id);
}

/** Current rendered window as [startIdx, endIdx) over chat.messages, derived from anchors.
 *  Returns null if anchors aren't resolvable (e.g. nothing rendered yet). */
function _currentWindowRange() {
    const msgs = _windowMessages();
    if (!msgs || !msgs.length) return null;
    let start = _windowIndexOfId(windowTopId);
    let end = _windowIndexOfId(windowBottomId);
    if (start === -1 || end === -1) return null;
    return [start, end + 1];   // endIdx exclusive
}

/** chat.messages index of the DOM's last rendered message (rows OR system events
 *  both carry an id resolving into chat.messages), or -1 if nothing message-backed
 *  is rendered. Walks children from the end, returning the first that resolves. */
function _windowBottomRenderedIndex() {
    if (!domChatMessages) return -1;
    const children = domChatMessages.children;
    for (let i = children.length - 1; i >= 0; i--) {
        const id = children[i].id;
        if (!id) continue;                       // overlays, date dividers, "New"
        const idx = _windowIndexOfId(id);
        if (idx === -1) continue;
        return idx;
    }
    return -1;   // nothing message-backed rendered
}

/** True when the window's bottom edge is the live tail (the newest message is on
 *  screen). O(1): reads the windowAtTail flag, which the extend/seek/append paths
 *  maintain. Pre-windowing, the DOM always held the tail, so legacy returns true.
 *  chat.messages is a bounded slice after a seek — its last index is NOT the tail. */
function isAtDataBottom() {
    if (!CHAT_WINDOW_ENABLED) return true;   // legacy: DOM always holds the tail
    return windowAtTail;
}

/**
 * Render a fresh contiguous slice [startIdx, endIdx) of chat.messages into
 * #chat-messages, clearing whatever is there. Records the window anchors.
 *
 * Reuses updateChat's renderer (day-separators via _dedupeAdjacentDaySeparators,
 * streak, reactions). The unread divider is re-inserted if its target lands in
 * the slice (caller may also re-insert). Returns the rendered slice array.
 */
async function renderWindow(startIdx, endIdx) {
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return [];
    const msgs = _windowMessages();
    if (!msgs || !msgs.length) return [];

    startIdx = Math.max(0, Math.min(startIdx, msgs.length - 1));
    endIdx = Math.max(startIdx + 1, Math.min(endIdx, msgs.length));

    // Gap clamp: a single window must stay within ONE contiguous segment. When the
    // buffer is [seek-region … GAP … tail], a range straddling the gap would weld the
    // tail onto the seek region (skipping the unfetched interior). Snap the range to
    // the segment containing startIdx; the gap-aware scroll-extends page across the gap.
    const gapAfterId = eventCache.getGapAfterId(strOpenChat);
    if (gapAfterId != null) {
        const gapIdx = msgs.findIndex(m => m.id === gapAfterId);
        if (gapIdx !== -1) {
            if (startIdx <= gapIdx) endIdx = Math.min(endIdx, gapIdx + 1);     // seek region
            else startIdx = Math.max(startIdx, gapIdx + 1);                    // tail
        }
    }

    const slice = msgs.slice(startIdx, endIdx);
    if (!slice.length) return [];

    // Cache the unread divider's anchor (row + mode) before we wipe the DOM, so
    // it can be re-inserted consistently if its row is in the new slice.
    const dividerTargetId = unreadDividerEl?._targetId || null;
    const dividerAnchorAfter = !!unreadDividerEl?._anchorAfter;

    const isGroup = chatIsGroup(chat);
    const profile = !isGroup ? getProfile(chat.id) : null;

    // Clear and render. clearUnreadDivider first so the renderer starts clean;
    // we re-insert below from the cached target. Suppress updateChat's auto-scroll
    // tail — the caller positions the viewport (centerInView / scrollToBottom).
    clearUnreadDivider();
    while (domChatMessages.firstElementChild) domChatMessages.firstElementChild.remove();
    _windowSuppressAutoScroll = true;
    try {
        await updateChat(chat, slice, profile, false);
    } finally { _windowSuppressAutoScroll = false; }

    // Set anchors from the slice edges.
    windowTopId = slice[0]?.id || null;
    windowBottomId = slice[slice.length - 1]?.id || null;

    // Re-insert the unread divider if its anchor row is in the window, preserving
    // the original anchor mode (after the boundary row vs before the first-unread).
    if (dividerTargetId) {
        const node = document.getElementById(dividerTargetId);
        if (node) insertUnreadDivider(node, dividerAnchorAfter);
    }
    // Land a pending manual-scroll frontier divider if its row just entered the slice.
    tryPlacePendingDivider();

    // Keep procedural-scroll bookkeeping consistent with what's rendered.
    proceduralScrollState.renderedMessageCount = slice.length;
    proceduralScrollState.totalMessageCount = msgs.length;

    // The rendered range moved — the scroll-return button's data-bottom term
    // may have flipped. (No scroll event fires from a programmatic render.)
    refreshScrollReturnButton();

    return slice;
}

/**
 * Drop `count` rows (by id) from the BOTTOM of the rendered window. The dropped
 * rows are below the viewport (we just prepended above), so scrollTop is
 * unaffected; only scrollHeight shrinks. Re-derives the new bottom anchor and
 * heals day-separators. Returns the number of .dmsg rows removed.
 */
function _windowDropBottom(count) {
    const range = _currentWindowRange();
    if (!range) return 0;
    const [start, end] = range;
    const newEnd = Math.max(start + 1, end - count);
    if (newEnd >= end) return 0;
    const msgs = _windowMessages();

    // Ids to keep = the new slice [start, newEnd). Anything rendered (with an id)
    // that ISN'T kept is dropped. O(window) Set build + O(children) walk.
    const keep = new Set();
    for (let i = start; i < newEnd; i++) if (msgs[i]) keep.add(msgs[i].id);

    let removed = 0;
    const children = Array.from(domChatMessages.children);
    for (const child of children) {
        if (!child.id) continue;                       // overlays, dividers w/o id
        if (!keep.has(child.id)) { child.remove(); removed++; }
    }
    windowBottomId = msgs[newEnd - 1]?.id || windowBottomId;
    _dedupeAdjacentDaySeparators();
    return removed;
}

/**
 * Drop `count` rows (by id) from the TOP of the rendered window. The dropped
 * rows are ABOVE the viewport, so the caller must apply scrollTop -= droppedHeight
 * synchronously to keep the viewport fixed. Returns the total pixel height removed.
 */
function _windowDropTop(count) {
    const range = _currentWindowRange();
    if (!range) return 0;
    const [start, end] = range;
    const newStart = Math.min(end - 1, start + count);
    if (newStart <= start) return 0;
    const msgs = _windowMessages();

    // Ids being dropped = the slice [start, newStart). Measuring offsetTop of the
    // first kept row BEFORE vs AFTER removal gives the exact removed height (rows
    // + separators + margins), which the caller subtracts from scrollTop.
    const drop = new Set();
    for (let i = start; i < newStart; i++) if (msgs[i]) drop.add(msgs[i].id);

    const firstKeptId = msgs[newStart]?.id;
    const firstKept = firstKeptId ? document.getElementById(firstKeptId) : null;
    const topBefore = firstKept ? firstKept.offsetTop : 0;

    // Fallback height: when the kept anchor isn't in the DOM the offsetTop delta
    // can't be measured, so sum the removed nodes' heights BEFORE removal instead
    // (else the caller skips scrollTop compensation and the viewport jumps).
    let droppedHeightSum = 0;
    const children = Array.from(domChatMessages.children);
    for (const child of children) {
        if (child.id && drop.has(child.id)) {
            if (!firstKept) droppedHeightSum += child.offsetHeight;
            child.remove();
        }
    }
    windowTopId = firstKeptId || windowTopId;
    // Rebuild date dividers (a dropped row may have orphaned a leading divider).
    _dedupeAdjacentDaySeparators();

    if (!firstKept) return droppedHeightSum;
    // Exact removed height from the kept row's offsetTop delta.
    const topAfter = firstKept.isConnected ? firstKept.offsetTop : 0;
    return Math.max(0, topBefore - topAfter);
}

/** Count rendered .dmsg rows currently in the DOM. */
function _windowRenderedRowCount() {
    return domChatMessages ? domChatMessages.querySelectorAll('.dmsg').length : 0;
}

/**
 * After a realtime append while pinned at the bottom, drop the oldest rows from
 * the top if the rendered window exceeds MAX. When pinned, the user is reading
 * the bottom so dropped top rows are above the viewport; we adjust scrollTop by
 * the removed height SYNCHRONOUSLY so the view doesn't jump, then keep the pin.
 */
function windowTrimTopIfOver() {
    if (!CHAT_WINDOW_ENABLED || !domChatMessages) return;
    const overflow = _windowRenderedRowCount() - MAX_WINDOW_ROWS;
    if (overflow <= 0) return;
    // Anchors may be stale after updateChat's own append — re-seat them to the
    // current DOM edges before dropping.
    _windowReseatAnchorsFromDom();
    const droppedHeight = _windowDropTop(overflow);
    if (chatPinnedToBottom) {
        // Stay pinned: simplest correct behaviour is to re-snap to bottom.
        scrollToBottom(domChatMessages, false);
    } else if (droppedHeight > 0) {
        // Drop-top moves scrollTop UP — guard so it isn't misread as a user scroll-up.
        beginProgrammaticScroll();
        domChatMessages.scrollTop -= droppedHeight;
    }
    // Content shrank below the viewport: no scroll event fires to clear the
    // scroll-away latch, so the pin would stay false forever. At the trivial
    // bottom, release the latch and re-pin so new tail messages auto-scroll.
    if (domChatMessages.scrollHeight <= domChatMessages.clientHeight + BOTTOM_EPSILON_PX) {
        _userScrolledAway = false;
        chatPinnedToBottom = true;
    }
    refreshScrollReturnButton();
}

/**
 * Re-seat the window at the TRUE live tail and pin. Used by the ↓ button and
 * own-send-while-scrolled-up. After a seek chat.messages is a bounded slice that
 * does NOT reach the tail, so rendering its own last MAX rows would land on stale
 * mid-history rows. When already at the tail (windowAtTail) the in-memory tail is
 * authoritative; otherwise pull the newest page from the DB and seed-replace the
 * window with it (drops the seeked slice).
 */
async function windowJumpToBottom() {
    // Explicit user action (↓ button / own-send): it MUST reach the live tail, never silently no-op.
    // If a scroll-triggered load holds the gate, WAIT for it to finish (don't bail) then claim it — so
    // the re-seed-to-tail still runs, and it stays race-free by serialising after the in-flight load.
    let _waitGuard = 0;
    while (proceduralScrollState.isLoading && _waitGuard++ < 160) {
        await new Promise(r => setTimeout(r, 25));
    }
    proceduralScrollState.isLoading = true;
    try {
    const chatId = strOpenChat;
    if (!chatId) { if (domChatMessages) scrollToBottom(domChatMessages, false); return; }

    if (!windowAtTail) {
        // Seeked away: the in-memory slice isn't the tail. Pull the newest page and
        // seed-replace so the window is contiguous with "now".
        let tail = [];
        try {
            tail = await invoke('get_message_views', { chatId, limit: MAX_WINDOW_ROWS, offset: 0 }) || [];
        } catch (e) { console.warn('[unread-jump] jump-to-bottom tail load failed:', e); }
        if (strOpenChat !== chatId) return;
        if (tail.length) {
            tail.sort((a, b) => a.at - b.at);   // get_message_views returns DESC
            // Carry over any pending/failed own sends not yet in the DB (on_persist
            // writes async) so an own-send-while-seeked jump still shows the row.
            const prior = eventCache.getEventsRef(chatId) || [];
            const tailIds = new Set(tail.map(m => m.id));
            const unpersisted = prior.filter(m => (m.pending || m.failed) && !tailIds.has(m.id));
            if (unpersisted.length) {
                tail = [...tail, ...unpersisted].sort((a, b) => a.at - b.at);
            }
            eventCache.seedWindow(chatId, tail);
        }
        // Collapse the non-congruent cache to its contiguous live tail: drop the stale seeked region
        // (+ gap) that seedWindow keeps behind a gap. Without this, the window math below
        // (msgs.length - MAX) lands IN the seek region in front of the gap and renderWindow's
        // gap-clamp snaps us to mid-history, not "now".
        eventCache.dropSeekSegment(chatId);
        const chat = arrChats.find(c => c.id === chatId);
        if (chat) chat.messages = eventCache.getEventsRef(chatId) || chat.messages;
    }

    const msgs = _windowMessages();
    if (!msgs || !msgs.length) {
        windowAtTail = true;
        if (domChatMessages) scrollToBottom(domChatMessages, false);
        clearUnreadBelow();
        syncBackendActiveChat();
        return;
    }
    const start = Math.max(0, msgs.length - MAX_WINDOW_ROWS);
    chatPinnedToBottom = true;
    _userScrolledAway = false;   // jumped back to the live tail
    windowAtTail = true;
    await renderWindow(start, msgs.length);
    refreshChatEmptyState();
    scrollToBottom(domChatMessages, false);
    clearUnreadBelow();
    syncBackendActiveChat();
    // Attachment/preview media (images, videos) finishes loading AFTER the render, grows the layout,
    // and nudges us off the exact bottom past the pin threshold — so a lone scrollToBottom lands a few
    // rows short. Wait for the window's media to settle, then re-snap (the reflow lenience openChat
    // uses), unless the user deliberately scrolled away during the wait.
    await waitForMediaToLoad();
    if (strOpenChat === chatId && !_userScrolledAway) scrollToBottom(domChatMessages, false);
    } finally { proceduralScrollState.isLoading = false; }
}

/** Re-seat window anchors from the current DOM's first/last rendered message
 *  (rows OR system events both carry an id that resolves into chat.messages).
 *  Used after updateChat mutates the DOM directly (append/prepend path). */
function _windowReseatAnchorsFromDom() {
    if (!domChatMessages) return;
    const children = domChatMessages.children;
    let first = null, last = null;
    for (let i = 0; i < children.length; i++) {
        const id = children[i].id;
        if (!id || _windowIndexOfId(id) === -1) continue;   // skip overlays/dividers
        if (!first) first = id;
        last = id;
    }
    if (!first) { return; }   // nothing message-backed rendered; keep prior anchors
    windowTopId = first;
    windowBottomId = last;
}

/**
 * Scroll-up: prepend the next WINDOW_STEP older rows above the rendered window,
 * then drop from the bottom to stay ≤ MAX. Viewport stays fixed via the prepend
 * correction. Returns true if it handled the scroll (so the caller doesn't also
 * hit the legacy backend path).
 *
 * Two sources, in order:
 *  1. In-memory rows above the window (a prior extend dropped them but chat.messages
 *     still holds them) — prepend the slice directly, no backend.
 *  2. Window top IS the slice start (post-seek the slice is bounded): anchored DB
 *     load `get_messages_around(windowTopId, before=WINDOW_STEP+1, after=0)`. The
 *     newest returned row IS windowTopId (the `<= anchor` include) — addEvents dedups
 *     it, so only genuinely-older rows enter chat.messages. For Community channels
 *     whose DB has no older rows yet, fill the DB from the network FIRST, then re-read.
 */
async function windowExtendOlder() {
    if (!CHAT_WINDOW_ENABLED) return false;
    _windowReseatAnchorsFromDom();
    const range = _currentWindowRange();
    if (!range) return false;
    const [start] = range;

    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return false;
    const isGroup = chatIsGroup(chat);
    const profile = !isGroup ? getProfile(chat.id) : null;
    const msgs0 = _windowMessages();

    const scrollHeightBefore = domChatMessages.scrollHeight;
    const scrollTopBefore = domChatMessages.scrollTop;

    // When the window TOP sits in the tail (a gap below it, in the seek region) an
    // in-memory step up must not cross the gap into the seek region. Floor the reach
    // at the tail's start; the step landing on the tail-start row then anchored-fills
    // the gap from the tail side next call (window top == array start path below).
    let inMemFloor = 0;
    const gapAfterId = eventCache.getGapAfterId(strOpenChat);
    if (gapAfterId != null) {
        const gapIdx = msgs0.findIndex(m => m.id === gapAfterId);
        if (gapIdx !== -1 && start > gapIdx) inMemFloor = gapIdx + 1;   // top is in the tail
    }

    let olderSlice;
    if (start > inMemFloor) {
        // In-memory rows sit above the window (same segment) — prepend them, no backend.
        const newStart = Math.max(inMemFloor, start - WINDOW_STEP);
        olderSlice = msgs0.slice(newStart, start);
        if (!olderSlice.length) return false;
    } else if (inMemFloor > 0 && start <= inMemFloor) {
        // Window top is at the tail's start with a gap below (a stale prior seek region
        // sits further up). Re-anchoring is the clean way back to that older history, so
        // discard the stale seek region: collapse to the tail and anchored-load older
        // from the tail start (the common contiguous scroll-up). Subsequent steps page
        // older normally.
        eventCache.dropSeekSegment(strOpenChat);
        chat.messages = eventCache.getEventsRef(strOpenChat) || chat.messages;
        _windowReseatAnchorsFromDom();
        const r2 = _currentWindowRange();
        if (!r2) return true;
        if (r2[0] > 0) {
            const m2 = _windowMessages();
            const ns = Math.max(0, r2[0] - WINDOW_STEP);
            olderSlice = m2.slice(ns, r2[0]);
            if (!olderSlice.length) return true;
        } else {
            // Fall through to the anchored older-load below by re-entering via recursion-free path.
            return await windowExtendOlder();
        }
    } else {
        // Window top is the slice start: load older rows from the DB anchored to it.
        const anchorId = windowTopId;
        if (!anchorId) return false;
        const chatId = strOpenChat;
        // Pill active → user is paging toward a (possibly deep) unread boundary; pull a larger batch
        // so the cache reaches it in far fewer round-trips (rendered window still steps by WINDOW_STEP).
        const seekFetch = (_unreadJumpPill && _unreadJumpPill.classList.contains('visible'))
            ? UNREAD_SEEK_FETCH_STEP : WINDOW_STEP;
        let older = [];
        try {
            older = await invoke('get_messages_around', { chatId, anchorId, before: seekFetch + 1, after: 0 }) || [];
        } catch (e) { console.warn('[unread-jump] extend-older anchored load failed:', e); }
        if (strOpenChat !== chatId) return true;

        // ≤1 row back = only the anchor (the `<= anchor` include) → DB history start.
        // For a Community, try the network to grow the DB first, THEN re-read.
        if (older.length <= 1 && chat.chat_type === 'Community') {
            await maybeLoadCommunityOlderFromNetwork(chatId);
            if (strOpenChat !== chatId) return true;
            try {
                older = await invoke('get_messages_around', { chatId, anchorId, before: seekFetch + 1, after: 0 }) || [];
            } catch (e) { console.warn('[unread-jump] extend-older re-read failed:', e); }
            if (strOpenChat !== chatId) return true;
        }
        if (older.length <= 1) return true;   // history start reached; handled, don't loop

        // addEvents(prepend) dedups the anchor row, so only the genuinely-older rows
        // enter chat.messages. Re-derive the slice from the cache so indices stay valid.
        eventCache.addEvents(chatId, older, true);
        chat.messages = eventCache.getEventsRef(chatId) || chat.messages;
        // Older messages just dropped the reveal bound — surface any buffered "X joined/left" system
        // events that now fall within the loaded range so they render interleaved (legacy
        // loadMoreMessages did this; the windowed scroll-up path must too, or joins stay hidden).
        revealSystemEventsInWindow(chatId);

        const msgs = _windowMessages();
        const topIdx = _windowIndexOfId(windowTopId);
        if (topIdx <= 0) return true;   // nothing genuinely older landed
        const newStart = Math.max(0, topIdx - WINDOW_STEP);
        olderSlice = msgs.slice(newStart, topIdx);
        if (!olderSlice.length) return true;
    }

    _windowSuppressAutoScroll = true;
    try {
        await updateChat(chat, olderSlice, profile, false);
    } finally { _windowSuppressAutoScroll = false; }
    windowTopId = olderSlice[0]?.id || windowTopId;

    // Maintain visual position for the prepended rows. Programmatic: prepend grows
    // scrollHeight, so we add back the delta to hold the viewport — must not read as user intent.
    const scrollHeightAfter = domChatMessages.scrollHeight;
    beginProgrammaticScroll();
    domChatMessages.scrollTop = scrollTopBefore + (scrollHeightAfter - scrollHeightBefore);

    // Drop from the bottom (below the viewport → scrollTop unaffected). This moves
    // the window off the live tail.
    const overOld = _windowRenderedRowCount() - MAX_WINDOW_ROWS;
    if (overOld > 0) { _windowDropBottom(overOld); windowAtTail = false; }
    refreshScrollReturnButton();
    tryPlacePendingDivider();
    return true;
}

/**
 * Scroll-down: append the next WINDOW_STEP newer rows below the rendered window,
 * then drop from the TOP and apply scrollTop -= droppedHeight SYNCHRONOUSLY so the
 * viewport stays fixed. Returns true if it handled the scroll.
 *
 * Gap-aware. Sources, in order:
 *  1. In-memory rows below the window AND the window bottom is NOT the gap edge:
 *     append the slice directly, no backend. (Within a buffered segment we never
 *     re-query — the user explicitly wants no re-decrypt while a region is buffered.)
 *  2. Window bottom IS the gap boundary (a seek left [seek … GAP … tail]): anchored
 *     DB load `get_messages_around(windowBottomId, after=WINDOW_STEP)` to FILL the
 *     gap interior. addEvents(append) inserts the fetched rows at the gap boundary
 *     (not the array end) and closes the gap when they reach the tail.
 *  3. Window bottom IS the array end (post-seek, no tail merged / gap already at the
 *     edge): same anchored load; fewer than WINDOW_STEP back = the live tail →
 *     windowAtTail = true.
 */
async function windowExtendNewer() {
    if (!CHAT_WINDOW_ENABLED) return false;
    _windowReseatAnchorsFromDom();
    const range = _currentWindowRange();
    if (!range) return false;
    const [, end] = range;   // endIdx exclusive
    const msgs0 = _windowMessages();
    if (!msgs0) return false;

    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return false;
    const isGroup = chatIsGroup(chat);
    const profile = !isGroup ? getProfile(chat.id) : null;

    // At the gap boundary the next in-memory rows belong to the TAIL (disjoint from
    // the seek region) — slicing across them would skip the unfetched gap interior.
    // So treat the gap edge as a backend boundary even though rows sit below it.
    const gapAfterId = eventCache.getGapAfterId(strOpenChat);
    const atGapEdge = gapAfterId != null && windowBottomId === gapAfterId;

    // When a gap sits ahead in the seek region, never slide an in-memory step past it
    // (that would weld the tail on). Clamp the in-memory reach to the gap boundary; the
    // step that lands exactly on the gap edge then triggers the anchored fill next call.
    let inMemReach = msgs0.length;
    if (gapAfterId != null) {
        const gapIdx = msgs0.findIndex(m => m.id === gapAfterId);
        if (gapIdx !== -1 && end - 1 <= gapIdx) inMemReach = gapIdx + 1;   // bottom is in/at the seek region
    }

    let newerSlice;
    if (end < inMemReach && !atGapEdge) {
        // In-memory rows sit below the window (same segment) — append them, no backend.
        const newEnd = Math.min(inMemReach, end + WINDOW_STEP);
        newerSlice = msgs0.slice(end, newEnd);
        if (!newerSlice.length) return false;
    } else {
        // Window bottom is the gap edge or the array end: load newer rows from the DB
        // anchored to it. When filling a gap, addEvents splices at the boundary and
        // closes the gap on meet; when at the array end (no tail), it appends.
        const anchorId = windowBottomId;
        if (!anchorId) return false;
        const chatId = strOpenChat;
        let newer = [];
        try {
            newer = await invoke('get_messages_around', { chatId, anchorId, before: 0, after: WINDOW_STEP }) || [];
        } catch (e) { console.warn('[unread-jump] extend-newer anchored load failed:', e); }
        if (strOpenChat !== chatId) return true;

        // No genuinely-newer rows in the DB past the anchor.
        if (!newer.length) {
            if (atGapEdge) {
                // The DB has nothing between the seek edge and the tail (the only rows
                // beyond are unpersisted tail sends). Collapse the gap so the in-memory
                // tail becomes reachable by in-segment scrolling; we are NOT yet at the
                // visible tail (its rows still sit below in the buffer).
                eventCache.closeGap(chatId);
                chat.messages = eventCache.getEventsRef(chatId) || chat.messages;
                const msgs = _windowMessages();
                const botIdx = _windowIndexOfId(windowBottomId);
                if (botIdx === -1) { _windowReseatAnchorsFromDom(); return true; }
                const newEnd = Math.min(msgs.length, botIdx + 1 + WINDOW_STEP);
                newerSlice = msgs.slice(botIdx + 1, newEnd);
                if (!newerSlice.length) { windowAtTail = true; refreshScrollReturnButton(); return true; }
            } else {
                // No gap, DB exhausted past the bottom → the live tail.
                windowAtTail = true;
                refreshScrollReturnButton();
                return true;
            }
        } else {
            // Splice the fetched rows in (gap-aware). When a gap was open, addEvents inserts
            // at the boundary and advances/closes the gap; otherwise appends to the tail.
            const wasGap = atGapEdge;
            eventCache.addEvents(chatId, newer, false);
            // A short batch means the DB interior past the anchor is exhausted → the rows
            // beyond are the tail. Force-close any still-open gap so the next scroll-down
            // slides into the tail in memory (addEvents keeps a gap open on a full batch).
            if (wasGap && newer.length < WINDOW_STEP) eventCache.closeGap(chatId);
            chat.messages = eventCache.getEventsRef(chatId) || chat.messages;

            const msgs = _windowMessages();
            const botIdx = _windowIndexOfId(windowBottomId);
            if (botIdx === -1) { _windowReseatAnchorsFromDom(); return true; }
            const startIdx = botIdx + 1;
            const newEnd = Math.min(msgs.length, startIdx + WINDOW_STEP);
            newerSlice = msgs.slice(startIdx, newEnd);
            if (!newerSlice.length) { windowAtTail = true; refreshScrollReturnButton(); return true; }
        }
    }

    _windowSuppressAutoScroll = true;
    try {
        await updateChat(chat, newerSlice, profile, false);
    } finally { _windowSuppressAutoScroll = false; }
    windowBottomId = newerSlice[newerSlice.length - 1]?.id || windowBottomId;

    // Authoritative tail check: the window's bottom row is the last in the buffer and
    // there's no gap left to fill. A gap-fill that just closed (seek met tail) still
    // leaves tail rows below us in memory, so this stays false until we scroll into them.
    {
        const msgsFinal = _windowMessages() || [];
        const atBufferEnd = msgsFinal.length > 0 && windowBottomId === msgsFinal[msgsFinal.length - 1].id;
        if (atBufferEnd && eventCache.getGapAfterId(strOpenChat) == null) windowAtTail = true;
    }

    // Drop from the top (above the viewport → must compensate scrollTop). Apply
    // synchronously in this frame (WKWebView flashes if deferred to rAF).
    const overNew = _windowRenderedRowCount() - MAX_WINDOW_ROWS;
    if (overNew > 0) {
        const droppedHeight = _windowDropTop(overNew);
        // Drop-top moves scrollTop UP — guard so it isn't misread as a user scroll-up.
        if (droppedHeight > 0) { beginProgrammaticScroll(); domChatMessages.scrollTop -= droppedHeight; }
    }
    refreshScrollReturnButton();
    tryPlacePendingDivider();
    return true;
}

// Procedural scroll state
const proceduralScrollState = {
    isLoading: false,
    renderedMessageCount: 0,
    totalMessageCount: 0,
    messagesPerBatch: 20,
    scrollThreshold: 300, // pixels from top to trigger load
    lastScrollHeight: 0, // Track scroll height for media load correction
    isLoadingOlderMessages: false, // Flag to indicate we're in procedural load mode
    chatId: null, // Current chat ID for cache lookups
    useCache: false // Whether to use the message cache
};

// Community network-pagination guards (keyed by channel id): one in-flight older-page fetch
// at a time, and a set of channels whose network history-start has been reached (stop trying).
const _communityOlderInFlight = new Set();
const _communityOlderExhausted = new Set();

/**
 * When a Community channel's LOCAL (DB) history is exhausted on scroll-up, fetch an older
 * page from the network, and — if the DB grew — load it (prepend). This is what blends DB
 * pagination (offline / already-fetched) with network pagination (no known prior history).
 * Anti-stampede here + in the backend; once the network has nothing older, we stop asking.
 */
async function maybeLoadCommunityOlderFromNetwork(chatId) {
    if (!chatId || _communityOlderInFlight.has(chatId) || _communityOlderExhausted.has(chatId)) return;
    const chat = arrChats.find(c => c.id === chatId);
    if (!chat || chat.chat_type !== 'Community') return;

    // Cursor = the TRUE oldest message we hold. The cache array is NOT reliably sorted after
    // back-paging (loadMoreEvents prepends descending batches), so reduce to the minimum `at`
    // rather than trusting index 0. getEventsRef is read-only (no LRU side effect).
    const events = eventCache.getEventsRef(chatId) || chat.messages || [];
    if (!events.length) return;
    const oldestMs = events.reduce((min, e) => (e.at < min ? e.at : min), events[0].at);

    _communityOlderInFlight.add(chatId);
    try {
        const prevTotal = (eventCache.getStats(chatId) || {}).totalInDb || 0;
        const res = await invoke('sync_community_channel', { channelId: chatId, beforeMs: oldestMs });
        // Refresh the cache count so hasMoreEvents reflects any newly-ingested older rows,
        // then load (prepend) them.
        const newTotal = await invoke('get_chat_message_count', { chatId });
        if (newTotal > prevTotal) {
            eventCache.updateTotalCount(chatId, newTotal);
            if (strOpenChat === chatId) loadMoreMessages();
        }
        // Terminate ONLY on the backend's authoritative "no more older" signal — a page that
        // returned just already-known events (no count growth) does NOT mean history's start.
        if (res && res.reached_start) _communityOlderExhausted.add(chatId);
    } catch (e) {
        console.warn('Community older-page fetch failed:', e);
    } finally {
        _communityOlderInFlight.delete(chatId);
    }
}

/**
 * Correct scroll position when media loads during procedural scroll
 * This prevents "snap-back" when images/videos load after messages are rendered
 */
function correctScrollForMediaLoad() {
    // Only correct if we're not at the bottom and we have a baseline
    if (!proceduralScrollState.lastScrollHeight || !domChatMessages) return;
    
    const currentScrollHeight = domChatMessages.scrollHeight;
    const scrollHeightDiff = currentScrollHeight - proceduralScrollState.lastScrollHeight;
    
    // Only correct if there's a meaningful difference (media loaded)
    if (scrollHeightDiff > 5) {
        const currentScrollTop = domChatMessages.scrollTop;
        // Programmatic media-load correction — don't read as user intent.
        beginProgrammaticScroll();
        domChatMessages.scrollTop = currentScrollTop + scrollHeightDiff;
        proceduralScrollState.lastScrollHeight = currentScrollHeight;
    }
}

/**
 * Re-pin / position-correct the chat after an in-chat element grows or shrinks
 * once its row is already laid out: media finishing load, an emoji-pack preview
 * resolving its canvas height, the "New" unread divider being inserted, etc.
 *
 * Same three branches the media-load path uses, so every resizable element
 * behaves identically:
 *  - inside the open window  → snap to bottom (user just opened, expects bottom)
 *  - back-paging older history → keep the visual anchor (don't snap away)
 *  - steady-state            → soft scroll only if the user is still pinned
 */
function compensateChatScrollForResize() {
    if (!domChatMessages) return;
    if (chatOpenTimestamp && Date.now() - chatOpenTimestamp < 100) {
        scrollToBottom(domChatMessages, false);
    } else if (proceduralScrollState.isLoadingOlderMessages) {
        correctScrollForMediaLoad();
    } else {
        softChatScroll();
    }
}

/**
 * Handle procedural scroll loading of older messages
 */
function handleProceduralScroll() {
    if (!strOpenChat || proceduralScrollState.isLoading) return;

    const scrollTop = domChatMessages.scrollTop;
    const maxScroll = domChatMessages.scrollHeight - domChatMessages.clientHeight;
    const pxFromBottom = maxScroll - scrollTop;
    const nearTop = scrollTop <= proceduralScrollState.scrollThreshold;
    const nearBottom = pxFromBottom <= proceduralScrollState.scrollThreshold;

    // Scroll DOWN near the DOM bottom: if newer in-memory rows sit below the
    // rendered window (we dropped them on an earlier extend), slide the window
    // toward "now" from memory — no backend.
    if (CHAT_WINDOW_ENABLED && nearBottom && !isAtDataBottom()) {
        proceduralScrollState.isLoading = true;
        windowExtendNewer().finally(() => { proceduralScrollState.isLoading = false; });
        return;
    }

    // Scroll UP near the top.
    if (!nearTop) return;

    // Windowed: windowExtendOlder slides up from in-memory rows above the window
    // and, when the window top reaches the slice start, anchored-loads older rows
    // from the DB itself (community network-fill included). It owns the whole
    // scroll-up path, so the legacy cache/community fall-through is skipped.
    if (CHAT_WINDOW_ENABLED) {
        proceduralScrollState.isLoading = true;
        windowExtendOlder().finally(() => { proceduralScrollState.isLoading = false; });
        return;
    }

    // Check if we're using cache mode
    if (proceduralScrollState.useCache) {
        // Use cache stats to determine if there are more events
        const cacheStats = eventCache.getStats(strOpenChat);
        if (!cacheStats?.hasMoreEvents) {
            // Local DB exhausted. Community channels then try the network for older history
            // (DB ⊕ network pagination); DMs have no further source, so they stop.
            maybeLoadCommunityOlderFromNetwork(strOpenChat);
            return;
        }
        // Load more events
        loadMoreMessages();
        return;
    }

    // Legacy mode: check chat.messages array
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat || !chat.messages) return;

    // Check if there are more messages to load
    const totalMessages = chat.messages.length;
    if (proceduralScrollState.renderedMessageCount >= totalMessages) return;

    // Load more messages
    loadMoreMessages();
}

/**
 * Load the next batch of older messages
 * Uses the message cache for on-demand loading from database
 */
async function loadMoreMessages() {
    if (proceduralScrollState.isLoading || !strOpenChat) return;

    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return;

    // Check if we should use the event cache
    if (proceduralScrollState.useCache) {
        // Use cache-based loading
        const cacheStats = eventCache.getStats(strOpenChat);

        // Check if there are more events to load
        if (!cacheStats?.hasMoreEvents) {
            return;
        }

        proceduralScrollState.isLoading = true;
        proceduralScrollState.isLoadingOlderMessages = true;

        // Store scroll position BEFORE rendering
        const scrollHeightBefore = domChatMessages.scrollHeight;
        const scrollTopBefore = domChatMessages.scrollTop;

        // Load more events from cache (fetches from DB if needed)
        const olderMessages = await eventCache.loadMoreEvents(
            strOpenChat,
            proceduralScrollState.messagesPerBatch
        );

        if (olderMessages.length === 0) {
            proceduralScrollState.isLoading = false;
            proceduralScrollState.isLoadingOlderMessages = false;
            return;
        }

        // Reveal any buffered system events now inside the expanded window so
        // they prepend in chronological order alongside the older messages.
        const revealedSys = revealSystemEventsInWindow(strOpenChat);

        // Update the chat object's messages array for compatibility
        chat.messages = eventCache.getEvents(strOpenChat) || [];

        // Get profile for rendering
        const isGroup = chatIsGroup(chat);
        const profile = !isGroup ? getProfile(chat.id) : null;

        // Render the older events + newly-revealed system events (prepend)
        await updateChat(chat, [...olderMessages, ...revealedSys], profile, false);

        // Update rendered count
        proceduralScrollState.renderedMessageCount += olderMessages.length;

        // Update total from cache stats
        const newStats = eventCache.getStats(strOpenChat);
        proceduralScrollState.totalMessageCount = newStats?.totalInDb || proceduralScrollState.totalMessageCount;

        // Correct scroll position to prevent "snapping"
        const scrollHeightAfter = domChatMessages.scrollHeight;
        const scrollHeightDiff = scrollHeightAfter - scrollHeightBefore;

        // Adjust scroll position to maintain visual position (programmatic).
        beginProgrammaticScroll();
        domChatMessages.scrollTop = scrollTopBefore + scrollHeightDiff;

        // DOM windowing: dropped rows are below the viewport (we just prepended
        // above), so scrollTop is unaffected — only scrollHeight shrinks. Trim
        // back down to MAX.
        if (CHAT_WINDOW_ENABLED) {
            _windowReseatAnchorsFromDom();
            const over = _windowRenderedRowCount() - MAX_WINDOW_ROWS;
            if (over > 0) _windowDropBottom(over);
            // Dropping the bottom can move the window off the live tail.
            refreshScrollReturnButton();
        }

        // Store the current scroll height for media load correction
        proceduralScrollState.lastScrollHeight = domChatMessages.scrollHeight;

        proceduralScrollState.isLoading = false;

        // Keep the flag active for a bit longer to catch late-loading media
        setTimeout(() => {
            proceduralScrollState.isLoadingOlderMessages = false;
            proceduralScrollState.lastScrollHeight = 0;
        }, 2000);

        return;
    }

    // Legacy behavior: load from chat.messages array
    if (!chat.messages) return;

    const totalMessages = chat.messages.length;
    const currentRendered = proceduralScrollState.renderedMessageCount;

    if (currentRendered >= totalMessages) return;

    proceduralScrollState.isLoading = true;
    proceduralScrollState.isLoadingOlderMessages = true;

    // Calculate how many more messages to load
    const messagesToLoad = Math.min(
        proceduralScrollState.messagesPerBatch,
        totalMessages - currentRendered
    );

    // Get the next batch of older messages
    const startIndex = totalMessages - currentRendered - messagesToLoad;
    const endIndex = totalMessages - currentRendered;
    const olderMessages = chat.messages.slice(startIndex, endIndex);

    if (olderMessages.length === 0) {
        proceduralScrollState.isLoading = false;
        proceduralScrollState.isLoadingOlderMessages = false;
        return;
    }

    // Store scroll position BEFORE rendering
    const scrollHeightBefore = domChatMessages.scrollHeight;
    const scrollTopBefore = domChatMessages.scrollTop;

    // Get profile for rendering
    const isGroup = chatIsGroup(chat);
    const profile = !isGroup ? getProfile(chat.id) : null;

    // Render the older messages
    await updateChat(chat, olderMessages, profile, false);

    // Update rendered count
    proceduralScrollState.renderedMessageCount += messagesToLoad;

    // Correct scroll position to prevent "snapping"
    const scrollHeightAfter = domChatMessages.scrollHeight;
    const scrollHeightDiff = scrollHeightAfter - scrollHeightBefore;

    // Adjust scroll position to maintain visual position (programmatic).
    beginProgrammaticScroll();
    domChatMessages.scrollTop = scrollTopBefore + scrollHeightDiff;

    // Store the current scroll height for media load correction
    proceduralScrollState.lastScrollHeight = domChatMessages.scrollHeight;

    proceduralScrollState.isLoading = false;

    // Keep the flag active for a bit longer to catch late-loading media
    setTimeout(() => {
        proceduralScrollState.isLoadingOlderMessages = false;
        proceduralScrollState.lastScrollHeight = 0;
    }, 2000);
}

/**
 * Initialize procedural scroll state for a chat (legacy - uses chat.messages)
 * @param {Object} chat - The chat object
 */
function initProceduralScroll(chat) {
    proceduralScrollState.useCache = false;
    proceduralScrollState.chatId = null;
    
    if (!chat || !chat.messages) {
        proceduralScrollState.renderedMessageCount = 0;
        proceduralScrollState.totalMessageCount = 0;
        return;
    }

    const totalMessages = chat.messages.length;
    proceduralScrollState.totalMessageCount = totalMessages;
    
    // Start by rendering the most recent batch
    const initialBatch = Math.min(proceduralScrollState.messagesPerBatch, totalMessages);
    proceduralScrollState.renderedMessageCount = initialBatch;
}

/**
 * Initialize procedural scroll state with message cache support
 * @param {string} chatId - The chat identifier
 * @param {number} initialCount - Number of messages initially loaded
 * @param {number} totalCount - Total messages in database
 */
function initProceduralScrollWithCache(chatId, initialCount, totalCount) {
    proceduralScrollState.useCache = true;
    proceduralScrollState.chatId = chatId;
    proceduralScrollState.renderedMessageCount = initialCount;
    proceduralScrollState.totalMessageCount = totalCount;
    proceduralScrollState.isLoading = false;
    proceduralScrollState.isLoadingOlderMessages = false;
    proceduralScrollState.lastScrollHeight = 0;
    // Re-arm network back-paging each open: if history truly ended, the backend's
    // history-start short-circuit answers instantly (no network) and we re-latch; if it
    // grew reachable again (rejoin, epoch backfill), scroll-up probes instead of staying dead.
    _communityOlderExhausted.delete(chatId);
}

/**
 * Reset procedural scroll state (call when closing chat)
 */
function resetProceduralScroll() {
    proceduralScrollState.isLoading = false;
    proceduralScrollState.renderedMessageCount = 0;
    proceduralScrollState.totalMessageCount = 0;
    proceduralScrollState.lastScrollHeight = 0;
    proceduralScrollState.isLoadingOlderMessages = false;
    proceduralScrollState.chatId = null;
    proceduralScrollState.useCache = false;
}

/**
 * Wait for all media elements in the chat to finish loading
 * @param {number} timeout - Maximum time to wait in milliseconds (default: 5000)
 */
async function waitForMediaToLoad(timeout = 5000) {
    // Get all media elements in the chat
    const images = Array.from(domChatMessages.querySelectorAll('img'));
    const videos = Array.from(domChatMessages.querySelectorAll('video'));
    const allMedia = [...images, ...videos];
    
    if (allMedia.length === 0) return;
    
    // Create promises for each media element
    const mediaPromises = allMedia.map(media => {
        return new Promise((resolve) => {
            // If already loaded, resolve immediately
            if (media instanceof HTMLImageElement) {
                if (media.complete && media.naturalHeight !== 0) {
                    resolve();
                    return;
                }
            } else if (media instanceof HTMLVideoElement) {
                if (media.readyState >= 1) { // HAVE_METADATA or better
                    resolve();
                    return;
                }
            }
            
            // Set up load event listeners
            const onLoad = () => {
                cleanup();
                resolve();
            };
            
            const onError = () => {
                cleanup();
                resolve(); // Resolve anyway to not block
            };
            
            const cleanup = () => {
                media.removeEventListener('load', onLoad);
                media.removeEventListener('loadedmetadata', onLoad);
                media.removeEventListener('error', onError);
            };
            
            // Add listeners
            if (media instanceof HTMLImageElement) {
                media.addEventListener('load', onLoad, { once: true });
                media.addEventListener('error', onError, { once: true });
            } else if (media instanceof HTMLVideoElement) {
                media.addEventListener('loadedmetadata', onLoad, { once: true });
                media.addEventListener('error', onError, { once: true });
            }
            
            // Timeout fallback
            setTimeout(() => {
                cleanup();
                resolve();
            }, timeout);
        });
    });
    
    // Wait for all media to load or timeout
    await Promise.race([
        Promise.all(mediaPromises),
        new Promise(resolve => setTimeout(resolve, timeout))
    ]);
    
    // Add a small buffer for layout stabilization
    await new Promise(resolve => setTimeout(resolve, 100));
}

/**
 * Load and scroll to a specific message that isn't currently rendered
 * If the message isn't in memory, fetches from backend database
 * @param {string} targetMsgId - The ID of the message to scroll to
 */
async function loadAndScrollToMessage(targetMsgId) {
    if (!strOpenChat) return;

    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return;

    // Initialize messages array if needed
    if (!chat.messages) chat.messages = [];

    // Find the target message in the chat
    let targetMsgIndex = chat.messages.findIndex(m => m.id === targetMsgId);

    // Target not in the loaded window → anchored-load a slice centered on it and MERGE it into
    // the cache via seedWindow. seedWindow mutates the events array IN PLACE (alias-safe) and
    // KEEPS the pinned tail, so the buffer becomes [seek-region … GAP … tail]: the chat-list
    // preview still reads the true latest, while renderWindow (reading the cache ref) sees the
    // target. A plain reassign would orphan the alias and the target would never render.
    if (targetMsgIndex === -1) {
        let slice = [];
        try {
            slice = await invoke('get_messages_around', {
                chatId: strOpenChat, anchorId: targetMsgId, before: 50, after: 50,
            }) || [];
        } catch (error) {
            return console.warn('Failed to anchored-load around reply target:', error);
        }
        if (!slice.length) return console.warn('Reply target not found in local DB');
        if (strOpenChat !== chat.id) return;   // navigated away mid-fetch
        eventCache.seedWindow(strOpenChat, slice);
        chat.messages = eventCache.getEventsRef(strOpenChat) || chat.messages;
        windowAtTail = false;                   // the target is mid-history, not the live tail
        targetMsgIndex = chat.messages.findIndex(m => m.id === targetMsgId);
        if (targetMsgIndex === -1) return console.warn('Reply target missing after anchored load');
    }

    // Render a BOUNDED window around the target (DOM windowing) rather than the
    // whole target→newest span — the rest of chat.messages stays in memory and
    // windows in as the user scrolls up/down. Falls back to target→newest when
    // the flag is off.
    const isGroup = chatIsGroup(chat);
    const profile = !isGroup ? getProfile(chat.id) : null;

    if (CHAT_WINDOW_ENABLED) {
        const start = targetMsgIndex - Math.floor(WINDOW_STEP / 2);
        const end = targetMsgIndex + WINDOW_STEP;
        await renderWindow(start, end);
        // The window is centered on the target, mid-history — its bottom is not the
        // live tail. Scrolling down will extend and re-latch windowAtTail at the tail.
        windowAtTail = false;
    } else {
        const contextMessages = 20;
        const startIndex = Math.max(0, targetMsgIndex - contextMessages);
        const messagesToLoad = chat.messages.slice(startIndex, chat.messages.length);
        proceduralScrollState.renderedMessageCount = messagesToLoad.length;
        proceduralScrollState.totalMessageCount = chat.messages.length;
        while (domChatMessages.firstElementChild) domChatMessages.firstElementChild.remove();
        await updateChat(chat, messagesToLoad, profile, false);
    }

    // Wait for all media (images, videos) to load before scrolling
    await waitForMediaToLoad();

    // Now scroll to the target message and flash to bring the user's eye to it.
    const domMsg = document.getElementById(targetMsgId);
    if (domMsg) {
        centerInView(domMsg);
        applyHighlight(domMsg, 'jumped');
    }
}

// ===========================================================================
// "Click to see Unread Messages" jump.
// Pill (top of chat) when unread sits above the opened window. On click, resolve the gap
// from now back to `last_read` — page LOCAL history, and for a community fetch older pages
// from the network (DM gaps fill via the negentropy archive, so we wait for them) — showing
// a spinner throughout, then drop the "New" divider above the first unread and center it.
// Bounded by a dual limit: give up only if last_read is >1 week old AND we've synced >500
// without reaching it; at the budget we checkpoint ("keep loading?") rather than hard-stop.
// ===========================================================================
const UNREAD_JUMP_STALE_MS = 7 * 24 * 60 * 60 * 1000;   // "over a week ago"
const UNREAD_JUMP_SYNC_BUDGET = 500;                    // events synced before the stale checkpoint
let _unreadJumpPill = null;
// The ORIGINAL off-page unread boundary, snapshotted when the pill appears. chat.last_read advances
// to the newest message on the open-time auto-mark, so it can't be read live for the divider.
let _unreadJumpLastReadId = null;
let _pendingUnreadDividerId = null;   // anchor row id awaiting render so the divider can land
let _pendingUnreadDividerAfter = false;   // true = insert the divider AFTER the anchor row (boundary anchor)

/** Land the pending manual-scroll "New" divider the moment its anchor row is in the DOM. The cache
 *  holds the boundary long before the bounded DOM window slides over it, so every render path calls
 *  this; it no-ops until the row renders, then inserts once and clears the pending id. The manual
 *  path anchors AFTER the boundary (last_read) row — that row is older than the first unread, so it
 *  survives the scroll-up bottom-trim longer and lands the divider on the first scroll-up pass. */
function tryPlacePendingDivider() {
    if (unreadDividerEl || !_pendingUnreadDividerId) return;
    const node = document.getElementById(_pendingUnreadDividerId);
    if (node) { insertUnreadDivider(node, _pendingUnreadDividerAfter); _pendingUnreadDividerId = null; }
}

/** The initial open renders a fixed COUNT of messages, not a viewport-sized amount. With short
 *  messages that count may not overflow the view → no scrollbar → the user can't scroll up to page
 *  older history or reach the unread frontier. Proactively page older until the content overflows
 *  (so the scroll-extend takes over) or history-start is hit; the prepends keep the user at the tail. */
async function ensureChatScrollable() {
    if (!CHAT_WINDOW_ENABLED || !domChatMessages) return;
    if (proceduralScrollState.isLoading) return;
    proceduralScrollState.isLoading = true;
    try {
        const chatId = strOpenChat;
        let guard = 0;
        while (guard++ < 15 && strOpenChat === chatId) {
            if (domChatMessages.scrollHeight - domChatMessages.clientHeight > 60) break;  // overflows → scrollable
            const before = (_windowMessages() || []).length;
            await windowExtendOlder();
            if (strOpenChat !== chatId) return;
            if ((_windowMessages() || []).length <= before) break;   // nothing older loaded → history start
        }
    } finally { proceduralScrollState.isLoading = false; }
}

/**
 * Manual unread-frontier reveal. When the jump pill is showing and the user scrolls UP to the
 * boundary themselves (instead of tapping the pill), drop the "New" divider above the first unread
 * as the boundary loads, then retire the pill + mark caught up the moment the divider scrolls into
 * view — same outcome as tapping the pill. Called (with cheap early-outs) from the scroll handler.
 */
function revealUnreadFrontierIfReached() {
    if (!_unreadJumpPill || !_unreadJumpPill.classList.contains('visible')) return;  // no active pill
    if (!domChatMessages || !_unreadJumpLastReadId) return;
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return;
    const msgs = _windowMessages();
    if (!msgs || !msgs.length) return;

    // Reveal the divider once the ORIGINAL boundary's row is loaded, so it's in place for the last
    // stretch up. Match against the snapshot, NOT chat.last_read — the open-time auto-mark advances
    // chat.last_read to the newest message, so reading it live always points at the bottom row.
    if (!unreadDividerEl) {
        const lastReadIdx = msgs.findIndex(m => m.id === _unreadJumpLastReadId);
        if (lastReadIdx === -1) return;   // boundary not loaded into the window yet
        // Anchor the divider AFTER the boundary (last_read) row, not before the first-unread row.
        // The boundary is OLDER than the first unread, so as the user scrolls UP it survives the
        // bounded-window bottom-trim longer (it's the exact row they're scrolling toward), landing
        // the divider on the FIRST pass. "After last_read" = "above the first new message" for any
        // sender, so this also drops the mine-skip dependency (own multi-device sends count).
        _pendingUnreadDividerId = _unreadJumpLastReadId;
        _pendingUnreadDividerAfter = true;
        tryPlacePendingDivider();
    }

    // Retire the pill + mark caught up the moment the divider is actually on screen.
    if (unreadDividerEl && unreadDividerEl.isConnected) {
        const dr = unreadDividerEl.getBoundingClientRect();
        const cr = domChatMessages.getBoundingClientRect();
        if (dr.bottom > cr.top && dr.top < cr.bottom) {   // divider overlaps the viewport
            hideUnreadJumpPill();
            if (chat.messages?.length) markAsRead(chat, chat.messages[chat.messages.length - 1]);
        }
    }
}

/** Put the pill into its spinner state with a status label. */
function setPillLoading(text) {
    if (!_unreadJumpPill) return;
    _unreadJumpPill.classList.add('loading', 'visible');
    _unreadJumpPill.textContent = text;
}

/** Floating pill at the top of the chat view; shown when unread history sits above the loaded window. */
function showUnreadJumpPill(count, lastReadId) {
    const chatView = document.getElementById('chat');
    if (!chatView) return;
    // Snapshot the boundary now — chat.last_read advances to the newest on the open-time auto-mark,
    // so the scroll-up divider reveal can't recover it later (it reads this instead).
    _unreadJumpLastReadId = lastReadId;
    _pendingUnreadDividerId = null;   // fresh pill: no divider target until the boundary is reached
    _pendingUnreadDividerAfter = false;
    if (!_unreadJumpPill) {
        _unreadJumpPill = document.createElement('button');
        _unreadJumpPill.className = 'unread-jump-pill';
        _unreadJumpPill.type = 'button';
        chatView.appendChild(_unreadJumpPill);
    }
    const n = Number(count) || 0;
    _unreadJumpPill.classList.remove('loading');
    // The pill only appears when last_read is OFF the opened page, so this count is a lower bound
    // (more unread sits in unsynced history) — always "N+" (capped 99+), never a false-exact figure.
    const label = n > 99 ? '99+' : (n > 0 ? `${n}+` : '');
    _unreadJumpPill.textContent = label ? `${label} new messages` : 'New messages';
    _unreadJumpPill.onclick = () => jumpToUnread(lastReadId);
    requestAnimationFrame(() => _unreadJumpPill && _unreadJumpPill.classList.add('visible'));
}

function hideUnreadJumpPill() {
    if (_unreadJumpPill) _unreadJumpPill.classList.remove('visible', 'loading');
    _pendingUnreadDividerId = null;
    _pendingUnreadDividerAfter = false;
}

/** Resolve the gap back to `last_read` (paging local + network), then drop the "New" divider
 *  above the first unread and center it. Spinner throughout; bounded by the dual-limit checkpoint. */
async function jumpToUnread(lastReadId) {
    if (!strOpenChat || !lastReadId) return;
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return;
    if (!chat.messages) chat.messages = [];
    const chatId = strOpenChat;
    setPillLoading('Syncing…');

    // last_read's timestamp WITHOUT merging its older context into chat.messages — merging would
    // drop the array's global-oldest below the boundary and trip termination before the gap fills.
    const isCommunity = chat.chat_type === 'Community';
    let boundaryAt = chat.messages.find(m => m.id === lastReadId)?.at ?? null;
    if (boundaryAt == null) {
        try {
            const around = await invoke('get_messages_around_id', { chatId, targetMessageId: lastReadId, contextBefore: 0 });
            boundaryAt = (around || []).find(m => m.id === lastReadId)?.at ?? null;
        } catch (e) { if (strOpenChat === chatId) console.warn('[unread-jump] last_read lookup failed:', e); }
    }
    if (boundaryAt == null) { hideUnreadJumpPill(); return; }

    // Freeze the window for the whole resolve: realtime echoes of the gap messages
    // must not touch the DOM or badge until Step 3 renders the jump. try/finally so a
    // navigate-away (the `strOpenChat !== chatId` bails) can't leave it stuck.
    _unreadJumpResolving = true;
    try {
    const stale = boundaryAt < (Date.now() - UNREAD_JUMP_STALE_MS);

    // Only REAL messages drive the resolve frontier. Presence/system events stream into chat.messages
    // during the walk (their own emit path) at timestamps spanning the whole gap — counting them would
    // drop "oldest" below the boundary and trip termination before the actual messages are paged in,
    // leaving a clump of joins with the interleaving messages missing.
    const oldestLoaded = () => {
        let min = Infinity;
        for (const m of chat.messages) if (!m.system_event && m.at < min) min = m.at;
        return min;   // Infinity when no real message is loaded yet
    };
    const reachedBoundary = () => chat.messages.some(m => m.id === lastReadId) || oldestLoaded() <= boundaryAt;

    // Progress % = how far the oldest-covered timestamp has walked from the opened window down to
    // last_read. The relay walk (Step 1) drives the frontier toward boundaryAt; monotonic, so a fast
    // local pull (Step 2) can't reset it. Meaningful to a user — "65% of the gap synced".
    const _span0 = oldestLoaded();
    const spanStart = Number.isFinite(_span0) ? _span0 : boundaryAt;
    let frontier = spanStart;
    const reportProgress = () => {
        if (strOpenChat !== chatId) return;
        const denom = spanStart - boundaryAt;
        const pct = denom > 0 ? Math.min(100, Math.max(0, Math.round((spanStart - frontier) / denom * 100))) : 100;
        setPillLoading(`Syncing… ${pct}%`);
    };

    // Step 1 (community): walk the relay backward by the page's `oldest_ms` cursor, ingesting the
    // now→last_read gap into the DB. This fills holes the local DB has from offline periods — which
    // offset-paging (Step 2) would otherwise silently skip. DMs skip this; negentropy fills the DB.
    if (isCommunity && chat.messages.length > 0 && !reachedBoundary()) {
        // Re-arm: a middle-gap fill must re-page even if a prior walk hit history-start. The first
        // page resets the backend floor so the walk pages from the recent window DOWN through the
        // gap (a hole newer than the bottom cursor that strict older-than-bottom paging can't reach).
        _communityOlderExhausted.delete(chatId);
        let cursor = oldestLoaded();
        let synced = 0, guard = 0, firstSync = true;
        while (cursor > boundaryAt && guard++ < 400) {
            if (strOpenChat !== chatId) return;
            if (_communityOlderExhausted.has(chatId)) break;
            // Dual-limit: only old AND large gets the checkpoint; recent or small walks freely.
            if (stale && synced >= UNREAD_JUMP_SYNC_BUDGET) {
                const keepGoing = await popupConfirm(
                    'Lots of unread history',
                    `Loaded ${UNREAD_JUMP_SYNC_BUDGET}+ messages and there's still more from over a week ago. Keep loading?`,
                    false, '', '', '', 'Keep loading'
                );
                if (!keepGoing) break;
                synced = 0;
            }
            let res = null;
            try { res = await invoke('sync_community_channel', { channelId: chatId, beforeMs: cursor, resetCursor: firstSync }); } catch (e) { if (strOpenChat === chatId) console.warn('[unread-jump] sync threw', e); break; }
            firstSync = false;
            if (res && res.reached_start) _communityOlderExhausted.add(chatId);
            synced += (res && res.new_messages) || 0;
            const next = res && res.oldest_ms;
            if (next == null || next >= cursor) break;   // relay returned nothing older
            cursor = next;
            frontier = Math.min(frontier, cursor);
            reportProgress();
        }
        // The walk grew the DB; refresh the cache count so loadMoreEvents will page the new rows.
        try {
            const _cnt = await invoke('get_chat_message_count', { chatId });
            eventCache.updateTotalCount(chatId, _cnt);
        } catch (e) { if (strOpenChat === chatId) console.warn('[unread-jump] count failed', e); }
    }

    // Step 2: ANCHORED random-access load. After Step 1 filled any DB holes, pull a
    // bounded window centered on last_read directly (O(window), not O(depth)) — read
    // above, unread below — and MERGE it into the cache, keeping the pinned tail.
    if (strOpenChat !== chatId) return;
    setPillLoading('Loading…');
    let slice = null;
    try {
        slice = await invoke('get_messages_around', { chatId, anchorId: lastReadId, before: 50, after: 50 });
    } catch (e) {
        if (strOpenChat === chatId) console.warn('[unread-jump] get_messages_around threw (anchor likely not in DB):', e);
    }
    if (strOpenChat !== chatId) return;
    if (!slice || !slice.length) {
        // Anchor not in DB (id drift / retention / hole the walk couldn't fill).
        // Bail the pill gracefully and leave the current window untouched.
        hideUnreadJumpPill();
        return;
    }

    // Seed the window: merge the slice into the cache in place (alias-safe), keeping
    // the pinned tail (so the chat-list preview stays correct) and recording the gap
    // between the seek region and the tail. Then render the slice.
    eventCache.seedWindow(chatId, slice);
    chat.messages = eventCache.getEventsRef(chatId) || chat.messages;

    // Resolve the divider against the SLICE. When last_read is present, the first
    // unread is the first message strictly newer than it; when it isn't (id drift /
    // retention), the first message at-or-after boundaryAt is itself the first unread.
    // Skip own sends + system-event joins so the divider lands on a real contact row.
    const lastReadIdx = slice.findIndex(m => m.id === lastReadId);
    const lastReadAt = lastReadIdx !== -1 ? slice[lastReadIdx].at : boundaryAt;
    let dividerIdx = -1;
    for (let i = 0; i < slice.length; i++) {
        const m = slice[i];
        if (!m || m.system_event || m.mine) continue;
        if (m.at > lastReadAt) { dividerIdx = i; break; }
    }
    // No newer real contact message in the slice — fall back to just-after last_read,
    // else the slice end (degenerate, divider hugs the bottom).
    if (dividerIdx === -1) {
        dividerIdx = lastReadIdx !== -1
            ? Math.min(lastReadIdx + 1, slice.length - 1)
            : slice.length - 1;
    }
    dividerIdx = Math.max(0, Math.min(dividerIdx, slice.length - 1));

    const isGroup = chatIsGroup(chat);
    const profile = !isGroup ? getProfile(chat.id) : null;

    // We're deliberately scrolling up to the boundary: drop the pin BEFORE rendering so a media
    // 'load' firing softChatScroll can't slam us back to the bottom mid-jump. The anchored slice
    // is centered on last_read — its bottom is NOT the live tail, so the window is seeked away.
    chatPinnedToBottom = false;
    windowAtTail = false;
    clearUnreadDivider();

    // Render a BOUNDED window around the divider's position IN THE MERGED BUFFER.
    // seedWindow kept the pinned tail, so the buffer is [seek-region … GAP … tail];
    // the slice's rows map to the seek region at the front, but rendering by raw
    // slice indices would walk into the gap/tail. Resolve the divider row's buffer
    // index and render a window centered on it (the seek region), with the gap-aware
    // scroll-extends paging beyond it.
    const dividerMsg = slice[dividerIdx];
    if (CHAT_WINDOW_ENABLED) {
        const buf = _windowMessages() || [];
        const bufDividerIdx = dividerMsg ? buf.findIndex(m => m.id === dividerMsg.id) : -1;
        const center = bufDividerIdx !== -1 ? bufDividerIdx : 0;
        const start = center - Math.floor(MAX_WINDOW_ROWS / 2);
        const end = center + Math.ceil(MAX_WINDOW_ROWS / 2);
        await renderWindow(start, end);
    } else {
        proceduralScrollState.renderedMessageCount = slice.length;
        proceduralScrollState.totalMessageCount = slice.length;
        while (domChatMessages.firstElementChild) domChatMessages.firstElementChild.remove();
        await updateChat(chat, slice, profile, false);
    }
    await waitForMediaToLoad();
    chatPinnedToBottom = false;   // re-assert after updateChat's own auto-scroll

    // Inject exactly one divider above the first unread row and center it (the
    // scroll is the visible proof the jump landed). clearUnreadDivider above left
    // unreadDividerEl null, so renderWindow did NOT re-insert one — this is the
    // sole divider in the DOM.
    const node = dividerMsg && document.getElementById(dividerMsg.id);
    if (node) {
        insertUnreadDivider(node);
        if (unreadDividerEl) centerInView(unreadDividerEl);
    } else {
        // Row not in DOM (shouldn't happen — the whole slice is rendered) — center last_read if present.
        const lr = document.getElementById(lastReadId);
        if (lr) centerInView(lr);
    }
    // Jumping to unread IS catching up — mark the whole chat read (last_read = the true newest, the
    // pinned tail's last row), so the badge + pill clear in one shot even though the viewport rests
    // at the divider. Without this last_read stays at the old boundary and the pill survives reopen.
    if (chat.messages?.length) markAsRead(chat, chat.messages[chat.messages.length - 1]);
    hideUnreadJumpPill();
    } finally { _unreadJumpResolving = false; }
}