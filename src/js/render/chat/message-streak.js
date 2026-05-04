/**
 * Temporal helpers for the Discord-style chat shell — streak collapse + day
 * separators. Both decide whether two messages belong "together" or "apart" by
 * timestamp; they live in the same file so the comparators stay co-located.
 *
 * Streak: consecutive messages from the same sender within 7 minutes collapse
 * into a continuation row (no avatar / no header). Replies always start a new
 * streak. A non-`.dmsg` element between two messages (day separator, system
 * event, legacy timestamp) ALWAYS breaks the streak, regardless of sender/time.
 *
 *   first         — full row with avatar + author + timestamp header
 *   continuation  — content-only, hover reveals timestamp in gutter
 *
 * Day separator: a `.msg-inline-timestamp` line is inserted whenever a message
 * crosses a calendar-day boundary from the previous message (every boundary,
 * not gap-based). `_dmsgIsDifferentDay` is the trigger.
 */

const STREAK_WINDOW_MS = 7 * 60 * 1000;

/**
 * Returns true when t1 and t2 fall on different calendar days (local time).
 * A null/undefined `t1` is treated as "different" so a chat's very first
 * message always gets a separator above it.
 */
function _dmsgIsDifferentDay(t1, t2) {
    if (!t2) return false;
    if (!t1) return true;
    const d1 = new Date(t1);
    const d2 = new Date(t2);
    return d1.getFullYear() !== d2.getFullYear()
        || d1.getMonth() !== d2.getMonth()
        || d1.getDate() !== d2.getDate();
}

/**
 * Pure decision: should `currMsg` collapse into the streak started by `prevMsg`?
 * Operates on plain message-shape objects {at, mine, npub, replied_to}.
 */
function shouldCollapseStreak(prevMsg, currMsg) {
    if (!prevMsg || !currMsg) return false;

    // Replies always start a new streak — the quote block needs visual breath.
    if (currMsg.replied_to) return false;

    // Time check (7-min window).
    if (!prevMsg.at || !currMsg.at) return false;
    if (currMsg.at - prevMsg.at >= STREAK_WINDOW_MS) return false;
    // Out-of-order delivery: if curr is older than prev, don't collapse.
    if (currMsg.at < prevMsg.at) return false;

    // Same sender check.
    //   own→own:  collapse
    //   them→them with same npub: collapse
    //   them→them in DM (no npub on either, single-counterparty): collapse
    if (currMsg.mine && prevMsg.mine) return true;
    if (!currMsg.mine && !prevMsg.mine) {
        if (currMsg.npub && prevMsg.npub) return currMsg.npub === prevMsg.npub;
        if (!currMsg.npub && !prevMsg.npub) return true;
        return false;
    }
    return false;
}

/**
 * Walk forward from `el` (inclusive) to find the nearest `.dmsg` row.
 */
function _dmsgWalkForwardToRow(el) {
    while (el) {
        if (el.classList && el.classList.contains('dmsg')) return el;
        el = el.nextElementSibling;
    }
    return null;
}

/**
 * Look up a Message object for a given row. Prefers the cached `row._dmsgMsg`
 * (set in renderMessage) so this runs in O(1); falls back to scanning
 * chat.messages by id only if the cache is missing (defensive).
 */
function _dmsgLookupMessage(rowEl) {
    if (!rowEl) return null;
    if (rowEl._dmsgMsg) return rowEl._dmsgMsg;
    if (!rowEl.id) return null;
    if (typeof arrChats === 'undefined' || typeof strOpenChat === 'undefined') return null;
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat || !chat.messages) return null;
    return chat.messages.find(m => m.id === rowEl.id) || null;
}

/**
 * Compute the streak attribute ('first' or 'continuation') for `msg` given the
 * element it sits immediately after in the DOM.
 *
 * If `anchorEl` is anything other than a `.dmsg` (timestamp, system event, day
 * separator, null), the streak is broken — return 'first'. Otherwise consult
 * shouldCollapseStreak with both message objects.
 */
function _dmsgComputeStreakAttr(msg, anchorEl) {
    if (!anchorEl) return 'first';
    if (!anchorEl.classList || !anchorEl.classList.contains('dmsg')) return 'first';

    const prevMsg = _dmsgLookupMessage(anchorEl);
    return shouldCollapseStreak(prevMsg, msg) ? 'continuation' : 'first';
}

/**
 * Re-evaluate the streak attribute for `rowEl` AND the row that follows it.
 * Call after any DOM mutation that might invalidate previously-computed streak
 * state — message insertion, removal, replacement, prepending older messages.
 */
function recomputeStreakBoundary(rowEl) {
    if (!rowEl || !rowEl.classList || !rowEl.classList.contains('dmsg')) return;

    const msg = _dmsgLookupMessage(rowEl);
    if (msg) {
        rowEl.dataset.streak = _dmsgComputeStreakAttr(msg, rowEl.previousElementSibling);
    }

    const nextRow = _dmsgWalkForwardToRow(rowEl.nextElementSibling);
    if (nextRow) {
        const nextMsg = _dmsgLookupMessage(nextRow);
        if (nextMsg) {
            nextRow.dataset.streak = _dmsgComputeStreakAttr(nextMsg, nextRow.previousElementSibling);
        }
    }
}
