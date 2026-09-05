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
    messages: (chatId) => eventCache.getEventsRef(chatId) || arrChats.find(c => c.id === chatId)?.messages || [],
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
    fillContent: (node, msg, sender, ctx) =>
        _dmsgFillContent(node, msg, sender, ctx.isGroupChat, ctx.currentChat, ctx.revealedBlocked),
    contentSig: (msg) => _dmsgContentSig(msg),
    reactionGroups: (msg) => Array.from(_dmsgAggregateReactions(msg), ([emoji, g]) => ({ emoji, ...g })),
    canAddReactionGroup: (msg, n) => _dmsgCanAddReactionGroup(msg, n),
    fillReactionGlyph: (span, emoji, url) => _dmsgFillReactionGlyph(span, emoji, url),
    rollReactionCount: (chip, count) => _dmsgRollReactionCount(chip, count),
    // A hover tip anchored to a chip that just left would float forever (mouseout
    // owns dismissal, and a removed anchor never fires it).
    reactionChipRemoved: () => { if (reactionHoverEl && !reactionHoverEl.isConnected) hideReactionHoverTip(); },
    buildReply: (msg, sender) => _dmsgBuildReplyContext(msg, sender),
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
 * Fill a `.dmsg-content` element: text, attachments, crypto address, pack and invite
 * previews, mini-app card, link preview, edited mark, status and self-destruct glyph,
 * in that order. Shared by the vanilla builder and the row island.
 */
function _dmsgFillContent(content, msg, sender, isGroupChat, currentChat, isRevealedBlockedMsg) {
    // ---- Text content -------------------------------------------------------
    // Defensive against null/undefined content (attachment-only messages from
    // some clients can omit content entirely).
    const displayContent = msg.content || '';

    // Defensive: msg.content can be null/undefined for attachment-only messages.
    const safeContent = msg.content || '';

    // Strip resolved `:shortcode:` tokens before the unicode-only check so
    // a message that's purely custom emojis (or a mix with stock emojis)
    // still qualifies for the jumbo emoji-only treatment.
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
    // Cap at 6 graphemes, not UTF-16 units — fully-qualified ZWJ sequences
    // (e.g. 👁️‍🗨️ = 7 code units) are still a single visual emoji.
    let graphemeCount = customEmojiCount;
    if (strEmojiCleaned) {
        const seg = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
        for (const _ of seg.segment(strEmojiCleaned)) {
            if (++graphemeCount > 6) break;
        }
    }
    const remainderIsEmojiOnly = !strEmojiCleaned || isEmojiOnly(strEmojiCleaned);
    const fEmojiOnly = graphemeCount > 0
        && graphemeCount <= 6
        && remainderIsEmojiOnly;

    const textSpan = _dmsgBuildText(msg, displayContent, fEmojiOnly, isGroupChat, currentChat, isRevealedBlockedMsg);
    if (textSpan && (textSpan.textContent || textSpan.querySelector('img,video,hr'))) {
        twemojify(textSpan);
        content.appendChild(textSpan);
    }

    // ---- Attachments --------------------------------------------------------
    // The wrapper is appended unconditionally when attachments exist — image
    // previews / file-boxes / spinners often arrive via async paths
    // (generate_thumbhash_preview, etc.), so we can't gate on childNodes.length
    // at this point. Doing so detaches the wrapper before the async path fires
    // and the message renders blank — particularly for images sent from clients
    // that don't ship a thumbhash (e.g. 0xChat).
    if (msg.attachments?.length) {
        const attachmentsDiv = document.createElement('div');
        attachmentsDiv.classList.add('dmsg-attachments');
        _dmsgBuildAttachments(attachmentsDiv, msg, sender, isGroupChat, isRevealedBlockedMsg);
        content.appendChild(attachmentsDiv);
    }

    // ---- Crypto address shortcut --------------------------------------------
    const cAddress = detectCryptoAddress(msg.content);
    if (cAddress) {
        content.appendChild(renderCryptoAddress(cAddress));
    }

    // ---- Emoji pack preview (NIP-19 naddr → NIP-30 kind 30030) -------------
    if (msg.content && typeof renderEmojiPackPreviews === 'function') {
        renderEmojiPackPreviews(content, msg.content);
    }

    // ---- Community invite card (vectorapp.io/invite share links) -----------
    if (msg.content && typeof renderCommunityInvitePreviews === 'function') {
        renderCommunityInvitePreviews(content, msg.content);
    }


    // ---- URL-shared Mini App -------------------------------------------------
    // An .xdc link renders as a playable game card (same file-box pipeline as
    // an uploaded Mini App); it also supersedes the OpenGraph card below.
    const xdcUrl = (!msg.pending && !msg.failed && !isRevealedBlockedMsg) ? findXdcUrl(msg.content) : null;
    if (xdcUrl) {
        const xdcTarget = document.createElement('div');
        content.appendChild(xdcTarget);
        renderXdcUrlCard(xdcTarget, msg, xdcUrl);
    }

    // ---- Link preview (OpenGraph) ------------------------------------------
    // A vectorapp.io profile link renders as a mention pill; an OpenGraph
    // card for the same URL would be redundant.
    const skipWebPreview = /https?:\/\/vectorapp\.io\/profile\/npub1[a-z0-9]{58}/i.test(msg.content || '');
    if (!msg.pending && !msg.failed && fWebPreviewsEnabled && !skipWebPreview && !xdcUrl && !isRevealedBlockedMsg) {
        const previewEl = _dmsgBuildLinkPreview(msg);
        if (previewEl) content.appendChild(previewEl);
    }

    // ---- Edited indicator ---------------------------------------------------
    if (msg.edited) {
        content.appendChild(_dmsgBuildEditedIndicator(msg));
    }

    // ---- Status indicator (own messages only) -------------------------------
    if (msg.mine) {
        content.appendChild(_dmsgBuildStatus(msg));
    }

    // ---- Self-Destruct Timer glyph (per-message NIP-40 expiry) --------------
    if (msg.expiration) {
        content.appendChild(_dmsgBuildSelfDestruct(msg));
    }

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

/** Build the subtle clock glyph on a self-destruct (NIP-40) message. Hovering
 *  surfaces a live-ticking "dissolves in mm:ss" tooltip. */
function _dmsgBuildSelfDestruct(msg) {
    const el = document.createElement('span');
    el.className = 'dmsg-selfdestruct';
    el.dataset.expiration = String(msg.expiration); // unix seconds
    // Inline SVG (not an .icon): .icon is position:absolute;inset:0 and would
    // escape this unsized span, rendering nothing in the message row.
    el.innerHTML = '<svg viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M12 7.5V12l3 2"/></svg>';
    if (typeof platformFeatures !== 'undefined' && platformFeatures.os === 'android') {
        // Touch has no hover — show the countdown inline beside the clock. The
        // parent is nowrap inline-flex, so clock + time never split across lines.
        const t = document.createElement('span');
        t.className = 'dmsg-selfdestruct-time';
        const remaining = msg.expiration - Math.floor(Date.now() / 1000);
        t.textContent = remaining > 0 ? _fmtCountdown(remaining) : '';
        el.appendChild(t);
    } else {
        el.addEventListener('mouseenter', () => _selfDestructTooltip(el));
        el.addEventListener('mouseleave', _selfDestructTooltipEnd);
    }
    return el;
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

/** Format a countdown: mm:ss under an hour, else Hh Mm / Dd Hh. */
function _fmtCountdown(secs) {
    if (secs >= 86400) { const d = Math.floor(secs / 86400); const h = Math.floor((secs % 86400) / 3600); return d + 'd ' + h + 'h'; }
    if (secs >= 3600)  { const h = Math.floor(secs / 3600);  const m = Math.floor((secs % 3600) / 60);   return h + 'h ' + m + 'm'; }
    const m = Math.floor(secs / 60), s = secs % 60;
    return m + ':' + String(s).padStart(2, '0');
}

function _dmsgBuildReplyContext(msg, sender) {
    const hasBackendContext = msg.replied_to_content !== undefined || msg.replied_to_has_attachment;
    // The quoted message lives in the chat being rendered. `sender` is the row's
    // author, so a DM lookup by their npub finds nothing in a Community and the
    // quote loses its in-memory fallback entirely.
    const chat = getChat(strOpenChat) || (sender ? getDMChat(sender.id) : undefined);
    const cMsg = chat?.messages.find(m => m.id === msg.replied_to);

    if (!hasBackendContext && !cMsg) return null;

    const divRef = document.createElement('div');
    divRef.classList.add('dmsg-reply', 'btn');

    const repliedToMine = cMsg?.mine ?? (msg.replied_to_npub === strPubkey);
    if (!repliedToMine) divRef.classList.add('dmsg-reply-them');
    divRef.id = `r-${msg.replied_to}`;

    const spanName = document.createElement('span');
    spanName.style.color = `rgba(255, 255, 255, 0.7)`;

    // Resolve the replied-to sender's profile. In DMs the backend doesn't
    // populate `replied_to_npub`, so the only signal that the reply target was
    // the current user (vs. the counterpart) is `cMsg.mine` from the in-memory
    // message. Reuse the `repliedToMine` decision computed above instead of
    // falling through to `sender` (which is the counterpart, not me).
    let cSenderProfile;
    if (repliedToMine) {
        cSenderProfile = getProfile(strPubkey);
    } else if (msg.replied_to_npub) {
        cSenderProfile = getProfile(msg.replied_to_npub);
    } else if (cMsg && cMsg.npub) {
        cSenderProfile = getProfile(cMsg.npub);
    } else {
        // DM, replying to the counterpart's message.
        cSenderProfile = sender;
    }

    // npub of the replied-to author — drives the small avatar + lets the profile_update handler
    // retro-resolve both name and avatar once the profile lands.
    const repliedToNpub = repliedToMine ? strPubkey
        : (msg.replied_to_npub || cMsg?.npub || cSenderProfile?.id || '');
    spanName.classList.add('dmsg-reply-name');
    if (repliedToNpub) spanName.dataset.npub = repliedToNpub;

    if (cSenderProfile?.nickname || cSenderProfile?.name || cSenderProfile?.display_name) {
        spanName.textContent = cSenderProfile.nickname || cSenderProfile.name || cSenderProfile.display_name;
        twemojify(spanName);
    } else {
        const fallbackId = (hasBackendContext ? msg.replied_to_npub : cMsg?.npub) || cSenderProfile?.id || '';
        spanName.textContent = fallbackId ? fallbackId.substring(0, 10) + '…' : 'Unknown';
    }

    let spanRef;
    const replyContent = hasBackendContext ? msg.replied_to_content : cMsg?.content;
    const hasAttachment = hasBackendContext ? msg.replied_to_has_attachment : cMsg?.attachments?.length > 0;

    if (replyContent) {
        spanRef = document.createElement('span');
        spanRef.classList.add('dmsg-reply-text');
        spanRef.style.color = `rgba(255, 255, 255, 0.45)`;
        spanRef.innerHTML = buildReplyPreviewHtml(replyContent);
        twemojify(spanRef);
        // Inline custom emojis in the quoted reply, matching in-chat rendering.
        // The message's tags can lose a hydration race on first paint and a
        // painted row never self-corrects, so the user's own equipped packs
        // backstop them. Message tags come LAST: the renderer's map keeps the
        // last entry per shortcode, so the sender's mapping wins conflicts.
        const msgTags = (cMsg?.emoji_tags?.length ? cMsg.emoji_tags : null)
            || msg.replied_to_emoji_tags || [];
        const equipped = (typeof equippedEmojiTags === 'function') ? equippedEmojiTags() : [];
        const replyEmojiTags = [...equipped, ...msgTags];
        if (replyEmojiTags.length && typeof renderCustomEmojiShortcodes === 'function') {
            renderCustomEmojiShortcodes(spanRef, replyEmojiTags);
        }
    } else if (hasAttachment) {
        spanRef = document.createElement('div');
        spanRef.style.display = 'flex';
        spanRef.style.alignItems = 'center';   // vertically center the type icon with its label
        // Prefer the backend-resolved extension for off-screen targets (cMsg is null then).
        const attachmentExt = (hasBackendContext ? msg.replied_to_attachment_extension : null) || cMsg?.attachments?.[0]?.extension;
        const cFileType = attachmentExt ? getFileTypeInfo(attachmentExt) : { icon: 'attachment', description: 'Attachment' };

        const spanIcon = document.createElement('span');
        spanIcon.classList.add('icon', 'icon-' + cFileType.icon);
        spanIcon.style.position = 'relative';
        spanIcon.style.backgroundColor = 'rgba(255, 255, 255, 0.45)';
        spanIcon.style.width = '18px';
        spanIcon.style.height = '18px';
        spanIcon.style.margin = '0px';

        const spanDesc = document.createElement('span');
        spanDesc.style.color = 'rgba(255, 255, 255, 0.45)';
        spanDesc.style.marginLeft = '5px';
        spanDesc.textContent = cFileType.description;

        spanRef.append(spanIcon, spanDesc);
    }

    // Avatar + name + snippet on ONE line (Discord-style one-liner). The snippet ellipsis-truncates
    // to the message width; see .dmsg-reply / .dmsg-reply-snippet. Cached/asset-only avatar src; the
    // profile_update handler swaps in the real one when the image lands.
    const replyAvatar = createAvatarImg(getProfileAvatarSrc(cSenderProfile), 16);
    replyAvatar.classList.add('dmsg-reply-avatar');
    if (repliedToNpub) replyAvatar.dataset.npub = repliedToNpub;
    if (spanRef) spanRef.classList.add('dmsg-reply-snippet');

    // Name + avatar open the replied-to author's mini profile; clicking anywhere else on the quote
    // (elbow, snippet) bubbles to the row's jump-to-message handler.
    if (repliedToNpub) {
        const openReplyProfile = (e) => { e.stopPropagation(); showMiniProfile(repliedToNpub, e.currentTarget); };
        spanName.addEventListener('click', openReplyProfile);
        replyAvatar.addEventListener('click', openReplyProfile);
    }

    divRef.appendChild(replyAvatar);
    divRef.appendChild(spanName);
    if (spanRef) divRef.appendChild(spanRef);

    return divRef;
}

/**
 * Fill in the reply strips of rows that quote `parentId`, now that it has arrived.
 * Rows render their quote once and never retry, so without this a reply delivered
 * ahead of its parent stays context-less until the chat is reopened.
 */
function backfillReplyContext(parentId) {
    if (!parentId) return;
    const rows = document.querySelectorAll(`[data-reply-pending="${CSS.escape(parentId)}"]`);
    if (!rows.length) return;
    const chat = getChat(strOpenChat);
    let inserted = false;
    for (const row of rows) {
        const msg = chat?.messages.find(m => m.id === row.id);
        if (!msg) continue;
        const replyDiv = _dmsgBuildReplyContext(msg, getProfile(msg.npub));
        if (!replyDiv) continue;
        const body = row.querySelector('.dmsg-body');
        if (!body) continue;
        body.insertBefore(replyDiv, body.firstChild);
        row.classList.add('dmsg--has-reply');
        delete row.dataset.replyPending;
        inserted = true;
    }
    // A strip is a row that grew after layout — same class as a media load or the
    // unread divider, so it goes through the same compensator or the view drifts
    // off the bottom. Only when something actually landed: an unconditional call
    // would soft-scroll on every arriving message.
    if (inserted) compensateChatScrollForResize();
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

/**
 * Render every attachment (image / audio / video / file) into `target`.
 * Branches on per-attachment state: `downloaded` (immediate display),
 * `downloading` (thumbhash + download spinner), and undownloaded (thumbhash
 * + download button OR auto-download trigger if size is within limit).
 */
function _dmsgBuildAttachments(target, msg, sender, isGroupChat, isRevealedBlockedMsg) {
    for (const cAttachment of msg.attachments) {
        if (cAttachment.downloaded) {
            const assetUrl = convertFileSrc(cAttachment.path);

            if (['png', 'jpeg', 'jpg', 'gif', 'webp', 'svg', 'bmp', 'tiff', 'tif', 'ico'].includes(cAttachment.extension)) {
                _dmsgRenderImageAttachment(target, msg, sender, isGroupChat, cAttachment, assetUrl);
            } else if (['wav', 'mp3', 'flac', 'aac', 'm4a', 'ogg'].includes(cAttachment.extension)) {
                handleAudioAttachment(cAttachment, target, msg);
            } else if (platformFeatures.os !== 'linux' && ['mp4', 'webm', 'mov'].includes(cAttachment.extension)) {
                _dmsgRenderVideoAttachment(target, cAttachment);
            } else {
                _dmsgRenderFileAttachment(target, msg, cAttachment);
            }

            if (msg.mine && msg.pending) {
                _dmsgAttachUploadProgress(target, msg);
            }
        } else if (cAttachment.downloading || downloadingAttachmentIds.has(cAttachment.id)) {
            _dmsgRenderDownloadingAttachment(target, msg, sender, isGroupChat, cAttachment);
        } else {
            _dmsgRenderUndownloadedAttachment(target, msg, sender, isGroupChat, cAttachment, isRevealedBlockedMsg);
        }
    }
}

/**
 * Size a thumbhash placeholder <img> to the SAME box its real image will occupy — fit within
 * 450×350 preserving aspect, matching `.dmsg-image-attachment`. So the real image swaps in with zero
 * resize (no jagged jump), and a centered download/upload spinner lands on the actual image rather
 * than a full-bleed blur. Driven by width/height attrs + aspect-ratio because the thumbhash's tiny
 * intrinsic size means CSS `width:auto` would collapse it back to ~32px.
 */
function _sizeThumbhashPlaceholder(imgEl, imgMeta) {
    if (imgMeta && imgMeta.width && imgMeta.height) {
        const iw = imgMeta.width, ih = imgMeta.height;
        const scale = Math.min(450 / iw, 350 / ih, 1);
        imgEl.width = Math.round(iw * scale);
        imgEl.height = Math.round(ih * scale);
        imgEl.style.aspectRatio = `${iw} / ${ih}`;
    }
    imgEl.style.maxWidth = 'min(100%, 450px)';
    imgEl.style.maxHeight = '350px';
    imgEl.style.height = 'auto';
    imgEl.style.borderRadius = '8px';
}

function _dmsgRenderImageAttachment(target, msg, sender, isGroupChat, cAttachment, assetUrl) {
    const imgContainer = document.createElement('div');
    imgContainer.style.position = 'relative';
    imgContainer.style.display = 'inline-block';

    if (isSpoilerAttachment(cAttachment)) {
        const spoilerNpub = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
        invoke('generate_thumbhash_preview', { npub: spoilerNpub, msgId: msg.id })
            .then(base64Image => {
                // Bail out if the chat was switched mid-flight: target became
                // detached during openChat()'s clear-children sweep. Without
                // this guard, the image still loads + appends to a detached
                // tree, leaks memory, and fires scroll callbacks against the
                // current chat from a stale render's load event.
                if (!target.isConnected) return;
                const imgPreview = document.createElement('img');
                imgPreview.className = 'spoiler-img';
                if (cAttachment.img_meta) {
                    // Pre-scale the placeholder dimensions to match what the
                    // revealed image will display at (fit within 450×350,
                    // preserve ratio). Without this, the thumbhash placeholder
                    // would render at 450×350 (squashed) while the revealed
                    // image fits the box correctly, and the spoiler appears
                    // wider than the real image.
                    const iw = cAttachment.img_meta.width;
                    const ih = cAttachment.img_meta.height;
                    const scale = Math.min(450 / iw, 350 / ih, 1);
                    imgPreview.width = Math.round(iw * scale);
                    imgPreview.height = Math.round(ih * scale);
                    imgPreview.style.aspectRatio = `${iw} / ${ih}`;
                }
                imgPreview.style.maxWidth = '100%';
                imgPreview.style.height = 'auto';
                imgPreview.style.borderRadius = '8px';
                imgPreview.src = base64Image;
                imgPreview.addEventListener('load', () => {
                    if (proceduralScrollState.isLoadingOlderMessages) correctScrollForMediaLoad();
                    else softChatScroll();
                }, { once: true });
                imgContainer.appendChild(imgPreview);

                if (msg.mine && msg.pending) {
                    imgPreview.style.opacity = '0.25';
                    const uploadOverlay = document.createElement('div');
                    uploadOverlay.className = 'attachment-progress-overlay';
                    const spinner = document.createElement('div');
                    spinner.className = 'miniapp-downloading-spinner';
                    spinner.id = msg.id + '_file';
                    spinner.style.width = '48px';
                    spinner.style.height = '48px';
                    applyPendingUploadProgress(spinner, msg.id);
                    uploadOverlay.appendChild(spinner);
                    const cancelBtn = document.createElement('div');
                    cancelBtn.className = 'upload-cancel-btn';
                    cancelBtn.addEventListener('click', (e) => {
                        e.stopPropagation();
                        invoke('cancel_upload', { pendingId: msg.id });
                    });
                    uploadOverlay.appendChild(cancelBtn);
                    imgContainer.appendChild(uploadOverlay);
                } else {
                    const overlay = document.createElement('div');
                    overlay.className = 'spoiler-overlay';
                    overlay.innerHTML = '<span class="icon icon-eye-off"></span><span class="spoiler-label">Spoiler</span>';
                    imgContainer.appendChild(overlay);
                    overlay.addEventListener('click', () => {
                        const realUrl = convertFileSrc(cAttachment.path);
                        imgPreview.src = realUrl;
                        imgPreview.classList.remove('spoiler-img');
                        imgPreview.style.aspectRatio = '';
                        overlay.remove();
                        attachImagePreview(imgPreview);
                    }, { once: true });
                }
            })
            .catch(() => {
                if (!target.isConnected) return;
                const imgPreview = document.createElement('img');
                imgPreview.style.maxWidth = '100%';
                imgPreview.style.height = 'auto';
                imgPreview.style.borderRadius = '8px';
                imgPreview.src = assetUrl;
                attachImagePreview(imgPreview);
                imgContainer.appendChild(imgPreview);
            });

        attachFileExtBadge(null, imgContainer, cAttachment.extension);
        if (msg.mine && msg.pending) imgContainer.dataset.spoilerUpload = '1';
        target.appendChild(imgContainer);
        return;
    }

    const imgPreview = document.createElement('img');
    if (cAttachment.extension === 'svg') {
        imgPreview.setAttribute('data-attachment-type', 'svg');
        imgPreview.style.width = '25vw';
    } else {
        // The CSS rule that fits the image in a 450×350 box (preserving
        // ratio for portrait shots) is scoped to this class so it doesn't
        // override sizing of unrelated `<img>`s in the attachment area
        // (audio cover art, mini-app icons, etc.).
        imgPreview.classList.add('dmsg-image-attachment');
        imgPreview.style.maxWidth = '100%';
    }
    imgPreview.style.height = 'auto';
    imgPreview.style.borderRadius = '8px';
    imgPreview.src = assetUrl;
    imgPreview.addEventListener('load', () => {
        // Bail if the row was detached during a chat-switch; firing scroll
        // adjustments against the new chat's viewport would be a regression.
        if (!imgPreview.isConnected) return;
        compensateChatScrollForResize();
    }, { once: true });
    attachImagePreview(imgPreview);
    imgContainer.appendChild(imgPreview);
    attachFileExtBadge(imgPreview, imgContainer, cAttachment.extension);
    target.appendChild(imgContainer);
}

function _dmsgRenderVideoAttachment(target, cAttachment) {
    const handleMetadataLoaded = (video) => {
        if (!video.isConnected) return;
        video.currentTime = 0.1;
        compensateChatScrollForResize();
    };

    const vidPreview = document.createElement('video');
    vidPreview.setAttribute('controlsList', 'nodownload');
    vidPreview.controls = true;
    // Width handled by CSS (max-width + auto so portrait videos can shrink
    // their width when max-height clamps, instead of squashing).
    vidPreview.style.height = 'auto';
    vidPreview.style.borderRadius = '8px';
    vidPreview.style.cursor = 'pointer';
    vidPreview.preload = 'metadata';
    vidPreview.playsInline = true;
    vidPreview.src = mediaUrl(cAttachment.path);
    vidPreview.addEventListener('loadedmetadata', () => {
        handleMetadataLoaded(vidPreview);
    }, { once: true });

    target.appendChild(vidPreview);
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

function _dmsgRenderDownloadingAttachment(target, msg, sender, isGroupChat, cAttachment) {
    if (['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'].includes(cAttachment.extension)) {
        const thumbhashNpub = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
        invoke('generate_thumbhash_preview', { npub: thumbhashNpub, msgId: msg.id })
            .then(base64Image => {
                if (!target.isConnected) return;
                const imgPreview = document.createElement('img');
                _sizeThumbhashPlaceholder(imgPreview, cAttachment.img_meta);
                imgPreview.style.opacity = '0.7';
                imgPreview.src = base64Image;
                imgPreview.addEventListener('load', () => {
                    if (proceduralScrollState.isLoadingOlderMessages) correctScrollForMediaLoad();
                    else softChatScroll();
                }, { once: true });

                // inline-block + line-height:0 so the container shrink-wraps the (correctly-sized)
                // placeholder exactly — the absolutely-positioned spinner overlay then centers on the
                // image, not a full-width box (and with no inline descender gap nudging it down).
                const container = document.createElement('div');
                container.style.position = 'relative';
                container.style.display = 'inline-block';
                container.style.lineHeight = '0';
                container.appendChild(imgPreview);

                const dlOverlay = document.createElement('div');
                dlOverlay.className = 'attachment-progress-overlay';
                const dlSpinner = document.createElement('div');
                dlSpinner.className = 'miniapp-downloading-spinner';
                dlSpinner.setAttribute('data-attachment-id', cAttachment.id);
                dlSpinner.style.width = '48px';
                dlSpinner.style.height = '48px';
                dlOverlay.appendChild(dlSpinner);
                container.appendChild(dlOverlay);

                target.appendChild(container);
            })
            .catch(() => {
                if (!target.isConnected) return;
                const { fileDiv } = createFileBox(cAttachment, 'downloading');
                target.appendChild(fileDiv);
            });
    } else {
        const { fileDiv } = createFileBox(cAttachment, 'downloading');
        target.appendChild(fileDiv);
    }
}

/** Failed-download label: carries the backend's reason so a red box is diagnosable at a glance. */
function _dmsgDownloadFailedText(cAttachment) {
    const reason = (cAttachment.download_error || '').slice(0, 64);
    return reason ? `Failed: ${reason} · Tap to Retry` : 'Download Failed · Tap to Retry';
}

function _dmsgRenderUndownloadedAttachment(target, msg, sender, isGroupChat, cAttachment, isRevealedBlockedMsg) {
    const willAutoDownload = AUTO_DOWNLOAD_ENABLED && !isRevealedBlockedMsg && cAttachment.size > 0
        && cAttachment.size <= MAX_AUTO_DOWNLOAD_BYTES && !cAttachment.download_failed;

    if (['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'].includes(cAttachment.extension)) {
        const thumbhashNpub = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
        invoke('generate_thumbhash_preview', { npub: thumbhashNpub, msgId: msg.id })
            .then(base64Image => {
                if (!target.isConnected) return;
                const imgPreview = document.createElement('img');
                _sizeThumbhashPlaceholder(imgPreview, cAttachment.img_meta);
                imgPreview.style.opacity = willAutoDownload ? '0.8' : '0.6';
                imgPreview.src = base64Image;
                imgPreview.addEventListener('load', () => {
                    if (proceduralScrollState.isLoadingOlderMessages) correctScrollForMediaLoad();
                    else softChatScroll();
                }, { once: true });

                // inline-block so the container shrink-wraps the placeholder — the centered spinner
                // (auto-download) or Download button then lands on the image, not a full-width box.
                // (No line-height:0 here: this container also hosts the text Download button.)
                const container = document.createElement('div');
                container.style.position = 'relative';
                container.style.display = 'inline-block';
                container.appendChild(imgPreview);

                if (!willAutoDownload) {
                    let strSize = 'Unknown Size';
                    if (cAttachment.size > 0) strSize = formatBytes(cAttachment.size);
                    const iDownload = document.createElement('i');
                    iDownload.setAttribute('data-attachment-id', cAttachment.id);
                    iDownload.toggleAttribute('download', true);
                    const downloadNpub2 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                    iDownload.setAttribute('npub', downloadNpub2);
                    iDownload.setAttribute('msg', msg.id);
                    iDownload.classList.add('btn');
                    iDownload.textContent = cAttachment.download_failed
                        ? _dmsgDownloadFailedText(cAttachment)
                        : `Download ${cAttachment.extension.toUpperCase()} (${strSize})`;
                    iDownload.style.cssText = 'position:absolute;top:50%;left:50%;transform:translate(-50%,-50%);background-color:rgba(0,0,0,0.8);padding:8px 15px;border-radius:6px;color:white;cursor:pointer;font-size:12px;white-space:nowrap;text-align:center;max-width:90%;overflow:hidden;text-overflow:ellipsis;';
                    container.appendChild(iDownload);
                } else {
                    const adOverlay = document.createElement('div');
                    adOverlay.className = 'attachment-progress-overlay';
                    const adSpinner = document.createElement('div');
                    adSpinner.className = 'miniapp-downloading-spinner';
                    adSpinner.setAttribute('data-attachment-id', cAttachment.id);
                    adSpinner.style.width = '48px';
                    adSpinner.style.height = '48px';
                    adOverlay.appendChild(adSpinner);
                    container.appendChild(adOverlay);
                }

                target.appendChild(container);
            })
            .catch(() => {
                if (!target.isConnected) return;
                const fallbackState = willAutoDownload ? 'downloading' : 'download';
                const { fileDiv: fallbackDiv, statusSpan: fallbackStatus } = createFileBox(cAttachment, fallbackState);
                if (cAttachment.download_failed && fallbackStatus) {
                    fallbackStatus.innerText = _dmsgDownloadFailedText(cAttachment);
                }
                if (!willAutoDownload) {
                    fallbackDiv.addEventListener('click', () => {
                        startAttachmentDownload(cAttachment, msg, isGroupChat, strOpenChat, sender);
                    }, { once: true });
                }
                target.appendChild(fallbackDiv);
            });
    } else if (!willAutoDownload) {
        const { fileDiv: dlFileDiv, statusSpan: dlStatus } = createFileBox(cAttachment, 'download');
        if (cAttachment.download_failed && dlStatus) {
            dlStatus.innerText = _dmsgDownloadFailedText(cAttachment);
        }
        dlFileDiv.addEventListener('click', () => {
            startAttachmentDownload(cAttachment, msg, isGroupChat, strOpenChat, sender);
        }, { once: true });
        target.appendChild(dlFileDiv);
    }

    if (willAutoDownload) {
        // Dedupe — without this, every re-render of an undownloaded message
        // (e.g. when reactions arrive before the backend echoes downloading=true)
        // would re-fire `download_attachment`, flooding the backend.
        if (downloadingAttachmentIds.has(cAttachment.id)) return;
        downloadingAttachmentIds.add(cAttachment.id);
        if (!['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'].includes(cAttachment.extension)) {
            const { fileDiv: autoFileDiv } = createFileBox(cAttachment, 'downloading');
            target.appendChild(autoFileDiv);
        }
        const downloadNpub4 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
        invoke('download_attachment', { npub: downloadNpub4, msgId: msg.id, attachmentId: cAttachment.id });
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

function _dmsgBuildEditedIndicator(msg) {
    const span = document.createElement('span');
    span.classList.add('dmsg-edited');
    span.textContent = '(edited)';
    if (msg.edit_history && msg.edit_history.length > 0) {
        span.classList.add('btn');
        span.setAttribute('data-msg-id', msg.id);
        span.title = 'Click to view edit history';
    }
    return span;
}

function _dmsgBuildStatus(msg) {
    const statusEl = document.createElement('span');
    statusEl.classList.add('dmsg-status');
    if (msg.failed) {
        statusEl.classList.add('dmsg-status-failed');
        statusEl.textContent = 'Failed · ';
        const retryBtn = document.createElement('span');
        retryBtn.className = 'dmsg-failed-action';
        retryBtn.dataset.action = 'retry';
        retryBtn.textContent = 'Retry';
        statusEl.appendChild(retryBtn);
        statusEl.appendChild(document.createTextNode(' · '));
        const deleteBtn = document.createElement('span');
        deleteBtn.className = 'dmsg-failed-action';
        deleteBtn.dataset.action = 'delete';
        deleteBtn.textContent = 'Delete';
        statusEl.appendChild(deleteBtn);
    } else if (msg.pending) {
        statusEl.textContent = 'Sending...';
    } else {
        statusEl.innerHTML = 'Sent <span class="icon icon-check-circle"></span>';
    }
    return statusEl;
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
