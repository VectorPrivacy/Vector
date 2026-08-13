// Pinned Messages (Concord v2, CORD-04 §7).
//
// A pin is a proof, not a quote: the backend verifies every entry before it
// reaches this layer, so everything rendered here is proven author + words.
// The drawer slides open under the chat header; the header pin button only
// exists on v2 community channels.

// The open chat's pin context. `canPin` gates the unpin ✕ and the menu action.
let pinsCtx = { communityId: null, channelId: null, canPin: false };
// Verified pins for the open channel, keyed for the toolbar's Pin/Unpin flip.
let pinsCache = { pins: [], sealed: false, version: 0 };
let pinsDrawerOpen = false;

const domPinsBtn = document.getElementById('chat-pins-btn');
const domPinsDrawer = document.getElementById('pins-drawer');
const domPinsList = document.getElementById('pins-drawer-list');
const domPinsClose = document.getElementById('pins-drawer-close');

/// Resolve the pin context when a chat opens. Pins exist only on Concord v2
/// community channels (64-hex community id); everything else hides the button.
async function pinsOnChatOpened(chatId) {
    pinsSetDrawerVisible(false, true);
    pinsCtx = { communityId: null, channelId: null, canPin: false };
    pinsCache = { pins: [], sealed: false, version: 0 };
    domPinsBtn.style.display = 'none';

    const chat = arrChats.find(c => c.id === chatId);
    const communityId = chat?.chat_type === 'Community'
        ? chat.metadata?.custom_fields?.community_id
        : null;
    if (!communityId || communityId.length !== 64) {
        return;
    }
    pinsCtx = { communityId, channelId: chatId, canPin: false };

    // Capability + current pins, both non-blocking for the chat open.
    try {
        const caps = await invoke('get_community_capabilities', { communityId });
        pinsCtx.canPin = !!caps?.pin_messages;
    } catch (e) { /* affordances stay read-only */ }
    await pinsRefresh();
}

function pinsOnChatClosed() {
    pinsSetDrawerVisible(false, true);
    domPinsBtn.style.display = 'none';
    pinsCtx = { communityId: null, channelId: null, canPin: false };
}

/// Whether a pinned message can actually be jumped to on THIS device: on
/// screen, in the loaded cache, or in the local DB. A pin is proof a message
/// existed, not proof this device holds it (pre-join history, older epochs).
async function pinsResolveJumpable(rumorId) {
    if (document.getElementById(rumorId)) return true;
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (chat?.messages?.some(m => m.id === rumorId)) return true;
    try {
        // A zero-width window returns nothing, ANCHOR INCLUDED — the probe
        // needs at least one neighbour requested for the anchor to appear.
        const slice = await invoke('get_messages_around', {
            chatId: strOpenChat, anchorId: rumorId, before: 1, after: 1,
        });
        return Array.isArray(slice) && slice.some(m => m.id === rumorId);
    } catch (e) {
        return false;
    }
}

/// Re-fetch the open channel's pins from the locally folded head.
async function pinsRefresh() {
    if (!pinsCtx.channelId) return;
    try {
        pinsCache = await invoke('get_channel_pins', {
            communityId: pinsCtx.communityId,
            channelId: pinsCtx.channelId,
        });
    } catch (e) {
        console.error('Pins: fetch failed:', e);
        pinsCache = { pins: [], sealed: false, version: 0 };
    }
    // Jumpability resolved HERE (async) so the render stays sync and rows
    // never flicker between clickable and inert.
    await Promise.all((pinsCache.pins || []).map(async (p) => {
        p._jumpable = await pinsResolveJumpable(p.rumor_id);
    }));
    pinsUpdateButton();
    if (pinsDrawerOpen) pinsRenderDrawer();
}

/// The header button exists only where pins exist: a sealed list counts (the
/// pins are real, just unreadable here), a truly empty list hides it. The
/// FIRST pin is made from the message context menu, so no button is needed.
function pinsUpdateButton() {
    const show = !!pinsCtx.channelId && (pinsCache.pins.length > 0 || pinsCache.sealed);
    domPinsBtn.style.display = show ? '' : 'none';
}

function pinsSetDrawerVisible(visible, instant = false) {
    pinsDrawerOpen = visible;
    domPinsBtn.classList.toggle('pins-open', visible);
    // Dim/blur the conversation while the drawer owns the screen. Toggled at
    // close START so the unblur transitions alongside the drawer's slide-up.
    document.getElementById('chat')?.classList.toggle('pins-focus', visible);
    if (visible) {
        domPinsDrawer.classList.remove('pins-drawer-closing');
        domPinsDrawer.style.display = '';
        return;
    }
    if (instant || domPinsDrawer.style.display === 'none') {
        domPinsDrawer.classList.remove('pins-drawer-closing');
        domPinsDrawer.style.display = 'none';
        return;
    }
    // Animated close: play the slide-up, then hide. A reopen mid-close removes
    // the class (cancelling the animation), and the guard below keeps a stale
    // listener from hiding the reopened drawer.
    domPinsDrawer.classList.add('pins-drawer-closing');
    domPinsDrawer.addEventListener('animationend', () => {
        if (!pinsDrawerOpen) domPinsDrawer.style.display = 'none';
        domPinsDrawer.classList.remove('pins-drawer-closing');
    }, { once: true });
}

const PINS_ROW_SVG = `<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M8.3767 15.6163L2.71985 21.2732M11.6944 6.64181L10.1335 8.2027C10.0062 8.33003 9.94252 8.39369 9.86999 8.44427C9.80561 8.48917 9.73616 8.52634 9.66309 8.555C9.58077 8.58729 9.49249 8.60495 9.31592 8.64026L5.65145 9.37315C4.69915 9.56361 4.223 9.65884 4.00024 9.9099C3.80617 10.1286 3.71755 10.4213 3.75771 10.7109C3.8038 11.0434 4.14715 11.3867 4.83387 12.0735L11.9196 19.1592C12.6063 19.8459 12.9497 20.1893 13.2821 20.2354C13.5718 20.2755 13.8645 20.1869 14.0832 19.9928C14.3342 19.7701 14.4294 19.2939 14.6199 18.3416L15.3528 14.6771C15.3881 14.5006 15.4058 14.4123 15.4381 14.33C15.4667 14.2569 15.5039 14.1875 15.5488 14.1231C15.5994 14.0505 15.663 13.9869 15.7904 13.8596L17.3512 12.2987C17.4326 12.2173 17.4734 12.1766 17.5181 12.141C17.5578 12.1095 17.5999 12.081 17.644 12.0558C17.6936 12.0274 17.7465 12.0048 17.8523 11.9594L20.3467 10.8904C21.0744 10.5785 21.4383 10.4226 21.6035 10.1706C21.7481 9.95025 21.7998 9.68175 21.7474 9.42348C21.6875 9.12813 21.4076 8.84822 20.8478 8.28839L15.7047 3.14526C15.1448 2.58543 14.8649 2.30552 14.5696 2.24565C14.3113 2.19329 14.0428 2.245 13.8225 2.38953C13.5705 2.55481 13.4145 2.91866 13.1027 3.64636L12.0337 6.14071C11.9883 6.24653 11.9656 6.29944 11.9373 6.34905C11.9121 6.39313 11.8836 6.43522 11.852 6.47496C11.8165 6.51971 11.7758 6.56041 11.6944 6.64181Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>`;

function pinsFormatDate(ms) {
    const d = new Date(ms);
    const pad = (n) => String(n).padStart(2, '0');
    return `${pad(d.getMonth() + 1)}/${pad(d.getDate())}/${d.getFullYear()}`;
}

function pinsRenderDrawer() {
    domPinsList.innerHTML = '';

    if (pinsCache.sealed) {
        const notice = document.createElement('div');
        notice.className = 'pins-drawer-notice';
        notice.textContent = "This channel's pins are protected by a key you don't hold yet.";
        domPinsList.appendChild(notice);
        return;
    }
    if (!pinsCache.pins.length) {
        const notice = document.createElement('div');
        notice.className = 'pins-drawer-notice';
        notice.textContent = 'No pinned messages yet.';
        domPinsList.appendChild(notice);
        return;
    }

    for (const pin of pinsCache.pins) {
        const row = document.createElement('div');
        row.className = 'pins-drawer-row';
        // File pins lead with their TYPE icon (the chat's own vocabulary);
        // everything else leads with the pin.
        const typeIcon = pinsAttachmentTypeIcon(pin);
        if (typeIcon) {
            row.innerHTML = `<span class="icon icon-${typeIcon} pins-drawer-row-type-icon"></span>`;
        } else {
            row.innerHTML = PINS_ROW_SVG;
        }

        // Controls FLOAT top-right inside the text flow: collapsed rows look
        // unchanged, but an expanded pin's lines wrap around them and reclaim
        // the FULL drawer width below the first line — no dead right gutter,
        // which matters most on narrow mobile widths.
        const text = document.createElement('div');
        text.className = 'pins-drawer-row-text';
        const controls = document.createElement('span');
        controls.className = 'pins-drawer-row-controls';
        const body = document.createElement('span');
        body.className = 'pins-drawer-row-body';
        text.append(controls, body);
        pinsRenderCollapsedContent(body, pin);
        row.appendChild(text);

        const date = document.createElement('span');
        date.className = 'pins-drawer-row-date';
        date.textContent = pinsFormatDate(pin.ms);
        controls.appendChild(date);

        if (pinsCtx.canPin) {
            const unpin = document.createElement('span');
            unpin.className = 'icon icon-x pins-drawer-row-unpin btn';
            unpin.title = 'Unpin';
            unpin.addEventListener('click', (e) => {
                e.stopPropagation();
                pinsUnpin(pin.rumor_id);
            });
            controls.appendChild(unpin);
        }
        row.dataset.rumorId = pin.rumor_id;
        // A row without its message on this device offers no jump: no pointer,
        // no dead click. (If it turns out truncated, the pass below rewires
        // the click to expansion instead — reading it is all it can offer.)
        if (pin._jumpable) {
            row.addEventListener('click', (e) => {
                if (pinsClickIsInteractive(e)) return;
                pinsJumpTo(pin.rumor_id);
            });
        } else {
            row.classList.add('pins-no-jump');
        }
        domPinsList.appendChild(row);
    }

    // Show-more chevrons, only where the text actually clips. A pin is often
    // the ONLY copy a member can read (the original may predate their DB), so
    // every pin must be fully readable inside the drawer itself. Runs after
    // the rows are in layout — the drawer is visible whenever this renders.
    for (const row of domPinsList.querySelectorAll('.pins-drawer-row')) {
        const text = row.querySelector('.pins-drawer-row-text');
        if (!text) continue;
        // The expander appears whenever expansion would SHOW more: a clipped
        // first line, further displayable lines beyond the one-line preview
        // (pixels can't detect those — the preview is only line one), or
        // previewable media (expansion renders it). A non-previewable file
        // gets no expander: its Reveal/Open affordance rides the collapsed
        // row, so expanding would reveal nothing.
        const rowPin = pinsCache.pins.find(p => p.rumor_id === row.dataset.rumorId);
        const hasMedia = !!rowPin && !!pinsPreviewableMedia(rowPin);
        const source = rowPin ? (rowPin.edited?.content ?? rowPin.content) : '';
        const hasMoreLines = source.split('\n').filter(l => l.trim()).length > 1;
        if (!pinsLineClips(text) && !hasMedia && !hasMoreLines) continue;
        const expander = document.createElement('span');
        expander.className = 'icon icon-chevron-down pins-drawer-row-expander btn';
        expander.title = 'Show more';
        expander.addEventListener('click', (e) => {
            e.stopPropagation();
            pinsToggleRowExpanded(row, text);
        });
        const controls = row.querySelector('.pins-drawer-row-controls');
        controls.insertBefore(expander, row.querySelector('.pins-drawer-row-unpin'));
        // No jump to offer, but there IS more to read: the whole row becomes
        // the expand toggle.
        if (row.classList.contains('pins-no-jump')) {
            row.classList.add('pins-expandable');
            row.addEventListener('click', (e) => {
                if (pinsClickIsInteractive(e)) return;
                pinsToggleRowExpanded(row, text);
            });
        }
    }

    // Non-previewable file pins get the file's OWN affordance in the same
    // slot: fetch (the pin-only path — works with zero chat state), then
    // reveal in the file manager (desktop) or open via the system chooser
    // (Android) — the chat's exact attachment actions.
    for (const row of domPinsList.querySelectorAll('.pins-drawer-row')) {
        const rowPin = pinsCache.pins.find(p => p.rumor_id === row.dataset.rumorId);
        if (!rowPin || !pinsAttachmentChips(rowPin).length || pinsPreviewableMedia(rowPin)) continue;
        const isAndroid = platformFeatures?.os === 'android';
        const open = document.createElement('span');
        open.className = 'icon icon-file-search pins-drawer-row-open btn';
        open.title = isAndroid ? 'Open file' : 'Reveal in folder';
        open.addEventListener('click', async (e) => {
            e.stopPropagation();
            try {
                const att = await invoke('fetch_pinned_attachment', {
                    communityId: pinsCtx.communityId,
                    channelId: pinsCtx.channelId,
                    messageId: rowPin.rumor_id,
                });
                if (!att?.path) return;
                if (isAndroid) {
                    await openAndroidAttachment(att.path);
                } else {
                    revealItemInDir(att.path);
                }
            } catch (err) {
                showToast(String(err));
            }
        });
        const controls = row.querySelector('.pins-drawer-row-controls');
        controls.insertBefore(open, row.querySelector('.pins-drawer-row-unpin'));
    }
}

/// Ride an expansion for its animation window, scrolling the list just enough
/// to keep the growing row's bottom in view — expanding the bottom pin stays
/// grounded instead of spilling below the fold (Scroll → Expand → Scroll
/// again). Clamped to the row's own top: a taller-than-viewport pin keeps its
/// first line on screen, since that's where reading starts. Collapse needs no
/// twin — the browser clamps scrollTop as the list shrinks.
function pinsFollowExpansion(row) {
    const list = domPinsList;
    const start = performance.now();
    const step = () => {
        const rowBottom = row.offsetTop + row.offsetHeight;
        const viewBottom = list.scrollTop + list.clientHeight;
        if (rowBottom > viewBottom) {
            list.scrollTop = Math.max(list.scrollTop, Math.min(rowBottom - list.clientHeight, row.offsetTop));
        }
        if (performance.now() - start < 330) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
}

/// Clicks on interactive content inside a pin (links, spoilers, mentions,
/// code) belong to that content, not to the row: links bubble to the
/// document-level anti-phish opener, spoilers to the document-level revealer,
/// mention pills to their own handlers, copy buttons to the document-level
/// copier, and inline code to the pins copier below. The row must not jump or
/// collapse underneath any of them.
function pinsClickIsInteractive(e) {
    return !!e.target.closest?.('a, .spoiler, .mention, code, pre, .code-copy-btn');
}

// Inline code in a pin copies itself on click (chat offers no affordance for
// this, but a pin is a reference document — relay URLs, keys, commands are
// exactly what gets pinned). Fenced blocks keep their own copy button, so the
// fence body stays neutral. Delegated: survives every drawer re-render.
domPinsList.addEventListener('click', (e) => {
    const code = e.target.closest?.('code');
    if (!code || code.closest('pre') || !domPinsList.contains(code)) return;
    e.stopPropagation();
    navigator.clipboard.writeText(code.textContent).then(
        () => showToast('Copied to clipboard'),
        () => showToast('Copy failed'),
    );
});

/// The pin's proven NIP-30 custom-emoji pairs, from the rumor's own tags.
function pinsEmojiTags(pin) {
    return (pin.tags || [])
        .filter(t => Array.isArray(t) && t[0] === 'emoji' && t[1] && t[2])
        .map(t => ({ shortcode: t[1], url: t[2] }));
}

/// Attachment chips from the pin's proven NIP-92 imeta tags. A pin references
/// the attachment, it doesn't embed it — the bytes are encrypted on media
/// servers and may not even be downloaded here — so the drawer shows an
/// honest labeled chip and the jump (when available) lands on the real thing.
function pinsAttachmentChips(pin) {
    return (pin.tags || [])
        .filter(t => Array.isArray(t) && t[0] === 'imeta')
        .map(t => {
            const fields = {};
            for (const s of t.slice(1)) {
                const i = String(s).indexOf(' ');
                if (i > 0) fields[s.slice(0, i)] = s.slice(i + 1);
            }
            const mime = fields.m || '';
            const name = fields.name || (mime ? mime.split('/')[1] : '') || 'file';
            const ext = (name.includes('.') ? name.split('.').pop() : mime.split('/')[1] || '').toLowerCase();
            return { name, ext };
        });
}

/// The chat's own file-type icon for an attachment pin's header slot — the
/// same `getFileTypeInfo` vocabulary in-chat placeholders use (image, film,
/// mic-on, file, …), so a pinned file reads identically everywhere.
function pinsAttachmentTypeIcon(pin) {
    const first = pinsAttachmentChips(pin)[0];
    if (!first) return null;
    return (typeof getFileTypeInfo === 'function' && getFileTypeInfo(first.ext)?.icon) || 'file';
}

/// Append the attachment's KIND (if any) to a rendered pin body — the same
/// wording the in-chat reply context uses ("Picture", "GIF Animation", …),
/// bold, plain. The row's leading icon already tells the type visually.
function pinsAppendAttachmentKinds(el, pin) {
    for (const chip of pinsAttachmentChips(pin)) {
        const b = document.createElement('b');
        b.className = 'pins-drawer-row-kind';
        b.textContent = (typeof getFileTypeInfo === 'function' && chip.ext)
            ? getFileTypeInfo(chip.ext).description
            : 'Attachment';
        el.appendChild(b);
    }
}

/// Whether the pin references media the drawer could preview inline.
function pinsPreviewableMedia(pin) {
    const mime = (pin.tags || [])
        .filter(t => Array.isArray(t) && t[0] === 'imeta')
        .flatMap(t => t.slice(1))
        .map(String)
        .find(s => s.startsWith('m '));
    if (!mime) return null;
    const m = mime.slice(2);
    if (m.startsWith('image/')) return 'image';
    if (m.startsWith('video/')) return 'video';
    return null;
}

/// Swap a media pin's chip for the real thing. The backend resolves FROM THE
/// PIN ALONE (§7): serve the verified local copy if one exists, else download
/// the blob and decrypt it with the imeta-carried keys — so media pins render
/// fully even for members with no chat history and no old epoch key.
/// Async on purpose: the chip renders instantly, the media upgrades it.
async function pinsUpgradeMediaPreview(row, body, pin, kind) {
    let att = null;
    try {
        att = await invoke('fetch_pinned_attachment', {
            communityId: pinsCtx.communityId,
            channelId: pinsCtx.channelId,
            messageId: pin.rumor_id,
        });
    } catch (e) {
        console.warn('Pins: media fetch failed:', e);
        return; // the chip stays — an honest reference
    }
    // The drawer may have re-rendered or collapsed while we fetched.
    if (!att?.path || !row.classList.contains('pins-expanded') || !body.isConnected) return;
    const src = convertFileSrc(att.path);
    const media = kind === 'video'
        ? Object.assign(document.createElement('video'), { src, controls: true, preload: 'metadata' })
        : Object.assign(document.createElement('img'), { src, alt: '' });
    media.className = 'pins-drawer-row-media';
    if (kind === 'image') {
        // The chat's fullscreen previewer, verbatim: pointer styling, its own
        // stopPropagation (the row never jumps under it), zoom/rotate included.
        attachImagePreview(media);
    } else {
        // Video owns its taps (controls); just keep the row from jumping.
        media.addEventListener('click', (e) => e.stopPropagation());
    }
    // The media arrives AFTER the row's expand animation settled (async fetch),
    // so its appearance drives its own growth: measure, insert, animate to the
    // new height on load — the image slides in instead of popping the layout.
    media.addEventListener(kind === 'video' ? 'loadedmetadata' : 'load', () => {
        const row2 = body.closest('.pins-drawer-row');
        if (!row2?.classList.contains('pins-expanded')) return;
        const text = row2.querySelector('.pins-drawer-row-text');
        const from = text.getBoundingClientRect().height;
        text.style.maxHeight = `${from}px`;
        requestAnimationFrame(() => {
            text.style.maxHeight = `${text.scrollHeight}px`;
            text.addEventListener('transitionend', () => {
                if (row2.classList.contains('pins-expanded')) text.style.maxHeight = 'none';
            }, { once: true });
        });
        pinsFollowExpansion(row2);
    }, { once: true });
    body.appendChild(media);
}

/// Does the collapsed line actually clip? WKWebView's scrollWidth on an
/// ellipsized block omits overflow from PLAIN TEXT runs (only inline-blocks
/// and images count), so a text-only line can paint "…" while measuring as
/// fitting. Measure the body's true width with an unconstrained offscreen
/// clone instead, against the space the floated controls leave it.
function pinsLineClips(text) {
    const body = text.querySelector('.pins-drawer-row-body');
    if (!body) return text.scrollWidth > text.clientWidth;
    const clone = body.cloneNode(true);
    clone.style.cssText = 'position:absolute; visibility:hidden; white-space:nowrap; width:auto; max-width:none;';
    text.appendChild(clone);
    const trueWidth = clone.getBoundingClientRect().width;
    clone.remove();
    const controls = text.querySelector('.pins-drawer-row-controls');
    const avail = text.clientWidth - (controls?.getBoundingClientRect().width || 0);
    return trueWidth > avail + 1;
}

/// The first line a reader would actually SEE in chat: blank lines, `---` rules
/// and code-fence markers render as structure, not text, so they never lead a
/// preview — without this the collapsed line glues line 2 onto the title.
function pinsFirstDisplayLine(source) {
    for (const raw of source.split('\n')) {
        const line = raw.trim();
        if (!line) continue;
        if (/^(-{3,}|\*{3,}|_{3,})$/.test(line)) continue;
        if (line.startsWith('```')) continue;
        return line;
    }
    return source;
}

/// COLLAPSED row content: the chat-list preview treatment — one escaped line,
/// inline markdown, non-interactive spoilers, mentions resolved to names.
function pinsRenderCollapsedContent(text, pin) {
    const source = pinsFirstDisplayLine(pin.edited?.content ?? pin.content);
    text.innerHTML = contentToPreviewHtml(resolveMentionText(source));
    // Bare URLs stay clickable even collapsed — a link is often the entire
    // point of a pin. Row clicks already defer to anchors.
    linkifyUrls(text);
    twemojify(text);
    const emojiTags = pinsEmojiTags(pin);
    if (emojiTags.length) renderCustomEmojiShortcodes(text, emojiTags);
    pinsAppendAttachmentKinds(text, pin);
}

/// EXPANDED row content: the REAL message renderer — the same pipeline as an
/// in-chat message row, so linebreaks, `---` rules, headers, code blocks,
/// clickable spoilers, mention pills, and hyperlinks (with the document-level
/// anti-phish guard + tooltips) all behave exactly as they do in chat. A pin
/// must be fully consumable in the drawer: the original message may not exist
/// on this device at all.
function pinsRenderExpandedContent(text, pin) {
    const source = pin.edited?.content ?? pin.content;
    text.innerHTML = parseMarkdown(source);
    linkifyUrls(text);
    renderMentions(text, false, { allowBare: true });
    const emojiTags = pinsEmojiTags(pin);
    if (emojiTags.length) renderCustomEmojiShortcodes(text, emojiTags);
    twemojify(text);
    pinsAppendAttachmentKinds(text, pin);
    // Previewable media upgrades its chip to the real thing when a local
    // decrypted copy exists (chip-first: the resolve is async).
    const kind = pinsPreviewableMedia(pin);
    if (kind) {
        const row = text.closest('.pins-drawer-row');
        if (row) pinsUpgradeMediaPreview(row, text, pin, kind);
    }
}

/// Smoothly grow a row to its full wrapped height, or shrink it back to the
/// one-line ellipsis. The white-space flip happens at the START of a grow and
/// the END of a shrink, so the clip always animates rather than snaps.
function pinsToggleRowExpanded(row, text) {
    const pin = pinsCache.pins.find(p => p.rumor_id === row.dataset.rumorId);
    // Renderers write the BODY span; the outer block (controls float + body)
    // is what the height animation drives.
    const body = row.querySelector('.pins-drawer-row-body');
    // The chevron flips at INTERACTION time in both directions — it signals
    // the user's choice, not the animation's progress.
    const chevron = row.querySelector('.pins-drawer-row-expander');
    if (!row.classList.contains('pins-expanded')) {
        // The animation anchors to the MEASURED collapsed height (and returns
        // to it on collapse): a hardcoded floor below the real line height
        // squeezes the line box for a frame, which reads as a pixel jump.
        const collapsedH = text.getBoundingClientRect().height;
        row.dataset.collapsedH = String(collapsedH);
        chevron?.classList.add('pins-open');
        row.classList.add('pins-expanded');
        // Swap to the full message renderer BEFORE measuring, so the height
        // animates to the real rendered size (paragraphs, rules, code blocks).
        if (pin && body) pinsRenderExpandedContent(body, pin);
        text.style.maxHeight = `${collapsedH}px`;
        requestAnimationFrame(() => { text.style.maxHeight = `${text.scrollHeight}px`; });
        pinsFollowExpansion(row);
        text.addEventListener('transitionend', () => {
            // Free growth after the animation (edits/emoji loads can reflow).
            if (row.classList.contains('pins-expanded')) text.style.maxHeight = 'none';
        }, { once: true });
    } else {
        const collapsedH = Number(row.dataset.collapsedH) || 24;
        chevron?.classList.remove('pins-open');
        text.style.maxHeight = `${text.scrollHeight}px`;
        requestAnimationFrame(() => { text.style.maxHeight = `${collapsedH}px`; });
        text.addEventListener('transitionend', () => {
            row.classList.remove('pins-expanded');
            text.style.maxHeight = '';
            // Back to the one-line preview treatment.
            if (pin && body) pinsRenderCollapsedContent(body, pin);
        }, { once: true });
    }
}

/// Jump the chat to a pinned message. A pin is proof a message existed, not
/// that this device holds it — history from before a join or an old epoch may
/// be absent, and that case gets a toast rather than a silent nothing.
async function pinsJumpTo(rumorId) {
    const chat = arrChats.find(c => c.id === strOpenChat);
    const reachable = !!document.getElementById(rumorId)
        || !!chat?.messages?.some(m => m.id === rumorId);
    if (!reachable) {
        // Cheap DB probe before handing off, so the miss is a message instead
        // of a no-op. jumpToMessage re-loads a full window on the hit path.
        let slice = [];
        try {
            slice = await invoke('get_messages_around', {
                chatId: strOpenChat, anchorId: rumorId, before: 1, after: 1,
            }) || [];
        } catch (e) { /* treated as a miss */ }
        if (!slice.some(m => m.id === rumorId)) {
            showToast("That message isn't in your local history");
            return;
        }
    }
    pinsSetDrawerVisible(false);
    jumpToMessage(rumorId);
}

async function pinsPin(messageId) {
    try {
        await invoke('pin_community_message', {
            communityId: pinsCtx.communityId,
            channelId: pinsCtx.channelId,
            messageId,
        });
        showToast('Message pinned');
    } catch (e) {
        showToast(String(e));
    }
    await pinsRefresh();
}

async function pinsUnpin(messageId) {
    try {
        await invoke('unpin_community_message', {
            communityId: pinsCtx.communityId,
            channelId: pinsCtx.channelId,
            messageId,
        });
    } catch (e) {
        showToast(String(e));
    }
    await pinsRefresh();
}

/// Context-menu items for a message row: Pin, or Unpin when already pinned.
/// Empty unless the open chat is a v2 community channel AND the user holds
/// PIN_MESSAGES — authority is re-verified by every reader's fold regardless.
function pinsMenuItems(messageId) {
    if (!pinsCtx.channelId || !pinsCtx.canPin || pinsCache.sealed) return [];
    const pinned = pinsCache.pins.some(p => p.rumor_id === messageId);
    if (pinned) {
        return [{ label: 'Unpin', icon: 'x', onClick: () => pinsUnpin(messageId) }];
    }
    return [{ label: 'Pin', icon: 'pin', onClick: () => pinsPin(messageId) }];
}

// ── Wiring ───────────────────────────────────────────────────────────────────

domPinsBtn.addEventListener('click', () => {
    pinsSetDrawerVisible(!pinsDrawerOpen);
    if (pinsDrawerOpen) pinsRenderDrawer();
});
domPinsClose.addEventListener('click', () => pinsSetDrawerVisible(false));

// Scrim behavior: with the drawer open the blurred chat is a backdrop, and
// clicking anywhere on it (messages, header, composer) dismisses the drawer.
// The pin button is excluded — its own handler toggles, and running both
// would close-then-reopen in one tap.
document.getElementById('chat').addEventListener('click', (e) => {
    if (!pinsDrawerOpen) return;
    if (e.target.closest('#pins-drawer') || e.target.closest('#chat-pins-btn')) return;
    pinsSetDrawerVisible(false);
});

// Live updates: the control follow persists a fresh head and announces it.
window.addEventListener('DOMContentLoaded', () => {
    window.__TAURI__.event.listen('community_pins_updated', (evt) => {
        if (evt.payload?.channel_id === pinsCtx.channelId) pinsRefresh();
    });
});
