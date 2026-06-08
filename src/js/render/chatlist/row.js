/**
 * Chat list row constructors.
 *
 * - `renderChat` — builds a `.chatlist-contact` row for a DM or MLS group chat.
 * - `renderInviteItem` — builds a `.chatlist-contact.chatlist-invite` row for a
 *   pending MLS welcome (group invite) with accept / decline action buttons.
 *
 * Both return a detached DocumentFragment-attachable `<div>` element. List
 * orchestration in list.js handles iteration, ordering, and DOM swap.
 */

/**
 * Render a Chat Preview for the Chat List
 * @param {Chat} chat - The profile we're rendering
 */
function renderChat(chat, primaryColor) {
    // For groups, we don't have a profile, for DMs we do
    const isGroup = chatIsGroup(chat);
    const profile = !isGroup ? getProfile(chat.id) : null;

    // Muted DMs stay silent; muted groups still surface pings (mentions of
    // you / admin @everyone). See `computeRowBadgeCount` for the policy.
    const nUnread = computeRowBadgeCount(chat);

    // The Chat container (The ID is the Contact's npub).
    // Theme accent piped through CSS var so theme switches re-color the
    // border without needing a chatlist re-render (inline color literals
    // would stay stuck on the previous theme until the next paint pass).
    const divContact = document.createElement('div');
    if (nUnread) divContact.style.borderColor = 'var(--icon-color-primary)';
    divContact.classList.add('chatlist-contact');
    divContact.id = `chatlist-${chat.id}`;

    // The Username + Message Preview container
    const divPreviewContainer = document.createElement('div');
    divPreviewContainer.classList.add('chatlist-contact-preview');

    // The avatar, if one exists
    const divAvatarContainer = document.createElement('div');
    divAvatarContainer.style.position = `relative`;

    if (isGroup) {
        const groupAvatarCached = chat.metadata?.avatar_cached;
        if (groupAvatarCached) {
            const imgAvatar = document.createElement('img');
            imgAvatar.src = convertFileSrc(groupAvatarCached);
            imgAvatar.style.width = '50px';
            imgAvatar.style.height = '50px';
            imgAvatar.style.objectFit = 'cover';
            imgAvatar.style.borderRadius = '50%';
            imgAvatar.onerror = () => imgAvatar.replaceWith(createPlaceholderAvatar(true, 50));
            divAvatarContainer.appendChild(imgAvatar);
        } else {
            divAvatarContainer.appendChild(createPlaceholderAvatar(true, 50));
        }
    } else {
        const avatarSrc = getProfileAvatarSrc(profile);
        if (avatarSrc) {
            const imgAvatar = document.createElement('img');
            imgAvatar.src = avatarSrc;
            // Fallback to placeholder if image fails to load
            imgAvatar.onerror = () => {
                imgAvatar.replaceWith(createPlaceholderAvatar(false, 50));
            };
            divAvatarContainer.appendChild(imgAvatar);
        } else {
            // Otherwise, generate a placeholder avatar
            divAvatarContainer.appendChild(createPlaceholderAvatar(false, 50));
        }
    }

    // Add the "Status Icon" to the avatar, then plug-in the avatar container
    // TODO: currently, we "emulate" the status; messages in the last 5m are "online", messages in the last 30m are "away", otherwise; offline.
    if (!isGroup) {
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
            divAvatarContainer.appendChild(divStatusIcon);
        }
        else if (cLastContactMsg && cLastContactMsg.at > Date.now() - 60000 * 30) {
            // set to orange (away)
            divStatusIcon.style.backgroundColor = '#fce459';
            divAvatarContainer.appendChild(divStatusIcon);
        }
        // offline... don't show status icon at all (no need to append the divStatusIcon)
    }

    divContact.appendChild(divAvatarContainer);

    // Header row: name + (group icon | bot icon) + (inline time-ago when unread).
    // Wrapping in a flex header lets the time-ago sit adjacent to the name on
    // unread rows while still letting the right-side count pill anchor far-right.
    const divHeader = document.createElement('div');
    divHeader.classList.add('chatlist-contact-header');

    const h4ContactName = document.createElement('h4');
    if (isGroup) {
        h4ContactName.textContent = chat.metadata?.custom_fields?.name || `Group ${chat.id.substring(0, 8)}...`;
    } else {
        h4ContactName.textContent = profile?.nickname || profile?.name || chat.id;
        if (profile?.nickname || profile?.name) twemojify(h4ContactName);
    }
    h4ContactName.classList.add('cutoff');
    divHeader.appendChild(h4ContactName);

    // Type marker: people-icon for groups, bot-icon for bot DMs.
    // Hover tooltip explains the badge for users who aren't familiar with
    // the iconography yet.
    if (isGroup) {
        const groupIcon = document.createElement('span');
        groupIcon.className = 'icon icon-users-multi chatlist-type-icon';
        groupIcon.addEventListener('mouseenter', () => showGlobalTooltip('Group Chat', groupIcon));
        groupIcon.addEventListener('mouseleave', hideGlobalTooltip);
        divHeader.appendChild(groupIcon);
    } else if (profile?.bot) {
        const botIcon = document.createElement('span');
        botIcon.className = 'icon icon-bot chatlist-type-icon';
        botIcon.addEventListener('mouseenter', () => showGlobalTooltip('Bot', botIcon));
        botIcon.addEventListener('mouseleave', hideGlobalTooltip);
        divHeader.appendChild(botIcon);
    }

    // Inline time-ago (unread-only). Read rows keep the right-aligned variant
    // appended further down.
    const cLastMsgForHeader = chat.messages[chat.messages.length - 1];
    if (nUnread && cLastMsgForHeader) {
        const spanInlineTime = document.createElement('span');
        spanInlineTime.classList.add('chatlist-contact-inline-time');
        spanInlineTime.textContent = timeAgo(cLastMsgForHeader.at);
        spanInlineTime.style.color = 'var(--icon-color-primary)';
        divHeader.appendChild(spanInlineTime);
    }

    divPreviewContainer.appendChild(divHeader);

    // Display either their Last Message or Typing Indicator
    const cLastMsg = chat.messages[chat.messages.length - 1];
    const pChatPreview = document.createElement('p');
    pChatPreview.classList.add('cutoff');

    const preview = generateChatPreviewText(chat);
    pChatPreview.classList.toggle('typing-indicator-text', preview.isTyping);
    if (preview.isHtml) {
        pChatPreview.innerHTML = preview.text;
    } else {
        pChatPreview.textContent = preview.text;
    }
    if (preview.needsTwemoji) twemojify(pChatPreview);
    if (preview.emojiTags && typeof renderCustomEmojiShortcodes === 'function') {
        renderCustomEmojiShortcodes(pChatPreview, preview.emojiTags);
    }

    divPreviewContainer.appendChild(pChatPreview);

    // Add the Chat Preview to the contact UI
    divContact.appendChild(divPreviewContainer);

    // Right-side slot: ping pill when there's something to flag (replaces
    // the time-ago, which has moved inline beside the name); plain time-ago
    // otherwise. A "ping" is anything that should grab attention given the
    // chat's mute state — see `computeRowBadgeCount` for the filtering rules.
    if (nUnread) {
        const spanCount = document.createElement('span');
        spanCount.classList.add('chatlist-contact-count');
        spanCount.textContent = String(nUnread);
        divContact.appendChild(spanCount);
    } else {
        const pTimeAgo = document.createElement('p');
        pTimeAgo.classList.add('chatlist-contact-timestamp', 'read');
        if (cLastMsg) pTimeAgo.textContent = timeAgo(cLastMsg.at);
        divContact.appendChild(pTimeAgo);
    }

    return divContact;
}


/**
 * Render a pending Community invite row (npub gift-wrap) — same look as an MLS invite
 * slot, pinned at the top of the chat list, with Accept / Decline actions.
 * @param {{community_id: string, name: string, inviter_npub: string}} invite
 */
function renderCommunityInviteItem(invite) {
    const divInvite = document.createElement('div');
    divInvite.classList.add('chatlist-contact', 'chatlist-invite');
    divInvite.id = `community-invite-${invite.community_id}`;
    divInvite.style.borderColor = 'var(--icon-color-primary)';

    // The private bundle carries no icon → group placeholder (real logo appears on join).
    const divAvatarContainer = document.createElement('div');
    divAvatarContainer.style.position = 'relative';
    divAvatarContainer.appendChild(createPlaceholderAvatar(true, 50));
    divInvite.appendChild(divAvatarContainer);

    const divPreviewContainer = document.createElement('div');
    divPreviewContainer.classList.add('chatlist-contact-preview');
    const h4Name = document.createElement('h4');
    h4Name.textContent = invite.name || 'Community';
    h4Name.classList.add('cutoff');
    divPreviewContainer.appendChild(h4Name);
    const pSub = document.createElement('p');
    pSub.classList.add('cutoff');
    pSub.textContent = 'Community invite';
    divPreviewContainer.appendChild(pSub);
    divInvite.appendChild(divPreviewContainer);

    const divActions = document.createElement('div');
    divActions.classList.add('invite-action-buttons');

    const btnAccept = document.createElement('button');
    btnAccept.classList.add('invite-action-btn', 'invite-accept-btn');
    btnAccept.title = 'Accept Invite';
    btnAccept.onclick = (e) => { e.stopPropagation(); acceptCommunityInvite(invite.community_id); };
    const acceptIcon = document.createElement('span');
    acceptIcon.classList.add('icon', 'icon-check');
    acceptIcon.style.cssText = 'width:16px;height:16px;background-color:#59fcb3;';
    btnAccept.appendChild(acceptIcon);

    const btnDecline = document.createElement('button');
    btnDecline.classList.add('invite-action-btn', 'invite-decline-btn');
    btnDecline.title = 'Decline Invite';
    btnDecline.onclick = (e) => { e.stopPropagation(); declineCommunityInvite(invite.community_id); };
    const declineIcon = document.createElement('span');
    declineIcon.classList.add('icon', 'icon-x');
    declineIcon.style.cssText = 'width:16px;height:16px;background-color:#ff2ea9;';
    btnDecline.appendChild(declineIcon);

    divActions.appendChild(btnAccept);
    divActions.appendChild(btnDecline);
    divInvite.appendChild(divActions);
    return divInvite;
}
