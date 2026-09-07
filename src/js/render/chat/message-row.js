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
    if (typeof domChatMessages === 'undefined' || !domChatMessages) return false;
    if (_dmsgListIsland && _dmsgListIsland._target === domChatMessages) return true;
    _dmsgListIsland = VectorSvelte.mountMessageList(domChatMessages, { h: _dmsgListHelpers });
    _dmsgListIsland._target = domChatMessages;
    // The mount cleared the container; the toolbar is re-created on demand.
    initMessageToolbar();
    return true;
}

/** Everything the list island derives from and hands to its rows. */
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
    // Render-time facts about a row, the way the vanilla builder read them once per
    // render: cached per message object so a re-derive of 80 rows costs 80 lookups,
    // not 80 scans. A changed message is a new object and gets a fresh context.
    ctxFor: (msg) => {
        let ctx = _dmsgRowCtxCache.get(msg);
        if (ctx && ctx.currentChat?.id === strOpenChat) return ctx;
        ctx = _dmsgRowCtx(msg);
        _dmsgRowCtxCache.set(msg, ctx);
        return ctx;
    },
    get row() { return _dmsgRowHelpers; },
};
const _dmsgRowCtxCache = new WeakMap();
function _dmsgRowCtx(msg) {
    {
        const currentChat = arrChats.find(c => c.id === strOpenChat);
        const isGroupChat = chatIsGroup(currentChat);
        const otherFullId = msg.npub || (!isGroupChat ? currentChat?.id : '') || '';
        const blockedAuthorProfile = isGroupChat && !msg.mine && otherFullId ? getProfile(otherFullId) : null;
        const blocked = !!blockedAuthorProfile?.is_blocked;
        return {
            myNpub: strPubkey,
            isGroupChat,
            currentChat,
            pinged: _dmsgIsPinged(msg, currentChat, isGroupChat),
            replyingTo: strCurrentReplyReference === msg.id,
            blocked: blocked && !revealedBlockedMessages.has(msg.id),
            revealedBlocked: blocked && revealedBlockedMessages.has(msg.id),
        };
    }
}

/**
 * Update a rendered row to `msg` in place: the island re-derives its shell and
 * refills its content on the same element, so the toolbar target, jump highlight,
 * streak state and scroll position all survive. Vanilla-built rows (system events,
 * PIVX, blocked) fall back to a full replace.
 */
function updateMessageRow(domMsg, msg, profile, oldId = '') {
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
    VectorSvelte.touchWindow();
    VectorSvelte.flushSync();
    const el = document.getElementById(msg.id);
    if (msg.mine) _dmsgUpdateLastSentVisibility();
    return el || domMsg;
}

/** Helpers the row island calls; the leaf builders stay here. */
const _dmsgRowHelpers = {
    getProfile: (npub) => getProfile(npub),
    getName: (x) => getName(x),
    getProfileAvatarSrc: (p) => getProfileAvatarSrc(p),
    twemojify: (el) => twemojify(el),
    showTooltip: (text, el) => showGlobalTooltip(text, el),
    hideTooltip: () => hideGlobalTooltip(),
    formatHourMinute: (at) => _dmsgFormatHourMinute(at),
    // MessageContent's leaves
    buildText: (msg, ctx) => _dmsgTextLeaf(msg, ctx),
    // Attachments' leaves and facts
    isImage: (ext) => ['png', 'jpeg', 'jpg', 'gif', 'webp', 'svg', 'bmp', 'tiff', 'tif', 'ico'].includes(ext),
    isAudio: (ext) => ['wav', 'mp3', 'flac', 'aac', 'm4a', 'ogg'].includes(ext),
    isVideo: (ext) => platformFeatures.os !== 'linux' && ['mp4', 'webm', 'mov'].includes(ext),
    isDownloading: (att) => !!att.downloading || downloadingAttachmentIds.has(att.id),
    willAutoDownload: (att, ctx) => AUTO_DOWNLOAD_ENABLED && !ctx.revealedBlocked && att.size > 0
        && att.size <= MAX_AUTO_DOWNLOAD_BYTES && !att.download_failed,
    // Once per attachment id across renders, or every repaint would re-fire the download.
    autoDownload: (att, msg, sender) => _dmsgStartDownload(att, msg, sender),
    startDownload: (att, msg, sender) => _dmsgStartDownload(att, msg, sender),
    renderAudio: (node, att, msg) => handleAudioAttachment(att, node, msg),
    fileBox: (node, att, state, opts) => _dmsgFileBoxLeaf(node, att, state, opts || {}),
    attachUploadProgress: (node, msg) => _dmsgAttachUploadProgress(node, msg),
    assetUrl: (path) => convertFileSrc(path),
    mediaUrl: (path) => mediaUrl(path),
    isSpoiler: (att) => isSpoilerAttachment(att),
    thumbhash: (npub, msgId) => invoke('generate_thumbhash_preview', { npub, msgId }),
    onImageLoad: () => compensateChatScrollForResize(),
    onThumbLoad: () => { if (proceduralScrollState.isLoadingOlderMessages) correctScrollForMediaLoad(); else softChatScroll(); },
    onVideoMeta: (video) => { if (!video.isConnected) return; video.currentTime = 0.1; compensateChatScrollForResize(); },
    attachImagePreview: (img) => attachImagePreview(img),
    attachFileExtBadge: (img, container, ext) => attachFileExtBadge(img, container, ext),
    cancelUpload: (pendingId) => invoke('cancel_upload', { pendingId }),
    openChat: () => strOpenChat,
    formatBytes: (n) => formatBytes(n),
    buildCryptoAddress: (msg) => { const c = detectCryptoAddress(msg.content); return c ? renderCryptoAddress(c) : null; },
    renderEmojiPackPreviews: (node, text) => renderEmojiPackPreviews(node, text),
    renderCommunityInvitePreviews: (node, text) => renderCommunityInvitePreviews(node, text),
    xdcUrl: (msg) => findXdcUrl(msg.content),
    renderXdcUrlCard: (node, msg, url) => renderXdcUrlCard(node, msg, url),
    webPreviewsEnabled: () => !!fWebPreviewsEnabled,
    buildLinkPreview: (msg) => _dmsgBuildLinkPreview(msg),
    isAndroid: () => typeof platformFeatures !== 'undefined' && platformFeatures.os === 'android',
    fmtCountdown: (secs) => _fmtCountdown(secs),
    selfDestructTooltip: (el) => _selfDestructTooltip(el),
    selfDestructTooltipEnd: () => _selfDestructTooltipEnd(),
    contentSig: (msg) => _dmsgContentSig(msg),
    reactionGroups: (msg) => Array.from(_dmsgAggregateReactions(msg), ([emoji, g]) => ({ emoji, ...g })),
    canAddReactionGroup: (msg, n) => _dmsgCanAddReactionGroup(msg, n),
    fillReactionGlyph: (span, emoji, url) => _dmsgFillReactionGlyph(span, emoji, url),
    rollReactionCount: (chip, count) => _dmsgRollReactionCount(chip, count),
    // A hover tip anchored to a chip that just left would float forever (mouseout
    // owns dismissal, and a removed anchor never fires it).
    reactionChipRemoved: () => { if (reactionHoverEl && !reactionHoverEl.isConnected) hideReactionHoverTip(); },
    replyView: (msg, sender) => _dmsgReplyView(msg, sender),
    renderCustomEmojiShortcodes: (el, tags) => renderCustomEmojiShortcodes(el, tags),
    createPlaceholderAvatar: (g, size) => createPlaceholderAvatar(g, size),
    buildPivxBubble: (msg) => renderPivxPaymentBubble(
        msg.pivx_payment.gift_code, msg.pivx_payment.amount_piv, msg.mine, msg.pivx_payment.address),
    buildBlockedPlaceholder: (msg) => _dmsgBuildBlockedPlaceholder(msg),
    systemEventName: (npub) => systemEventName(npub),
    systemEventSuffix: (type) => systemEventSuffix(type),
    showMiniProfile: (npub, el) => showMiniProfile(npub, el),
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

function _dmsgResolveProfile(authorFullId, sender, msg) {
    if (msg.mine) return getProfile(strPubkey);
    return sender || (authorFullId ? getProfile(authorFullId) : null);
}

function _dmsgBuildGutter(authorFullId, authorProfile, msg) {
    const gutter = document.createElement('div');
    gutter.classList.add('dmsg-gutter');

    const avatarSrc = getProfileAvatarSrc(authorProfile);
    const avatar = createAvatarImg(avatarSrc, 40, false);
    avatar.classList.add('dmsg-avatar', 'btn');
    if (authorFullId) avatar.dataset.npub = authorFullId;
    avatar.style.margin = '0';
    gutter.appendChild(avatar);

    // Hover-only time pill shown on streak-continuation rows; CSS toggles its visibility on row hover.
    const hoverTime = document.createElement('time');
    hoverTime.classList.add('dmsg-time-hover');
    hoverTime.textContent = _dmsgFormatHourMinute(msg.at);
    gutter.appendChild(hoverTime);

    return gutter;
}

function _dmsgBuildHeader(authorFullId, authorProfile, msg, isGroupChat, currentChat) {
    const header = document.createElement('div');
    header.classList.add('dmsg-header');

    const author = document.createElement('span');
    author.classList.add('dmsg-author', 'btn');
    if (authorFullId) author.dataset.npub = authorFullId;

    const displayName = getName(authorProfile || authorFullId);
    author.textContent = displayName;
    twemojify(author);

    header.appendChild(author);

    // Bot marker next to the name — same iconography as the chat list so
    // bot identity stays consistent. Tooltip explains the badge.
    if (authorProfile?.bot) {
        const botIcon = document.createElement('span');
        botIcon.className = 'icon icon-bot dmsg-author-bot-icon';
        botIcon.addEventListener('mouseenter', () => showGlobalTooltip('Bot', botIcon));
        botIcon.addEventListener('mouseleave', hideGlobalTooltip);
        header.appendChild(botIcon);
    }

    const senderIsAdmin = isGroupChat && currentChat?.metadata?.admins?.includes(authorFullId);
    if (senderIsAdmin) {
        const adminBadge = document.createElement('span');
        adminBadge.classList.add('dmsg-author-badge', 'admin');
        adminBadge.textContent = 'admin';
        header.appendChild(adminBadge);
    }

    // Community owner badge (gold, matches the member-list crown) — gated on the PROVEN owner
    // npub from the verified attestation, never an unchecked claim.
    const ownerNpub = currentChat?.metadata?.custom_fields?.owner_npub;
    if (ownerNpub && authorFullId && ownerNpub === authorFullId) {
        const ownerBadge = document.createElement('span');
        ownerBadge.classList.add('dmsg-author-badge', 'owner');
        ownerBadge.textContent = 'Owner';
        header.appendChild(ownerBadge);
    }

    const time = document.createElement('time');
    time.classList.add('dmsg-time');
    time.textContent = _dmsgFormatHourMinute(msg.at);
    header.appendChild(time);

    return header;
}


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
        const equipped = (typeof equippedEmojiTags === 'function') ? equippedEmojiTags() : [];
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


function _dmsgBuildBlockedPlaceholder(msg) {
    const blockedSpan = document.createElement('span');
    blockedSpan.style.cssText = 'color: rgba(255,255,255,0.3); font-style: italic; cursor: pointer; display: flex; align-items: center; gap: 5px;';
    const blockedIcon = document.createElement('span');
    blockedIcon.classList.add('icon', 'icon-cancel');
    blockedIcon.style.cssText = 'width: 14px; height: 14px; position: relative; margin: 0; flex-shrink: 0; background-color: rgba(255,255,255,0.3);';
    blockedSpan.appendChild(blockedIcon);
    blockedSpan.appendChild(document.createTextNode('Blocked message'));
    blockedSpan.onclick = (e) => {
        e.stopPropagation();
        revealedBlockedMessages.add(msg.id);
        openChat(strOpenChat);
    };
    return blockedSpan;
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

/**
 * The passive invocation line: "JSKitty ran /roll with ◎ Concordia" — dim
 * prose, no bubble, the params deliberately absent (long values would drown
 * the row; the content still carries them for bots). The row renders as a
 * continuation (no header/avatar) since the sentence names the author.
 */
function _dmsgBuildCommandLine(msg, cmd) {
    const line = document.createElement('span');
    line.classList.add('dmsg-command-line');

    const author = document.createElement('span');
    author.classList.add('dmsg-command-author');
    // Tagged like every other rendered name so a rename can find it — the chat
    // is DOM-windowed, so nothing rebuilds these rows to pick a new name up.
    const strAuthorNpub = msg.mine ? strPubkey : (msg.npub || '');
    if (strAuthorNpub) author.dataset.npub = strAuthorNpub;
    author.textContent = getName(strAuthorNpub);
    line.appendChild(author);

    line.appendChild(document.createTextNode(' ran '));

    // The command name is a one-tap shortcut: clicking it drops `/name` back
    // into the composer and reopens the picker (routed in the click delegate).
    const name = document.createElement('span');
    name.classList.add('dmsg-command-name', 'btn');
    name.textContent = '/' + cmd.name;
    line.appendChild(name);

    if (cmd.botNpub) {
        line.appendChild(document.createTextNode(' with '));
        const profile = getProfile(cmd.botNpub);
        // Avatar + name carry data-npub so the shared profile delegate opens
        // the bot's mini profile, exactly like a normal author name/avatar.
        // The nowrap unit keeps them on the same line when the sentence wraps
        // (WebKit breaks around images even with no whitespace between).
        const unit = document.createElement('span');
        unit.classList.add('dmsg-command-bot-unit');
        const img = document.createElement('img');
        img.classList.add('dmsg-command-bot-avatar', 'btn');
        img.src = (profile && getProfileAvatarSrc(profile)) || 'icons/user-placeholder.svg';
        img.alt = '';
        img.dataset.npub = cmd.botNpub;
        unit.appendChild(img);
        const bot = document.createElement('span');
        bot.classList.add('dmsg-command-bot', 'btn');
        bot.textContent = getName(cmd.botNpub);
        bot.dataset.npub = cmd.botNpub;
        unit.appendChild(bot);
        line.appendChild(unit);
    }
    return line;
}

function _dmsgBuildText(msg, displayContent, fEmojiOnly, isGroupChat, currentChat, isRevealedBlockedMsg) {
    const span = document.createElement('span');
    span.classList.add('dmsg-text');

    // Command invocations render as the passive line instead of raw text.
    if (!fEmojiOnly) {
        const cmd = _dmsgCommandInfo(msg);
        if (cmd) {
            span.appendChild(_dmsgBuildCommandLine(msg, cmd));
            return span;
        }
    }

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
    if (typeof stripEmojiPackNaddrs === 'function') {
        textBody = stripEmojiPackNaddrs(textBody);
    }
    // Community invite links likewise render as their own card.
    if (typeof stripCommunityInviteUrls === 'function') {
        textBody = stripCommunityInviteUrls(textBody);
    }
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
function _dmsgStartDownload(att, msg, sender) {
    if (downloadingAttachmentIds.has(att.id)) return;
    downloadingAttachmentIds.add(att.id);
    const isGroupChat = chatIsGroup(getChat(strOpenChat));
    const npub = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
    invoke('download_attachment', { npub, msgId: msg.id, attachmentId: att.id })
        .catch(() => downloadingAttachmentIds.delete(att.id));
}

/** A file box in `node`: a downloaded file (opens / reveals), or a download / downloading state. */
function _dmsgFileBoxLeaf(node, att, state, opts) {
    if (state === 'downloaded') {
        _dmsgRenderFileAttachment(node, opts.msg, att);
        return;
    }
    const { fileDiv, statusSpan } = createFileBox(att, state);
    if (opts.failed && statusSpan) {
        const reason = (att.download_error || '').slice(0, 64);
        statusSpan.innerText = reason ? `Failed: ${reason} · Tap to Retry` : 'Download Failed · Tap to Retry';
    }
    if (opts.onClick) fileDiv.addEventListener('click', opts.onClick, { once: true });
    node.appendChild(fileDiv);
}

function _dmsgRenderFileAttachment(target, msg, cAttachment) {
    const { fileDiv, isMiniApp } = createFileBox(cAttachment, 'downloaded');
    fileDiv.addEventListener('click', async (e) => {
        const path = e.currentTarget.getAttribute('filepath');
        if (!path) return;

        if (isMiniApp) {
            try {
                // URL-shared Mini Apps pass a synthetic attachment that is not
                // in msg.attachments — fall back to the one we rendered from
                const attachment = msg.attachments.find(a => a.path === path) || cAttachment;
                const topicId = attachment?.webxdc_topic || null;
                const shouldOpen = await checkChatMiniAppPermissions(path);
                if (!shouldOpen) return;
                // A declined Tor consent opens nothing — the optimistic
                // "Playing" below must not paint over a cancelled launch.
                const opened = await openMiniApp(path, strOpenChat, msg.id, null, topicId);
                if (opened === false) return;
                if (fileDiv._updateMiniAppStatus) {
                    if (topicId) {
                        invoke('miniapp_get_realtime_status', { topicId })
                            .then(status => fileDiv._updateMiniAppStatus(true, status?.peer_count || 0, status?.peers))
                            .catch(() => fileDiv._updateMiniAppStatus(true, 0, []));
                    } else {
                        fileDiv._updateMiniAppStatus(true, 0);
                    }
                }
            } catch (err) {
                console.error('Failed to open Mini App:', err);
                // Surface WHY (e.g. "Invalid Mini App package: Missing index.html") instead of a silent
                // no-op — the open threw before any optimistic status, so the card stays "Click to Play".
                showToast(String(err));
            }
        } else if (platformFeatures.os === 'android') {
            // No file manager to reveal into — open the file itself
            // (an .apk routes through the system installer flow)
            openAndroidAttachment(path);
        } else {
            revealItemInDir(path);
        }
    });
    target.appendChild(fileDiv);
}

function _dmsgAttachUploadProgress(target, msg) {
    let hasSpinner = false;
    const uploadMsgId = msg.id;

    const fileBoxIcon = target.querySelector('.custom-audio-player > span[class*="icon-"], .custom-audio-player > img');
    if (fileBoxIcon) {
        hasSpinner = true;
        if (fileBoxIcon.tagName === 'IMG') {
            const textSpan = fileBoxIcon.parentElement?.querySelector('span');
            if (textSpan) textSpan.style.marginLeft = '55px';
        }
        createFileBoxSpinner(fileBoxIcon, { id: msg.id + '_file' });
        const cancelBtn = document.createElement('div');
        cancelBtn.className = 'upload-cancel-btn';
        cancelBtn.addEventListener('click', (e) => {
            e.stopPropagation();
            invoke('cancel_upload', { pendingId: uploadMsgId });
        });
        setTimeout(() => {
            const player = target.querySelector('.custom-audio-player');
            if (player) player.appendChild(cancelBtn);
        }, 210);
    }

    const audioPlayBtn = target.querySelector('.audio-play-btn');
    if (audioPlayBtn) {
        hasSpinner = true;
        const wrapper = document.createElement('div');
        wrapper.style.position = 'relative';
        wrapper.style.width = '40px';
        wrapper.style.height = '40px';
        wrapper.style.minWidth = '40px';
        wrapper.style.flexShrink = '0';
        const spinner = document.createElement('div');
        spinner.className = 'miniapp-downloading-spinner';
        spinner.id = msg.id + '_file';
        spinner.style.width = '40px';
        spinner.style.height = '40px';
        applyPendingUploadProgress(spinner, msg.id);
        wrapper.appendChild(spinner);
        const cancelBtn = document.createElement('div');
        cancelBtn.className = 'upload-cancel-btn audio-upload-cancel';
        cancelBtn.addEventListener('click', (e) => {
            e.stopPropagation();
            invoke('cancel_upload', { pendingId: uploadMsgId });
        });
        wrapper.appendChild(cancelBtn);
        audioPlayBtn.replaceWith(wrapper);
    }

    const mediaEl = target.querySelector('img:not(.emoji), video');
    if (!hasSpinner && mediaEl) {
        hasSpinner = true;
        // Wrap the media in a relative box and PIN it to the media's actual rendered width so the
        // absolutely-positioned overlay/spinner centers exactly on the media. The real image has
        // `width:auto` + percentage `max-width` (.dmsg-image-attachment), which no auto-sized container
        // (inline-block OR fit-content) reliably shrink-wraps — it drifts to the row width, leaving the
        // image left and the centered spinner off to the right. `offsetWidth` after layout is the truth
        // and handles landscape, small, and height-clamped portrait alike. `line-height:0` drops the
        // inline descender gap so the height matches too.
        const parent = mediaEl.parentElement;
        const wrapper = document.createElement('div');
        wrapper.style.position = 'relative';
        wrapper.style.display = 'inline-block';
        wrapper.style.lineHeight = '0';
        wrapper.style.maxWidth = '100%';
        parent.replaceChild(wrapper, mediaEl);
        wrapper.appendChild(mediaEl);
        mediaEl.style.opacity = '0.25';
        if (mediaEl.tagName === 'VIDEO') mediaEl.removeAttribute('controls');
        const pinWrapperToMedia = () => {
            const w = mediaEl.offsetWidth;
            if (w) wrapper.style.width = w + 'px';
        };
        pinWrapperToMedia();
        mediaEl.addEventListener('load', pinWrapperToMedia, { once: true });
        mediaEl.addEventListener('loadedmetadata', pinWrapperToMedia, { once: true });
        const overlay = document.createElement('div');
        overlay.className = 'attachment-progress-overlay';
        const spinner = document.createElement('div');
        spinner.className = 'miniapp-downloading-spinner';
        spinner.id = msg.id + '_file';
        spinner.style.width = '48px';
        spinner.style.height = '48px';
        applyPendingUploadProgress(spinner, msg.id);
        overlay.appendChild(spinner);
        const cancelBtn = document.createElement('div');
        cancelBtn.className = 'upload-cancel-btn';
        cancelBtn.addEventListener('click', (e) => {
            e.stopPropagation();
            invoke('cancel_upload', { pendingId: uploadMsgId });
        });
        overlay.appendChild(cancelBtn);
        wrapper.appendChild(overlay);
    }

    const hasSpoilerUpload = target.querySelector('[data-spoiler-upload]');
    if (!hasSpinner && !hasSpoilerUpload && target.lastElementChild) {
        target.lastElementChild.style.opacity = 0.25;
    }
}

function _dmsgBuildLinkPreview(msg) {
    // Emoji-pack share URLs already render via the rich pack preview
    // card (built by `renderEmojiPackPreviews`); skipping both the OG
    // fetch and the OG render prevents the duplicate website-style
    // card from stacking under the pack preview.
    const isPackShareUrl = (url) => typeof url === 'string'
        && /https?:\/\/(?:www\.)?vectorapp\.io\/emojis\/pack\//i.test(url);
    if (msg.preview_metadata && isPackShareUrl(msg.preview_metadata.og_url)) {
        return null;
    }
    // Community invite links likewise render their own dedicated card.
    const isInviteShareUrl = (url) => typeof url === 'string'
        && /https?:\/\/(?:www\.)?vectorapp\.io\/invite(?:\/|$|#|\?)/i.test(url);
    if (msg.preview_metadata && isInviteShareUrl(msg.preview_metadata.og_url)) {
        return null;
    }

    const hasMetadata = msg.preview_metadata && (
        msg.preview_metadata.og_image
        || msg.preview_metadata.og_title
        || msg.preview_metadata.title
        || msg.preview_metadata.og_description
        || msg.preview_metadata.description
    );

    if (!hasMetadata) {
        if (!msg.preview_metadata && msg.content) {
            // Strip pack-share URLs + bare naddrs before deciding whether
            // to ask the backend for OG metadata — a message that's pack-
            // refs only has no "real" link to preview.
            const contentForPreview = msg.content
                .replace(/<https?:\/\/[^\s>]+>/g, '')
                .replace(/(?:https?:\/\/(?:www\.)?vectorapp\.io\/emojis\/pack\/|nostr:)?naddr1[ac-hj-np-z02-9]{20,}(?:\.html)?\/?/gi, '')
                .replace(/(?:https?:\/\/(?:www\.)?vectorapp\.io\/invite\/?|vector:\/\/invite\/?)#[A-Za-z0-9_-]+/gi, '');
            if (contentForPreview.includes('https') && !isImageUrl(msg.content)) {
                // Dedupe — every re-render (e.g., reactions update) of a
                // metadata-less message would otherwise re-fire this invoke.
                if (!_dmsgPreviewFetchedIds.has(msg.id)) {
                    _dmsgPreviewFetchedIds.add(msg.id);
                    invoke('fetch_msg_metadata', { chatId: strOpenChat, msgId: msg.id });
                }
            }
        }
        return null;
    }

    const divPrev = document.createElement('div');
    divPrev.classList.add('dmsg-preview', 'btn');
    divPrev.setAttribute('url', msg.preview_metadata.og_url || msg.preview_metadata.domain);

    const description = msg.preview_metadata.og_description || msg.preview_metadata.description;
    const hasImage = !!msg.preview_metadata.og_image;
    if (description && hasImage) divPrev.style.paddingBottom = '0';

    const imgFavicon = document.createElement('img');
    imgFavicon.classList.add('favicon');
    imgFavicon.addEventListener('load', () => {
        if (!imgFavicon.isConnected) return;
        if (proceduralScrollState.isLoadingOlderMessages) correctScrollForMediaLoad();
        else softChatScroll();
    }, { once: true });
    imgFavicon.addEventListener('error', () => imgFavicon.style.display = 'none', { once: true });
    // Backend-cached: the favicon URL points at the linked (attacker-chosen)
    // host — a raw img.src would be a clearnet fetch that bypasses Tor.
    bindBackendCachedImg(imgFavicon, msg.preview_metadata.favicon);

    const spanPreviewTitle = document.createElement('span');
    spanPreviewTitle.appendChild(imgFavicon);
    spanPreviewTitle.appendChild(document.createTextNode(
        msg.preview_metadata.title || msg.preview_metadata.og_title || 'Link Preview'
    ));
    divPrev.appendChild(spanPreviewTitle);

    if (description) {
        const spanDescription = document.createElement('span');
        spanDescription.classList.add('dmsg-preview-description');
        const parts = description.split(/<br\s*\/?>/i);
        parts.forEach((part, index) => {
            const subParts = part.split('\n');
            subParts.forEach((subPart, subIndex) => {
                if (subPart) spanDescription.appendChild(document.createTextNode(subPart));
                if (subIndex < subParts.length - 1) spanDescription.appendChild(document.createElement('br'));
            });
            if (index < parts.length - 1) spanDescription.appendChild(document.createElement('br'));
        });
        if (hasImage) spanDescription.style.borderRadius = '0';
        divPrev.appendChild(spanDescription);
    }

    if (hasImage) {
        const imgPreview = document.createElement('img');
        imgPreview.classList.add('dmsg-preview-img');
        imgPreview.onerror = () => imgPreview.remove();
        imgPreview.addEventListener('load', () => {
            if (!imgPreview.isConnected) return;
            if (proceduralScrollState.isLoadingOlderMessages) correctScrollForMediaLoad();
            else softChatScroll();
        }, { once: true });
        // Backend-cached: og:image is served by the attacker-controlled
        // linked page — never fetch it from the WebView (Tor bypass).
        bindBackendCachedImg(imgPreview, msg.preview_metadata.og_image);
        divPrev.appendChild(imgPreview);
    }

    return divPrev;
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

// The count is its own clipped roller (`.reaction-count > .rc-value`) so a
// change can slide the old number out and the new one in — see
// _dmsgRollReactionCount.
function _dmsgBuildReactionCountEl(count) {
    const countEl = document.createElement('span');
    countEl.className = 'reaction-count';
    const valEl = document.createElement('span');
    valEl.className = 'rc-value';
    valEl.textContent = String(count);
    countEl.appendChild(valEl);
    return countEl;
}

// Fill a reaction chip's glyph: a NIP-30 custom-emoji image or the text emoji.
// The chip element itself (attributes, count roller) belongs to the row island.
function _dmsgFillReactionGlyph(span, emoji, url) {
    // Prefer the URL persisted on the reaction itself (survives reload + unsubscribe),
    // fall back to a live lookup against subscribed packs, then to the literal
    // `:shortcode:` text if neither knows it.
    let customUrl = url || null;
    if (!customUrl) {
        const m = /^:([a-zA-Z0-9_~-]+):$/.exec(emoji);
        if (m && typeof arrEmojiPacks !== 'undefined' && Array.isArray(arrEmojiPacks)) {
            const sc = m[1];
            for (const pack of arrEmojiPacks) {
                if (!pack.emojis) continue;
                // Match the disambiguated code first (`love~2`), then the bare one.
                const found = pack.emojis.find(e => (e.dispCode || e.shortcode) === sc);
                if (found) { customUrl = found.url; break; }
            }
        }
    }

    if (customUrl) {
        const img = document.createElement('img');
        img.alt = emoji;
        img.className = 'reaction-custom-emoji';
        span.appendChild(img);
        // Route reaction emoji bytes through the Rust cache — raw Blossom URL
        // never lands on <img src>, so Tor traffic stays contained and repeat
        // renders skip the network entirely.
        if (typeof bindCachedEmojiImg === 'function') {
            // Deleted/404 emoji → twemoji'd question mark so the chip stays a
            // recognisable glyph instead of an empty box.
            bindCachedEmojiImg(img, customUrl, 'emoji', (el) => {
                el.replaceWith(document.createTextNode('❓'));
                twemojify(span);
            });
        } else {
            img.src = customUrl;
        }
    } else {
        // Fuzz defence: any reaction glyph that isn't a resolvable custom emoji
        // gets ONE uniform hard cap by code point (surrogate-safe, so a multi-char
        // emoji is never split into a lone surrogate). data-emoji keeps the full
        // value for the toggle handler; only the DISPLAY is capped.
        const REACTION_GLYPH_CAP = 16;
        const cps = Array.from(emoji);
        const shown = cps.length > REACTION_GLYPH_CAP
            ? cps.slice(0, REACTION_GLYPH_CAP).join('') + '…'
            : emoji;
        const glyph = document.createElement('span');
        glyph.className = 'reaction-glyph';
        glyph.textContent = shown;
        span.appendChild(glyph);
        twemojify(glyph);
    }
}

/** Current displayed count of a reaction chip (from its `.rc-value` roller). */
function _dmsgReactionCount(chip) {
    const v = chip && chip.querySelector('.rc-value');
    const n = v ? parseInt(v.textContent, 10) : NaN;
    return Number.isFinite(n) ? n : 1;
}

function _dmsgReducedMotion() {
    return typeof window !== 'undefined' && window.matchMedia
        && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/** Roll a reaction chip's count to `toCount`: the old number slides out (up on
 *  an increment, down on a decrement) while the new slides in from the opposite
 *  edge. Transform + opacity only, so it composites on the GPU at 60fps. */
function _dmsgRollReactionCount(chip, toCount) {
    const countEl = chip && chip.querySelector('.reaction-count');
    const valEl = countEl && countEl.querySelector('.rc-value');
    if (!valEl) return;
    const from = parseInt(valEl.textContent, 10);
    // Drop any still-animating leftover so rapid updates can't pile up ghosts.
    countEl.querySelectorAll('.rc-old').forEach(o => o.remove());
    if (!Number.isFinite(from) || from === toCount || _dmsgReducedMotion() || !valEl.animate) {
        valEl.textContent = String(toCount);
        return;
    }
    const up = toCount > from;
    const old = valEl.cloneNode(true);
    old.classList.add('rc-old');
    countEl.appendChild(old);
    valEl.textContent = String(toCount);
    // Slightly leisurely (Discord-ish) so the roll is clearly readable, not a blink.
    const duration = 340;
    const easing = 'cubic-bezier(0.22, 1, 0.36, 1)';
    const outY = up ? '-100%' : '100%';
    const inY = up ? '100%' : '-100%';
    const outAnim = old.animate(
        [{ transform: 'translateY(0)', opacity: 1 }, { transform: `translateY(${outY})`, opacity: 0 }],
        { duration, easing });
    outAnim.onfinish = outAnim.oncancel = () => old.remove();
    valEl.animate(
        [{ transform: `translateY(${inY})`, opacity: 0 }, { transform: 'translateY(0)', opacity: 1 }],
        { duration, easing });
}

/** Whether the row can open one more reaction group (the inline "+" shows then). */
function _dmsgCanAddReactionGroup(msg, uniqueCount) {
    return uniqueCount > 0 && uniqueCount < MAX_DISPLAYED_REACTIONS && !newReactionGroupBlockReason(msg);
}

/** The fields the content builders render from, as one comparable string. A
 *  message whose signature is unchanged (a reaction, a profile) keeps its body:
 *  a refill would reset video playback, audio playhead and spoiler reveals. */
function _dmsgContentSig(msg) {
    const parts = [msg.id, msg.content, msg.replied_to, msg.at, !!msg.pending, !!msg.failed, !!msg.edited];
    for (const a of (msg.attachments || [])) parts.push(a.id, !!a.downloaded, a.path);
    // Link-preview metadata arrives async via message_update.
    const pm = msg.preview_metadata;
    if (pm) parts.push(pm.og_title, pm.og_image, pm.og_description, pm.title, pm.description);
    return parts.join('\u0000');
}

function _dmsgUpdateLastSentVisibility() {
    if (!domChatMessages) return;
    const allMine = domChatMessages.querySelectorAll('.dmsg[data-mine="true"]');
    allMine.forEach((el, index) => {
        const isLast = index === allMine.length - 1;
        const statusEl = el.querySelector('.dmsg-status:not(.dmsg-status-failed)');
        if (statusEl && !statusEl.textContent.includes('Sending')) {
            statusEl.classList.toggle('dmsg-status-hidden', !isLast);
        }
    });
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
    if (typeof softChatScroll === 'function') softChatScroll();
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
