/**
 * Chat list row context menu + pending community invite row.
 *
 * - `_showChatRowContextMenu` — right-click / long-press actions for a
 *   `.chatlist-contact` row (mark read/unread, mute, pin, block, leave).
 * - `renderCommunityInviteItem` — a `.chatlist-contact.chatlist-invite` row for a
 *   pending community invite (npub gift-wrap) with accept / decline actions.
 *
 * The row DOM itself is rendered by the Svelte island (src/components/Chatlist.svelte);
 * these builders are handed to it via the mount-time `h` helper bundle.
 */

/**
 * Row context menu (right-click / long-press): Mark as Read (when unread) or
 * Mark as Unread (when caught up), a Mute/Unmute toggle, and — for DMs only —
 * Block. Actions reuse the same backend commands as the profile/group panels
 * and repaint the list.
 */
function _showChatRowContextMenu(chat, isGroup, nUnread, x, y) {
    if (chat._joining) return; // nothing actionable until the join finalises
    // A mobile long-press synthesises a trailing tap; stamp the time so the
    // chatlist open handler can swallow it instead of opening the chat.
    window._chatRowMenuAt = Date.now();

    const items = [];
    if (nUnread > 0) {
        items.push({
            label: 'Mark as Read',
            icon: 'check',
            onClick: () => { markChatCaughtUp(chat, /* explicit */ true); chatChanged(chat); },
        });
    } else if (!chat.muted && chatCanMarkUnread(chat)) {
        // Muted chats never show an unread badge, so offering it there would be a silent no-op.
        items.push({
            label: 'Mark as Unread',
            icon: 'eye-off',
            onClick: () => markChatUnread(chat),
        });
    }
    items.push({
        label: chat.muted ? 'Unmute' : 'Mute',
        icon: 'volume-mute',
        onClick: async () => {
            if (blockedBySync()) return;
            chat.muted = await invoke('toggle_chat_mute', { chatId: chat.id });
            chatChanged(chat);
            // A muted sender is silent in every community too.
            if (!isGroup) communitiesChanged();
        },
    });
    // Pin/Unpin. Keyed by chatPinKey, so a Community pins as the COMMUNITY —
    // its general row is what the pin then hoists.
    const strPinKey = chatPinKey(chat);
    const fPinned = arrPinnedChats.includes(strPinKey);
    items.push({
        label: fPinned ? 'Unpin' : 'Pin',
        icon: 'pin',
        onClick: async () => {
            if (blockedBySync()) return;
            try {
                arrPinnedChats = await invoke(fPinned ? 'unpin_chat' : 'pin_chat', { chatId: strPinKey });
                listChanged();
            } catch (e) {
                showToast(e);
            }
        },
    });
    if (!isGroup) {
        items.push({ divider: true });
        items.push({
            label: 'Block',
            icon: 'x-user',
            danger: true,
            onClick: async () => {
                if (blockedBySync()) return;
                const confirmed = await popupConfirm('Block User', 'Are you sure you want to block this user? You will no longer receive DMs from them.', false, '', 'vector_warning.svg');
                if (!confirmed) return;
                await invoke('block_user', { npub: chat.id });
                showToast('User Blocked');
                profileChanged(chat.id);
            },
        });
    } else if (chat.metadata?.custom_fields?.is_owner !== 'true' && chat.metadata?.custom_fields?.community_id) {
        // Owners get Delete (in Group Overview, type-to-confirm); everyone else
        // gets a quick Leave here.
        items.push({ divider: true });
        items.push({
            label: 'Leave',
            icon: 'x-user',
            danger: true,
            onClick: () => leaveCommunityFromList(chat),
        });
    }
    showContextMenu({ x, y, items });
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

    // Show the bundled community icon (fetched + decrypted like a public-invite preview) when present;
    // fall back to the group placeholder for icon-less / pre-icon-bundle invites.
    const divAvatarContainer = document.createElement('div');
    divAvatarContainer.style.position = 'relative';
    const placeholder = createPlaceholderAvatar(true, 50);
    divAvatarContainer.appendChild(placeholder);
    divInvite.appendChild(divAvatarContainer);
    if (invite.icon) {
        invoke('cache_invite_logo', { image: invite.icon }).then(path => {
            if (path && placeholder.isConnected) {
                placeholder.replaceWith(createAvatarImg(convertFileSrc(path), 50, true));
            }
        }).catch(() => {});
    }

    const divPreviewContainer = document.createElement('div');
    divPreviewContainer.classList.add('chatlist-contact-preview');
    // Name + Group Chat icon in a header, matching the real community row (renderChat) so the invite
    // row and the joined row look consistent.
    const divHeader = document.createElement('div');
    divHeader.classList.add('chatlist-contact-header');
    const h4Name = document.createElement('h4');
    h4Name.textContent = invite.name || 'Community';
    h4Name.classList.add('cutoff');
    divHeader.appendChild(h4Name);
    const groupIcon = document.createElement('span');
    groupIcon.className = 'icon icon-users-multi chatlist-type-icon';
    groupIcon.addEventListener('mouseenter', () => showGlobalTooltip('Group Chat', groupIcon));
    groupIcon.addEventListener('mouseleave', hideGlobalTooltip);
    divHeader.appendChild(groupIcon);
    divPreviewContainer.appendChild(divHeader);
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
    btnAccept.appendChild(acceptIcon);

    const btnDecline = document.createElement('button');
    btnDecline.classList.add('invite-action-btn', 'invite-decline-btn');
    btnDecline.title = 'Decline Invite';
    btnDecline.onclick = (e) => { e.stopPropagation(); declineCommunityInvite(invite.community_id); };
    const declineIcon = document.createElement('span');
    declineIcon.classList.add('icon', 'icon-x');
    btnDecline.appendChild(declineIcon);

    divActions.appendChild(btnAccept);
    divActions.appendChild(btnDecline);
    divInvite.appendChild(divActions);
    return divInvite;
}