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
            renderChatlist();
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

/**
 * Flip the section in place rather than asking for a re-render: `renderChatlist`
 * is gated on a hash of CHAT state, which a collapsed section is not part of, so
 * a render request here is a no-op and the section could never reopen.
 */
function toggleChannelSection(communityId, sectionId, wrap) {
    const key = sectionKey(communityId, sectionId);
    const closing = !closedChannelSections.has(key);
    if (closing) closedChannelSections.add(key);
    else closedChannelSections.delete(key);
    try {
        localStorage.setItem(CHANNEL_SECTION_KEY, JSON.stringify([...closedChannelSections]));
    } catch { /* a full quota must not break navigation */ }
    wrap?.classList.toggle('is-closed', closing);
}

/**
 * Group a community's channels into sections. A section with no channels is not
 * rendered at all, so a community with nothing private never sees the word.
 */
function buildChannelSections(channels, canManage) {
    const out = [];
    const open = channels.filter(c => !c.private);
    const shut = channels.filter(c => c.private);
    if (open.length) out.push({ id: 'public', label: 'Public', channels: open, canAdd: canManage });
    if (shut.length) out.push({ id: 'private', label: 'Private', channels: shut, canAdd: canManage });
    // Nothing at all yet: still offer the one section you can add into.
    if (!out.length && canManage) out.push({ id: 'public', label: 'Public', channels: [], canAdd: true });
    return out;
}

function renderChannelSection(communityId, section) {
    const wrap = document.createElement('div');
    wrap.className = 'chatlist-channel-section';
    const closed = channelSectionClosed(communityId, section.id);
    if (closed) wrap.classList.add('is-closed');

    const head = document.createElement('div');
    head.className = 'chatlist-channel-section-head';

    const toggle = document.createElement('div');
    toggle.className = 'chatlist-channel-section-toggle btn';
    const label = document.createElement('span');
    label.className = 'chatlist-channel-section-label';
    label.textContent = section.label;
    toggle.appendChild(label);
    const caret = document.createElement('span');
    caret.className = 'chatlist-channel-section-caret';
    caret.innerHTML = '<span class="icon icon-chevron-down"></span>';
    toggle.appendChild(caret);
    toggle.onclick = () => toggleChannelSection(communityId, section.id, wrap);
    head.appendChild(toggle);

    if (section.canAdd) {
        const add = document.createElement('div');
        add.className = 'chatlist-channel-section-add btn';
        add.title = `Add a ${section.label.toLowerCase()} channel`;
        add.innerHTML = '<span class="icon icon-plus"></span>';
        add.onclick = (e) => { e.stopPropagation(); promptCreateChannel(communityId, section.id === 'private'); };
        head.appendChild(add);
    }
    wrap.appendChild(head);

    const body = document.createElement('div');
    body.className = 'chatlist-channel-section-body';
    for (const channel of section.channels) {
        body.appendChild(renderChannelRow(communityId, channel, section.canAdd));
    }
    wrap.appendChild(body);
    return wrap;
}

function renderCommunityChannels(communityId, { pane = false } = {}) {
    const channels = getCommunityChannels(communityId);
    if (!channels) return null;
    const canManage = communityCanAddChannels(communityId);
    // Nested under a row, the list is an optional disclosure: it hides when
    // collapsed, and a lone channel isn't worth unfolding. As the pane it IS the
    // navigation — a single-channel community still has to show that channel.
    if (!pane && (!communityChannelsShown(communityId) || (channels.length < 2 && !canManage))) return null;
    const wrap = document.createElement('div');
    wrap.className = pane ? 'chatlist-channels chatlist-channels-pane' : 'chatlist-channels';
    // Nested under a community row the list is a quick disclosure, so it stays a
    // flat set; the pane is the navigation and gets the sections.
    if (!pane) {
        for (const channel of channels) {
            wrap.appendChild(renderChannelRow(communityId, channel, canManage));
        }
        if (canManage) wrap.appendChild(renderAddChannelRow(communityId));
        return wrap;
    }
    for (const section of buildChannelSections(channels, canManage)) {
        wrap.appendChild(renderChannelSection(communityId, section));
    }
    return wrap;
}

/**
 * The community's identity at the top of its channel pane: icon, name, member
 * count. Clicking it opens the details sidebar, which is where the roster and
 * the community's actions already live.
 */
function renderCommunityListHeader(communityId) {
    const primary = arrChats.find(c => communityIdOfChat(c) === communityId && isPrimaryChannelChat(c))
        || arrChats.find(c => communityIdOfChat(c) === communityId);
    const cf = primary?.metadata?.custom_fields || {};

    const head = document.createElement('div');
    head.className = 'chatlist-community-head btn';
    head.id = 'chatlist-community-head';

    const avatarSrc = primary?.metadata?.avatar_cached ? convertFileSrc(primary.metadata.avatar_cached) : null;
    const avatar = avatarSrc ? createAvatarImg(avatarSrc, 36, true) : createPlaceholderAvatar(true, 36);
    avatar.classList.add('chatlist-community-head-avatar');
    head.appendChild(avatar);

    const meta = document.createElement('div');
    meta.className = 'chatlist-community-head-meta';

    const name = document.createElement('span');
    name.className = 'chatlist-community-head-name cutoff';
    name.textContent = cf.name || 'Community';
    twemojify(name);
    meta.appendChild(name);

    const members = document.createElement('span');
    members.className = 'chatlist-community-head-members';
    // The glyph says "people" before the number is read, and holds the line's
    // height while the count is still empty.
    const membersIcon = document.createElement('span');
    membersIcon.className = 'icon icon-users-multi chatlist-community-head-members-icon';
    members.appendChild(membersIcon);
    const membersText = document.createElement('span');
    // Empty until the count lands; the fetch refreshes the header when it does.
    membersText.textContent = communityMemberSubtext(communityId);
    members.appendChild(membersText);
    meta.appendChild(members);
    head.appendChild(meta);

    // `.icon` is absolutely positioned to fill its parent, so it needs a box of its
    // own — dropped straight into the (sticky, therefore positioned) header it
    // spans the whole thing and lands over the title.
    const caretBox = document.createElement('div');
    caretBox.className = 'chatlist-community-head-caret';
    const caret = document.createElement('span');
    caret.className = 'icon icon-chevron-down';
    caretBox.appendChild(caret);
    head.appendChild(caretBox);

    refreshCommunityMemberCount(communityId);
    if (primary?.metadata?.custom_fields?.proto_version === '2') refreshCommunityRaidAlert(communityId, head);
    head.onclick = (e) => openCommunityMenu(primary, e);
    return head;
}

/// Last raid verdict per community, so the menu can escalate its Moderation entry
/// without waiting on a round-trip while the user is already looking at the menu.
const communityRaidAlerts = new Map();

/// Forget a cached verdict. Every moderation action changes who is a member, and a
/// stale entry leaves the menu quoting a count from before the action ran.
function clearCommunityRaidAlert(communityId) {
    communityRaidAlerts.delete(communityId);
    for (const pip of document.querySelectorAll('.chatlist-community-head-alert')) pip.remove();
}

/**
 * Paint the header's raid pip. Asynchronous by design: the assessment reads a window of
 * message history, so it must never sit in front of the chat list rendering.
 */
async function refreshCommunityRaidAlert(communityId, head) {
    let verdict = null;
    try {
        verdict = await invoke('check_community_raid', { communityId });
    } catch (_) {
        return;
    }
    communityRaidAlerts.set(communityId, verdict);
    if (!verdict?.detected || !head.isConnected) return;
    if (head.querySelector('.chatlist-community-head-alert')) return;
    const pip = document.createElement('span');
    pip.className = 'chatlist-community-head-alert';
    pip.title = `${verdict.suspects} accounts flagged as a raid \u2014 open Moderation`;
    head.insertBefore(pip, head.querySelector('.chatlist-community-head-caret'));
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
            renderChatlist();
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

function renderChannelRow(communityId, channel, canManage) {
    const row = document.createElement('div');
    row.className = 'chatlist-channel';
    row.id = `chatlist-channel-${channel.id}`;
    if (channel.id === strOpenChat) row.classList.add('active');

    const hash = document.createElement('span');
    hash.className = 'chatlist-channel-hash';
    hash.innerHTML = '<span class="icon icon-channel-hash"></span>';
    row.appendChild(hash);

    const name = document.createElement('span');
    name.className = 'chatlist-channel-name cutoff';
    name.textContent = channel.name;
    row.appendChild(name);

    // Three tiers, loudest first: something to read, nothing to read, and a room
    // you asked to be quiet. Muted wins outright — it is a standing instruction,
    // not a state that unread can override.
    const chat = arrChats.find(c => c.id === channel.id);
    if (chat?.muted) row.classList.add('is-muted');
    else if (chat && computeRowBadgeCount(chat) > 0) row.classList.add('has-unread');
    else row.classList.add('is-read');

    // A NUMBER only for someone calling your name. Ordinary unread is carried by
    // the row's own weight, so the list stays scannable at a glance.
    const pings = chat ? countPingMessages(chat) : 0;
    if (pings) {
        const badge = document.createElement('span');
        badge.className = 'chatlist-channel-badge';
        badge.textContent = pings > 99 ? '99+' : String(pings);
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
    // Widescreen names the community at the top of its own channel pane, so the
    // header only has to say which channel you're in. Bare, with no '#': there
    // the hash is drawn as a glyph beside it, and a literal one would double it.
    if (typeof wsActive === 'function' && wsActive() && communityIdOfChat(chat)) {
        return cf.channel_name || name;
    }
    if (isPrimaryChannelChat(chat) || !cf.channel_name) return name;
    return `${name} › #${cf.channel_name}`;
}
