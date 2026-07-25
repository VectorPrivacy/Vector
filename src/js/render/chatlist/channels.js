/**
 * Community channel rows for the chat list.
 *
 * The chat list shows ONE row per community (its primary channel). A community with
 * more than one channel gets an expander on that row; expanding nests its channels
 * underneath, which is the only way to reach a non-primary channel in the UI.
 *
 * The community DOCUMENT is the authority on which channels exist, not the local chat
 * rows: a tombstoned channel's chat row (and its history) stays on disk, so listing
 * from `arrChats` would resurrect deleted channels after a restart. Chat rows are
 * still where unread counts and messages come from, looked up by channel id.
 */

/** communityId → [{ id, name }], as last read from the community documents. */
const communityChannelsCache = new Map();
/** communityIds whose channel list is expanded in the chat list. */
const expandedCommunities = new Set();
/** Guards the shared load so a render pass can't stampede the backend. */
let communityChannelsLoading = false;

/**
 * Fill the channel cache for every held community in one call. Renders are frequent and
 * synchronous, so they call this and read whatever is cached; the load re-renders when it
 * lands (the channel counts feed the list's state hash, so that render isn't a no-op).
 */
function loadCommunityChannels() {
    if (communityChannelsLoading) return;
    communityChannelsLoading = true;
    invoke('list_communities')
        .then(list => {
            for (const community of list || []) {
                communityChannelsCache.set(community.community_id,
                    (community.channels || []).map(c => ({ id: c.channel_id, name: c.name })));
            }
            renderChatlist();
        })
        .catch(() => {})
        .finally(() => { communityChannelsLoading = false; });
}

/** Adopt a channel set straight off a `get_community` summary the caller already fetched. */
function setCommunityChannels(communityId, channels) {
    if (!communityId || !Array.isArray(channels)) return;
    communityChannelsCache.set(communityId, channels.map(c => ({ id: c.channel_id, name: c.name })));
}

/** Channels for a community, or null before the first load lands. */
function getCommunityChannels(communityId) {
    if (!communityChannelsCache.has(communityId)) {
        loadCommunityChannels();
        return null;
    }
    return communityChannelsCache.get(communityId);
}

/** Re-read the channel sets (a create/delete/rename, or a folded control change). */
function refreshCommunityChannels() {
    loadCommunityChannels();
}

/**
 * Values the chat list's state hash needs so channel changes actually repaint: the channel
 * set per community, its expanded state, and each channel's unread count.
 */
function channelStateHashParts(chat, states) {
    const communityId = communityIdOfChat(chat);
    if (!communityId) return;
    const channels = communityChannelsCache.get(communityId);
    // The caps flag lands asynchronously and gates both the expander and the "Add
    // channel" row, so the gate has to see it or that first repaint is a no-op.
    states.push(communityId, expandedCommunities.has(communityId),
        communityChannelCaps.get(communityId) === true, channels ? channels.length : -1);
    for (const channel of channels || []) {
        const channelChat = arrChats.find(c => c.id === channel.id);
        states.push(channel.id, channel.name, channelChat ? computeRowBadgeCount(channelChat) : 0);
    }
}

/** The community id a chat row belongs to, or null for anything that isn't a channel. */
function communityIdOfChat(chat) {
    return chat?.metadata?.custom_fields?.community_id || null;
}

/**
 * Whether this Community chat is the row the list renders. Non-primary channels are
 * real chats with real history; they just live under their community's row instead of
 * beside it. Rows predating the primary stamp fall back to rendering (better a
 * duplicate row than a community that vanishes from the list).
 */
function isPrimaryChannelChat(chat) {
    const primary = chat?.metadata?.custom_fields?.primary_channel;
    return !primary || primary === chat.id;
}

/**
 * Whether the community's row gets a channel expander. More than one channel is the
 * obvious case; a community you can manage also qualifies with a single channel, or
 * "Add channel" would be unreachable on every community that has never had a second one.
 */
function communityHasChannelList(communityId) {
    const channels = communityChannelsCache.get(communityId);
    if (!channels || !channels.length) return false;
    return channels.length > 1 || communityCanAddChannels(communityId);
}

/** v2 + MANAGE_CHANNELS: the two conditions for creating or deleting a channel. */
function communityCanAddChannels(communityId) {
    return communityIsV2(communityId) && communityCanManageChannels(communityId);
}

function toggleCommunityExpanded(communityId) {
    if (expandedCommunities.has(communityId)) expandedCommunities.delete(communityId);
    else expandedCommunities.add(communityId);
    renderChatlist();
}

/** Whether a community's channel list is currently showing. */
function communityChannelsShown(communityId) {
    return expandedCommunities.has(communityId);
}

/**
 * Expand the open chat's community so the list shows where you are. Returns true when
 * that changed something, i.e. when the caller owes a re-render — opening a chat is not
 * otherwise a reason to rebuild every row.
 */
function ensureOpenChannelVisible() {
    const communityId = communityIdOfChat(arrChats.find(c => c.id === strOpenChat));
    if (!communityId || expandedCommunities.has(communityId)) return false;
    const channels = communityChannelsCache.get(communityId);
    // Only auto-expand a real list; a lone channel plus an "Add channel" row is a
    // management affordance, not something to unfold every time you open a chat.
    if (!channels || channels.length < 2) return false;
    expandedCommunities.add(communityId);
    return true;
}

/** communityId → whether this user may add/remove its channels (lazy, cached). */
const communityChannelCaps = new Map();

function communityCanManageChannels(communityId) {
    if (communityChannelCaps.has(communityId)) return communityChannelCaps.get(communityId);
    communityChannelCaps.set(communityId, false);
    invoke('get_community_capabilities', { communityId })
        .then(caps => {
            if (!caps?.manage_channels) return;
            communityChannelCaps.set(communityId, true);
            renderChatlist();
        })
        .catch(() => {});
    return false;
}

/** Channel create/delete are CORD-03 editions: v2 only. */
function communityIsV2(communityId) {
    return arrChats.some(c =>
        communityIdOfChat(c) === communityId && c.metadata?.custom_fields?.proto_version === '2');
}

/**
 * Rows for a community's channels, or null when there's nothing worth showing.
 * Rendered directly after the community's own row in the list.
 */
function renderCommunityChannels(communityId) {
    const channels = getCommunityChannels(communityId);
    if (!channels || !communityChannelsShown(communityId)) return null;
    const canManage = communityCanAddChannels(communityId);
    if (channels.length < 2 && !canManage) return null;
    const wrap = document.createElement('div');
    wrap.className = 'chatlist-channels';
    for (const channel of channels) {
        wrap.appendChild(renderChannelRow(communityId, channel, canManage));
    }
    if (canManage) wrap.appendChild(renderAddChannelRow(communityId));
    return wrap;
}

function renderAddChannelRow(communityId) {
    const row = document.createElement('div');
    row.className = 'chatlist-channel chatlist-channel-add';
    row.innerHTML = '<span class="chatlist-channel-hash">+</span>';
    const name = document.createElement('span');
    name.className = 'chatlist-channel-name';
    name.textContent = 'Add channel';
    row.appendChild(name);
    row.onclick = () => promptCreateChannel(communityId);
    return row;
}

async function promptCreateChannel(communityId) {
    const name = await popupConfirm('New channel',
        'Everyone in the community can read and post in it.', false, 'channel name');
    if (!name || !String(name).trim()) return;
    try {
        const channelId = await invoke('create_community_channel', { communityId, name, private: false });
        refreshCommunityChannels();
        expandedCommunities.add(communityId);
        openCommunityChannel(communityId, { id: channelId, name });
    } catch (e) {
        await popupConfirm("Couldn't add the channel", escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

async function promptDeleteChannel(communityId, channel) {
    const ok = await popupConfirm('Delete channel',
        `Delete <b>#${escapeHtml(channel.name)}</b>? Everyone loses access to it. Messages already on this device are kept.`,
        false, '', 'vector_warning.svg');
    if (!ok) return;
    try {
        await invoke('delete_community_channel', { communityId, channelId: channel.id });
        if (strOpenChat === channel.id) closeChat();
        refreshCommunityChannels();
    } catch (e) {
        await popupConfirm("Couldn't delete the channel", escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

function renderChannelRow(communityId, channel, canManage) {
    const row = document.createElement('div');
    row.className = 'chatlist-channel';
    row.id = `chatlist-channel-${channel.id}`;
    if (channel.id === strOpenChat) row.classList.add('active');

    const hash = document.createElement('span');
    hash.className = 'chatlist-channel-hash';
    hash.textContent = '#';
    row.appendChild(hash);

    const name = document.createElement('span');
    name.className = 'chatlist-channel-name cutoff';
    name.textContent = channel.name;
    row.appendChild(name);

    const chat = arrChats.find(c => c.id === channel.id);
    const unread = chat ? computeRowBadgeCount(chat) : 0;
    if (unread) {
        row.classList.add('has-unread');
        const badge = document.createElement('span');
        badge.className = 'chatlist-channel-badge';
        badge.textContent = unread > 99 ? '99+' : String(unread);
        row.appendChild(badge);
    }

    // The primary channel anchors the community's list row and its history — the backend
    // refuses to tombstone it, so no affordance for it here either.
    const isPrimary = arrChats.some(c =>
        c.id === channel.id && c.metadata?.custom_fields?.primary_channel === channel.id);
    if (canManage && !isPrimary) {
        const remove = document.createElement('div');
        remove.className = 'chatlist-channel-delete btn';
        remove.title = 'Delete channel';
        remove.innerHTML = '<span class="icon icon-x"></span>';
        remove.onclick = (e) => { e.stopPropagation(); promptDeleteChannel(communityId, channel); };
        row.appendChild(remove);
    }

    row.onclick = () => openCommunityChannel(communityId, channel);
    return row;
}

/**
 * Open a community channel, creating its chat row if this device has never synced it.
 * A channel chat needs its community's identity metadata (owner, admins, protocol) for
 * the message renderer, so it's seeded from the community's primary row.
 */
function openCommunityChannel(communityId, channel) {
    let chat = arrChats.find(c => c.id === channel.id);
    if (!chat) {
        const primary = arrChats.find(c =>
            communityIdOfChat(c) === communityId && isPrimaryChannelChat(c));
        chat = getOrCreateChat(channel.id, 'Community');
        chat.metadata = {
            ...(primary?.metadata || {}),
            custom_fields: { ...(primary?.metadata?.custom_fields || {}) },
        };
        chat.metadata.custom_fields.channel_name = channel.name;
        invoke('sync_community_channel', { channelId: channel.id, beforeMs: null }).catch(() => {});
    } else if (!chat.metadata?.custom_fields?.channel_name) {
        chat.metadata.custom_fields.channel_name = channel.name;
    }
    openChat(channel.id);
}

/**
 * The title for a Community chat's header: the community's name, plus the channel when
 * you're in one of its secondary channels (the primary channel IS the community row, so
 * naming it there would be noise on every single-channel community).
 */
function communityChatTitle(chat) {
    const cf = chat?.metadata?.custom_fields || {};
    const name = cf.name || '';
    if (isPrimaryChannelChat(chat) || !cf.channel_name) return name;
    return `${name} › #${cf.channel_name}`;
}
