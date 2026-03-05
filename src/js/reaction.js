/**
 * Reaction Details Popup
 * Long-press on a reaction badge to see who reacted with that emoji.
 */

let reactionDetailsPopup = null;
let reactionLongPressTimer = null;
let reactionLongPressed = false;

/**
 * Show a popup listing who reacted with a specific emoji
 * @param {HTMLElement} reactionEl - The .reaction element that was long-pressed
 */
function showReactionDetails(reactionEl) {
    hideReactionDetails();

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
    // Look up the emoji's name from the dataset
    const emojiEntry = typeof arrEmojis !== 'undefined' && arrEmojis.find(e => e.emoji === emoji);
    const emojiName = emojiEntry ? emojiEntry.name.split(' ')[0] : '';
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
}, true);
