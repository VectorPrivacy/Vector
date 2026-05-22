/**
 * Chat list orchestration: state hashing, full re-render, partial updates,
 * and unread counting.
 *
 * - `lastChatlistStateHash` — script-scoped state-hash gate so successive
 *   no-op renders short-circuit without rebuilding the DOM.
 * - `generateChatlistStateHash` / `renderChatlist` — gated full re-render path.
 * - `updateChatlistPreview` — single-row preview/timestamp refresh, falls back
 *   to full render if the row isn't in the DOM yet.
 * - `updateChatlistTimestamps` — periodic tick that refreshes "5m ago" labels
 *   and online/away status dots without rebuilding the row.
 * - `countUnreadMessages` — walks backwards from the tail until it hits the
 *   user's own message or `last_read`. Used by both row.js and the state hash.
 */

// Store a hash of the last rendered state to detect actual changes
let lastChatlistStateHash = '';

/**
 * Generate a hash representing the current state of all chats
 */
function generateChatlistStateHash() {
    // Build a simple array of state values (faster than creating objects)
    const states = [];

    // Add invite IDs and avatar state
    for (const inv of arrMLSInvites) {
        states.push(inv.id || inv.welcome_event_id || inv.group_id, inv.avatar_cached);
    }

    // Add chat states (including chat ID to capture order changes)
    for (const chat of arrChats) {
        const isGroup = chat.chat_type === 'MlsGroup';
        const profile = !isGroup && chat.participants.length === 1 ? getProfile(chat.id) : null;
        const cLastMsg = chat.messages[chat.messages.length - 1];
        const nUnread = computeRowBadgeCount(chat);
        const activeTypers = chat.active_typers || [];

        // Push values directly (faster than creating object)
        // Include chat.id to ensure order changes are detected
        states.push(
            chat.id,
            nUnread,
            activeTypers.length,
            cLastMsg?.id,
            cLastMsg?.pending,
            profile?.nickname || profile?.name,
            profile?.avatar,
            profile?.avatar_cached,
            chat.muted,
            profile?.is_blocked,
            isGroup ? chat.metadata?.avatar_cached : undefined,
            isGroup ? chat.metadata?.custom_fields?.name : undefined
        );
    }

    return JSON.stringify(states);
}

/**
 * A "thread" function dedicated to rendering the Chat UI in real-time
 */
function renderChatlist() {
    if (fInit) return;

    // Generate a hash of the current RENDERABLE state
    const currentStateHash = generateChatlistStateHash();

    // If the renderable state hasn't changed, skip rendering entirely
    if (currentStateHash === lastChatlistStateHash) return;
    lastChatlistStateHash = currentStateHash;

    // Cache the accent color once (getComputedStyle is expensive per-call)
    const primaryColor = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();

    // Prep a fragment to re-render the full list in one sweep
    const fragment = document.createDocumentFragment();

    // Render invites first (at the top of the chat list)
    for (const invite of arrMLSInvites) {
        const divInvite = renderInviteItem(invite, primaryColor);
        fragment.appendChild(divInvite);
    }

    // Then render regular chats
    for (const chat of arrChats) {
        // For groups, we show them even if they have no messages yet
        // For DMs, we only show them if they have messages
        if (chat.chat_type !== 'MlsGroup' && chat.messages.length === 0) continue;

        // Do not render our own profile: it is accessible via the Bookmarks/Notes section
        if (chat.id === strPubkey) continue;

        // Hide DM chats with blocked users from the chat list
        if (chat.chat_type !== 'MlsGroup') {
            const chatProfile = getProfile(chat.id);
            if (chatProfile?.is_blocked) continue;
        }

        const divContact = renderChat(chat, primaryColor);
        fragment.appendChild(divContact);
    }

    // Give the final element a bottom-margin boost to allow scrolling past the fadeout
    if (fragment.lastElementChild) fragment.lastElementChild.style.marginBottom = `50px`;

    // Empty-state intro for fresh accounts (no chats, no invites). The
    // visible chat list normally only contains DMs with at least one
    // message and groups the user has joined; if the fragment came out
    // empty AND there are no pending invites, surface a friendly nudge
    // so the user understands what to do next.
    if (!fragment.firstElementChild && arrMLSInvites.length === 0) {
        fragment.appendChild(buildChatlistEmptyState());
    }

    // Replace the existing list in one native call
    domChatList.replaceChildren(fragment);

    // Update the back button notification
    updateChatBackNotification();
}

/**
 * Build the empty-state placeholder shown when the chat list has no
 * chats or invites. Pulls the user toward the New Chat / Group Chat
 * buttons at the top of the screen, plus a one-tap "Share My Contact"
 * button that copies the user's vectorapp.io profile link to the
 * clipboard so they can paste it into another channel and bootstrap
 * their first conversations.
 */
function buildChatlistEmptyState() {
    const wrap = document.createElement('div');
    wrap.className = 'chatlist-empty-state';
    wrap.innerHTML = `
        <div class="chatlist-empty-state-icon">
            <span class="icon icon-chats"></span>
        </div>
        <h3>No chats yet</h3>
        <p>Start a <strong>New Chat</strong> or create a <strong>Group Chat</strong> from the buttons above. Messages you receive from other Vector users will appear here automatically.</p>
        <button class="chatlist-empty-state-share cancel-btn" type="button">
            Share My Contact
        </button>
    `;
    const btn = wrap.querySelector('.chatlist-empty-state-share');
    btn.addEventListener('click', () => {
        if (!strPubkey) return;
        const profileUrl = `https://vectorapp.io/profile/${strPubkey}`;
        navigator.clipboard.writeText(profileUrl).then(() => {
            showToast('Contact Link Copied');
        }).catch(() => {
            showToast('Failed to Copy Contact Link');
        });
    });
    return wrap;
}

/**
 * Update only the preview text and timestamp for a specific chat in the chatlist
 * This is more efficient than re-rendering the entire chatlist for a single message edit
 * @param {string} chatId - The chat ID to update
 */
function updateChatlistPreview(chatId) {
    const chatElement = document.getElementById(`chatlist-${chatId}`);
    if (!chatElement) {
        // Chat not in DOM - fallback to full render
        renderChatlist();
        return;
    }

    const cChat = getChat(chatId);
    if (!cChat) return;

    // Find the preview text element (p.cutoff inside the preview container)
    const previewContainer = chatElement.querySelector('.chatlist-contact-preview');
    if (!previewContainer) return;

    const pChatPreview = previewContainer.querySelector('p.cutoff');
    const pTimeAgo = chatElement.querySelector('.chatlist-contact-timestamp, .chatlist-contact-inline-time');

    if (pChatPreview) {
        const preview = generateChatPreviewText(cChat);
        pChatPreview.classList.toggle('typing-indicator-text', preview.isTyping);
        if (preview.isHtml) {
            pChatPreview.innerHTML = preview.text;
        } else {
            pChatPreview.textContent = preview.text;
        }
        if (preview.needsTwemoji) twemojify(pChatPreview, { layoutHint: true });
    }

    // Update timestamp
    const cLastMsg = cChat.messages[cChat.messages.length - 1];
    if (pTimeAgo && cLastMsg) {
        pTimeAgo.textContent = timeAgo(cLastMsg.at);
    }
}

/**
 * Count the quantity of unread messages
 * @param {Chat} chat - The Chat we're checking
 * @returns {number} - The amount of unread messages, if any
 */
function countUnreadMessages(chat) {
    // If no messages, return 0
    if (!chat.messages || !chat.messages.length) return 0;

    // Walk backwards from the end to count unread messages
    // Stop when we hit: 1) our own message, or 2) the last_read message
    let unreadCount = 0;

    for (let i = chat.messages.length - 1; i >= 0; i--) {
        const msg = chat.messages[i];

        // System events (wallpaper changes, member joined/left, etc.) are
        // state notifications, not conversation — skip them entirely so they
        // can't drive the unread badge or block the walk-back from hitting a
        // real read marker.
        if (msg.system_event) {
            continue;
        }

        // If we hit our own message, stop - we clearly read everything before it
        if (msg.mine) {
            break;
        }

        // If we hit the last_read message, stop - everything at and before this is read
        if (chat.last_read && msg.id === chat.last_read) {
            break;
        }

        // Skip messages from blocked users in group chats
        if (chat.chat_type === 'MlsGroup' && msg.npub) {
            const authorProfile = getProfile(msg.npub);
            if (authorProfile?.is_blocked) continue;
        }

        // Count this message as unread
        unreadCount++;
    }

    return unreadCount;
}

/**
 * Count messages in `chat` that ping the user (a direct @-mention of our
 * npub, or an @everyone from a group admin). Walks the same window as
 * `countUnreadMessages` (back to last_read or our own latest message).
 * Used for muted group rows so the badge reflects "things you'd want to
 * see" rather than the full unread count.
 */
/**
 * Resolve the badge count for a chat row:
 *  - Muted DM/single-user chat: 0 (silenced entirely).
 *  - Muted group: count only pings (mentions of us / admin @everyone).
 *  - Anything else: full unread count.
 */
function computeRowBadgeCount(chat) {
    if (chat.muted) {
        return chat.chat_type === 'MlsGroup' ? countPingMessages(chat) : 0;
    }
    return countUnreadMessages(chat);
}

function countPingMessages(chat) {
    if (!chat.messages || !chat.messages.length) return 0;
    const isGroup = chat.chat_type === 'MlsGroup';
    const admins = chat.metadata?.admins;
    let pings = 0;
    for (let i = chat.messages.length - 1; i >= 0; i--) {
        const msg = chat.messages[i];
        if (msg.system_event) continue; // not a conversation message
        if (msg.mine) break;
        if (chat.last_read && msg.id === chat.last_read) break;
        if (isGroup && msg.npub) {
            const authorProfile = getProfile(msg.npub);
            if (authorProfile?.is_blocked) continue;
        }
        if (!msg.content) continue;
        const mentionedMe = strPubkey && msg.content.includes('@' + strPubkey);
        const mentionedEveryone = isGroup
            && /@everyone\b/.test(msg.content)
            && admins?.includes(msg.npub || '');
        if (mentionedMe || mentionedEveryone) pings++;
    }
    return pings;
}

/**
 * Update the notification dot on the chat back button
 * Shows the dot if there are unread messages in OTHER chats (not the currently open one) OR unanswered invites
 */
function updateChatlistTimestamps() {
    // Get all chatlist items that are currently displayed
    const chatListItems = document.querySelectorAll('.chatlist-contact');

    // For each chat item, find and update the timestamp and status
    chatListItems.forEach(item => {
        // Extract chat ID from the item's ID (format: chatlist-{chatId})
        const chatId = item.id.substring(9);

        // Find the corresponding chat in our array
        const chat = arrChats.find(c => c.id === chatId);

        if (chat && chat.messages.length > 0) {
            // Get the last message timestamp
            const lastMessage = chat.messages[chat.messages.length - 1];

            // Skip updating if the message is older than 1 week (for performance)
            // Messages older than 1 week display as "1w", "2w", etc. and are unlikely to change
            if (lastMessage?.at < Date.now() - 604800000) return;

            // Tick whichever timestamp element this row has: right-side
            // (read rows) or inline next to the name (unread rows).
            const timestampElement = item.querySelector('.chatlist-contact-timestamp, .chatlist-contact-inline-time');
            if (timestampElement) {
                timestampElement.textContent = timeAgo(lastMessage.at);
            }

            // Update status indicator if needed (for DMs only)
            const avatarContainer = item.querySelector('.avatar-status-icon')?.parentElement;
            if (avatarContainer && chat.chat_type !== 'MlsGroup') {
                // Remove existing status icon if present
                const existingStatusIcon = avatarContainer.querySelector('.avatar-status-icon');
                if (existingStatusIcon) {
                    existingStatusIcon.remove();
                }

                // Add new status icon based on last message time
                const divStatusIcon = document.createElement('div');
                divStatusIcon.classList.add('avatar-status-icon');

                // Find the last message from the contact (not from the user)
                let cLastContactMsg = null;
                for (let i = chat.messages.length - 1; i >= 0; i--) {
                    if (!chat.messages[i].mine) {
                        cLastContactMsg = chat.messages[i];
                        break;
                    }
                }

                if (cLastContactMsg && cLastContactMsg.at > Date.now() - 60000 * 5) {
                    // set the divStatusIcon .backgroundColor to green (online)
                    divStatusIcon.style.backgroundColor = '#59fcb3';
                    avatarContainer.appendChild(divStatusIcon);
                }
                else if (cLastContactMsg && cLastContactMsg.at > Date.now() - 60000 * 30) {
                    // set to orange (away)
                    divStatusIcon.style.backgroundColor = '#fce459';
                    avatarContainer.appendChild(divStatusIcon);
                }
                // offline... don't show status icon at all (no need to append the divStatusIcon)
            }
        }
    });
}
