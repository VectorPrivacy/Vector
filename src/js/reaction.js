/**
 * Reaction Details Popup
 * Long-press on a reaction badge to see who reacted with that emoji.
 */

let reactionDetailsPopup = null;
let reactionLongPressTimer = null;
let reactionLongPressed = false;

// Hover summary (desktop only — mobile uses long-press for the full popup).
let reactionHoverTip = null;
let reactionHoverTimer = null;
let reactionHoverEl = null; // currently armed reaction; null when nothing hovered
const REACTION_HOVER_DELAY_MS = 250;
const REACTION_HOVER_NAMES_VISIBLE = 3;

/**
 * Build the inline names string for a hover tip:
 *   1 → "Alice"
 *   2 → "Alice and Bob"
 *   3 → "Alice, Bob, and Charlie"
 *   4+ → "Alice, Bob, Charlie, and N others"
 */
function _formatReactorNames(names) {
    if (names.length === 1) return names[0];
    if (names.length === 2) return `${names[0]} and ${names[1]}`;
    if (names.length === 3) return `${names[0]}, ${names[1]}, and ${names[2]}`;
    const shown = names.slice(0, REACTION_HOVER_NAMES_VISIBLE);
    const others = names.length - REACTION_HOVER_NAMES_VISIBLE;
    return `${shown.join(', ')}, and ${others} other${others === 1 ? '' : 's'}`;
}

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

    const matchingReactions = msg.reactions.filter(r => r.emoji === emoji);
    if (matchingReactions.length === 0) return;

    const names = matchingReactions.map(r => {
        const profile = getProfile(r.author_id);
        return profile?.nickname || profile?.name || profile?.display_name
            || r.author_id.slice(0, 8) + '…';
    });

    const tip = document.createElement('div');
    tip.className = 'reaction-hover-tip';

    const emojiSpan = document.createElement('span');
    emojiSpan.className = 'reaction-hover-tip-emoji';
    emojiSpan.textContent = emoji;
    twemojify(emojiSpan);
    tip.appendChild(emojiSpan);

    const text = document.createElement('span');
    text.className = 'reaction-hover-tip-text';
    text.textContent = `reacted by ${_formatReactorNames(names)}`;
    tip.appendChild(text);

    const hint = document.createElement('span');
    hint.className = 'reaction-hover-tip-hint';
    hint.textContent = 'Right-click for details';
    tip.appendChild(hint);

    document.body.appendChild(tip);
    reactionHoverTip = tip;

    // Position above the chip, fall to below if no room. Clamp horizontally.
    const rect = reactionEl.getBoundingClientRect();
    const tipRect = tip.getBoundingClientRect();
    let top = rect.top - tipRect.height - 6;
    if (top < 10) top = rect.bottom + 6;
    let left = rect.left + (rect.width / 2) - (tipRect.width / 2);
    left = Math.max(10, Math.min(left, window.innerWidth - tipRect.width - 10));
    tip.style.left = `${left}px`;
    tip.style.top = `${top}px`;
}

function hideReactionHoverTip() {
    if (reactionHoverTip) {
        reactionHoverTip.remove();
        reactionHoverTip = null;
    }
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

    // Find the message across all chats
    let msg = null;
    for (const chat of arrChats) {
        msg = chat.messages.find(m => m.id === msgId);
        if (msg) break;
    }
    if (!msg) return;

    // Filter reactions for this emoji
    const matchingReactions = msg.reactions.filter(r => r.emoji === emoji);
    if (matchingReactions.length === 0) return;

    // Build popup
    const popup = document.createElement('div');
    popup.className = 'reaction-details-popup';

    // Header: count + emoji + first keyword
    const header = document.createElement('div');
    header.className = 'reaction-details-header';
    // Use the dataset's canonical `display` field (CLDR tts, e.g.
    // "thumbs up", "rolling on the floor laughing"). Falls back to the
    // search-keyword `name` for ancient entries that pre-date `display`.
    const emojiEntry = typeof arrEmojis !== 'undefined' && arrEmojis.find(e => e.emoji === emoji);
    const emojiName = emojiEntry ? (emojiEntry.display || emojiEntry.name) : '';
    const countSpan = document.createElement('span');
    countSpan.className = 'reaction-details-count';
    countSpan.textContent = matchingReactions.length;
    const emojiSpan = document.createElement('span');
    emojiSpan.className = 'reaction-details-emoji';
    emojiSpan.textContent = emoji;
    twemojify(emojiSpan);
    header.appendChild(countSpan);
    header.appendChild(emojiSpan);
    if (emojiName) {
        const nameSpan = document.createElement('span');
        nameSpan.className = 'reaction-details-label';
        nameSpan.textContent = emojiName.charAt(0).toUpperCase() + emojiName.slice(1);
        header.appendChild(nameSpan);
    }
    popup.appendChild(header);

    // Reactor rows
    const body = document.createElement('div');
    body.className = 'reaction-details-body';
    for (const reaction of matchingReactions) {
        const row = document.createElement('div');
        row.className = 'reaction-detail-row';

        const profile = getProfile(reaction.author_id);
        const avatarSrc = getProfileAvatarSrc(profile);
        const avatarEl = createAvatarImg(avatarSrc, 25);
        avatarEl.classList.add('reaction-avatar');
        row.appendChild(avatarEl);

        const name = document.createElement('span');
        name.className = 'reaction-detail-name';
        name.textContent = profile?.name || profile?.display_name || reaction.author_id.slice(0, 12) + '...';
        row.appendChild(name);

        body.appendChild(row);
    }
    popup.appendChild(body);

    document.body.appendChild(popup);
    reactionDetailsPopup = popup;

    // Position relative to the reaction element (similar to edit history popup)
    const rect = reactionEl.getBoundingClientRect();
    const popupRect = popup.getBoundingClientRect();

    // Try above first, fall below if no space
    let top = rect.top - popupRect.height - 4;
    if (top < 10) {
        top = rect.bottom + 4;
    }

    // Align horizontally, clamp to viewport
    let left = rect.left;
    left = Math.max(10, Math.min(left, window.innerWidth - popupRect.width - 10));

    popup.style.left = `${left}px`;
    popup.style.top = `${top}px`;
}

/**
 * Hide the reaction details popup
 */
function hideReactionDetails() {
    if (reactionDetailsPopup) {
        reactionDetailsPopup.remove();
        reactionDetailsPopup = null;
    }
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

// Hover summary (desktop only). Uses a 250ms delay so brief cursor flyovers
// don't fire the tip. mousein/mouseout via mouseover/mouseout (capture-style)
// because mouseenter/mouseleave don't bubble.
document.addEventListener('mouseover', (e) => {
    if (typeof platformFeatures !== 'undefined' && platformFeatures?.is_mobile) return;
    const reactionEl = e.target.closest('.reaction');
    if (!reactionEl) return;
    // Skip only if this exact chip already has a live timer or shown tip — a
    // bare tracker without either means stale state we should refresh through.
    if (reactionEl === reactionHoverEl && (reactionHoverTimer || reactionHoverTip)) return;

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
    if (reactionDetailsPopup && !reactionDetailsPopup.contains(e.target)) {
        hideReactionDetails();
    }
});

// Dismiss on Escape
document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && reactionDetailsPopup) {
        hideReactionDetails();
    }
});

// Dismiss on chat scroll (but not when scrolling inside the popup itself)
document.addEventListener('scroll', (e) => {
    if (reactionDetailsPopup && !reactionDetailsPopup.contains(e.target)) {
        hideReactionDetails();
    }
    // Hover tip is anchored to chip geometry — drop it on any scroll.
    hideReactionHoverTip();
}, true);
