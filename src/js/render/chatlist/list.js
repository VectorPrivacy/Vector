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
 * Whether a Community chat is the one row its community gets in the list.
 *
 * Every channel is registered and synced, but this release renders a community as a
 * SINGLE row: its primary channel ("general", else the first one). The backend stamps
 * `primary_channel` onto every channel row, so a sibling is any row whose own id isn't
 * that value. Rows written before the stamp existed fall back to rendering — better a
 * duplicate row than a community that silently disappears from the list.
 */
function isPrimaryChannelChat(chat) {
    const primary = chat?.metadata?.custom_fields?.primary_channel;
    return !primary || primary === chat.id;
}

/**
 * Whether a chat gets a row in the chat list — i.e. whether the user can SEE it.
 *
 * The single source of truth for "does this chat exist in the UI", shared by the row
 * builder and by every unread indicator. Anything invisible here must not be counted
 * anywhere, or an indicator lights for a chat the user cannot open to clear: a blocked
 * DM or a sibling channel has no row, so its unread is unreachable.
 *
 * Keep this as the ONLY definition. The back-chevron dot previously had its own copy of
 * these rules and drifted out of sync with the rows.
 */
function chatIsVisibleInList(chat) {
    if (!chat) return false;
    const isGroup = chatIsGroup(chat);
    // Own profile lives in Bookmarks/Notes, not the list.
    if (chat.id === strPubkey) return false;
    if (isGroup) {
        // A Community row with no owning community is a bare persistence anchor.
        if (!chat.metadata?.custom_fields?.community_id) return false;
        // Sibling channels stay synced and addressable but get no row of their own.
        if (!isPrimaryChannelChat(chat)) return false;
        return true;
    }
    // DMs appear once they have content, and blocked senders never appear.
    if (chat.messages.length === 0) return false;
    if (getProfile(chat.id)?.is_blocked) return false;
    return true;
}

/**
 * Generate a hash representing the current state of all chats
 */
function generateChatlistStateHash() {
    // Build a simple array of state values (faster than creating objects)
    const states = [];

    // Add pending Community invite ids
    for (const inv of arrCommunityInvites) {
        states.push(inv.community_id, inv.name);
    }

    // Add chat states (including chat ID to capture order changes)
    for (const chat of arrChats) {
        const isGroup = chatIsGroup(chat);
        const profile = !isGroup ? getProfile(chat.id) : null;
        const cLastMsg = chat.messages[chat.messages.length - 1];
        const nUnread = computeRowBadgeCount(chat);
        const activeTypers = chat.active_typers || [];

        // Push values directly (faster than creating object)
        // Include chat.id to ensure order changes are detected
        states.push(
            chat.id,
            nUnread,
            activeTypers.length,
            // Message count so a REMOVAL re-renders even when the raw last array
            // element is unchanged — a self-destruct purges the preview message
            // while a later system event (presence/join) stays the last element.
            chat.messages.length,
            cLastMsg?.id,
            cLastMsg?.pending,
            profile?.nickname || profile?.name || profile?.display_name,
            profile?.avatar,
            profile?.avatar_cached,
            chat.muted,
            // Pin state, not just position: pinning the chat that already sits
            // at the top changes no order, so without this the glyph would
            // never paint (the hash would match and the render be skipped).
            arrPinnedChats.includes(chatPinKey(chat)),
            profile?.is_blocked,
            isGroup ? chat.metadata?.avatar_cached : undefined,
            isGroup ? chat.metadata?.custom_fields?.name : undefined,
            chat._joining // so the "Joining…" lock clearing re-renders the row
        );
    }

    return JSON.stringify(states);
}

/**
 * A "thread" function dedicated to rendering the Chat UI in real-time
 */
function renderChatlist() {
    if (fInit) return;

    // Pinned first, then newest-first with a creation/join-time fallback for
    // message-less communities — the one chokepoint that guarantees order no
    // matter which path added a chat (create, join, boot, message). Without it a
    // freshly-surfaced chat stays wherever it was appended, and a pin set by any
    // other path is undone by the next render.
    sortChats();

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
    for (const invite of arrCommunityInvites) {
        fragment.appendChild(renderCommunityInviteItem(invite));
    }

    // Then render regular chats
    for (const chat of arrChats) {
        // Visibility (own profile, bare anchors, sibling channels, empty or blocked DMs)
        // is decided by `chatIsVisibleInList` so the unread indicators can share it.
        if (!chatIsVisibleInList(chat)) continue;

        // Message-less community: lazy-load its latest membership event so the preview can show
        // "X has joined" instead of "No messages yet" (cached onto chat.lastSystemEvent).
        if (chatIsGroup(chat)) ensureCommunityPreviewActivity(chat);

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
    const fEmptyList = !fragment.firstElementChild && arrCommunityInvites.length === 0;
    if (fEmptyList) {
        fragment.appendChild(buildChatlistEmptyState());
        fragment.appendChild(buildChatlistIntro());
    }

    // The bottom fadeout exists to soften a scrolling list; over the empty
    // state it just washes out the intro.
    const fadeout = document.querySelector('#chats .fadeout-bottom');
    if (fadeout) fadeout.style.display = fEmptyList ? 'none' : '';

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
    wrap.className = 'chatlist-get-started btn';
    wrap.setAttribute('role', 'button');
    wrap.innerHTML = `
        <div class="chatlist-get-started-badge">
            <span class="icon icon-add-user"></span>
        </div>
        <div class="chatlist-get-started-text">
            <h4>Get Started</h4>
            <p>Create your first private chat.</p>
        </div>
        <div class="chatlist-get-started-watermark">
            <span class="icon icon-add-user"></span>
        </div>
    `;
    // Rides the New Chat button's own handler, so the two can never diverge.
    wrap.addEventListener('click', () => document.getElementById('new-chat-btn')?.click());
    return wrap;
}

/**
 * Bottom-of-list welcome: Viktor points fresh accounts at the Hub. Rides the
 * list fragment, so the first real chat render sweeps it away with the rest.
 */
function buildChatlistIntro() {
    const wrap = document.createElement('div');
    wrap.className = 'chatlist-intro';
    wrap.innerHTML = `
        <img class="chatlist-intro-viktor" alt="Viktor">
        <div class="chatlist-intro-text">
            <h4>Welcome to Vector!</h4>
            <p>Feel free to <span class="chatlist-intro-link">join the public community</span> to learn more about Vector, discuss privacy, and make some new friends.</p>
        </div>
    `;
    wrap.querySelector('.chatlist-intro-link').addEventListener('click', () => {
        openUrl('https://vectorapp.io/hub');
    });

    bindViktor(wrap.querySelector('.chatlist-intro-viktor'));
    return wrap;
}

/** Viktor greets on the first paint after login; page-lifetime latch. */
let fViktorGreeted = false;

const VIKTOR_SMILE = '/icons/viktor-smile.gif';

/**
 * Idle pose, rasterised once from the smile clip's first frame — drawImage
 * of an animated image always takes frame one, so no separate still ships
 * and the idle can never drift from the clip it pauses.
 */
let viktorIdleSrc = null;
const viktorIdleReady = (() => {
    const probe = new Image();
    probe.src = VIKTOR_SMILE;
    return probe.decode().then(() => {
        const c = document.createElement('canvas');
        c.width = probe.naturalWidth;
        c.height = probe.naturalHeight;
        c.getContext('2d').drawImage(probe, 0, 0);
        viktorIdleSrc = c.toDataURL('image/png');
    }).catch(() => { viktorIdleSrc = VIKTOR_SMILE; });
})();

/**
 * Viktor's little state machine. GIFs can't be paused, so every state is a
 * file swap: idle = the smile's first frame, hover = the smile loop, click =
 * one exclamation. Leaving mid-smile lets the loop in progress finish rather
 * than cutting him off, and both clips share the idle frame at their seams.
 */
function bindViktor(img) {
    const SMILE = VIKTOR_SMILE;
    const EXCLAIM = '/icons/viktor-exclaim.gif';
    const SMILE_MS = 1500;
    const EXCLAIM_MS = 1600;
    let mode = 'idle';
    let hovering = false;
    let smileStart = 0;
    let timer = null;

    // WebKit animates GIFs on a shared document-wide clock: re-assigning the
    // same URL joins the cycle mid-flight instead of starting at frame one,
    // which reads as a snap against the still. A unique query per play forces
    // a genuine restart; the file is a local asset, so the refetch is free.
    let playSeq = 0;
    const fresh = (url) => `${url}?play=${++playSeq}`;

    const toIdle = () => { mode = 'idle'; if (viktorIdleSrc) img.src = viktorIdleSrc; };
    const smile = () => {
        clearTimeout(timer);
        mode = 'smile';
        smileStart = Date.now();
        img.src = fresh(SMILE);
    };
    const exclaim = () => {
        clearTimeout(timer);
        mode = 'exclaim';
        img.src = fresh(EXCLAIM);
        timer = setTimeout(() => (hovering ? smile() : toIdle()), EXCLAIM_MS);
    };

    // The still rasterises async on first use; paint it as soon as it lands.
    if (viktorIdleSrc) toIdle();
    else viktorIdleReady.then(() => { if (mode === 'idle') toIdle(); });

    img.addEventListener('pointerenter', (e) => {
        if (e.pointerType !== 'mouse') return;
        hovering = true;
        if (mode === 'idle') smile();
        else if (mode === 'smile') clearTimeout(timer);
    });
    img.addEventListener('pointerleave', (e) => {
        if (e.pointerType !== 'mouse') return;
        hovering = false;
        if (mode !== 'smile') return;
        // Let the loop in progress run to its end before settling to idle.
        const remainder = SMILE_MS - ((Date.now() - smileStart) % SMILE_MS);
        clearTimeout(timer);
        timer = setTimeout(() => {
            if (mode === 'smile' && !hovering) toIdle();
        }, remainder);
    });
    img.addEventListener('click', exclaim);

    // Boot greeting: one exclamation as the login fade-in lands.
    if (!fViktorGreeted) {
        fViktorGreeted = true;
        const kick = () => setTimeout(exclaim, 150);
        if (domChatList.classList.contains('intro-anim')) {
            domChatList.addEventListener('animationend', kick, { once: true });
        } else {
            setTimeout(kick, 400);
        }
    }
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
        if (preview.emojiTags && typeof renderCustomEmojiShortcodes === 'function') {
            renderCustomEmojiShortcodes(pChatPreview, preview.emojiTags);
        }
    }

    // Update timestamp
    const cLastMsg = cChat.messages[cChat.messages.length - 1];
    if (pTimeAgo && cLastMsg) {
        pTimeAgo.textContent = timeAgo(cLastMsg.at);
    }
}

/**
 * Whether a sender's DM chat is muted: a muted person is silent in every
 * chat, so their community messages don't badge either. DM ids ARE npubs.
 */
function senderIsMuted(npub) {
    return arrChats.some(c => c.muted && c.id === npub);
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

        // Skip messages from blocked or muted users in group chats
        if (chatIsGroup(chat) && msg.npub) {
            const authorProfile = getProfile(msg.npub);
            if (authorProfile?.is_blocked) continue;
            if (senderIsMuted(msg.npub)) continue;
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
        return chatIsGroup(chat) ? countPingMessages(chat) : 0;
    }
    // DB-sourced count (set by refreshUnreadCounts) is authoritative across restarts, when only
    // the last message per chat is in RAM. Fall back to the in-memory walk before the first
    // refresh lands (or if it ever failed).
    return (typeof chat.unread === 'number') ? chat.unread : countUnreadMessages(chat);
}

function countPingMessages(chat) {
    if (!chat.messages || !chat.messages.length) return 0;
    const isGroup = chatIsGroup(chat);
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
            // A muted sender never pings, even through an unmuted channel.
            if (senderIsMuted(msg.npub)) continue;
        }
        if (!msg.content) continue;
        const mentionedMe = strPubkey && msg.content.includes('@' + strPubkey);
        // Authorized @everyone = owner or admin (owner isn't in the admins list — it's its own tier).
        const everyoneAuthor = msg.npub || '';
        const mentionedEveryone = isGroup
            && /@everyone\b/.test(msg.content)
            && (admins?.includes(everyoneAuthor) || chat.metadata?.custom_fields?.owner_npub === everyoneAuthor);
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
            if (avatarContainer && !chatIsGroup(chat)) {
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
