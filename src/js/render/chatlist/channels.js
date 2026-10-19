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

/**
 * The channels a summary lists that this member may see. A Private Channel shows only
 * while we hold its CURRENT key: a rekey that leaves us out drops the key backend-side
 * (the channel is then recorded keyless, as the cursor for a future re-grant), so
 * `readable` false is exactly "not, or no longer, entitled".
 */
function visibleChannels(channels) {
    return (channels || [])
        .filter(c => !(c.private && c.readable === false))
        // On the new roles a channel you can't view is hidden like a private one you lack.
        .filter(c => c.my_permissions == null || (BigInt(c.my_permissions) & (1n << 13n)) !== 0n)
        .map(c => ({
            id: c.channel_id, name: c.name || 'Private channel', private: !!c.private, readable: true,
            category: c.category_id || null, topic: c.topic || '', view: c.view || 'chat',
            perms: c.my_permissions ?? null,
            restricted: !!c.restricted, writer: c.can_post !== false,
        }));
}

/** Whether the local user may post in a channel: always on the legacy roster. */
function channelAllowsSend(chat) {
    const communityId = communityIdOfChat(chat);
    const perms = communityId && communityChannelsCache.get(communityId)?.find(c => c.id === chat.id)?.perms;
    return perms == null || (BigInt(perms) & (1n << 14n)) !== 0n;
}

/** Whether the local user may post in a restricted channel but its writer key hasn't arrived. */
function channelAwaitsWriterKey(chat) {
    const communityId = communityIdOfChat(chat);
    const ch = communityId && communityChannelsCache.get(communityId)?.find(c => c.id === chat.id);
    return !!(ch?.restricted && !ch.writer);
}

function lockedChannels(channels) {
    return (channels || [])
        .filter(c => c.private && c.readable === false)
        // A private channel's name is sealed from members without its key.
        .map(c => ({ id: c.channel_id, name: c.name || 'Private channel' }));
}

/**
 * Leave a channel that is no longer visible, for the community's primary: a revoke
 * landing while it is on screen must not leave the user reading a room they are out of.
 */
function leaveHiddenChannel(communityId) {
    if (!strOpenChat) return;
    const open = arrChats.find(c => c.id === strOpenChat);
    if (!open || communityIdOfChat(open) !== communityId) return;
    const channels = communityChannelsCache.get(communityId);
    if (!channels || channels.some(c => c.id === strOpenChat)) return;
    const primary = arrChats.find(c => communityIdOfChat(c) === communityId && isPrimaryChannelChat(c));
    const target = primary && channels.some(c => c.id === primary.id) ? primary.id : channels[0]?.id;
    if (target) openChat(target);
}

/** communityId → [{ id, name, private, category, topic, view }] in sidebar order, as last read. */
const communityChannelsCache = new Map();
/** communityId → [{ id, name }]: its Categories in sidebar order. */
const communityCategoriesCache = new Map();
/** Per community, the private channels it lists that we hold no key for: named, never opened. */
const communityLockedChannels = new Map();
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
                communityChannelsCache.set(community.community_id, visibleChannels(community.channels));
                communityLockedChannels.set(community.community_id, lockedChannels(community.channels));
                communityCategoriesCache.set(community.community_id, categoriesOf(community.categories));
            }
            for (const community of list || []) {
                communityChanged(community.community_id);
                leaveHiddenChannel(community.community_id);
            }
        })
        .catch(() => {})
        .finally(() => { communityChannelsLoading = false; });
}

function categoriesOf(categories) {
    return (categories || []).map(k => ({ id: k.category_id, name: k.name }));
}

/** Adopt a channel set straight off a `get_community` summary the caller already fetched. */
function setCommunityChannels(communityId, channels, categories) {
    if (!communityId || !Array.isArray(channels)) return;
    communityChannelsCache.set(communityId, visibleChannels(channels));
    communityLockedChannels.set(communityId, lockedChannels(channels));
    if (Array.isArray(categories)) communityCategoriesCache.set(communityId, categoriesOf(categories));
    communityChanged(communityId);
    leaveHiddenChannel(communityId);
}

/** communityId → the Timeouts running there, npub → { from, until }, as last read. */
const communityTimeoutsCache = new Map();
/** channelId → the authors who can't send there, as last read. */
const channelSendDeniedCache = new Map();
function channelSendDenied(channelId) {
    return channelSendDeniedCache.get(channelId) || null;
}
/** Per community: who may mention everyone, and its Roles for mentioning (CORD-04 §8). */
const communityMentionsCache = new Map();

async function loadCommunityMentions(communityId) {
    if (!communityId) return;
    let m;
    try { m = await invoke('get_community_mentions', { communityId }); } catch { return; }
    const print = (x) => x && JSON.stringify([x.anyone, [...x.pingers], x.roles]);
    const had = print(communityMentionsCache.get(communityId));
    communityMentionsCache.set(communityId, { anyone: !!m.anyone, pingers: new Set(m.pingers || []), roles: m.roles || [] });
    if (had !== print(communityMentionsCache.get(communityId))) refreshMentionRows(communityId);
}

/** Repaint the open channel's rows whose chips depend on who may mention what. */
function refreshMentionRows(communityId) {
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat || communityIdOfChat(chat) !== communityId) return;
    for (const msg of chat.messages || []) {
        if (msg.role_mentions?.length || /@everyone\b/.test(msg.content || '')) updateMessageRow(msg, msg.id);
    }
}

/** Whether `npub` may mention everyone, and so any Role, where `chat` sits. */
function mayMentionEveryone(chat, npub) {
    const m = communityMentionsCache.get(communityIdOfChat(chat));
    if (m) return m.anyone || m.pingers.has(npub);
    return !!(chat?.metadata?.admins?.includes(npub) || chat?.metadata?.custom_fields?.owner_npub === npub);
}

/** The Roles `msg` names in its text and tags that stand: mentionable, or named by someone who may mention everyone. */
function standingRoleMentions(msg, chat) {
    const ids = msg.role_mentions;
    const m = ids?.length && communityMentionsCache.get(communityIdOfChat(chat));
    if (!m) return [];
    const byAnyone = mayMentionEveryone(chat, msg.npub || (msg.mine ? strPubkey : ''));
    return m.roles.filter(r => ids.includes(r.id) && (msg.content || '').includes('@' + r.name) && (r.mentionable || byAnyone));
}

/** Whether `msg` pings me through a Role I hold. */
function rolePingsMe(msg, chat) {
    return !msg.mine && standingRoleMentions(msg, chat).some(r => r.held);
}

/** The Roles I may mention where `chat` sits: the mentionable ones, or all of them. */
function mentionableRoles(chat) {
    const m = communityMentionsCache.get(communityIdOfChat(chat));
    if (!m) return [];
    const all = mayMentionEveryone(chat, strPubkey) || chat?.metadata?.custom_fields?.is_owner === 'true';
    return m.roles.filter(r => all || r.mentionable);
}

function communityTimeoutsFor(communityId) {
    return communityId ? communityTimeoutsCache.get(communityId) : null;
}

let timeoutLockTimer = 0;

/**
 * Read the open chat's community's Timeouts: the rows of what a timed-out member posted
 * collapse, and the composer locks while the local user is the one timed out.
 */
async function refreshTimeoutLock(chat) {
    const communityId = communityIdOfChat(chat);
    if (!communityId) return;
    let running = null;
    try { running = await invoke('get_community_timeouts', { communityId }); } catch { return; }
    const had = JSON.stringify(communityTimeoutsCache.get(communityId) || {});
    communityTimeoutsCache.set(communityId, running || {});
    // In an open channel a post whose author can't send there collapses too.
    const authors = [...new Set((chat.messages || []).filter(m => !m.mine && m.npub).map(m => m.npub))];
    let denied = [];
    try { denied = await invoke('get_channel_send_denied', { communityId, channelId: chat.id, npubs: authors }); } catch { /* the legacy roster: nothing */ }
    const hadDenied = (channelSendDeniedCache.get(chat.id) || []).join();
    channelSendDeniedCache.set(chat.id, new Set(denied));
    const changed = had !== JSON.stringify(running || {}) || hadDenied !== [...channelSendDeniedCache.get(chat.id)].join();
    if (changed && strOpenChat === chat.id) openChat(strOpenChat);
    const mine = running?.[strPubkey];
    if (!mine || strOpenChat !== chat.id) return;
    const until = new Date(mine.until * 1000).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' });
    VectorSvelte.setLock('timeout', `You're timed out until ${until}`);
    VectorSvelte.flushSync();
    // Read again as it ends, which reopens the composer; a timer can't wait past ~24.8 days.
    clearTimeout(timeoutLockTimer);
    timeoutLockTimer = setTimeout(() => { if (strOpenChat === chat.id) refreshTimeoutLock(chat); }, Math.min(mine.until * 1000 - Date.now() + 1000, 2 ** 31 - 1));
}

/** The open channel's topic, '' when it has none. */
function communityChannelTopic(chat) {
    const communityId = communityIdOfChat(chat);
    return (communityId && communityChannelsCache.get(communityId)?.find(c => c.id === chat.id)?.topic) || '';
}

/** A community's Categories in sidebar order ([] before the first load lands). */
function getCommunityCategories(communityId) {
    return communityCategoriesCache.get(communityId) || [];
}

/**
 * Move a channel into `categoryId` (null = uncategorized) at `index` among that group's
 * channels. Shown at once; the list re-reads what the backend kept when the publish lands.
 */
async function moveCommunityChannel(communityId, channelId, categoryId, index) {
    const channels = communityChannelsCache.get(communityId);
    const moved = channels?.find(c => c.id === channelId);
    if (!moved) return;
    const rest = channels.filter(c => c.id !== channelId);
    const group = rest.filter(c => (c.category || null) === (categoryId || null));
    const anchor = group[index - 1];
    const next = { ...moved, category: categoryId || null };
    let at = anchor ? rest.indexOf(anchor) + 1 : (group[0] ? rest.indexOf(group[0]) : rest.length);
    if (!anchor && !group[0] && categoryId) {
        // An empty Category: after the last channel of every Category above it.
        const order = getCommunityCategories(communityId).map(k => k.id);
        const before = new Set(order.slice(0, order.indexOf(categoryId)));
        const last = rest.reduce((i, c, j) => (!c.category || before.has(c.category) ? j : i), -1);
        at = last + 1;
    }
    rest.splice(at, 0, next);
    communityChannelsCache.set(communityId, rest);
    communityChanged(communityId);
    try {
        await invoke('move_community_channel', { communityId, channelId, categoryId: categoryId || null, index });
    } catch (e) {
        showToast(String(e));
    }
    refreshCommunityChannels();
}

/** Move a Category to `index` among the Categories. */
async function moveCommunityCategory(communityId, categoryId, index) {
    const cats = getCommunityCategories(communityId);
    const moved = cats.find(k => k.id === categoryId);
    if (!moved) return;
    const rest = cats.filter(k => k.id !== categoryId);
    rest.splice(Math.min(index, rest.length), 0, moved);
    communityCategoriesCache.set(communityId, rest);
    communityChanged(communityId);
    try {
        await invoke('move_community_category', { communityId, categoryId, index });
    } catch (e) {
        showToast(String(e));
    }
    refreshCommunityChannels();
}

async function promptCreateCategory(communityId) {
    const name = await popupConfirm('New category', 'A heading to group channels under.', false, 'category name');
    if (!name || !String(name).trim()) return;
    try {
        await invoke('create_community_category', { communityId, name: String(name).trim() });
        refreshCommunityChannels();
    } catch (e) {
        await popupConfirm("Couldn't add the category", escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

async function promptRenameCategory(communityId, category) {
    const name = await popupConfirm('Rename category', escapeHtml(category.name), false, 'category name');
    if (!name || !String(name).trim()) return;
    try {
        await invoke('rename_community_category', { communityId, categoryId: category.id, name: String(name).trim() });
        refreshCommunityChannels();
    } catch (e) {
        showToast(String(e));
    }
}

async function promptDeleteCategory(communityId, category) {
    const ok = await popupConfirm('Delete category',
        `Delete <b>${escapeHtml(category.name)}</b>? Its channels stay, uncategorized.`,
        false, '', 'vector_warning.svg');
    if (!ok) return;
    try {
        await invoke('delete_community_category', { communityId, categoryId: category.id });
        refreshCommunityChannels();
    } catch (e) {
        showToast(String(e));
    }
}

/** A Category header's menu: what a manager can do with it. */
function openCategoryMenu(communityId, category, x, y) {
    if (!communityCanAddChannels(communityId)) return;
    showContextMenu({ x, y, items: [
        { label: 'Create Channel', icon: 'plus', onClick: () => promptCreateChannel(communityId, false, category.id) },
        { label: 'Create Private Channel', icon: 'locked', onClick: () => promptCreateChannel(communityId, true, category.id) },
        { label: 'Create Voice Channel', icon: 'volume-max', onClick: () => promptCreateChannel(communityId, false, category.id, true) },
        { label: 'Edit Permissions', icon: 'shield-filled', onClick: () => openChannelEditorFor(communityId, { id: category.id, name: category.name }, true) },
        { divider: true },
        { label: 'Rename Category', icon: 'edit', onClick: () => promptRenameCategory(communityId, category) },
        { label: 'Delete Category', icon: 'x', danger: true, onClick: () => promptDeleteCategory(communityId, category) },
    ] });
}

/** Channel Settings, with its permissions read alongside on the new roles. */
function openChannelEditorFor(communityId, channel, isCategory = false) {
    VectorSvelte.openChannelEditor(communityId, channel, isCategory);
    pushBack('channel-editor', closeChannelEditorGuarded);
    loadChannelPerms(communityId, channel.id);
}

function loadChannelPerms(communityId, channelId) {
    invoke('get_channel_permissions', { communityId, channelId })
        .then((perms) => {
            const st = VectorSvelte.channelEditorState();
            if (st.communityId !== communityId || st.channelId !== channelId) return;
            const names = {};
            for (const o of [...(perms.overwrites || []).filter(o => o.kind === 'member'), ...(perms.members || [])]) {
                names[o.id] = getName(getProfile(o.npub) || o.npub);
            }
            VectorSvelte.setChannelPerms({ ...perms, member_names: names });
        })
        .catch(() => {});
}

/**
 * Close, unless there are unsaved changes: then the save bar says so and Channel Settings
 * stays, as Community Settings does.
 */
function closeChannelEditorGuarded() {
    if (VectorSvelte.ceOverlay.closing()) return;
    const st = VectorSvelte.channelEditorState();
    if (st.busy || VectorSvelte.channelEditorDirty()) {
        VectorSvelte.channelEditorNudge();
        // The back stack pops its entry before calling this; a refused close keeps it.
        pushBack('channel-editor', closeChannelEditorGuarded);
        return;
    }
    popBack('channel-editor');
    VectorSvelte.closeChannelEditor();
}

/** Save what changed: the name, topic and view, then the open target's rules. */
async function saveChannelEditor() {
    const st = VectorSvelte.channelEditorState();
    const { communityId, channelId } = st;
    VectorSvelte.setChannelEditorBusy(true);
    try {
        if (VectorSvelte.channelOverviewDirty()) {
            const saved = { name: st.name.trim(), topic: st.topic.trim(), view: st.view };
            await invoke('edit_community_channel', { communityId, channelId, name: saved.name, topic: saved.topic || null, view: saved.view });
            st.name = saved.name;
            st.topic = saved.topic;
            VectorSvelte.channelEditorSaved(saved);
            refreshCommunityChannels();
        }
        if (VectorSvelte.overwriteDirty()) {
            const [kind, targetId] = st.target === 'everyone' ? ['everyone', null] : st.target.split(':');
            await invoke('set_channel_overwrite', { communityId, scopeId: channelId, kind, targetId, allow: st.draft.allow, deny: st.draft.deny });
            loadChannelPerms(communityId, channelId);
        }
        VectorSvelte.setChannelEditorBusy(false);
    } catch (e) {
        VectorSvelte.setChannelEditorBusy(false, String(e));
    }
}

/**
 * @typedef {Object} ChannelEditorHelpers
 * @property {() => void} close   refused, with the save bar saying why, while edits are unsaved
 * @property {() => void} save
 * @property {() => void} reset
 * @property {(sync: { communityId: string, channelId: string, categoryId: string|null }) => void} setSync
 * @property {(npub: string) => string} name   empty when they have no name of their own
 * @property {(npub: string) => boolean} veiled   Streamer Mode hides the name and key
 * @property {(npub: string) => string} searchName
 * @property {(npub: string) => string|null} avatarSrc
 * @property {(npub: string) => object|null} profile
 * @property {object} ui   MemberRow's { twemojify, showTooltip, hideTooltip }
 */
VectorSvelte.setScreen('channelEditor', {
    h: {
        close: () => closeChannelEditorGuarded(),
        save: () => saveChannelEditor(),
        reset: () => VectorSvelte.resetChannelEditor(),
        setSync: async ({ communityId, channelId, categoryId }) => {
            VectorSvelte.setChannelEditorBusy(true);
            try {
                await invoke('set_channel_sync', { communityId, channelId, categoryId });
                VectorSvelte.setChannelEditorBusy(false);
                loadChannelPerms(communityId, channelId);
            } catch (e) {
                VectorSvelte.setChannelEditorBusy(false, String(e));
            }
        },
        name: (npub) => { const p = getProfile(npub); return streamHidden(npub) || searchName(p) ? getName(p || npub) : ''; },
        veiled: (npub) => streamHidden(npub),
        searchName: (npub) => searchName(npub),
        avatarSrc: (npub) => { const p = getProfile(npub); return p ? getProfileAvatarSrc(p) || null : null; },
        profile: (npub) => getProfile(npub) || null,
        ui: {
            twemojify: (el) => twemojify(el),
            showTooltip: (text, el) => showGlobalTooltip(text, el),
            hideTooltip: () => hideGlobalTooltip(),
        },
    },
});

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
 * Whether the community's row gets a channel expander. More than one channel is the
 * obvious case; a community you can manage also qualifies with a single channel, or
 * "Add channel" would be unreachable on every community that has never had a second one.
 */
function communityHasChannelList(communityId) {
    const channels = communityChannelsCache.get(communityId);
    if (!channels || !channels.length) return false;
    return channels.length > 1 || communityCanAddChannels(communityId);
}

/** MANAGE_CHANNELS: the condition for creating or deleting a channel. */
function communityCanAddChannels(communityId) {
    return communityCanManageChannels(communityId);
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

const _bannerResolving = new Set();

/**
 * A community's banner, resolved once when its pane first shows and again (`force`)
 * whenever its metadata refreshes. A download that fails leaves it bannerless until then.
 */
async function resolveCommunityBanner(communityId, force = false) {
    if (!communityId || _bannerResolving.has(communityId)) return;
    if (!force && communityId in VectorSvelte.bannerState().src) return;
    _bannerResolving.add(communityId);
    try {
        const path = await invoke('cache_community_image', { communityId, isBanner: true });
        VectorSvelte.setCommunityBanner(communityId, path ? convertFileSrc(path) : null);
    } catch (e) {
        console.warn('[Banner] could not load the banner:', e);
        VectorSvelte.setCommunityBanner(communityId, null);
    } finally {
        _bannerResolving.delete(communityId);
    }
}

/** The account's hidden banners, from the local mirror; edits elsewhere stream in after. */
async function loadHiddenBanners() {
    VectorSvelte.setHiddenBanners(await invoke('get_hidden_banners').catch(() => []));
}

async function setCommunityBannerHidden(communityId, hidden) {
    try {
        VectorSvelte.setHiddenBanners(await invoke('set_banner_hidden', { communityId, hidden }));
    } catch (e) {
        showToast(String(e));
    }
}

/**
 * The community's own menu, hung off its header — Discord's server dropdown.
 * Reuses the context-menu component, so it inherits its viewport clamping,
 * outside-click dismissal and styling rather than growing a second one.
 */
async function openCommunityMenu(chat, ev, at) {
    if (!chat) return;
    const cf = chat.metadata?.custom_fields || {};
    // Hung off a header button, or opened where a right-click landed on a rail shortcut.
    const anchor = at || (() => {
        const rect = ev.currentTarget.getBoundingClientRect();
        return { x: rect.left, y: rect.bottom + 4 };
    })();
    // No description row: a menu item that does nothing still hovers like one, and a
    // sentence-long label stretched the menu to twice its useful width. It reads in
    // the details pane, which has the room for it.
    const items = [];
    if (cf.community_id && communityHasUnread(cf.community_id)) {
        items.push({
            label: 'Mark as Read',
            icon: 'check',
            onClick: () => markCommunityCaughtUp(cf.community_id),
        });
        items.push({ divider: true });
    }

    // Awaited before the menu is built: a late push lands in an array the component
    // has already read.
    const caps = cf.community_id
        ? await invoke('get_community_capabilities', { communityId: cf.community_id }).catch(() => null)
        : null;
    items.push({
        label: 'Invite People',
        icon: 'add-user',
        onClick: () => openCommunityInvitePanel(chat),
    });
    if (caps?.manage_channels) {
        items.push({
            label: 'Create Category',
            icon: 'plus',
            onClick: () => promptCreateCategory(cf.community_id),
        });
    }
    if (communitySettingsWritable(caps)) {
        items.push({
            label: 'Community Settings',
            icon: 'settings',
            // Land in the community first, as Moderation does: from the rail you may
            // be somewhere else entirely, and closing should leave you there.
            onClick: () => {
                const target = wsChannelForCommunity(cf.community_id) || chat.id;
                if (target && !chatOnScreen(target)) openChat(target);
                openCommunitySettings(cf.community_id);
            },
        });
    }
    // Scoped to the COMMUNITY, so it reaches every channel rather than whichever
    // one happens to anchor its row.
    if (cf.community_id) {
        items.push({ divider: true });
        items.push(...await notifyMenuItems(cf.community_id, cf.community_id));
        const banners = VectorSvelte.bannerState();
        if (banners.src[cf.community_id] && banners.hidden.includes(cf.community_id)) {
            items.push({
                label: 'Show Banner',
                icon: 'eye',
                onClick: () => setCommunityBannerHidden(cf.community_id, false),
            });
        }
    }
    // Batch containment (raid triage, invite revocation, key rotation). Needs BAN rather than KICK.
    if (cf.community_id) {
        if (caps?.ban) {
            const raid = communityRaidAlerts.get(cf.community_id);
            items.push({
                label: 'Moderation',
                // Under a raid the entry stops being one option among five.
                hint: raid?.detected ? `${raid.suspects} flagged` : undefined,
                icon: 'warning',
                danger: !!raid?.detected,
                // The console acts on a community, so land in it first: the menu
                // is reachable from the rail, where you may be somewhere else
                // entirely, and closing the console should leave you there.
                onClick: () => {
                    const target = wsChannelForCommunity(cf.community_id) || chat.id;
                    if (target && !chatOnScreen(target)) openChat(target);
                    openModerationPanel(cf.community_id);
                },
            });
        }
    }
    if (cf.community_id) items.push(...copyLinkItems('Community', { community: cf.community_id }));
    items.push(...copyIdItems('Community', cf.community_id));
    items.push({ divider: true });
    // Owner or member, the same entry point decides which flow it is — and both
    // ask before doing anything.
    items.push({
        label: cf.is_owner === 'true' ? 'Delete Community' : 'Leave Community',
        icon: 'x-user',
        danger: true,
        onClick: () => communityLeaveOrDelete(chat),
    });

    showContextMenu({ x: anchor.x, y: anchor.y, items });
}

/**
 * A channel's own menu. Reachable by right-click or long-press from either
 * shape of the channel list; there is no affordance in the row itself, which
 * stays navigation.
 */
async function openChannelMenu(communityId, channel, x, y) {
    const chat = arrChats.find(c => c.id === channel.id);
    const items = [];
    if (chat && computeRowUnreadCount(chat) > 0) {
        items.push({
            label: 'Mark as Read',
            icon: 'check',
            onClick: () => { markChatCaughtUp(chat, /* explicit */ true); chatChanged(chat); },
        });
        items.push({ divider: true });
    }
    items.push(...await notifyMenuItems(channel.id, communityId));
    if (communityCanAddChannels(communityId)) {
        items.push({ divider: true });
        items.push({ label: 'Edit Channel', icon: 'edit', onClick: () => openChannelEditorFor(communityId, channel) });
    }
    items.push(...copyLinkItems('Channel', { community: communityId, channel: channel.id }));
    items.push(...copyIdItems('Channel', channel.id));
    // The primary channel anchors the community's row and history; the backend refuses to tombstone it.
    const fPrimary = arrChats.some(c => c.id === channel.id && c.metadata?.custom_fields?.primary_channel === channel.id);
    if (communityCanAddChannels(communityId) && !fPrimary) {
        items.push({ divider: true });
        items.push({
            label: 'Delete Channel',
            icon: 'x',
            danger: true,
            onClick: () => promptDeleteChannel(communityId, channel),
        });
    }
    showContextMenu({ x, y, items });
}

/** The section "+": a text channel or a voice one. */
function openCreateChannelMenu(communityId, isPrivate, categoryId, x, y) {
    showContextMenu({ x, y, items: [
        { label: isPrivate ? 'Private Text Channel' : 'Text Channel', icon: 'channel-hash', onClick: () => promptCreateChannel(communityId, isPrivate, categoryId) },
        { label: isPrivate ? 'Private Voice Channel' : 'Voice Channel', icon: 'volume-max', onClick: () => promptCreateChannel(communityId, isPrivate, categoryId, true) },
    ] });
}

async function promptCreateChannel(communityId, isPrivate = false, categoryId = null, voice = false) {
    const title = voice ? (isPrivate ? 'New private voice channel' : 'New voice channel') : (isPrivate ? 'New private channel' : 'New channel');
    const about = voice
        ? (isPrivate ? 'Only members you let in can join its calls and read its chat.' : 'Anyone in the community can hop into its call, with a chat beside it.')
        : (isPrivate ? 'Only members you grant access to can read it.' : 'Everyone in the community can read and post in it.');
    const name = await popupConfirm(title, about, false, 'channel name');
    if (!name || !String(name).trim()) return;
    try {
        const channelId = await invoke('create_community_channel', { communityId, name, private: isPrivate, categoryId, voice });
        refreshCommunityChannels();
        expandedCommunities.add(communityId);
        openCommunityChannel(communityId, { id: channelId, name, view: voice ? 'voice' : 'chat' });
    } catch (e) {
        await popupConfirm("Couldn't add the channel", escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

async function promptDeleteChannel(communityId, channel) {
    const ok = await popupConfirm('Delete channel',
        `Delete <b>${channel.view === 'voice' ? '' : '#'}${escapeHtml(channel.name)}</b>? Everyone loses access to it. Messages already on this device are kept.`,
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
    // A voice channel's stage shows above its chat while it's on screen.
    if (channel.view === 'voice') {
        VectorSvelte.setVoiceStage({ communityId, channelId: channel.id, name: channel.name });
        refreshVoicePresent(channel.id);
    }
    if (chatOnScreen(channel.id)) return;
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
    if (wsActive() && communityIdOfChat(chat)) {
        return cf.channel_name || name;
    }
    if (isPrimaryChannelChat(chat) || !cf.channel_name) return name;
    const voice = (getCommunityChannels(communityIdOfChat(chat)) || []).some((c) => c.id === chat.id && c.view === 'voice');
    return `${name} › ${voice ? '' : '#'}${cf.channel_name}`;
}
