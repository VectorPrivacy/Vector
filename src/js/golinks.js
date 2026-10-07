// Vector links: vectorapp.io/go#… names a community, a channel or a message by the ids its
// members already hold (the grammar is vector-core's golink.rs). The ids ride the fragment, so no
// server sees them, and a link resolves only against this device's own chats, never the network.

const GO_LINK_BASE = 'https://vectorapp.io/go#';
const GO_ID = '[0-9a-fA-F]{64}';
/** The three forms a link arrives in: the shareable one, the app scheme, Vector Web's own. */
const GO_LINK_RE = new RegExp(
    `(?:https?://(?:www\\.)?vectorapp\\.io/go/?#|vector://go/?#|https://web\\.vectorapp\\.io/#go/)`
    + `((?:c/${GO_ID}(?:/${GO_ID}){0,2}|m/${GO_ID}))(?![0-9a-zA-Z/])`,
    'g',
);

/** Anything wearing a Vector link's address: what's left of it after the valid ones is broken. */
const GO_LINK_ANY_RE = /(?:https?:\/\/(?:www\.)?vectorapp\.io\/go\b\/?|vector:\/\/go\b\/?|https:\/\/web\.vectorapp\.io\/#go\/)[^\s<>"]*/g;

/** `c/<community>[/<channel>[/<message>]]` or `m/<message>`, as its parts; null when malformed. */
function parseGoPayload(payload) {
    const parts = String(payload || '').toLowerCase().replace(/\/+$/, '').split('/');
    const isId = (s) => /^[0-9a-f]{64}$/.test(s || '');
    if (parts[0] === 'm' && parts.length === 2 && isId(parts[1])) {
        return { community: null, channel: null, message: parts[1] };
    }
    if (parts[0] === 'c' && parts.length >= 2 && parts.length <= 4 && parts.slice(1).every(isId)) {
        return { community: parts[1], channel: parts[2] || null, message: parts[3] || null };
    }
    return null;
}

/** A whole URL as a link; punctuation a sentence left on the end doesn't make it broken. */
function parseGoLink(url) {
    const s = String(url || '').trim();
    GO_LINK_RE.lastIndex = 0;
    const m = GO_LINK_RE.exec(s);
    return m && m.index === 0 && /^[.,;:!?)\]'"]*$/.test(s.slice(m[0].length)) ? parseGoPayload(m[1]) : null;
}

function goLinkUrl({ community, channel, message }) {
    if (!community) return `${GO_LINK_BASE}m/${message}`;
    return GO_LINK_BASE + ['c', community, channel, message].filter(Boolean).join('/');
}

/** A message's link: a community message names its room, a DM message only itself. */
function goLinkForMessage(chatId, messageId) {
    const community = communityIdOfChat(arrChats.find(c => c.id === chatId));
    return community ? { community, channel: chatId, message: messageId } : { community: null, channel: null, message: messageId };
}

/** A menu's Copy Link entry. Links are for everyone; raw ids are Advanced Mode's. */
function copyLinkItems(what, link) {
    if (!link) return [];
    return [{
        label: 'Copy Link',
        icon: 'share',
        onClick: () => {
            navigator.clipboard.writeText(goLinkUrl(link))
                .then(() => showToast(`Copied ${what} Link`))
                .catch(() => showToast('Failed to Copy'));
        },
    }];
}

/**
 * What this device can say about a link, synchronously, from the chats and community documents
 * it holds. `state` is ok | unknown | locked | deleted, or pending for a message the database has
 * not been asked about yet (see `settleGoMessage`).
 */
function resolveGoLink(link) {
    if (!link.community) return { kind: 'dm-message', state: 'pending' };
    const rooms = arrChats.filter(c => communityIdOfChat(c) === link.community);
    if (!rooms.length) return { kind: 'community', state: 'unknown' };
    const communityName = rooms[0].metadata?.custom_fields?.name || 'Community';
    if (!link.channel) return { kind: 'community', state: 'ok', communityName };
    // The community document decides which channels exist and which we may read; a chat row
    // outlives both a delete and a revoke.
    const locked = communityLockedChannels.get(link.community)?.find(c => c.id === link.channel);
    if (locked) return { kind: 'channel', state: 'locked', communityName, channelName: locked.name || 'private channel' };
    const room = rooms.find(c => c.id === link.channel);
    const listed = communityChannelsCache.get(link.community);
    const channelName = room?.metadata?.custom_fields?.channel_name || 'channel';
    if (!room) return { kind: 'channel', state: 'unknown', communityName };
    if (listed && !listed.some(c => c.id === link.channel)) return { kind: 'channel', state: 'deleted', communityName, channelName };
    const kind = link.message ? 'message' : 'channel';
    return { kind, state: kind === 'message' ? 'pending' : 'ok', communityName, channelName, chatId: room.id };
}

/** Where a message is on this device: `{ chat, deleted }`. */
function locateGoMessage(messageId) {
    return invoke('locate_message', { messageId }).catch(() => ({ chat: null, deleted: false }));
}

/** A message link's state once the database has answered. A link naming a room counts only there. */
async function settleGoMessage(link, r) {
    const where = await locateGoMessage(link.message);
    if (r.kind === 'message') {
        const state = where.chat === r.chatId ? 'ok' : where.deleted ? 'deleted' : 'unsynced';
        return { ...r, state };
    }
    // Named only by its id: said in a community after all, the pill names that room.
    const community = communityIdOfChat(arrChats.find(c => c.id === where.chat));
    if (community) return { ...resolveGoLink({ community, channel: where.chat, message: link.message }), state: 'ok' };
    return { kind: 'dm-message', state: where.chat ? 'ok' : where.deleted ? 'deleted' : 'unsynced', chatId: where.chat };
}

/** Why a link can't be followed, in the words a click answers with. */
function goLinkRefusal(r) {
    switch (r.kind) {
        case 'broken': return 'This link is broken or incomplete';
        case 'community': return "You're not in this community";
        case 'channel':
            if (r.state === 'locked') return `You don't have access to #${r.channelName}`;
            if (r.state === 'deleted') return `#${r.channelName} was deleted`;
            return "That channel isn't available to you";
        default:
            if (r.state === 'deleted') return 'That message was deleted';
            return r.kind === 'message' ? "That message isn't on this device yet" : "That message isn't on this device";
    }
}

/** Go where a link points, or say why it can't. Every answer comes from this device. */
async function openGoLink(link) {
    let r = resolveGoLink(link);
    if (r.state === 'pending') r = await settleGoMessage(link, r);
    if (r.state !== 'ok') {
        // A message that hasn't synced still has a room to show.
        if (r.kind === 'message' && r.state === 'unsynced') await openChat(r.chatId);
        showToast(goLinkRefusal(r));
        return;
    }
    if (r.kind === 'community') {
        const target = wsChannelForCommunity(link.community);
        if (target) await openChat(target);
        return;
    }
    if (r.kind === 'channel') {
        await openChat(r.chatId);
        return;
    }
    const chatId = r.chatId;
    await openChat(chatId);
    jumpToMessage(link.message);
    // Stored but never drawn: say so, not nothing. Having appeared once is success; the user is
    // free to scroll it away straight after.
    const deadline = Date.now() + 2500;
    const watch = () => {
        if (document.getElementById(link.message) || strOpenChat !== chatId) return;
        if (Date.now() < deadline) setTimeout(watch, 100);
        else showToast("That message can't be shown");
    };
    watch();
}

function _goIcon(name) {
    const i = document.createElement('span');
    i.className = `icon icon-${name} go-link-icon`;
    return i;
}

function _goText(text, cls) {
    const s = document.createElement('span');
    s.className = cls;
    s.textContent = text;
    return s;
}

const GO_MESSAGE_TITLES = { deleted: 'Deleted message', unsynced: 'Not on this device yet' };

/** The community's logo as a small round icon, or the placeholder logo-less communities wear. */
function _goCommunityIcon(communityId) {
    const withIcon = arrChats.find(c => communityIdOfChat(c) === communityId && c.metadata?.avatar_cached);
    const img = document.createElement('img');
    img.className = 'go-link-avatar';
    img.alt = '';
    img.src = withIcon ? convertFileSrc(withIcon.metadata.avatar_cached) : 'icons/group-placeholder.svg';
    img.onerror = () => { img.onerror = null; img.src = 'icons/group-placeholder.svg'; };
    return img;
}

/** Fill a pill from what this device knows; the link's own text never decides what it says. */
function _paintGoPill(pill, r, hereCommunity) {
    pill.replaceChildren();
    const dim = r.state !== 'ok' && r.state !== 'pending';
    pill.classList.toggle('is-unknown', dim);
    pill.classList.toggle('is-gone', r.state === 'deleted');
    const sep = () => _goText('›', 'go-link-sep');
    const room = () => {
        if (r.communityName && pill.dataset.community !== hereCommunity) {
            pill.append(_goCommunityIcon(pill.dataset.community), _goText(r.communityName, 'go-link-name go-link-community'), sep());
        }
    };
    switch (r.kind) {
        case 'broken':
            pill.append(_goIcon('warning'), _goText('Broken link', 'go-link-name'));
            pill.title = goLinkRefusal(r);
            return;
        case 'community':
            pill.append(r.state === 'ok' ? _goCommunityIcon(pill.dataset.community) : _goIcon('users-multi'),
                _goText(r.state === 'ok' ? r.communityName : 'Unknown community', 'go-link-name'));
            pill.title = r.state === 'ok' ? r.communityName : "A community you're not in";
            return;
        case 'channel':
            if (r.state === 'unknown') {
                room();
                pill.append(_goIcon('channel-hash'), _goText('Unknown channel', 'go-link-name'));
                pill.title = "A channel that isn't available to you";
                return;
            }
            room();
            pill.append(_goIcon(r.state === 'locked' ? 'locked' : 'channel-hash'), _goText(r.channelName, 'go-link-name go-link-room'));
            pill.title = r.state === 'locked' ? `Private channel in ${r.communityName}`
                : r.state === 'deleted' ? `Deleted channel in ${r.communityName}`
                : `#${r.channelName} in ${r.communityName}`;
            return;
        case 'message':
            room();
            pill.append(_goIcon('channel-hash'), _goText(r.channelName, 'go-link-name'), sep(), _goIcon('chat-bubble'));
            if (r.state === 'deleted') pill.append(_goText('deleted', 'go-link-name'));
            pill.title = GO_MESSAGE_TITLES[r.state] || `Message in #${r.channelName}, ${r.communityName}`;
            return;
        default: {
            if (r.state === 'ok' && r.chatId) {
                const name = getName(getProfile(r.chatId) || r.chatId);
                pill.append(_goText('@' + name, 'go-link-name'), sep(), _goIcon('chat-bubble'));
                pill.title = `Message with ${name}`;
                return;
            }
            pill.append(_goIcon('chat-bubble'), _goText(r.state === 'deleted' ? 'Deleted message' : 'Message', 'go-link-name'));
            pill.title = GO_MESSAGE_TITLES[r.state] || '';
        }
    }
}

function _buildGoPill(link, hereCommunity) {
    const pill = document.createElement('span');
    pill.className = 'mention go-link';
    pill.setAttribute('role', 'link');
    pill.tabIndex = 0;
    if (link?.community) pill.dataset.community = link.community;
    const r = link ? resolveGoLink(link) : { kind: 'broken', state: 'broken' };
    _paintGoPill(pill, r, hereCommunity);
    // A message's state is the database's to say: paint what is known now, settle after.
    if (r.state === 'pending') {
        settleGoMessage(link, r).then((settled) => {
            if (!pill.isConnected) return;
            if (settled.communityName && !pill.dataset.community) pill.dataset.community = communityIdOfChat(arrChats.find(c => c.id === settled.chatId)) || '';
            _paintGoPill(pill, settled, hereCommunity);
        });
    }
    const open = (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (link) openGoLink(link);
        else showToast(goLinkRefusal(r));
    };
    pill.addEventListener('click', open);
    pill.addEventListener('keydown', (e) => { if (e.key === 'Enter') open(e); });
    return pill;
}

/** Replace each match of `re` in the element's loose text (not code, links or pills) with `build(match)`. */
function _goReplaceText(element, re, build) {
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT, {
        acceptNode(node) {
            for (let p = node.parentElement; p && p !== element; p = p.parentElement) {
                if (p.tagName === 'A' || p.tagName === 'CODE' || p.tagName === 'PRE' || p.classList.contains('mention')) {
                    return NodeFilter.FILTER_REJECT;
                }
            }
            return NodeFilter.FILTER_ACCEPT;
        },
    });
    const nodes = [];
    while (walker.nextNode()) nodes.push(walker.currentNode);
    for (const node of nodes) {
        const text = node.textContent;
        re.lastIndex = 0;
        if (!re.test(text)) continue;
        re.lastIndex = 0;
        const frag = document.createDocumentFragment();
        let last = 0;
        let m;
        while ((m = re.exec(text)) !== null) {
            if (m.index > last) frag.appendChild(document.createTextNode(text.slice(last, m.index)));
            frag.appendChild(build(m));
            last = m.index + m[0].length;
        }
        if (last < text.length) frag.appendChild(document.createTextNode(text.slice(last)));
        node.parentNode.replaceChild(frag, node);
    }
}

/**
 * Turn Vector links in rendered text into pills: bare ones in the text, and anchors the
 * linkifier or markdown already made, whatever their label claimed. One that doesn't parse
 * becomes a Broken link pill rather than a trip to the website.
 */
function renderGoLinks(element) {
    const hereCommunity = communityIdOfChat(arrChats.find(c => c.id === strOpenChat));
    for (const a of element.querySelectorAll('a')) {
        const href = a.getAttribute('href') || '';
        GO_LINK_ANY_RE.lastIndex = 0;
        if (!GO_LINK_ANY_RE.test(href)) continue;
        a.replaceWith(_buildGoPill(parseGoLink(href), hereCommunity));
    }
    _goReplaceText(element, GO_LINK_RE, (m) => _buildGoPill(parseGoPayload(m[1]), hereCommunity));
    _goReplaceText(element, GO_LINK_ANY_RE, () => _buildGoPill(null, hereCommunity));
}
