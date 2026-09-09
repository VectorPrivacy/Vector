/**
 * Reaction Details Popup
 * Long-press on a reaction badge to see who reacted with that emoji.
 */

/** Per-effective-tier (0-3) cap on NEW reaction groups one user can open on a
 *  single message. Mirrors vector-core's NEW_REACTIONS_PER_POST_BY_TIER —
 *  joining an existing reaction (+1) is never gated. */
const NEW_REACTIONS_PER_POST_BY_TIER = [6, 6, 9, 12];

/** Message-wide distinct-emoji ceiling. Mirrors vector-core's MAX_REACTION_GROUPS. */
const MAX_REACTION_GROUPS = 12;

/**
 * Can the user OPEN a new reaction group on `cMsg` at all? Null when yes,
 * else a user-facing reason (message-wide ceiling or their per-tier fresh
 * allowance). "Groups I opened" is judged by insertion order — the earliest
 * held reaction per emoji — matching the backend's view. The reaction row's
 * "+" hides on this too, so a capped user isn't offered a dead picker.
 */
function newReactionGroupBlockReason(cMsg) {
    if (!cMsg?.reactions?.length) return null;
    const firstByEmoji = new Map();
    for (const r of cMsg.reactions) {
        if (!firstByEmoji.has(r.emoji)) firstByEmoji.set(r.emoji, r.author_id);
    }
    if (firstByEmoji.size >= MAX_REACTION_GROUPS) {
        return 'This message has reached its reaction limit';
    }
    const tier = Math.min(Math.max(_myBadges?.tier | 0, 0), 3);
    let spent = 0;
    for (const author of firstByEmoji.values()) {
        if (author === strPubkey) spent++;
    }
    if (spent >= NEW_REACTIONS_PER_POST_BY_TIER[tier]) {
        return "You've used all your new reactions on this message";
    }
    return null;
}

/**
 * Pre-gate for adding `emoji` to `cMsg`: null when the send may proceed, else
 * a user-facing reason. Joining an existing group always passes.
 */
function reactionTierGate(cMsg, emoji) {
    if (!cMsg?.reactions?.length) return null;
    if (cMsg.reactions.some(r => r.emoji === emoji)) return null;
    return newReactionGroupBlockReason(cMsg);
}

let reactionLongPressTimer = null;
let reactionLongPressed = false;

// Hover summary (desktop only — mobile uses long-press for the full popup).
let reactionHoverTimer = null;
let reactionHoverEl = null; // currently armed reaction; null when nothing hovered
let reactionTipWatchdog = null; // liveness interval while a tip is on screen

// Ground truth for "still hovering": the last real cursor position. The hide
// path otherwise rests entirely on `mouseout` from the chip — an event that
// never fires when the chip is REPAINTED under the cursor (someone else's
// reaction re-renders the row) or SCROLLS away beneath a stationary pointer
// (WKWebView synthesises no boundary events on scroll). Both leave the tip
// stuck on screen forever.
let _lastPointerX = -1;
let _lastPointerY = -1;
document.addEventListener('mousemove', (e) => {
    _lastPointerX = e.clientX;
    _lastPointerY = e.clientY;
}, { passive: true });

/** While the tip is visible, verify 4x/sec that its chip still exists and the
 *  cursor is still on it; anything else hides the tip. */
function _startReactionTipWatchdog(reactionEl) {
    if (reactionTipWatchdog) clearInterval(reactionTipWatchdog);
    reactionTipWatchdog = setInterval(() => {
        let alive = reactionEl.isConnected;
        if (alive && _lastPointerX >= 0) {
            const r = reactionEl.getBoundingClientRect();
            alive = _lastPointerX >= r.left - 2 && _lastPointerX <= r.right + 2
                 && _lastPointerY >= r.top - 2 && _lastPointerY <= r.bottom + 2;
        }
        if (!alive) {
            hideReactionHoverTip();
            reactionHoverEl = null;
        }
    }, 250);
}
const REACTION_HOVER_DELAY_MS = 500;

// Both popups are one island; this side opens and closes them and owns the gestures.
let _reactionPopupsMounted = false;
function _mountReactionPopups() {
    _reactionPopupsMounted = true;
    VectorSvelte.mountReactionPopups({
        h: {
            findMessage: (msgId) => {
                for (const chat of arrChats) {
                    const m = chat.messages.find(x => x.id === msgId);
                    if (m) return m;
                }
                return null;
            },
            getProfile,
            getProfileAvatarSrc,
            twemojify,
            // The dataset's canonical `display` (CLDR tts); `name` for entries predating it.
            emojiLabel: (emoji) => {
                const entry = typeof arrEmojis !== 'undefined' && arrEmojis.find(e => e.emoji === emoji);
                return entry ? (entry.display || entry.name) : '';
            },
        },
    });
}
const _reactionTipEl = () => document.querySelector('.reaction-hover-tip');
const _reactionDetailsEl = () => document.querySelector('.reaction-details-popup');

/**
 * Show the hover summary above a reaction chip. Self-contained — does its own
 * data lookup. Skipped if data is missing or the chip is no longer hovered by
 * the time the delay elapses.
 */
function showReactionHoverTip(reactionEl) {
    hideReactionHoverTip();
    const emoji = reactionEl.getAttribute('data-emoji');
    const msgId = reactionEl.getAttribute('data-msg-id');
    if (!emoji || !msgId) return;
    let msg = null;
    for (const chat of arrChats) {
        msg = chat.messages.find(m => m.id === msgId);
        if (msg) break;
    }
    if (!msg) return;
    const matching = msg.reactions.filter(r => r.emoji === emoji);
    if (!matching.length) return;
    if (!_reactionPopupsMounted) _mountReactionPopups();
    VectorSvelte.openReactionTip({ emoji, names: matching.map(r => getName(r.author_id)), anchor: reactionEl });
    _startReactionTipWatchdog(reactionEl);
}

function hideReactionHoverTip() {
    if (reactionTipWatchdog) {
        clearInterval(reactionTipWatchdog);
        reactionTipWatchdog = null;
    }
    VectorSvelte.closeReactionTip();
    if (reactionHoverTimer) {
        clearTimeout(reactionHoverTimer);
        reactionHoverTimer = null;
    }
    // Note: `reactionHoverEl` is intentionally NOT cleared here — the mouseover
    // handler uses it to dedupe against the same chip on every mousemove inside
    // the chip's children. Clearing it would make showReactionHoverTip() (which
    // calls this to wipe any prior tip) drop the dedupe key, and the next
    // mousemove would then re-fire showReactionHoverTip in a hide/show flicker.
    // mouseout owns the lifecycle of `reactionHoverEl`.
}

/**
 * Show a popup listing who reacted with a specific emoji
 * @param {HTMLElement} reactionEl - The .reaction element that was long-pressed
 */
function showReactionDetails(reactionEl) {
    hideReactionDetails();
    // Right-click / long-press supersedes the lightweight hover tip.
    hideReactionHoverTip();
    const emoji = reactionEl.getAttribute('data-emoji');
    const msgId = reactionEl.getAttribute('data-msg-id');
    if (!emoji || !msgId) return;
    if (!_reactionPopupsMounted) _mountReactionPopups();
    VectorSvelte.openReactionDetails({ emoji, msgId, anchor: reactionEl });
}

/**
 * Hide the reaction details popup
 */
function hideReactionDetails() {
    VectorSvelte.closeReactionDetails();
}

/**
 * Check and reset the long-press flag (called by main.js click handler to skip click after hold)
 * @returns {boolean}
 */
function isReactionLongPressed() {
    if (reactionLongPressed) {
        reactionLongPressed = false;
        return true;
    }
    return false;
}

function cancelReactionLongPress() {
    if (reactionLongPressTimer) {
        clearTimeout(reactionLongPressTimer);
        reactionLongPressTimer = null;
    }
}

// Hover summary (desktop only). Uses a 500ms delay so brief cursor flyovers
// don't fire the tip. mousein/mouseout via mouseover/mouseout (capture-style)
// because mouseenter/mouseleave don't bubble.
document.addEventListener('mouseover', (e) => {
    if (typeof platformFeatures !== 'undefined' && platformFeatures?.is_mobile) return;
    const reactionEl = e.target.closest('.reaction');
    if (!reactionEl) return;
    // Skip only if this exact chip already has a live timer or shown tip — a
    // bare tracker without either means stale state we should refresh through.
    if (reactionEl === reactionHoverEl && (reactionHoverTimer || _reactionTipEl())) return;

    if (reactionHoverTimer) clearTimeout(reactionHoverTimer);
    hideReactionHoverTip();

    reactionHoverEl = reactionEl;
    reactionHoverTimer = setTimeout(() => {
        reactionHoverTimer = null;
        // Re-check we're still hovering — the chip may have been removed mid-delay.
        if (reactionHoverEl === reactionEl && document.body.contains(reactionEl)) {
            showReactionHoverTip(reactionEl);
        }
    }, REACTION_HOVER_DELAY_MS);
});

document.addEventListener('mouseout', (e) => {
    const reactionEl = e.target.closest('.reaction');
    if (!reactionEl || reactionEl !== reactionHoverEl) return;
    // Ignore mouseout when the cursor moves to a child of the chip (e.g. the
    // emoji <img> twemojified inside the span).
    const related = e.relatedTarget;
    if (related && reactionEl.contains(related)) return;
    hideReactionHoverTip();
    reactionHoverEl = null; // we've truly left the chip
});

// Long-press detection (delegated on document)
document.addEventListener('mousedown', (e) => {
    const reactionEl = e.target.closest('.reaction');
    if (!reactionEl) return;
    cancelReactionLongPress();
    reactionLongPressTimer = setTimeout(() => {
        reactionLongPressed = true;
        reactionLongPressTimer = null;
        showReactionDetails(reactionEl);
    }, 500);
});

document.addEventListener('mouseup', cancelReactionLongPress);
document.addEventListener('mouseleave', cancelReactionLongPress);

document.addEventListener('touchstart', (e) => {
    const reactionEl = e.target.closest('.reaction');
    if (!reactionEl) return;
    cancelReactionLongPress();
    reactionLongPressTimer = setTimeout(() => {
        reactionLongPressed = true;
        reactionLongPressTimer = null;
        e.preventDefault();
        showReactionDetails(reactionEl);
    }, 500);
}, { passive: false });

document.addEventListener('touchend', cancelReactionLongPress);
document.addEventListener('touchcancel', cancelReactionLongPress);
document.addEventListener('touchmove', cancelReactionLongPress);

// Right-click on a reaction badge to show details instantly
document.addEventListener('contextmenu', (e) => {
    const reactionEl = e.target.closest('.reaction');
    if (!reactionEl) return;
    e.preventDefault();
    cancelReactionLongPress();
    reactionLongPressed = true;
    showReactionDetails(reactionEl);
});

// Dismiss on click outside
document.addEventListener('click', (e) => {
    const popup = _reactionDetailsEl();
    // The path, not contains: a row the click re-renders is already unmounted here.
    if (popup && !popup.contains(e.target) && !(e.composedPath?.() || []).includes(popup)) hideReactionDetails();
});

// Dismiss on Escape
document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') hideReactionDetails();
});

// Dismiss on chat scroll (but not when scrolling inside the popup itself)
document.addEventListener('scroll', (e) => {
    const popup = _reactionDetailsEl();
    if (popup && !popup.contains(e.target)) hideReactionDetails();
    // Hover tip is anchored to chip geometry — drop it on any scroll.
    hideReactionHoverTip();
}, true);
