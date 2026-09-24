/**
 * Chat list row context menu: right-click / long-press actions for a
 * `.chatlist-contact` row (mark read/unread, mute, pin, block, leave). The rows
 * themselves are the Svelte island (src/components/chatlist/), which gets this
 * through the mount-time `h` helper bundle.
 */

/**
 * Row context menu (right-click / long-press): Mark as Read (when unread) or
 * Mark as Unread (when caught up), a Mute/Unmute toggle, and — for DMs only —
 * Block. Actions reuse the same backend commands as the profile/group panels
 * and repaint the list.
 */
async function _showChatRowContextMenu(chat, isGroup, nUnread, x, y) {
    if (chat._joining) return; // nothing actionable until the join finalises
    // A mobile long-press synthesises a trailing tap; stamp the time so the
    // chatlist open handler can swallow it instead of opening the chat.
    window._chatRowMenuAt = Date.now();

    const items = [];
    if (nUnread > 0) {
        items.push({
            label: 'Mark as Read',
            icon: 'check',
            onClick: () => {
                const communityId = isGroup && chat.metadata?.custom_fields?.community_id;
                if (communityId) markCommunityCaughtUp(communityId);
                else { markChatCaughtUp(chat, /* explicit */ true); chatChanged(chat); }
            },
        });
    } else if (!chat.muted && chatCanMarkUnread(chat)) {
        // Muted chats never show an unread badge, so offering it there would be a silent no-op.
        items.push({
            label: 'Mark as Unread',
            icon: 'eye-off',
            onClick: () => markChatUnread(chat),
        });
    }
    // A community row stands for the whole community, so its settings are the
    // community's, not its anchoring channel's.
    const strNotifyScope = isGroup
        ? (chat.metadata?.custom_fields?.community_id || chat.id)
        : chat.id;
    if (!blockedBySync()) {
        items.push(...await notifyMenuItems(strNotifyScope, isGroup ? strNotifyScope : null));
    }
    if (!isGroup) {
        const fPinned = arrPinnedChats.includes(chat.id);
        items.push({
            label: fPinned ? 'Unpin' : 'Pin',
            icon: 'pin',
            onClick: async () => {
                if (blockedBySync()) return;
                try {
                    arrPinnedChats = await invoke(fPinned ? 'unpin_chat' : 'pin_chat', { chatId: chat.id });
                    listChanged();
                } catch (e) {
                    showToast(e);
                }
            },
        });
    }
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
