/**
 * Discord-style message row renderer.
 *
 * Flat row layout: avatar + author + timestamp header on the left, content
 * underneath, reactions row beneath, single floating hover toolbar.
 *
 * The reaction chip class `reaction` is kept (rather than renamed `dmsg-reaction`)
 * because the global click handler in main.js dispatches on `.reaction` for
 * toggle-reaction behaviour — keeping the class avoids wider refactor.
 */

// Per-message-id dedupe set for the "no metadata yet → ask backend" probe.
// Reset implicitly on chat switch via openChat clearing the chat-messages tree;
// the set is allowed to grow across chats (an msg id is unique to its event).
const _dmsgPreviewFetchedIds = new Set();

// Unique-emoji ceiling for a message's reaction row. At this count the "+"
// add-reaction shortcut is dropped (no more can be shown), and the reaction
// picker's shift multi-react auto-closes. Mirrors vector-core's
// MAX_REACTION_GROUPS — the backend refuses groups past it.
const MAX_DISPLAYED_REACTIONS = 12;

/** The mounted list island, one per page life (re-mounted if its target was replaced). */
let _dmsgListIsland = null;
function ensureMessageList() {
    if (!domChatMessages) return false;
    if (_dmsgListIsland && _dmsgListIsland._target === domChatMessages) return true;
    _dmsgListIsland = VectorSvelte.mountMessageList(domChatMessages, { h: _dmsgListHelpers });
    _dmsgListIsland._target = domChatMessages;
    // The mount cleared the container; the toolbar is re-created on demand.
    initMessageToolbar();
    return true;
}

/** Everything the list island derives from and hands to its rows. */
/**
 * ListHelpers: the message list island. `row` is RowHelpers for every row it renders.
 * @typedef {Object} ListHelpers
 * @property {(chatId: string) => object[]} messages
 * @property {{ collapse: (prev: object, curr: object) => boolean, differentDay: (a: object, b: object) => boolean, isCommand: (m: object) => boolean, mergeable: (type: string) => boolean }} rules
 * @property {(at: number) => string} dayLabel
 * @property {number} maxRows
 * @property {(msg: object) => object|null} senderFor
 * @property {(msg: object) => object} ctxFor
 * @property {RowHelpers} row
 */
const _dmsgListHelpers = {
    // The chat's own array is the complete one: the cache entry can be a fresh
    // post-eviction stub holding only the newest arrival.
    messages: (chatId) => arrChats.find(c => c.id === chatId)?.messages || eventCache.getEventsRef(chatId) || [],
    rules: {
        collapse: (prev, curr) => shouldCollapseStreak(prev, curr),
        differentDay: (a, b) => _dmsgIsDifferentDay(a, b),
        isCommand: (m) => !!_dmsgCommandInfo(m),
        mergeable: (t) => MERGEABLE_SYSTEM_EVENTS.has(t),
    },
    dayLabel: (at) => dayDividerHtml(at),
    get maxRows() { return typeof MAX_WINDOW_ROWS === 'number' ? MAX_WINDOW_ROWS : 80; },
    senderFor: (msg) => {
        if (msg.mine) return getProfile(strPubkey) || null;
        const chat = arrChats.find(c => c.id === strOpenChat);
        return chatIsGroup(chat) ? (msg.npub ? getProfile(msg.npub) : null) : getProfile(chat?.id);
    },
    // Render-time facts about a row, read once per render: cached per message object
    // so a re-derive of 80 rows costs 80 lookups,
    // not 80 scans. A changed message is a new object and gets a fresh context.
    ctxFor: (msg) => {
        const ctx = _dmsgRowCtxCache.get(msg);
        // Two inputs change under a mounted row: the author's block state and a reveal.
        // Everything else in the context is fixed per message and chat.
        if (ctx && ctx.currentChat?.id === strOpenChat) {
            const revealed = revealedBlockedMessages.has(msg.id);
            const isBlocked = _dmsgAuthorBlocked(msg, ctx.currentChat, ctx.isGroupChat);
            if (ctx.blocked === (isBlocked && !revealed) && ctx.revealedBlocked === (isBlocked && revealed)) return ctx;
        }
        const fresh = _dmsgRowCtx(msg);
        _dmsgRowCtxCache.set(msg, fresh);
        return fresh;
    },
    get row() { return _dmsgRowHelpers; },
};
const _dmsgRowCtxCache = new WeakMap();
/** Whether the row's author is blocked: only a group's other members can be. */
function _dmsgAuthorBlocked(msg, currentChat, isGroupChat) {
    const otherFullId = msg.npub || (!isGroupChat ? currentChat?.id : '') || '';
    const p = isGroupChat && !msg.mine && otherFullId ? getProfile(otherFullId) : null;
    return !!p?.is_blocked;
}
function _dmsgRowCtx(msg) {
    {
        const currentChat = arrChats.find(c => c.id === strOpenChat);
        const isGroupChat = chatIsGroup(currentChat);
        const blocked = _dmsgAuthorBlocked(msg, currentChat, isGroupChat);
        return {
            myNpub: strPubkey,
            isGroupChat,
            currentChat,
            pinged: _dmsgIsPinged(msg, currentChat, isGroupChat),
            blocked: blocked && !revealedBlockedMessages.has(msg.id),
            revealedBlocked: blocked && revealedBlockedMessages.has(msg.id),
        };
    }
}

/**
 * Update a rendered row to `msg` in place: the row re-derives its shell and refills
 * its content on the same element, so the toolbar target, jump highlight, streak
 * state and scroll position all survive.
 */
function updateMessageRow(msg, oldId = '') {
    // The list derives from the array: make sure it holds `msg`, then re-derive. Same
    // id → the row's prop changes and it refills in place; a new id (pending → sent)
    // is a new keyed row, as a replace was.
    ensureMessageList();
    const msgs = _dmsgListHelpers.messages(strOpenChat);
    const idx = msgs.findIndex(m => m === msg || m.id === msg.id || (oldId && m.id === oldId));
    if (idx !== -1 && msgs[idx] !== msg) msgs[idx] = msg;
    // An id swap must carry the window anchor with it.
    if (oldId && oldId !== msg.id) {
        if (windowTopId === oldId) windowTopId = msg.id;
        if (windowBottomId === oldId) windowBottomId = msg.id;
        VectorSvelte.setWindow(strOpenChat, windowTopId, windowBottomId);
    }
    VectorSvelte.touchMessage(msg.id);
    // A row keeps its key across an id swap and its version signal was read under the
    // old id; touching that entry too is what re-derives it under the new one.
    if (oldId && oldId !== msg.id) VectorSvelte.touchMessage(oldId);
    VectorSvelte.touchWindow();
    VectorSvelte.flushSync();
}

/** Helpers the row island calls; the leaf builders stay here. */
// The row family's helpers, one bag per consumer so a leaf can only reach what it uses.
// The few lookups several bags need are repeated by reference. Each bag's contract is the
// typedef above it; a component names the typedef it takes.
//
// @typedef {Object} RowLookups   shared by every bag that lists them
// @property {(npub: string) => object|null} getProfile
// @property {(profileOrNpub: object|string) => string} getName
// @property {(profile: object|null) => string|null} getProfileAvatarSrc
// @property {() => string} myNpub
// @property {(el: Element) => void} twemojify
// @property {(text: string, el: Element) => void} showTooltip
// @property {() => void} hideTooltip
// @property {() => boolean} isAndroid
// @property {(n: number, dec?: number, short?: boolean) => string} formatBytes
// @property {() => string} openChat            the open chat's id
// @property {(url: string) => void} openUrl     with the link-spoof guard
// @property {(img: HTMLImageElement, url: string) => void} backendCachedImg
// @property {() => void} onThumbLoad           a thumbnail landed: keep the scroll anchored
/** The subset of `obj` under `keys`, by reference. */
function _dmsgPick(obj, keys) { return Object.fromEntries(keys.map((k) => [k, obj[k]])); }

const _dmsgLookups = {
    getProfile: (npub) => getProfile(npub),
    getName: (x) => getName(x),
    getProfileAvatarSrc: (p) => getProfileAvatarSrc(p),
    myNpub: () => strPubkey,
    twemojify: (el) => twemojify(el),
    showTooltip: (text, el) => showGlobalTooltip(text, el),
    hideTooltip: () => hideGlobalTooltip(),
    isAndroid: () => platformFeatures.os === 'android',
    formatBytes: (n, dec, short) => formatBytes(n, dec, short),
    openChat: () => strOpenChat,
    openUrl: (url) => _dmsgOpenPreviewUrl(url),
    backendCachedImg: (img, url) => bindBackendCachedImg(img, url),
    onThumbLoad: () => { if (proceduralScrollState.isLoadingOlderMessages) correctScrollForMediaLoad(); else softChatScroll(); },
};

/**
 * ContentHelpers: MessageContent and its leaves (CommandLine, CryptoAddress, InviteCard, LinkPreview).
 * @typedef {Pick<RowLookups, 'getProfile'|'getName'|'getProfileAvatarSrc'|'myNpub'|'isAndroid'|'openChat'|'openUrl'|'backendCachedImg'|'onThumbLoad'> & {
 *   buildText: (msg: object, ctx: object) => HTMLElement|null,
 *   commandInfo: (msg: object) => object|null,
 *   cryptoAddress: (msg: object) => object|null,
 *   renderEmojiPackPreviews: (node: Element, text: string) => void,
 *   destroyEmojiPackPreviews: (node: Element) => void,
 *   inviteKeys: (text: string) => string[],
 *   invitePreviewSettled: (key: string) => boolean,
 *   resolveInvite: (key: string) => Promise<void>,
 *   joinedChat: (communityId: string) => object|null,
 *   joinFromCard: (key: string, communityId: string) => void,
 *   fileSrc: (path: string) => string,
 *   showEditHistory: (msgId: string, el: Element) => void,
 *   xdcUrl: (msg: object) => string|null,
 *   renderXdcUrlCard: (node: Element, msg: object, url: string) => void,
 *   webPreviewsEnabled: () => boolean,
 *   linkPreviewData: (msg: object) => object|null,
 *   fmtCountdown: (secs: number) => string,
 *   selfDestructTooltip: (el: Element) => void,
 *   selfDestructTooltipEnd: () => void,
 * }} ContentHelpers
 */
const _dmsgContentHelpers = {
    ..._dmsgPick(_dmsgLookups, ['getProfile', 'getName', 'getProfileAvatarSrc', 'myNpub', 'isAndroid', 'openChat', 'openUrl', 'backendCachedImg', 'onThumbLoad']),
    buildText: (msg, ctx) => _dmsgTextLeaf(msg, ctx),
    commandInfo: (msg) => _dmsgCommandInfo(msg),
    cryptoAddress: (msg) => detectCryptoAddress(msg.content),
    renderEmojiPackPreviews: (node, text) => renderEmojiPackPreviews(node, text),
    destroyEmojiPackPreviews: (node) => destroyEmojiPackPreviews(node),
    inviteKeys: (text) => communityInviteKeys(text),
    invitePreviewSettled: (key) => { const c = _invitePreviewCache.get(key); return !!(c && c.state !== 'loading'); },
    resolveInvite: (key) => _resolveCommunityInvitePreview(key),
    joinedChat: (communityId) => findCommunityChat(communityId),
    joinFromCard: (key, communityId) => _joinCommunityFromCard(key, communityId),
    fileSrc: (path) => convertFileSrc(path),
    showEditHistory: (id, el) => showEditHistory(id, el),
    xdcUrl: (msg) => findXdcUrl(msg.content),
    renderXdcUrlCard: (node, msg, url) => renderXdcUrlCard(node, msg, url),
    webPreviewsEnabled: () => !!fWebPreviewsEnabled,
    linkPreviewData: (msg) => _dmsgLinkPreviewData(msg),
    fmtCountdown: (secs) => _fmtCountdown(secs),
    selfDestructTooltip: (el) => _selfDestructTooltip(el),
    selfDestructTooltipEnd: () => _selfDestructTooltipEnd(),
};

/**
 * MediaHelpers: Attachments and its leaves (Image, Video, Thumbhash, FileBox, UploadOverlay);
 * `audio` is AudioPlayer's own bag.
 * @typedef {Pick<RowLookups, 'getProfile'|'getProfileAvatarSrc'|'showTooltip'|'hideTooltip'|'formatBytes'|'openChat'|'backendCachedImg'|'onThumbLoad'> & {
 *   isImage: (ext: string) => boolean, isAudio: (ext: string) => boolean, isVideo: (ext: string) => boolean,
 *   isDownloading: (att: object) => boolean,
 *   willAutoDownload: (att: object, ctx: object) => boolean,
 *   autoDownload: (att: object, msg: object, sender: object|null) => void,
 *   startDownload: (att: object, msg: object, sender: object|null) => void,
 *   audio: object,
 *   fileTypeInfo: (ext: string) => object,
 *   loadMiniAppInfo: (path: string) => Promise<object|null>,
 *   marketplaceApp: (hash: string) => Promise<object|null>,
 *   openFile: (att: object, msg: object) => void,
 *   assetUrl: (path: string) => string, mediaUrl: (path: string) => string,
 *   isSpoiler: (att: object) => boolean,
 *   thumbhash: (chatId: string, msgId: string) => Promise<string|null>,
 *   onImageLoad: () => void, onVideoMeta: (video: HTMLVideoElement) => void,
 *   attachImagePreview: (img: HTMLImageElement) => void,
 *   attachFileExtBadge: (img: HTMLImageElement, container: Element, ext: string) => void,
 *   cancelUpload: (pendingId: string) => Promise<void>,
 * }} MediaHelpers
 */
const _dmsgMediaHelpers = {
    ..._dmsgPick(_dmsgLookups, ['getProfile', 'getProfileAvatarSrc', 'showTooltip', 'hideTooltip', 'formatBytes', 'openChat', 'backendCachedImg', 'onThumbLoad']),
    isImage: (ext) => ['png', 'jpeg', 'jpg', 'gif', 'webp', 'svg', 'bmp', 'tiff', 'tif', 'ico'].includes(ext),
    isAudio: (ext) => ['wav', 'mp3', 'flac', 'aac', 'm4a', 'ogg'].includes(ext),
    isVideo: (ext) => platformFeatures.os !== 'linux' && ['mp4', 'webm', 'mov'].includes(ext),
    isDownloading: (att) => !!att.downloading || downloadingAttachmentIds.has(att.id),
    willAutoDownload: (att, ctx) => AUTO_DOWNLOAD_ENABLED && !ctx.revealedBlocked && att.size > 0
        && att.size <= MAX_AUTO_DOWNLOAD_BYTES && !att.download_failed,
    // Once per attachment id across renders, or every repaint would re-fire the download.
    autoDownload: (att, msg) => _dmsgStartDownload(att, msg),
    startDownload: (att, msg) => _dmsgStartDownload(att, msg),
    get audio() { return AUDIO_PLAYER_HELPERS; },
    fileTypeInfo: (ext) => getFileTypeInfo(ext),
    loadMiniAppInfo: (path) => loadMiniAppInfo(path),
    marketplaceApp: (hash) => invoke('marketplace_get_app_by_hash', { fileHash: hash }),
    openFile: (att, msg) => _dmsgOpenFile(att, msg),
    assetUrl: (path) => convertFileSrc(path),
    mediaUrl: (path) => mediaUrl(path),
    isSpoiler: (att) => isSpoilerAttachment(att),
    thumbhash: (chatId, msgId) => invoke('generate_thumbhash_preview', { npub: chatId, msgId }),
    onImageLoad: () => compensateChatScrollForResize(),
    onVideoMeta: (video) => { if (!video.isConnected) return; video.currentTime = 0.1; compensateChatScrollForResize(); },
    attachImagePreview: (img) => attachImagePreview(img),
    attachFileExtBadge: (img, container, ext) => attachFileExtBadge(img, container, ext),
    cancelUpload: (pendingId) => invoke('cancel_upload', { pendingId }),
};

/**
 * RowHelpers: MessageRow's own chrome, plus ReplyQuote, ReactionChip and SystemEvent.
 * Carries the other bags for the row to hand down.
 * @typedef {Pick<RowLookups, 'getProfile'|'getName'|'getProfileAvatarSrc'|'myNpub'|'twemojify'|'showTooltip'|'hideTooltip'> & {
 *   content: ContentHelpers, media: MediaHelpers,
 *   pivx: { fiat: (amt: number) => string, ensure: (pay: object, mine: boolean) => void, claim: (code: string) => void },
 *   formatHourMinute: (at: number) => string,
 *   contentSig: (msg: object) => string,
 *   replyView: (msg: object, sender: object|null) => object|null,
 *   jumpToMessage: (id: string) => void,
 *   revealBlocked: (msg: object) => void,
 *   renderCustomEmojiShortcodes: (el: Element, tags: object[]|null) => void,
 *   showMiniProfile: (npub: string, el: Element) => void,
 *   systemEventName: (npub: string) => string, systemEventSuffix: (type: string) => string,
 *   reactionGroups: (msg: object) => Array<{ emoji: string, count: number, mine: boolean, url?: string }>,
 *   canAddReactionGroup: (msg: object, n: number) => boolean,
 *   reactionClick: (msgId: string, emoji: string) => void,
 *   customEmojiUrl: (emoji: string, url: string|null) => string|null,
 *   bindCachedImg: (img: HTMLImageElement, url: string, onUnavailable?: () => void) => void,
 *   reducedMotion: () => boolean,
 *   reactionChipRemoved: () => void,
 * }} RowHelpers
 */
const _dmsgRowHelpers = {
    ..._dmsgPick(_dmsgLookups, ['getProfile', 'getName', 'getProfileAvatarSrc', 'myNpub', 'twemojify', 'showTooltip', 'hideTooltip']),
    content: _dmsgContentHelpers,
    media: _dmsgMediaHelpers,
    pivx: { fiat: (amt) => pivxFiatLine(amt), ensure: (pay, mine) => pivxEnsureBubble(pay, mine), claim: (code) => claimPivxPayment(code) },
    formatHourMinute: (at) => _dmsgFormatHourMinute(at),
    contentSig: (msg) => _dmsgContentSig(msg),
    replyView: (msg, sender) => _dmsgReplyView(msg, sender),
    jumpToMessage: (id) => jumpToMessage(id),
    revealBlocked: (msg) => { revealedBlockedMessages.add(msg.id); openChat(strOpenChat); },
    renderCustomEmojiShortcodes: (el, tags) => renderCustomEmojiShortcodes(el, tags),
    showMiniProfile: (npub, el) => showMiniProfile(npub, el),
    systemEventName: (npub) => systemEventName(npub),
    systemEventSuffix: (type) => systemEventSuffix(type),
    // Reactions
    reactionGroups: (msg) => Array.from(_dmsgAggregateReactions(msg), ([emoji, g]) => ({ emoji, ...g })),
    canAddReactionGroup: (msg, n) => _dmsgCanAddReactionGroup(msg, n),
    reactionClick: (msgId, emoji) => _dmsgReactionClick(msgId, emoji),
    customEmojiUrl: (emoji, url) => _dmsgCustomEmojiUrl(emoji, url),
    // Reaction emoji bytes go through the Rust cache: a raw Blossom URL never lands on
    // an <img src>, so Tor traffic stays contained and repeat renders skip the network.
    bindCachedImg: (img, url, onUnavailable) => bindCachedEmojiImg(img, url, 'emoji', onUnavailable),
    reducedMotion: () => window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false,
    // A hover tip anchored to a chip that just left would float forever (mouseout
    // owns dismissal, and a removed anchor never fires it).
    reactionChipRemoved: () => { if (reactionHoverEl && !reactionHoverEl.isConnected) hideReactionHoverTip(); },
};

/** Whether a message pings the reader: a mention of them, an authorised @everyone, or a reply to their own message. */
function _dmsgIsPinged(msg, currentChat, isGroupChat) {
    if (msg.mine) return false;
    // msg.mentions_me() is a Rust method that does not survive IPC; mentions are
    // stamped as `@npub1...` in the content, so a substring match is reliable.
    const mentionedMe = strPubkey && msg.content && msg.content.includes('@' + strPubkey);
    const senderNpub = msg.npub || '';
    const senderIsAdmin = isGroupChat && (currentChat?.metadata?.admins?.includes(senderNpub)
        || currentChat?.metadata?.custom_fields?.owner_npub === senderNpub);
    const mentionedEveryone = senderIsAdmin && msg.content && /@everyone\b/.test(msg.content);
    // A reply to one of my own messages is an implicit ping. Prefer the in-memory
    // target's `mine` flag (DMs don't populate replied_to_npub); fall back to the
    // backend-resolved reply author for history not held in memory.
    let repliedToMe = false;
    if (msg.replied_to) {
        const tgt = currentChat?.messages?.find(m => m.id === msg.replied_to);
        repliedToMe = tgt ? !!tgt.mine : (msg.replied_to_npub === strPubkey);
    }
    return !!(mentionedMe || mentionedEveryone || repliedToMe);
}

/**
 * The text span for a message, or null when there is nothing to show. Decides the
 * jumbo emoji-only treatment: up to six graphemes, counting resolved `:shortcode:`
 * tokens as one each, and nothing else but whitespace.
 */
function _dmsgTextLeaf(msg, ctx) {
    const safeContent = msg.content || '';
    const emojiTagSet = (msg.emoji_tags && msg.emoji_tags.length)
        ? new Set(msg.emoji_tags.map(t => t.shortcode))
        : null;
    let customEmojiCount = 0;
    let strippedContent = safeContent;
    if (emojiTagSet) {
        strippedContent = safeContent.replace(/:([a-zA-Z0-9_~-]+):/g, (m, code) => {
            if (emojiTagSet.has(code)) {
                customEmojiCount++;
                return '';
            }
            return m;
        });
    }
    const strEmojiCleaned = strippedContent.replace(/\s/g, '');
    // Graphemes, not UTF-16 units: a fully-qualified ZWJ sequence is one visual emoji.
    let graphemeCount = customEmojiCount;
    if (strEmojiCleaned) {
        const seg = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
        for (const _ of seg.segment(strEmojiCleaned)) {
            if (++graphemeCount > 6) break;
        }
    }
    const remainderIsEmojiOnly = !strEmojiCleaned || isEmojiOnly(strEmojiCleaned);
    const fEmojiOnly = graphemeCount > 0 && graphemeCount <= 6 && remainderIsEmojiOnly;

    const textSpan = _dmsgBuildText(msg, safeContent, fEmojiOnly, ctx.isGroupChat, ctx.currentChat, ctx.revealedBlocked);
    if (!(textSpan && (textSpan.textContent || textSpan.querySelector('img,video,hr')))) return null;
    twemojify(textSpan);
    return textSpan;
}

// ----------------------------------------------------------------------------
// Sub-builders
// ----------------------------------------------------------------------------

let _sdTooltipTimer = null;
function _selfDestructTooltip(el) {
    const exp = parseInt(el.dataset.expiration, 10);
    if (!exp) return;
    const tick = () => {
        const remaining = exp - Math.floor(Date.now() / 1000);
        showGlobalTooltip(remaining > 0
            ? 'Message dissolves in ' + _fmtCountdown(remaining)
            : 'Dissolving...', el);
    };
    tick();
    if (_sdTooltipTimer) clearInterval(_sdTooltipTimer);
    _sdTooltipTimer = setInterval(tick, 1000);
}
function _selfDestructTooltipEnd() {
    if (_sdTooltipTimer) { clearInterval(_sdTooltipTimer); _sdTooltipTimer = null; }
    hideGlobalTooltip();
}

/** Format a countdown: mm:ss under an hour, else Hh Mm / Dd Hh. */
function _fmtCountdown(secs) {
    if (secs >= 86400) { const d = Math.floor(secs / 86400); const h = Math.floor((secs % 86400) / 3600); return d + 'd ' + h + 'h'; }
    if (secs >= 3600)  { const h = Math.floor(secs / 3600);  const m = Math.floor((secs % 3600) / 60);   return h + 'h ' + m + 'm'; }
    const m = Math.floor(secs / 60), s = secs % 60;
    return m + ':' + String(s).padStart(2, '0');
}

/**
 * The quoted parent of a reply as data for the row's ReplyQuote, or null when nothing
 * is known about it yet (neither the backend's context fields nor the parent in memory).
 */
function _dmsgReplyView(msg, sender) {
    const hasBackendContext = msg.replied_to_content !== undefined || msg.replied_to_has_attachment;
    // The quoted message lives in the chat being rendered. `sender` is the row's author,
    // so a DM lookup by their npub finds nothing in a Community.
    const chat = getChat(strOpenChat) || (sender ? getDMChat(sender.id) : undefined);
    const cMsg = chat?.messages.find(m => m.id === msg.replied_to);
    if (!hasBackendContext && !cMsg) return null;

    const mine = cMsg?.mine ?? (msg.replied_to_npub === strPubkey);
    // In DMs the backend leaves `replied_to_npub` empty, so `cMsg.mine` is the one signal
    // the target was me rather than the counterpart; otherwise the chat names them.
    let profile;
    if (mine) profile = getProfile(strPubkey);
    else if (msg.replied_to_npub) profile = getProfile(msg.replied_to_npub);
    else if (cMsg?.npub) profile = getProfile(cMsg.npub);
    else profile = (chat && !chatIsGroup(chat) ? getProfile(chat.id) : null) || sender;
    const npub = mine ? strPubkey : (msg.replied_to_npub || cMsg?.npub || profile?.id || '');

    let name = profile?.nickname || profile?.name || profile?.display_name;
    if (!name) {
        const fallbackId = (hasBackendContext ? msg.replied_to_npub : cMsg?.npub) || profile?.id || '';
        name = fallbackId ? fallbackId.substring(0, 10) + '…' : 'Unknown';
    }

    const content = hasBackendContext ? msg.replied_to_content : cMsg?.content;
    const hasAttachment = hasBackendContext ? msg.replied_to_has_attachment : cMsg?.attachments?.length > 0;
    let html = '';
    let emojiTags = [];
    let attachment = null;
    if (content) {
        html = buildReplyPreviewHtml(content);
        // The parent's tags can lose a hydration race on first paint, so the reader's own
        // equipped packs backstop them; the parent's come last so the sender's mapping wins.
        const msgTags = (cMsg?.emoji_tags?.length ? cMsg.emoji_tags : null) || msg.replied_to_emoji_tags || [];
        const equipped = equippedEmojiTags();
        emojiTags = [...equipped, ...msgTags];
    } else if (hasAttachment) {
        // The backend-resolved extension covers an off-screen parent (no cMsg then).
        const ext = (hasBackendContext ? msg.replied_to_attachment_extension : null) || cMsg?.attachments?.[0]?.extension;
        attachment = ext ? getFileTypeInfo(ext) : { icon: 'attachment', description: 'Attachment' };
    }
    return { parentId: msg.replied_to, mine, npub, name, avatarSrc: getProfileAvatarSrc(profile), html, emojiTags, attachment };
}

/**
 * The parent of pending replies has arrived: their quotes re-derive from its version.
 * A quote is a row that grew after layout, so the scroll compensator runs as for a
 * media load, only when a row was actually waiting.
 */
function backfillReplyContext(parentId) {
    if (!parentId) return;
    const waiting = document.querySelectorAll(`[data-reply-pending="${CSS.escape(parentId)}"]`).length;
    VectorSvelte.touchMessage(parentId);
    if (waiting) {
        VectorSvelte.flushSync();
        compensateChatScrollForResize();
    }
}


/**
 * Detect a slash-command invocation worth the passive render. Only when it
 * provably IS one: the bot routing tag is present, the message is a bare
 * /command with nothing after the name, or a bot in this chat declares that
 * command. An untagged "/word plus prose" whose word no bot declares stays
 * ordinary text, so real content can never be hidden by mistake. The declared
 * set is how a 1:1 DM, which sends invocations untagged, recognises its bot's.
 */
function _dmsgCommandInfo(msg) {
    const content = (msg.content || '').trim();
    const m = /^\/([a-z0-9_-]{1,32})(\s|$)/.exec(content);
    if (!m) return null;
    const tagged = msg.addressed_bots && msg.addressed_bots.length;
    if (!tagged && content !== '/' + m[1]) {
        const known = commandCtrl ? commandCtrl.commandNames(strOpenChat) : null;
        if (!known || !known.has(m[1])) return null;
    }
    return { name: m[1], botNpub: tagged ? msg.addressed_bots[0] : null };
}

function _dmsgBuildText(msg, displayContent, fEmojiOnly, isGroupChat, currentChat, isRevealedBlockedMsg) {
    const span = document.createElement('span');
    span.classList.add('dmsg-text');

    if (fEmojiOnly) {
        span.textContent = displayContent;
        span.style.whiteSpace = 'pre-wrap';
        span.classList.add('dmsg-emoji-only');
        // Custom-emoji shortcodes still need swapping to <img>; the jumbo
        // sizing rule (.dmsg-emoji-only .custom-emoji-inline) takes it from
        // there.
        if (msg.emoji_tags && msg.emoji_tags.length) {
            renderCustomEmojiShortcodes(span, msg.emoji_tags);
        }
        return span;
    }

    // NIP-19 naddrs for emoji packs are rendered as a preview card; strip
    // the bech32 string so it doesn't double up as a long unreadable line.
    let textBody = (displayContent || '').trim();
            textBody = stripEmojiPackNaddrs(textBody);
    // Community invite links likewise render as their own card.
            textBody = stripCommunityInviteUrls(textBody);
    // Defensive: displayContent can be null/undefined for attachment-only messages.
    span.innerHTML = parseMarkdown(textBody);
    linkifyUrls(span);
    if (!isRevealedBlockedMsg) processInlineImages(span);

    const senderNpub = msg.mine ? strPubkey : (msg.npub || '');
    const senderIsAdmin = isGroupChat && (currentChat?.metadata?.admins?.includes(senderNpub)
        || currentChat?.metadata?.custom_fields?.owner_npub === senderNpub);
    // Bare and nostr:-prefixed npubs (and vectorapp.io profile links) render
    // as mention pills, same treatment as bios.
    renderMentions(span, senderIsAdmin, { allowBare: true, queueSync: true });

    // NIP-30 custom emojis ride along on the rumor; resolve them before
    // the parent pass runs twemoji so a `:smile:` from a pack doesn't get
    // mistaken for stray punctuation.
    if (!isRevealedBlockedMsg && msg.emoji_tags && msg.emoji_tags.length) {
        renderCustomEmojiShortcodes(span, msg.emoji_tags);
    }

    return span;
}

/** Start an attachment download once; the backend's result event clears the gate. */
function _dmsgStartDownload(att, msg) {
    if (downloadingAttachmentIds.has(att.id)) return;
    downloadingAttachmentIds.add(att.id);
    // The box derives its phase under the message's version, so the start has to
    // move it, or the box shows nothing until the finished result lands.
    att.downloading = true;
    VectorSvelte.touchMessage(msg.id);
    // The chat, never the sender: a file you sent from another device has you as its
    // sender, and the backend files it under the conversation it belongs to.
    invoke('download_attachment', { npub: strOpenChat, msgId: msg.id, attachmentId: att.id })
        .catch(() => {
            downloadingAttachmentIds.delete(att.id);
            att.downloading = false;
            VectorSvelte.touchMessage(msg.id);
        });
}

/** Open a downloaded file: a Mini App launches (and reports its session), a file reveals or opens. */
async function _dmsgOpenFile(att, msg) {
    const path = att.path;
    if (!path) return;
    if (getFileTypeInfo((att.extension || '').toLowerCase()).isMiniApp) {
        try {
            const topicId = att.webxdc_topic || null;
            const shouldOpen = await checkChatMiniAppPermissions(path);
            if (!shouldOpen) return;
            // A declined Tor consent opens nothing; "Playing" must not paint over a cancelled launch.
            const opened = await openMiniApp(path, strOpenChat, msg.id, null, topicId);
            if (opened === false) return;
            const key = topicId || `att:${att.id}`;
            if (topicId) {
                invoke('miniapp_get_realtime_status', { topicId })
                    .then(status => VectorSvelte.setMiniappStatus(key, { active: true, peerCount: status?.peer_count || 0, peers: status?.peers }))
                    .catch(() => VectorSvelte.setMiniappStatus(key, { active: true, peerCount: 0, peers: [] }));
            } else {
                VectorSvelte.setMiniappStatus(key, { active: true, peerCount: 0 });
            }
        } catch (err) {
            console.error('Failed to open Mini App:', err);
            // Say WHY (e.g. a package missing index.html) rather than a silent no-op.
            showToast(String(err));
        }
    } else if (platformFeatures.os === 'android') {
        // No file manager to reveal into: open the file itself (an .apk routes through the installer).
        openAndroidAttachment(path);
    } else {
        revealItemInDir(path);
    }
}

/**
 * The card's data for a message with a link, or null: nothing to show, a URL another card
 * already renders (pack share, community invite), or metadata not fetched yet (the fetch
 * fires once per message here).
 */
function _dmsgLinkPreviewData(msg) {
    const isPackShareUrl = (url) => typeof url === 'string'
        && /https?:\/\/(?:www\.)?vectorapp\.io\/emojis\/pack\//i.test(url);
    const isInviteShareUrl = (url) => typeof url === 'string'
        && /https?:\/\/(?:www\.)?vectorapp\.io\/invite(?:\/|$|#|\?)/i.test(url);
    const meta = msg.preview_metadata;
    if (meta && (isPackShareUrl(meta.og_url) || isInviteShareUrl(meta.og_url))) return null;

    const hasMetadata = meta && (meta.og_image || meta.og_title || meta.title || meta.og_description || meta.description);
    if (!hasMetadata) {
        if (!meta && msg.content) {
            // Pack-share URLs and bare naddrs are not links to preview.
            const contentForPreview = msg.content
                .replace(/<https?:\/\/[^\s>]+>/g, '')
                .replace(/(?:https?:\/\/(?:www\.)?vectorapp\.io\/emojis\/pack\/|nostr:)?naddr1[ac-hj-np-z02-9]{20,}(?:\.html)?\/?/gi, '')
                .replace(/(?:https?:\/\/(?:www\.)?vectorapp\.io\/invite\/?|vector:\/\/invite\/?)#[A-Za-z0-9_-]+/gi, '');
            if (contentForPreview.includes('https') && !isImageUrl(msg.content) && !_dmsgPreviewFetchedIds.has(msg.id)) {
                _dmsgPreviewFetchedIds.add(msg.id);
                invoke('fetch_msg_metadata', { chatId: strOpenChat, msgId: msg.id });
            }
        }
        return null;
    }
    return {
        url: meta.og_url || meta.domain,
        title: meta.title || meta.og_title || 'Link Preview',
        description: meta.og_description || meta.description || '',
        favicon: meta.favicon,
        image: meta.og_image || null,
    };
}

/** og:url is the linked page's own metadata, so the scheme is gated before the OS opener. */
function _dmsgOpenPreviewUrl(strURL) {
    if (!strURL) return;
    let safe = null;
    try {
        const u = new URL(/^[a-z][a-z0-9+.-]*:/i.test(strURL) ? strURL : 'https://' + strURL);
        if (u.protocol === 'http:' || u.protocol === 'https:') safe = u.href;
    } catch (_) { /* unparseable: nothing to open */ }
    if (safe) openUrl(safe);
}

// Aggregate a message's flat reaction list into per-emoji groups, preserving
// first-occurrence order. Carries the first non-null `emoji_url` so custom-pack
// reactions render their image even when the originating pack is unsubscribed.
function _dmsgAggregateReactions(msg) {
    const groups = new Map();  // emoji → { count, mine, url }
    for (const r of (msg.reactions || [])) {
        const g = groups.get(r.emoji) || { count: 0, mine: false, url: null };
        g.count += 1;
        if (r.author_id === strPubkey) g.mine = true;
        if (!g.url && r.emoji_url) g.url = r.emoji_url;
        groups.set(r.emoji, g);
    }
    return groups;
}

/** The image for a custom reaction: the URL persisted on the reaction (survives reload
 *  and unsubscribe), else a live lookup against the subscribed packs, else null and the
 *  chip shows the literal `:shortcode:`. */
function _dmsgCustomEmojiUrl(emoji, url) {
    if (url) return url;
    const m = /^:([a-zA-Z0-9_~-]+):$/.exec(emoji);
    if (!m || !Array.isArray(arrEmojiPacks)) return null;
    const sc = m[1];
    for (const pack of arrEmojiPacks) {
        if (!pack.emojis) continue;
        // The disambiguated code first (`love~2`), then the bare one.
        const found = pack.emojis.find(e => (e.dispCode || e.shortcode) === sc);
        if (found) return found.url;
    }
    return null;
}

/** Whether the row can open one more reaction group (the inline "+" shows then). */
function _dmsgCanAddReactionGroup(msg, uniqueCount) {
    return uniqueCount > 0 && uniqueCount < MAX_DISPLAYED_REACTIONS && !newReactionGroupBlockReason(msg);
}

/** The fields the content builders render from, as one comparable string. A
 *  message whose signature is unchanged (a reaction, a profile) keeps its body:
 *  a refill would reset video playback, audio playhead and spoiler reveals. */
/**
 * What the body's leaves are built from. Deliberately NOT the id, the send state or the
 * attachments: a send swaps the id and clears pending, and the attachments derive on their
 * own, so none of that may rebuild the text and the cards.
 */
function _dmsgContentSig(msg) {
    const parts = [msg.content, msg.replied_to, !!msg.edited];
    // An edit's authoritative update can carry new emoji tags under unchanged text.
    if (msg.emoji_tags?.length) parts.push(...msg.emoji_tags.map(t => t.shortcode + '=' + t.url));
    // Link-preview metadata arrives async via message_update.
    const pm = msg.preview_metadata;
    if (pm) parts.push(pm.og_title, pm.og_image, pm.og_description, pm.title, pm.description);
    return parts.join('\u0000');
}

// Cached formatter — `Intl.DateTimeFormat` construction is expensive vs. .format().
// `.format()` accepts a ms timestamp directly (ES2018+), so we skip `new Date(at)`.
const _dmsgTimeFormatter = new Intl.DateTimeFormat([], { hour: 'numeric', minute: '2-digit', hour12: true });

function _dmsgFormatHourMinute(at) {
    if (!at) return '';
    return _dmsgTimeFormatter.format(at);
}

/**
 * React optimistically: a provisional entry lands in the message's reactions
 * (the row re-derives its chips) and rides `pendingReactions` until the echo
 * confirms it. Returns a retraction for a failed send, or null when the user
 * already holds this emoji (the set is keyed by author + emoji, so a repeat
 * adds nothing and the send would be refused as a duplicate).
 */
function dmsgReactOptimistic(msgId, emoji, url = null) {
    let cMsg = null;
    for (const cChat of arrChats) {
        cMsg = cChat.messages.find(m => m.id === msgId);
        if (cMsg) break;
    }
    if (!cMsg) return null;
    cMsg.reactions = cMsg.reactions || [];
    if (cMsg.reactions.some(r => r.emoji === emoji && r.author_id === strPubkey)) return null;
    const provisional = { id: `pending-react-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`, reference_id: msgId, author_id: strPubkey, emoji, emoji_url: url, at: Date.now() };
    cMsg.reactions.push({ id: provisional.id, reference_id: msgId, author_id: strPubkey, emoji, emoji_url: url });
    const inflight = pendingReactions.get(msgId) || [];
    inflight.push(provisional);
    pendingReactions.set(msgId, inflight);
    VectorSvelte.touchMessage(msgId);
    VectorSvelte.flushSync();
    // A chip can grow the row; a bottom-pinned reader stays pinned (softChatScroll
    // no-ops when they have scrolled up).
    softChatScroll();
    return () => {
        const l = (pendingReactions.get(msgId) || []).filter(p => p.id !== provisional.id);
        if (l.length) pendingReactions.set(msgId, l); else pendingReactions.delete(msgId);
        const i = cMsg.reactions.findIndex(r => r.id === provisional.id);
        if (i !== -1) cMsg.reactions.splice(i, 1);
        VectorSvelte.touchMessage(msgId);
    };
}

/** True once no more reactions can be added to a message — the row holds the
 *  max unique emojis it can show, or the user's personal fresh allowance is
 *  spent (the "+" is gone either way). The reaction picker uses this to
 *  auto-close a shift multi-react when there's no more room to add. */
function _reactionRowAtCapacity(msgId) {
    if (!msgId) return false;
    const rowEl = document.getElementById(msgId);
    const chips = rowEl?.querySelector('.dmsg-reactions')?.querySelectorAll('.reaction');
    if (chips && chips.length >= MAX_DISPLAYED_REACTIONS) return true;
    return !!newReactionGroupBlockReason(_dmsgLookupMessage(rowEl));
}

// Delegated click handler — replaces per-row inline onclick closures for
// avatar / author / retry / delete / add-reaction. One listener instead of
// 4-5 closures per message row, which matters on chats with hundreds of
// rows. Routing keys: data-npub (avatar/author), data-action (retry/delete),
// data-msg-id (add-reaction). The row's cached `_dmsgMsg` is consulted via
// `_dmsgLookupMessage` for actions that need the full Message object.
//
// Reactions themselves keep their existing document-level long-press / right-
// click delegation in reaction.js — that's intentional and not touched here.
(function _dmsgInstallClickDelegate() {
    if (!domChatMessages || domChatMessages._dmsgClickInstalled) return;
    domChatMessages._dmsgClickInstalled = true;
    domChatMessages.addEventListener('click', (e) => {
        const target = e.target;
        if (!(target instanceof Element)) return;

        // Failed-message actions (retry / delete) come first — the spans live
        // inside .dmsg-status which is inside .dmsg, so we'd otherwise hit the
        // profile branch on the row's author.
        const failedAction = target.closest('.dmsg-failed-action');
        if (failedAction) {
            e.stopPropagation();
            const row = failedAction.closest('.dmsg');
            const msg = row ? _dmsgLookupMessage(row) : null;
            if (!msg) return;
            const action = failedAction.dataset.action;
            if (action === 'retry') retryFailedMessage(msg);
            else if (action === 'delete') deleteFailedMessage(msg.id);
            return;
        }

        // Add-reaction "+" button
        const addReact = target.closest('.dmsg-reactions-add');
        if (addReact) {
            e.stopPropagation();
            const msgId = addReact.getAttribute('data-msg-id');
            if (msgId) _dmsgOpenReactionPicker(msgId);
            return;
        }

        // Command name in a passive invocation line → prime the composer with
        // that command (a one-tap "run it again" shortcut). Reuses the picker's
        // own input path, so the panel opens and selection flows as if typed.
        const cmdRerun = target.closest('.dmsg-command-name');
        if (cmdRerun) {
            e.stopPropagation();
            if (commandCtrl && commandCtrl.isComposing && commandCtrl.isComposing()) commandCtrl.exitComposer();
            domChatMessageInput.value = cmdRerun.textContent || '';
            domChatMessageInput.focus();
            domChatMessageInput.dispatchEvent(new Event('input', { bubbles: true }));
            return;
        }

        // Avatar / author → open the mini profile popup. The popup itself
        // surfaces "View Profile" → openProfile() if the user wants the full screen.
        // The command line's bot avatar/name join here (same data-npub contract).
        const profileBtn = target.closest('.dmsg-avatar, .dmsg-author, .dmsg-command-bot-avatar, .dmsg-command-bot');
        if (profileBtn) {
            const npub = profileBtn.dataset.npub;
            if (!npub) return;
            showMiniProfile(npub, profileBtn);
            return;
        }

        // (System-event user names open the mini-profile via a direct listener attached in
        // insertSystemEvent — they aren't always children of this delegate's container.)
    });
})();

/**
 * A reaction chip click: add your matching reaction, or revoke it if you already reacted
 * (one per emoji per user). Revoke only acts on your OWN reaction.
 */
function _dmsgReactionClick(msgId, emoji) {
    if (isReactionLongPressed()) return;
    for (const cChat of arrChats) {
        const cMsg = cChat.messages.find(a => a.id === msgId);
        if (!cMsg) continue;
        const mine = cMsg.reactions.find(r => r.emoji === emoji && r.author_id === strPubkey);
        if (mine) {
            // A provisional entry means our add is still in flight — swallow the
            // click (debounce) rather than revoke an id the backend never issued.
            if (!String(mine.id).startsWith('pending-react-')) {
                // Already reacted → revoke. The backend optimistically removes the reaction
                // and emits message_update, so the chip refreshes without local bookkeeping.
                invoke('revoke_reaction', { reactionId: mine.id })
                    .catch(err => console.error('revoke_reaction failed:', err));
            }
        } else {
            // Not yet reacted → add. The provisional debounces double-clicks and
            // keeps the chip reacted through an earlier click's echo.
            const retract = dmsgReactOptimistic(msgId, emoji, null);
            reactToMessageRouted(msgId, cChat.id, emoji).catch(() => { if (retract) retract(); });
        }
        break;
    }
}
