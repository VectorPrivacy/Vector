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
                    (community.channels || []).map(c => ({
                        id: c.channel_id, name: c.name, private: !!c.private, readable: c.readable !== false,
                    })));
            }
            for (const community of list || []) communityChanged(community.community_id);
        })
        .catch(() => {})
        .finally(() => { communityChannelsLoading = false; });
}

/** Adopt a channel set straight off a `get_community` summary the caller already fetched. */
function setCommunityChannels(communityId, channels) {
    if (!communityId || !Array.isArray(channels)) return;
    communityChannelsCache.set(communityId, channels.map(c => ({
        id: c.channel_id, name: c.name, private: !!c.private, readable: c.readable !== false,
    })));
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
    communityChanged(communityId);
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
            communityChanged(communityId);
        })
        .catch(() => {});
    return false;
}

/** Channel create/delete are CORD-03 editions: v2 only. */
function communityIsV2(communityId) {
    return arrChats.some(c =>
        communityIdOfChat(c) === communityId && c.metadata?.custom_fields?.proto_version === '2');
}

/* ── Sections ──────────────────────────────────────────────────────────────
 * A section is `{ id, label, channels, canAdd }` and nothing more, so the day
 * the backend grows user-defined sections this file only has to change where
 * the list is BUILT — every renderer below already speaks the shape. Today the
 * only grouping the protocol knows is public vs private.
 */

/** Collapsed sections, per community, kept across restarts. */
const CHANNEL_SECTION_KEY = 'ws_channel_sections_closed';

function loadClosedSections() {
    try {
        return new Set(JSON.parse(localStorage.getItem(CHANNEL_SECTION_KEY) || '[]'));
    } catch {
        return new Set();
    }
}

let closedChannelSections = loadClosedSections();

function sectionKey(communityId, sectionId) {
    return `${communityId}:${sectionId}`;
}

function channelSectionClosed(communityId, sectionId) {
    return closedChannelSections.has(sectionKey(communityId, sectionId));
}

/** Persist a section fold; the channel list island flips the fold itself. */
function toggleChannelSection(communityId, sectionId) {
    const key = sectionKey(communityId, sectionId);
    if (closedChannelSections.has(key)) closedChannelSections.delete(key);
    else closedChannelSections.add(key);
    try {
        localStorage.setItem(CHANNEL_SECTION_KEY, JSON.stringify([...closedChannelSections]));
    } catch { /* a full quota must not break navigation */ }
}

/// Last raid verdict per community, so the menu can escalate its Moderation entry
/// without waiting on a round-trip while the user is already looking at the menu.
const communityRaidAlerts = new Map();

/// Forget a cached verdict. Every moderation action changes who is a member, and a
/// stale entry leaves the menu quoting a count from before the action ran.
function clearCommunityRaidAlert(communityId) {
    communityRaidAlerts.delete(communityId);
    VectorSvelte.touchCommunity(communityId);
}

/**
 * Refresh the community's raid verdict (the pane head's pip and the menu read it).
 * Asynchronous by design: the assessment reads a window of message history, so it
 * must never sit in front of the chat list rendering.
 */
async function refreshCommunityRaidAlert(communityId) {
    let verdict = null;
    try {
        verdict = await invoke('check_community_raid', { communityId });
    } catch (_) {
        return;
    }
    const before = communityRaidAlerts.get(communityId);
    communityRaidAlerts.set(communityId, verdict);
    if (!!before?.detected !== !!verdict?.detected || before?.suspects !== verdict?.suspects) {
        VectorSvelte.touchCommunity(communityId);
    }
}

/**
 * The community's own menu, hung off its header — Discord's server dropdown.
 * Reuses the context-menu component, so it inherits its viewport clamping,
 * outside-click dismissal and styling rather than growing a second one.
 */
async function openCommunityMenu(chat, ev) {
    if (!chat) return;
    const cf = chat.metadata?.custom_fields || {};
    const rect = ev.currentTarget.getBoundingClientRect();
    // No description row: a menu item that does nothing still hovers like one, and a
    // sentence-long label stretched the menu to twice its useful width. It reads in
    // the details pane, which has the room for it.
    const items = [];

    items.push({
        label: 'Invite People',
        icon: 'add-user',
        onClick: () => openCommunityInvitePanel(chat),
    });
    items.push({
        label: chat.muted ? 'Unmute Community' : 'Mute Community',
        icon: chat.muted ? 'volume-max' : 'volume-mute',
        onClick: async () => {
            chat.muted = await invoke('toggle_chat_mute', { chatId: chat.id });
            chatChanged(chat);
        },
    });
    items.push({
        label: 'Members',
        icon: 'users-multi',
        onClick: () => openCommunityDetails(chat),
    });
    // Batch containment (raid triage, invite revocation, key rotation). Needs BAN
    // rather than KICK, and only v2 can rotate. Awaited before the menu is built —
    // a late push lands in an array the component has already read.
    if (cf.proto_version === '2' && cf.community_id) {
        const caps = await invoke('get_community_capabilities', { communityId: cf.community_id }).catch(() => null);
        if (caps?.ban) {
            const raid = communityRaidAlerts.get(cf.community_id);
            items.push({
                label: 'Moderation',
                // Under a raid the entry stops being one option among five.
                hint: raid?.detected ? `${raid.suspects} flagged` : undefined,
                icon: 'warning',
                danger: !!raid?.detected,
                onClick: () => openModerationPanel(cf.community_id),
            });
        }
    }
    items.push({ divider: true });
    // Owner or member, the same entry point decides which flow it is — and both
    // ask before doing anything.
    items.push({
        label: cf.is_owner === 'true' ? 'Delete Community' : 'Leave Community',
        icon: 'x-user',
        danger: true,
        onClick: () => communityLeaveOrDelete(chat),
    });

    showContextMenu({ x: rect.left, y: rect.bottom + 4, items });
}

async function promptCreateChannel(communityId, isPrivate = false) {
    const name = await popupConfirm(isPrivate ? 'New private channel' : 'New channel',
        isPrivate
            ? 'Only members you grant access to can read it.'
            : 'Everyone in the community can read and post in it.',
        false, 'channel name');
    if (!name || !String(name).trim()) return;
    try {
        const channelId = await invoke('create_community_channel', { communityId, name, private: isPrivate });
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
    // Widescreen names the community at the top of its own channel pane, so the
    // header only has to say which channel you're in. Bare, with no '#': there
    // the hash is drawn as a glyph beside it, and a literal one would double it.
    if (typeof wsActive === 'function' && wsActive() && communityIdOfChat(chat)) {
        return cf.channel_name || name;
    }
    if (isPrimaryChannelChat(chat) || !cf.channel_name) return name;
    return `${name} › #${cf.channel_name}`;
}
