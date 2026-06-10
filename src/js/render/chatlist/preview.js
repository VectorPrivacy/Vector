/**
 * Chat list preview-text helpers.
 *
 * - `generateTypingText` — produces "Typing..." / "X is typing..." for active typers.
 * - `generateChatPreviewText` — last-message preview (text, attachment, payment,
 *   system event, etc.) with sender prefix in groups and mention resolution.
 *
 * Both consumed by row.js (full render) and list.js (partial preview update).
 */

/**
 * Generate typing indicator text for a chat
 * @param {Chat} chat - The chat object
 * @returns {string|null} - The typing text, or null if no one is typing
 */
function generateTypingText(chat) {
    let activeTypers = chat.active_typers || [];
    if (activeTypers.length === 0) return null;

    const isGroup = chatIsGroup(chat);

    // Filter out blocked users from typing indicators in group chats
    if (isGroup) {
        activeTypers = activeTypers.filter(npub => !getProfile(npub)?.is_blocked);
        if (activeTypers.length === 0) return null;
    }

    // DMs just show "Typing..." since we already know who it is
    if (!isGroup) return 'Typing...';

    // Groups show names
    if (activeTypers.length === 1) {
        const typer = getProfile(activeTypers[0]);
        const name = typer?.nickname || typer?.name || 'Someone';
        return `${name} is typing...`;
    } else if (activeTypers.length === 2) {
        const typer1 = getProfile(activeTypers[0]);
        const typer2 = getProfile(activeTypers[1]);
        const name1 = typer1?.nickname || typer1?.name || 'Someone';
        const name2 = typer2?.nickname || typer2?.name || 'Someone';
        return `${name1} and ${name2} are typing...`;
    } else if (activeTypers.length === 3) {
        const typer1 = getProfile(activeTypers[0]);
        const typer2 = getProfile(activeTypers[1]);
        const typer3 = getProfile(activeTypers[2]);
        const name1 = typer1?.nickname || typer1?.name || 'Someone';
        const name2 = typer2?.nickname || typer2?.name || 'Someone';
        const name3 = typer3?.nickname || typer3?.name || 'Someone';
        return `${name1}, ${name2}, and ${name3} are typing...`;
    } else {
        return 'Several people are typing...';
    }
}

/**
 * Generate chat preview text for the chatlist
 * @param {Chat} chat - The chat object
 * @returns {{ text: string, isTyping: boolean, needsTwemoji: boolean }}
 */
function generateChatPreviewText(chat) {
    const isGroup = chatIsGroup(chat);

    // Walk back to find the latest actual conversation message. System
    // events (wallpaper changes, member joined/left) and — in group chats —
    // messages from blocked users are skipped so they don't hijack the
    // preview line. Mirrors `findLatestContactMessage` but keeps own
    // messages eligible since the preview shows "You: …" outgoing context.
    let cLastMsg = null;
    for (let i = chat.messages.length - 1; i >= 0; i--) {
        const m = chat.messages[i];
        if (m.system_event) continue;
        if (isGroup && m.npub && !m.mine) {
            const authorProfile = getProfile(m.npub);
            if (authorProfile?.is_blocked) continue;
        }
        cLastMsg = m;
        break;
    }

    // Handle typing indicators
    const typingText = generateTypingText(chat);
    if (typingText) {
        return { text: typingText, isTyping: true, needsTwemoji: false };
    }

    // No messages
    if (!cLastMsg) {
        if (isGroup) {
            const memberCount = chat.metadata?.custom_fields?.member_count ? parseInt(chat.metadata.custom_fields.member_count) : null;
            return {
                text: (memberCount != null) ? `${memberCount} ${memberCount === 1 ? 'member' : 'members'}` : 'No messages yet',
                isTyping: false,
                needsTwemoji: false
            };
        } else {
            return { text: 'Start a conversation', isTyping: false, needsTwemoji: false };
        }
    }

    // Pending message
    if (cLastMsg.pending) {
        return { text: 'Sending...', isTyping: false, needsTwemoji: false };
    }

    // Build sender prefix for groups
    let senderPrefix = '';
    if (cLastMsg.mine) {
        senderPrefix = 'You: ';
    } else if (isGroup && cLastMsg.npub) {
        const senderProfile = getProfile(cLastMsg.npub);
        const senderName = senderProfile?.nickname || senderProfile?.name || cLastMsg.npub.substring(0, 16);
        senderPrefix = `${senderName}: `;
    }

    // Attachment message
    if (!cLastMsg.content && cLastMsg.attachments?.length) {
        return {
            text: senderPrefix + 'Sent a ' + getFileTypeInfo(cLastMsg.attachments[0].extension).description,
            isTyping: false,
            needsTwemoji: false
        };
    }

    // PIVX payment message
    if (cLastMsg.pivx_payment) {
        return { text: senderPrefix + 'Sent a PIVX Payment', isTyping: false, needsTwemoji: false };
    }

    // System event (member joined/left, etc.)
    if (cLastMsg.system_event) {
        return { text: cLastMsg.content, isTyping: false, needsTwemoji: false };
    }

    // Regular text message — strip HTML/markdown, render inline formatting, resolve @npub mentions.
    // emojiTags lets the renderer swap :shortcode: for inline custom emojis (like in-chat).
    let previewSource = cLastMsg.content;
    // Invite links render as a card in-chat; the snippet shows a friendly tag, not the raw URL.
    if (typeof replaceCommunityInviteUrlsForPreview === 'function') {
        previewSource = replaceCommunityInviteUrlsForPreview(previewSource);
    }
    return { text: escapeHtml(senderPrefix) + contentToPreviewHtml(resolveMentionText(previewSource)), isTyping: false, needsTwemoji: true, isHtml: true, emojiTags: cLastMsg.emoji_tags };
}
