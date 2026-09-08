// The chat view and its composer: the wiring of their chrome and, nested for now,
// the send pipeline. One global scope: this loads before main.js and shares its globals.

/**
 * A simple state tracker for the last message ID, if it changes, we auto-scroll
 */
let strLastMsgID = "";

/**
 * The current Message ID being replied to
 */
let strCurrentReplyReference = "";

/**
 * The slash-command selector/composer controller. Declared at top level so
 * openChat/edit/event handlers can reach it; assigned at composer init.
 */
let commandCtrl = null;

/**
 * The current Message ID being edited (if in edit mode)
 */
let strCurrentEditMessageId = "";

/**
 * The original content of the message being edited (for cancel restoration)
 */
let strCurrentEditOriginalContent = "";

/**
 * Updates the current chat (to display incoming and outgoing messages)
 * @param {Chat} chat - The chat to update
 * @param {Array<Message>} arrMessages - The messages to efficiently insert into the chat
 * @param {Profile} profile - Optional profile for display info
 * @param {boolean} fClicked - Whether the chat was opened manually or not
 */
/**
 * Synchronously set the chat header (avatar + name + subtext + click handlers).
 * Called both from openChat (immediately, so the header is visible the instant
 * the chat panel reveals) and from updateChat (in case profile data changed
 * while the chat was open).
 *
 * @param {Object} chat - The chat object (may be null while loading)
 * @param {Profile} profile - DM profile (null for groups)
 * @param {boolean} isGroup
 * @param {boolean} fNotes - Self-DM "Notes" mode
 */
// ============================================================================
// Self-Destruct Timer — per-chat NIP-40 message expiry ("disappearing messages")
// ============================================================================

const SELF_DESTRUCT_OPTIONS = [
    { label: 'Permanent',  secs: null },
    { label: '1 Week',     secs: 604800 },
    { label: '1 Day',      secs: 86400 },
    { label: '6 Hours',    secs: 21600 },
    { label: '1 Hour',     secs: 3600 },
    { label: '5 Minutes',  secs: 300 },
    { label: '60 Seconds', secs: 60 },
    { label: '10 Seconds', secs: 10 },
];

/** Open the duration picker for a chat's Self-Destruct Timer, anchored to a
 *  rect. Marks the current value; writes apply immediately to future sends. */
async function openSelfDestructPicker(chatId, anchor) {
    if (!chatId || !chatSupportsSelfDestruct(getChat(chatId))) return;
    let current = null;
    try { current = await invoke('get_self_destruct_timer', { chatId }); } catch (_) {}
    const items = SELF_DESTRUCT_OPTIONS.map(o => ({
        label: o.label,
        hint: ((o.secs || null) === (current || null)) ? '✓' : undefined,
        onClick: async () => {
            try { await invoke('set_self_destruct_timer', { chatId, secs: o.secs }); }
            catch (_) { return; }
            updateSelfDestructIndicator(chatId);
        },
    }));
    const rect = anchor || { right: window.innerWidth / 2, bottom: window.innerHeight / 2 };
    showContextMenu({ x: rect.right, y: rect.bottom + 4, items });
}

/** Reflect the open chat's timer as a "temporary send" badge on the send +
 *  voice buttons (whichever is visible shows it). */
async function updateSelfDestructIndicator(chatId) {
    const sendBtn = document.getElementById('chat-input-send');
    if (!sendBtn) return;
    const apply = (secs) => {
        if (secs) { sendBtn.dataset.sdSecs = String(secs); sendBtn.classList.add('has-self-destruct'); }
        else { delete sendBtn.dataset.sdSecs; sendBtn.classList.remove('has-self-destruct'); }
    };
    if (!chatId || !chatSupportsSelfDestruct(getChat(chatId))) { apply(null); return; }
    let secs = null;
    try { secs = await invoke('get_self_destruct_timer', { chatId }); } catch (_) {}
    apply(secs && chatId === strOpenChat ? secs : null);
}

/** Wire the composer: a "temporary send" clock badge on the send + voice
 *  buttons, and right-click / long-press on Send to open the timer picker.
 *  Runs once. */
function setupSelfDestructComposer() {
    const sendBtn = document.getElementById('chat-input-send');
    if (!sendBtn || sendBtn.dataset.sdWired) return;
    sendBtn.dataset.sdWired = '1';

    _ensureSelfDestructBadge();

    const openFromBtn = () => {
        if (strOpenChat && chatSupportsSelfDestruct(getChat(strOpenChat))) {
            openSelfDestructPicker(strOpenChat, sendBtn.getBoundingClientRect());
        }
    };
    sendBtn.addEventListener('contextmenu', (e) => { e.preventDefault(); openFromBtn(); });
    // Right-clicking the mic does nothing on desktop (suppress the native menu).
    const voiceBtn = document.getElementById('chat-input-voice');
    if (voiceBtn) voiceBtn.addEventListener('contextmenu', (e) => e.preventDefault());
    let pressTimer = null;
    sendBtn.addEventListener('touchstart', () => { pressTimer = setTimeout(openFromBtn, 500); }, { passive: true });
    const cancelPress = () => { if (pressTimer) { clearTimeout(pressTimer); pressTimer = null; } };
    sendBtn.addEventListener('touchend', cancelPress);
    sendBtn.addEventListener('touchmove', cancelPress);
    sendBtn.addEventListener('touchcancel', cancelPress);
}

/** Inject the "temporary send" badge into the composer CONTAINER (not the send
 *  button — so it never inherits the mic<->send swap rotation) and mirror the
 *  send button's visibility onto it via `.is-visible`, so it fades with the
 *  swap. Inline SVG so nothing inflates it; pointer-events:none so it never
 *  swallows a send tap. */
function _ensureSelfDestructBadge() {
    const send = document.getElementById('chat-input-send');
    const container = send && send.closest('.chat-input-container');
    if (!container || container.dataset.sdBadge) return;
    container.dataset.sdBadge = '1';
    const badge = document.createElement('span');
    badge.className = 'self-destruct-badge';
    badge.innerHTML = '<svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M12 7.5V12l3 2"/></svg>';
    container.appendChild(badge);

    // Recompute the badge's visibility whenever the send button's state flips
    // (swap in/out, shown/hidden, timer toggled) — one observer, no swap-logic edits.
    new MutationObserver(_syncSelfDestructBadge)
        .observe(send, { attributes: true, attributeFilter: ['class', 'style'] });
    _syncSelfDestructBadge();
}

/** Show the badge only while the send button is present, timer-active, and not
 *  mid swap-out — so it fades in/out with the button rather than spinning. */
function _syncSelfDestructBadge() {
    const send = document.getElementById('chat-input-send');
    const badge = document.querySelector('.self-destruct-badge');
    if (!send || !badge) return;
    const show = send.classList.contains('has-self-destruct')
        && send.style.display !== 'none'
        && !send.classList.contains('button-swap-out');
    badge.classList.toggle('is-visible', show);
}

/** Play the Tron "derez" dissolve on a message row, then remove it. Idempotent. */
function _derezRowDom(domMsg) {
    if (!domMsg || domMsg.dataset.derezzing) return;
    domMsg.dataset.derezzing = '1';
    if (_dmsgToolbarTarget === domMsg) hideMessageToolbar();
    // The array was already spliced; the list re-derives (the next row's streak with it).
    _windowReleaseAnchor(domMsg.id);
    VectorSvelte.touchWindow();
    VectorSvelte.flushSync();
    _dmsgUpdateLastSentVisibility();
}

/** A message just vanished (deletion or self-destruct) — bail out of any UI
 *  mode still pointed at it: reply mode and the reaction emoji panel. */
function _exitModesForRemovedMessage(id) {
    if (id && strCurrentReplyReference === id) cancelReply();
    if (id && strCurrentReactionReference === id) closeEmojiPanel();
}

/** Client-side self-destruct: derez a visible expired message now (precise
 *  visual) and drop it from the frontend caches. The backend sweep purges
 *  STATE/DB + own-blobs on its own interval. */
function derezMessageLocally(id, chatId) {
    _exitModesForRemovedMessage(id);
    const cChat = getChat(chatId);
    if (cChat) {
        const mi = cChat.messages.findIndex(m => m.id === id);
        if (mi !== -1) cChat.messages.splice(mi, 1);
        if (eventCache.has(chatId)) {
            const ce = eventCache.getEvents(chatId);
            if (ce) { const ci = ce.findIndex(m => m.id === id); if (ci !== -1) ce.splice(ci, 1); }
        }
    }
    if (strOpenChat === chatId) {
        const domMsg = document.getElementById(id);
        if (domMsg) _derezRowDom(domMsg);
    }
    chatChanged(chatId);
    scheduleUnreadRefresh();
}

// Drive the per-message countdown + derez for the OPEN chat. Off-screen and
// closed-chat expiries fall to the backend sweep. One cheap DOM scan per second.
setInterval(() => {
    const glyphs = document.querySelectorAll('.dmsg-selfdestruct[data-expiration]');
    if (!glyphs.length) return;
    const now = Math.floor(Date.now() / 1000);
    glyphs.forEach(el => {
        const exp = parseInt(el.dataset.expiration, 10);
        // Keep the inline countdown (Android) ticking.
        const timeEl = el.querySelector('.dmsg-selfdestruct-time');
        if (timeEl) {
            const remaining = exp - now;
            timeEl.textContent = remaining > 0 ? _fmtCountdown(remaining) : '';
        }
        if (!exp || exp > now || el.dataset.fired) return;
        el.dataset.fired = '1';
        const row = el.closest('.dmsg');
        if (row && row.id && strOpenChat) derezMessageLocally(row.id, strOpenChat);
    });
}, 1000);

/** Build the chat-header overflow ("hamburger") menu items for a chat.
 *  Single source of truth for both the click handler and the button's
 *  visibility — when this returns empty (e.g. group chats, which have no
 *  per-chat options yet), the button is hidden rather than opening an
 *  empty menu. */
/** DMs and Concord v2 community channels support the Self-Destruct Timer
 *  (sender-controlled NIP-40 TTL). A v1 community's send path ignores the tag,
 *  so it's gated out here to avoid an indicator that would lie. */
function chatSupportsSelfDestruct(chat) {
    if (!chat) return false;
    if (chat.chat_type === 'DirectMessage') return true;
    return chat.chat_type === 'Community' && chat.metadata?.custom_fields?.proto_version === '2';
}

function buildChatMenuItems(chat) {
    const items = [];
    if (chatSupportsSelfDestruct(chat)) {
        items.push({
            label: 'Self-Destruct Timer',
            icon: 'clock',
            onClick: () => {
                const btn = document.getElementById('chat-menu-btn');
                const rect = btn ? btn.getBoundingClientRect() : null;
                requestAnimationFrame(() => openSelfDestructPicker(strOpenChat, rect));
            },
        });
    }
    if (chat?.chat_type === 'DirectMessage') {
        items.push({
            label: 'Change Wallpaper',
            icon: 'image',
            onClick: () => startWallpaperChange(strOpenChat),
        });
        if (chat?.wallpaper_path) {
            items.push({
                label: 'Remove Wallpaper',
                icon: 'trash',
                onClick: () => removeWallpaper(strOpenChat),
            });
        }
    }
    return items;
}

/** The header derives from the open chat's signals; this paints it synchronously
 *  for the open path and warms a community's member count. */
function setChatHeader(chat) {
    VectorSvelte.setOpenChat(strOpenChat);
    if (chat) {
        touchChatRow(chat);
        const communityId = chat.metadata?.custom_fields?.community_id;
        if (communityId) refreshCommunityMemberCount(communityId);
    }
    VectorSvelte.flushSync();
}

async function updateChat(chat, arrMessages = [], profile = null, fClicked = false, arrival = false) {
    // Queue profiles for this chat — fire-and-forget so rendering is not delayed
    // by an IPC roundtrip. Awaiting this here used to race the very first
    // attachment_upload_progress events ahead of the spinner DOM, leaving the
    // upload ring frozen.
    if (chat) {
        invoke("queue_chat_profiles_sync", {
            chatId: chat.id,
            isOpening: true
        });
    }
    
    // Check if this is a group chat
    const isGroup = chatIsGroup(chat);

    // If no profile is provided and it's not a group, try to get it from the chat ID
    if (!profile && chat && !isGroup) {
        profile = getProfile(chat.id);
    }
    
    // If this chat is our own npub: then we consider this our Bookmarks/Notes section
    const fNotes = strOpenChat === strPubkey;

    // Header is set synchronously by openChat before this runs, but call it
    // again here in case profile data has changed while the chat was open.
    setChatHeader(chat);

    if (chat?.messages.length || arrMessages.length) {

        // markAsRead is handled by callers (openChat synchronously, message_new
        // handlers for real-time arrivals, closeChat on exit, onFocusChanged on
        // window refocus). An async focus-gated markAsRead used to live here,
        // but the IPC could hang/fail and leave chat.last_read stuck behind.

        if (!arrMessages.length) return;

        // Sort messages by timestamp (oldest first) to ensure correct insertion order
        // This is critical for timestamp insertion logic - without this, newer messages
        // get inserted first and older messages compare gaps against distant ancestors
        // instead of their actual chronological neighbors
        const sortedMessages = [...arrMessages].sort((a, b) => a.at - b.at);

        // Pre-load delete/hide meta for this batch in one bulk call (own → retained
        // keys; any community msg → admin-hide authority), so the hover toolbar
        // reads a cache instead of an IPC per hover. Desktop-only (hover toolbar).
        if (!platformFeatures?.is_mobile) {
            const isCommunity = chat?.chat_type === 'Community';
            // Skip pending/failed: a pending Community message is inserted with its
            // final id BEFORE its retained key is stored (store_message_key runs
            // post-publish), so prefetching now would cache a premature
            // has_retained_keys:false that never refreshes (the id doesn't change) —
            // exactly what made fresh sends show "limited". The toolbar suppresses the
            // delete affordance for pending/failed anyway; once sent, the key exists
            // and a hover (or re-render) resolves it correctly.
            const metaIds = sortedMessages
                .filter(m => (m.mine || isCommunity) && !m.pending && !m.failed)
                .map(m => m.id);
            if (metaIds.length) dmsgQueueDeleteMeta(metaIds);
        }

        // The list island derives rows, separators and system events from the window;
        // widen it to cover the batch (a single arrival also gets the entry extras).
        ensureMessageList();
        // Only a live arrival gets the entry extras; an open re-paints history, however small.
        _updateChatWindow(chat, sortedMessages, arrival && arrMessages.length === 1 ? sortedMessages[0] : null);

        // Auto-scroll on new messages (if the user hasn't scrolled up, or on manual chat open).
        // Gated on the intent-aware pin, NOT raw distance: a user resting just below the
        // pin threshold during a sync must not be yanked to the tail. Suppressed during a
        // window slide (extend-newer/drop), which owns scrollTop itself.
        const pxFromBottom = domChatMessages.scrollHeight - domChatMessages.scrollTop - domChatMessages.clientHeight;
        if (!_windowSuppressAutoScroll && ((chatPinnedToBottom && pxFromBottom < 500) || fClicked)) {
            const cLastMsg = chat.messages[chat.messages.length - 1];
            if (strLastMsgID !== cLastMsg.id || fClicked) {
                strLastMsgID = cLastMsg.id;
                adjustSize();
                // Force an auto-scroll, given soft-scrolling won't accurately work when the entire list has just rendered
                scrollToBottom(domChatMessages, false);
                // A render is where media starts resolving — hold the bottom through
                // it. Load-driven arming alone misses a cold render that lands after
                // the open's own hold expired, and the pin then releases on distance
                // with nothing left to recover it.
                holdChatBottom();
            }
        }
    } else {
        // Probably a 'New Chat': an empty window. Never wipe the container itself,
        // the list island lives in it.
        if (fClicked) {
            ensureMessageList();
            VectorSvelte.clearWindow();
            VectorSvelte.flushSync();
            windowTopId = windowBottomId = null;
        }

        // The header derives from the open chat id even without a chat entry yet.
        setChatHeader(chat);
    }

    adjustSize();
    
    // Update the back button notification dot after chat updates
}

/**
 * The list island's half of updateChat: the messages are already in the chat's array,
 * so widen the window to cover them, flush synchronously (the callers measure right
 * after), and apply the single-arrival extras the vanilla append branch had (entry
 * animation, unread badge and divider, snap on an own send).
 */
function _updateChatWindow(chat, sortedMessages, single) {
    initMessageToolbar();   // lives inside the container; the vanilla row builder used to init it
    const msgs = _dmsgListHelpers.messages(chat.id);
    let lo = Infinity, hi = -1;
    for (const m of sortedMessages) {
        const i = msgs.findIndex(x => x === m || x.id === m.id);
        if (i === -1) continue;
        if (i < lo) lo = i;
        if (i > hi) hi = i;
    }
    if (hi === -1) return;
    const cur = _currentWindowRange();
    // Only a window that is still on screen extends; a stale one (another chat, a
    // cleared list) is replaced.
    const sameChat = cur && document.getElementById(windowTopId);
    const start = sameChat ? Math.min(cur[0], lo) : lo;
    const end = sameChat ? Math.max(cur[1], hi + 1) : hi + 1;
    windowTopId = msgs[start].id;
    windowBottomId = msgs[end - 1].id;
    VectorSvelte.setWindow(chat.id, windowTopId, windowBottomId);
    VectorSvelte.flushSync();

    // `single` is only ever a live arrival, and the extras need a window that was already on screen.
    if (single && sameChat && !single.mine && single.id === windowBottomId && hi === msgs.length - 1) {
        const domMsg = document.getElementById(single.id);
        if (domMsg) {
            domMsg.classList.add('new-anim');
            domMsg.addEventListener('animationend', () => domMsg.classList.remove('new-anim'), { once: true });
            // Bump the scroll-down badge if the user is reading above; drop a divider
            // when the window is inactive (pinned but tabbed out, so unseen).
            if (!chatPinnedToBottom) {
                incrementUnreadBelow();
                insertUnreadDivider(domMsg);
            } else if (!isWindowActive()) {
                insertUnreadDivider(domMsg);
            }
        }
    }
    if (single && single.mine && single.pending) {
        // Sending counts as "read up to here".
        scrollToBottom(domChatMessages, false);
        clearUnreadDivider();
    }
    // Only the newest own message shows its "Sent" mark.
    _dmsgUpdateLastSentVisibility();
}

/** The inner HTML of a day divider for `timestamp` ("Today, 4:08 pm"). */
function dayDividerHtml(timestamp) {
    const messageDate = new Date(timestamp);
    const timeStr = _insertTimestampTimeFmt.format(messageDate);
    if (isToday(messageDate)) return `<strong>Today</strong>, ${timeStr}`;
    if (isYesterday(messageDate)) return `<strong>Yesterday</strong>, ${timeStr}`;
    return `<strong>${_insertTimestampDateFmt.format(messageDate)}</strong>, ${timeStr}`;
}

// Cached formatters — Intl.DateTimeFormat construction is expensive vs. .format()
const _insertTimestampTimeFmt = new Intl.DateTimeFormat([], { hour: 'numeric', minute: '2-digit', hour12: true });
const _insertTimestampDateFmt = new Intl.DateTimeFormat();

/** System events that collapse when they repeat back-to-back. Membership is
 *  deliberately absent: WHO joined or left is the content, so those never merge. */
const MERGEABLE_SYSTEM_EVENTS = new Set([
    SystemEventType.WallpaperChanged,
    SystemEventType.WallpaperRemoved,
    SystemEventType.PinsModified,
]);

/**
 * Helper function to create and insert a system event (member joined/left, etc.)
 * Uses the same styling as timestamps (centered, lower opacity)
 * @param {string} content - The system event text (e.g., "John has left")
 * @param {HTMLElement} parent - Optional parent to append to
 * @returns {HTMLElement} - The created system event element
 */
function insertSystemEvent(content, parent = null) {
    const pSystemEvent = document.createElement('p');
    pSystemEvent.classList.add('msg-inline-timestamp'); // Reuse timestamp styling
    pSystemEvent.textContent = content;
    if (parent) parent.appendChild(pSystemEvent);
    return pSystemEvent;
}

// ============================================================================
// Per-DM Wallpaper
// ============================================================================
// Three states the chat can be in:
//   • No wallpaper — empty `chat.wallpaper_path`, default chat background.
//   • Active wallpaper — `chat.wallpaper_path` set, layer renders that file.
//   • Previewing — user picked an image but hasn't confirmed yet. The layer
//     is swapped to the staged preview file and the slider bar appears
//     above the composer. The bar's lifecycle is tracked by
//     `wallpaperPreviewState`.
//
// Visual settings (blur + brightness) are driven by CSS variables on
// `#chat-wallpaper-layer` so live slider drags are GPU-friendly and don't
// hit the rumor pipeline. The values are persisted alongside the image on
// confirm.

/** {chatId, previewPath, blur, dim} while a preview is active, else null. */
let wallpaperPreviewState = null;

const WALLPAPER_DEFAULT_BLUR = 5;
const WALLPAPER_DEFAULT_DIM = 50;

/** Cache key (`path|ts`) for the wallpaper currently on `--wp-image`.
 *  Slider drags reuse the same key so the image URL isn't re-issued
 *  (which would cache-bust + flicker). A new rumor advances `ts`, which
 *  changes the key and forces a fresh fetch even when the on-disk
 *  filename is identical (deterministic `<chat_npub>.<ext>` path). */
let _lastAppliedWallpaperKey = null;

/** Apply a wallpaper file path + visual settings to the open chat. The
 *  image URL is only re-set when the path actually changes — slider
 *  drags reuse the loaded image and only touch the blur/brightness vars. */
function applyChatWallpaper(chatId, path, blur, dim, ts) {
    if (strOpenChat !== chatId) return;
    const chatEl = document.getElementById('chat');
    const layer = document.getElementById('chat-wallpaper-layer');
    if (!chatEl || !layer) return;
    // Honor the global "Background Wallpaper" display toggle: when it's off,
    // suppress the committed wallpaper so the default theme shows through.
    // A live preview still renders (the user may be setting it for their
    // chat partner and needs to see what they're picking).
    const previewing = chatEl.getAttribute('data-wallpaper-previewing') === 'true';
    const bgDisabled = document.body.classList.contains('chat-bg-disabled');
    const newPath = (bgDisabled && !previewing) ? '' : (path || '');
    // The on-disk filename is deterministic per chat, so an inbound rumor
    // overwrites bytes at the same path. Include `ts` in the cache key
    // so a new wallpaper forces a re-fetch even when the path is unchanged.
    const newKey = newPath + '|' + (ts || 0);
    if (newKey !== _lastAppliedWallpaperKey) {
        if (newPath) {
            const url = convertFileSrc(newPath);
            const busted = url + (url.includes('?') ? '&' : '?') + 't=' + (ts || Date.now());
            layer.style.setProperty('--wp-image', `url("${busted}")`);
            chatEl.setAttribute('data-wallpaper', 'true');
        } else {
            layer.style.removeProperty('--wp-image');
            chatEl.removeAttribute('data-wallpaper');
        }
        _lastAppliedWallpaperKey = newKey;
    }
    const blurPx = Math.max(0, Math.min(30, blur ?? WALLPAPER_DEFAULT_BLUR));
    const brightness = Math.max(0, Math.min(100, dim ?? WALLPAPER_DEFAULT_DIM)) / 100;
    // Build the filter directly. `blur(0px)` clashes with brightness() in
    // WebKit (the layer washes out to solid white), so omit blur entirely at
    // zero rather than passing a 0px radius.
    layer.style.filter = blurPx > 0
        ? `blur(${blurPx}px) brightness(${brightness})`
        : `brightness(${brightness})`;
}

/** Refresh the wallpaper layer from the open chat's persisted state. */
function refreshChatWallpaper() {
    const chat = getChat(strOpenChat);
    applyChatWallpaper(
        strOpenChat,
        chat?.wallpaper_path || '',
        chat?.wallpaper_blur,
        chat?.wallpaper_dim,
        chat?.wallpaper_ts,
    );
}

/** Show or hide the wallpaper edit UI: the bottom slider/trash bar plus the
 *  Cancel/Save overlay on the chat header. Also flags the chat so the
 *  scroll-return button can be hidden via CSS while the preview is up. */
function setWallpaperPreviewBarVisible(visible) {
    const bar = document.getElementById('wallpaper-preview-bar');
    if (bar) bar.style.display = visible ? '' : 'none';
    const editBar = document.getElementById('wallpaper-edit-bar');
    if (editBar) {
        if (visible) {
            editBar.style.opacity = '0';
            editBar.style.display = 'flex';
            setTimeout(() => { editBar.style.opacity = '1'; }, 10);
        } else {
            editBar.style.opacity = '0';
            setTimeout(() => { editBar.style.display = 'none'; }, 250);
        }
    }
    const chatEl = document.getElementById('chat');
    if (chatEl) {
        if (visible) chatEl.setAttribute('data-wallpaper-previewing', 'true');
        else chatEl.removeAttribute('data-wallpaper-previewing');
    }
}

/** Lock the edit-bar buttons while a publish/removal is in flight. */
function setWallpaperEditBusy(busy) {
    for (const id of ['wallpaper-edit-save-btn', 'wallpaper-edit-cancel-btn']) {
        const el = document.getElementById(id);
        if (!el) continue;
        el.style.pointerEvents = busy ? 'none' : '';
        el.style.opacity = busy ? '0.5' : '';
    }
}

/** Read the current slider values from the preview bar. */
function readWallpaperSliders() {
    const blurEl = document.getElementById('wallpaper-blur-slider');
    const dimEl = document.getElementById('wallpaper-dim-slider');
    const blur = blurEl ? parseInt(blurEl.value, 10) : WALLPAPER_DEFAULT_BLUR;
    const dim = dimEl ? parseInt(dimEl.value, 10) : WALLPAPER_DEFAULT_DIM;
    return {
        blur: Number.isFinite(blur) ? blur : WALLPAPER_DEFAULT_BLUR,
        dim: Number.isFinite(dim) ? dim : WALLPAPER_DEFAULT_DIM,
    };
}

/** Compute the 0..100% the slider's value occupies of its range. */
function _wallpaperSliderPct(el) {
    if (!el) return 0;
    const min = parseFloat(el.min) || 0;
    const max = parseFloat(el.max) || 100;
    const val = parseFloat(el.value) || 0;
    if (max === min) return 0;
    return Math.max(0, Math.min(100, ((val - min) / (max - min)) * 100));
}

/** Set the slider values + sync the CSS variable that drives the track fill
 *  gradient. Without this the WebKit `accent-color` fill drifts away from
 *  the thumb position. */
function writeWallpaperSliders(blur, dim) {
    const blurEl = document.getElementById('wallpaper-blur-slider');
    const dimEl = document.getElementById('wallpaper-dim-slider');
    if (blurEl) blurEl.value = String(blur);
    if (dimEl) dimEl.value = String(dim);
    if (blurEl) blurEl.style.setProperty('--slider-pct', `${_wallpaperSliderPct(blurEl)}%`);
    if (dimEl) dimEl.style.setProperty('--slider-pct', `${_wallpaperSliderPct(dimEl)}%`);
}

/**
 * Open the image picker, hand the result to the backend, and switch the
 * chat into preview mode. Animated sources are converted to a static
 * first-frame server-side; we surface a friendly notice when that happens.
 */
/** Full-screen "processing" overlay with a dimmed, blurred backdrop that blocks
 *  interaction while a short CPU-bound task (image decode/resize/re-encode) runs
 *  in the backend. Idempotent; pair with hideProcessingOverlay(). */
let processingMounted = false;
function showProcessingOverlay(message = 'Processing image...') {
    if (!processingMounted) { processingMounted = true; VectorSvelte.mountProcessingOverlay(); }
    VectorSvelte.showProcessing(message);
}
function hideProcessingOverlay() {
    VectorSvelte.hideProcessing();
}

async function startWallpaperChange(chatId) {
    if (!chatId) return;
    const chat = getChat(chatId);
    if (!chat || chat.chat_type !== 'DirectMessage') return;

    try {
        // Native picker on both platforms. Desktop returns a filesystem path,
        // Android a content:// URI — the backend reads either natively (the
        // WebView's file.arrayBuffer() returns nothing for content URIs).
        const { open } = window.__TAURI__.dialog;
        const filePath = await open({
            multiple: false,
            directory: false,
            filters: [
                { name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif'] },
            ],
        });
        if (!filePath) return;
        showProcessingOverlay();
        let previewResult;
        try {
            previewResult = await invoke('preview_wallpaper', { chatId, filePath });
        } finally {
            hideProcessingOverlay();
        }
        await applyWallpaperPreview(chatId, previewResult);
    } catch (err) {
        popupConfirm('Couldn’t use that image', String(err), true);
    }
}

/** Apply the staged preview file to the chat + open the slider bar. */
async function applyWallpaperPreview(chatId, previewResult) {
    if (!previewResult?.path) return;
    if (previewResult.was_animated) {
        await popupConfirm(
            'Static wallpapers only',
            'Vector wallpapers don’t animate. We grabbed the first frame of your image to use instead.',
            true,
        );
    }
    // Pick the slider's initial values. If the chat already has a wallpaper,
    // re-picking preserves the user's previous customisation. For first-time
    // picks, fall back to the backend's per-image suggested brightness so
    // photos that are very bright don't ship with text-killing contrast.
    const chat = getChat(chatId);
    const hadWallpaper = !!chat?.wallpaper_path;
    const blur = hadWallpaper ? (chat.wallpaper_blur ?? WALLPAPER_DEFAULT_BLUR) : WALLPAPER_DEFAULT_BLUR;
    const dim = hadWallpaper
        ? (chat.wallpaper_dim ?? WALLPAPER_DEFAULT_DIM)
        : (previewResult.recommended_dim ?? WALLPAPER_DEFAULT_DIM);
    writeWallpaperSliders(blur, dim);
    // Use Date.now() as the cache key so picking a second image with the
    // same extension (same on-disk preview path) forces a refetch.
    const previewTs = Date.now();
    wallpaperPreviewState = { chatId, previewPath: previewResult.path, blur, dim, ts: previewTs };
    clearWallpaperUploadProgress();
    setWallpaperEditBusy(false);
    // Flag previewing FIRST so applyChatWallpaper renders the staged image
    // even when the global "Background Wallpaper" toggle is off.
    setWallpaperPreviewBarVisible(true);
    applyChatWallpaper(chatId, previewResult.path, blur, dim, previewTs);
}

/** Slider input → live-update the layer's CSS variables and preview state. */
function onWallpaperSliderInput() {
    if (!wallpaperPreviewState) return;
    const { blur, dim } = readWallpaperSliders();
    wallpaperPreviewState.blur = blur;
    wallpaperPreviewState.dim = dim;
    const blurEl = document.getElementById('wallpaper-blur-slider');
    const dimEl = document.getElementById('wallpaper-dim-slider');
    if (blurEl) blurEl.style.setProperty('--slider-pct', `${_wallpaperSliderPct(blurEl)}%`);
    if (dimEl) dimEl.style.setProperty('--slider-pct', `${_wallpaperSliderPct(dimEl)}%`);
    applyChatWallpaper(wallpaperPreviewState.chatId, wallpaperPreviewState.previewPath, blur, dim, wallpaperPreviewState.ts);
}

/** Publish the preview as the chat's wallpaper (with current slider values). */
async function confirmWallpaperChange() {
    if (!wallpaperPreviewState) return;
    const { chatId, blur, dim } = wallpaperPreviewState;
    setWallpaperEditBusy(true);
    setWallpaperUploadProgress(0);
    try {
        await invoke('publish_wallpaper', { chatId, blur, dim });
        wallpaperPreviewState = null;
        setWallpaperPreviewBarVisible(false);
        // The wallpaper_updated event will land momentarily with the final
        // cached path; nothing else to do here.
        if (document.body.classList.contains('chat-bg-disabled')) {
            // Setting is off — clear the just-previewed image and let them know
            // it's hidden on their end (their chat partner still sees it).
            applyChatWallpaper(chatId, '', 0, WALLPAPER_DEFAULT_DIM, Date.now());
            popupConfirm(
                'Background Wallpaper',
                'Your wallpaper is set. If you want to be able to see it, please enable <b>Settings → Display → Background Wallpaper</b>.',
                true,
            );
        }
    } catch (err) {
        popupConfirm('Wallpaper not sent', String(err), true);
    } finally {
        clearWallpaperUploadProgress();
        setWallpaperEditBusy(false);
    }
}

/** Drive the header label as the encrypted blob streams to Blossom, so the
 *  user knows something is happening even before the first chunk lands. */
function setWallpaperUploadProgress(percentage) {
    const label = document.getElementById('wallpaper-edit-mode-label');
    if (!label) return;
    const pct = Math.max(0, Math.min(100, Math.round(percentage || 0)));
    label.textContent = pct > 0 ? `Uploading… ${pct}%` : 'Uploading…';
}

function clearWallpaperUploadProgress() {
    const label = document.getElementById('wallpaper-edit-mode-label');
    if (label) label.textContent = 'Edit Mode is enabled.';
}

/** Remove the chat's wallpaper, reverting both sides to the default theme.
 *  Reached from the "Remove Wallpaper" chat-menu item (the edit overlay
 *  covers the menu button, so this never fires mid-preview). */
async function removeWallpaper(chatId) {
    if (!chatId) return;
    const ok = await popupConfirm(
        'Remove wallpaper?',
        'This will remove the wallpaper and revert to the default theme. This change syncs to your contact and your other devices.',
        false, '', 'vector_warning.svg'
    );
    if (!ok) return;
    try {
        await invoke('remove_wallpaper', { chatId });
        applyChatWallpaper(chatId, '', 0, 50, Date.now());
    } catch (err) {
        popupConfirm('Wallpaper not removed', String(err), true, '', 'vector_warning.svg');
    }
}

/** Discard the staged preview and revert the chat background. */
async function cancelWallpaperChange() {
    if (!wallpaperPreviewState) return;
    const { chatId } = wallpaperPreviewState;
    wallpaperPreviewState = null;
    setWallpaperPreviewBarVisible(false);
    refreshChatWallpaper();
    try {
        await invoke('cancel_wallpaper_preview', { chatId });
    } catch (err) {
        console.warn('[wallpaper] cancel cleanup failed:', err);
    }
}

/**
 * Creates a file attachment box (the .custom-audio-player styled div) for all download states.
 * @param {Object} cAttachment - the attachment object
 * @param {'downloaded'|'download'|'downloading'} state - the download state
 * @returns {{ fileDiv: HTMLElement, isMiniApp: boolean, descriptionSpan: HTMLElement, iconElement: HTMLElement, updateMiniAppStatus: Function|null, statusSpan: HTMLElement|null }}
 */
/**
 * Create a conical progress spinner for file box icons and replace the target element.
 * Handles clearing the text margin so the in-flow spinner doesn't cause layout shift.
 * @param {HTMLElement} target - the icon element to replace (or null to just create)
 * @param {object} [opts] - options: id (element id), attachmentId (data-attribute)
 * @returns {HTMLDivElement} the spinner element
 */
function createFileBoxSpinner(target, opts = {}) {
    const spinner = document.createElement('div');
    spinner.className = 'miniapp-downloading-spinner';
    if (opts.id) spinner.id = opts.id;
    if (opts.attachmentId) spinner.setAttribute('data-attachment-id', opts.attachmentId);
    // Pick up any progress that was emitted before this DOM was attached
    if (opts.id) applyPendingUploadProgress(spinner, opts.id.replace(/_file$/, ''));
    // Match the Mini App icon position (marginLeft:5px + padding:10px = 15px from edge)
    spinner.style.position = 'absolute';
    spinner.style.left = '15px';
    spinner.style.top = '0';
    spinner.style.bottom = '0';
    spinner.style.margin = 'auto';
    spinner.style.width = '40px';
    spinner.style.height = '40px';
    spinner.style.opacity = '0';
    spinner.style.scale = '0.5';
    spinner.style.transition = 'opacity 0.25s ease, scale 0.25s ease, --progress 0.3s ease';
    const settleTransition = () => {
        // After intro animation, keep only the progress transition
        spinner.style.transition = '--progress 0.3s ease';
    };
    if (target) {
        // Shrink + fade out icon, then swap to spinner and grow + fade it in
        target.style.transition = 'opacity 0.2s ease, scale 0.2s ease';
        target.style.opacity = '0';
        target.style.scale = '0.5';
        setTimeout(() => {
            // Spinner is absolute-positioned — push sibling text past it
            const textSibling = target.parentElement?.querySelector('span');
            if (textSibling) textSibling.style.marginLeft = '60px';
            target.replaceWith(spinner);
            requestAnimationFrame(() => { spinner.style.opacity = '1'; spinner.style.scale = '1'; });
            setTimeout(settleTransition, 300);
        }, 200);
    } else {
        // No target (initial render as downloading) — just grow + fade in
        requestAnimationFrame(() => { spinner.style.opacity = '1'; spinner.style.scale = '1'; });
        setTimeout(settleTransition, 300);
    }
    return spinner;
}

function isSpoilerAttachment(attachment) {
    const fileName = attachment.name || '';
    return fileName.toUpperCase().startsWith('SPOILER_');
}



async function retryFailedMessage(msg) {
    const chatId = strOpenChat;
    if (!chatId) return;
    const chat = arrChats.find(c => c.id === chatId);
    const isCommunity = chat && chat.chat_type === 'Community';

    // DMs: republish the EXACT retained gift wrap first, so a first send that
    // silently landed can't double-post (same outer id → relays no-op the dup).
    // Only when nothing was retained (old/pruned msg, or an upload that failed
    // before a wrap existed) do we fall through to a fresh send.
    if (!isCommunity) {
        // Optimistic feedback: flip the red row straight back to the gray "pending"
        // look the instant Retry is tapped, so it's visibly in-flight and the user
        // doesn't keep re-tapping. on_pending's message_new is deduped for the
        // existing row, so nudge it here; the backend's on_sent/on_failed corrects
        // it when the resend resolves.
        const setSendState = (failed, pending) => {
            const local = chat?.messages?.find(m => m.id === msg.id);
            if (!local) return;
            local.failed = failed;
            local.pending = pending;
            if (eventCache.has(chatId)) {
                const cached = eventCache.getEvents(chatId);
                const idx = cached ? cached.findIndex(m => m.id === msg.id) : -1;
                if (idx !== -1) cached[idx] = local;
            }
            if (strOpenChat === chatId) {
                const dom = document.getElementById(msg.id);
                if (dom) updateMessageRow(dom, local, getProfile(chatId), msg.id);
            }
        };
        setSendState(false, true);
        try {
            const resent = await invoke('retry_failed_dm', { receiver: chatId, messageId: msg.id });
            if (resent) return;
            // Nothing retained → fall through to a fresh send (the delete below
            // removes this row and message() re-creates it as a new pending).
        } catch (e) {
            // Ambiguous state (e.g. a retained wrap we couldn't read) — do NOT
            // fall back to a fresh wrap, or we might double-post. Revert to red;
            // the user can tap Retry again.
            console.error('Idempotent retry failed:', e);
            setSendState(true, false);
            return;
        }
    }

    // Fallback (community always; DM only when nothing was retained): drop the
    // failed row and send fresh.
    try {
        await invoke('delete_failed_message', { messageId: msg.id });
    } catch (e) {
        console.error('Failed to delete failed message for retry:', e);
        return;
    }
    // Re-send: use file_message if attachment exists, otherwise text message
    try {
        if (msg.attachments && msg.attachments.length > 0) {
            if (isCommunity) {
                // Community resends all attachments + caption in one event (multi-attachment).
                // Bail if any local file is gone — silently resending fewer files would
                // publish a different message than the one that failed.
                const paths = msg.attachments.map(a => a.path);
                if (paths.some(p => !p)) {
                    popupConfirm('Cannot retry', 'One or more attachments are no longer available locally. Re-attach the files to send again.', true, '', 'vector_warning.svg');
                    return;
                }
                // Preserve each attachment's name (incl. SPOILER_ prefix) on resend. The local
                // files were already compressed on first send, so don't re-compress.
                const names = msg.attachments.map(a => a.name || '');
                await invoke('send_community_files', { channelId: chatId, content: msg.content || '', filePaths: paths, nameOverrides: names, useCompression: false, keepMetadata: false, repliedTo: msg.replied_to || '' });
            } else {
                const att = msg.attachments[0];
                await invoke('file_message', {
                    receiver: chatId,
                    repliedTo: msg.replied_to || '',
                    filePath: att.path,
                    keepMetadata: false,
                    nameOverride: att.name || ''
                });
            }
        } else {
            // Route through message() so Community retries hit send_community_message
            // (not the DM `message` command, which can't address a channel id).
            await message(chatId, msg.content, msg.replied_to || '');
        }
    } catch (e) {
        console.error('Retry send failed:', e);
    }
}

async function deleteFailedMessage(msgId) {
    try {
        await invoke('delete_failed_message', { messageId: msgId });
    } catch (e) {
        console.error('Failed to delete failed message:', e);
    }
}


/**
 * Center the message `targetMsgId` in the chat and flash-highlight it. If the
 * row isn't in the current window, load its surrounding messages and scroll
 * there. Shared by the inline reply-quote tap and the composer's reply bar.
 */
function jumpToMessage(targetMsgId) {
    if (!targetMsgId) return;
    const domMsg = document.getElementById(targetMsgId);
    if (domMsg) {
        centerInView(domMsg);
        applyHighlight(domMsg, 'jumped');
    } else {
        loadAndScrollToMessage(targetMsgId);
    }
}

/**
 * Cancel any ongoing replies and reset the messaging interface
 */
/**
 * Set mic/send to exactly one visible button, derived from the input's text
 * (no animation). Programmatic value changes fire no 'input' event, and a
 * WebKit swap animation wedges if the composer hides mid-flight — so every
 * chat open and reply/draft transition re-syncs through here.
 */
function syncSendMicToInput() {
    VectorSvelte.setDraftEmpty(domChatMessageInput.value.trim().length === 0, false);
    VectorSvelte.flushSync();
}

/** Per-chat composer drafts, runtime-only (chat id → unsent text). */
const chatDrafts = new Map();

/**
 * Stash the open chat's composer text as its draft and clear the input, so
 * the next chat starts clean and restores its own draft. Edit text is not a
 * draft — callers cancel an in-progress edit first.
 */
function stashComposerDraft() {
    if (!strOpenChat) return;
    const text = domChatMessageInput.value;
    if (text) chatDrafts.set(strOpenChat, text);
    else chatDrafts.delete(strOpenChat);
    domChatMessageInput.value = '';
    autoResizeChatInput();
    syncSendMicToInput();
}

function cancelReply() {
    VectorSvelte.cancelReply();
    VectorSvelte.flushSync();

    // Focus the message input (desktop only - mobile keyboards are disruptive)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }

    // Clear the replying-to highlight on the previously-selected row.
    if (strCurrentReplyReference) {
        const domMsg = document.getElementById(strCurrentReplyReference);
        if (domMsg) clearHighlight(domMsg, 'replying');
    }

    // Remove the reply ID
    strCurrentReplyReference = '';

    // Reset send button state based on current input
    syncSendMicToInput();
}

/**
 * Start editing a message
 * @param {string} messageId - The ID of the message to edit
 * @param {string} content - The current content of the message
 */
function startEditMessage(messageId, content) {
    // Cancel any existing reply first
    if (strCurrentReplyReference) {
        cancelReply();
    }

    // Cancel any existing edit
    if (strCurrentEditMessageId) {
        cancelEdit();
    }

    // Editing needs the real textarea back — drop any open command composer.
    if (commandCtrl) commandCtrl.exitComposer();

    // Store the edit state
    strCurrentEditMessageId = messageId;
    strCurrentEditOriginalContent = content;

    // Populate the input with the message content; the chrome (cancel button,
    // placeholder, send button) follows the edit mode.
    domChatMessageInput.value = content;
    VectorSvelte.startEdit(messageId, content);
    VectorSvelte.setDraftEmpty(false, false);
    VectorSvelte.flushSync();

    // Focus the input and move cursor to end
    domChatMessageInput.focus();
    domChatMessageInput.setSelectionRange(content.length, content.length);

    // Auto-resize the input
    autoResizeChatInput();
}

/**
 * Cancel editing and restore the input to normal state
 */
function cancelEdit() {
    // Clear the edit state
    strCurrentEditMessageId = '';
    strCurrentEditOriginalContent = '';

    // Clear the input; the chrome follows the mode back to idle.
    domChatMessageInput.value = '';
    VectorSvelte.cancelEdit();
    VectorSvelte.setDraftEmpty(true, false);
    VectorSvelte.flushSync();

    // Focus the input (desktop only)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }

    // Auto-resize the input back to normal
    autoResizeChatInput();
}

/**
 * Build `[{shortcode, url}]` for every emoji in the equipped packs (the same set
 * the picker/autocomplete resolve against). Lets optimistic, pre-relay renders
 * (edit echo, edit history) swap `:shortcode:` for the image without waiting on
 * the backend's authoritative tags. `dispCode` carries duplicate-name disambig.
 */
function equippedEmojiTags() {
    const tags = [];
    const seen = new Set();
    for (const pack of (arrEmojiPacks || [])) {
        if (packIsDead(pack)) continue;
        for (const e of (pack.emojis || [])) {
            const code = e.dispCode || e.shortcode;
            if (code && e.url && !seen.has(code)) {
                tags.push({ shortcode: code, url: e.url });
                seen.add(code);
            }
        }
    }
    return tags;
}

/** Merge emoji tag lists, first-wins per shortcode (authoritative tags before fallbacks). */
function mergeEmojiTags(...lists) {
    const out = [];
    const seen = new Set();
    for (const list of lists) {
        for (const t of (list || [])) {
            if (t && t.shortcode && t.url && !seen.has(t.shortcode)) {
                out.push({ shortcode: t.shortcode, url: t.url });
                seen.add(t.shortcode);
            }
        }
    }
    return out;
}

/**
 * Show the edit history popup for a message
 * @param {string} messageId - The ID of the message to show history for
 * @param {HTMLElement} targetElement - The element that was clicked (for positioning)
 */
let strCurrentEditHistoryMsgId = '';

function showEditHistory(messageId, targetElement) {
    const popup = document.getElementById('edit-history-popup');
    const content = document.getElementById('edit-history-content');
    if (!popup || !content) return;

    // If clicking the same message that's already open, ignore
    if (strCurrentEditHistoryMsgId === messageId && popup.style.display !== 'none') {
        return;
    }

    // Find the message in the current chat
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return;

    const msg = chat.messages.find(m => m.id === messageId);
    if (!msg || !msg.edit_history || msg.edit_history.length === 0) {
        return;
    }

    strCurrentEditHistoryMsgId = messageId;

    // Custom-emoji tags for the history. The live message only carries the
    // CURRENT revision's tags, so older revisions (which may use a different
    // `:shortcode:`) are filled from the equipped packs — same source the
    // picker/autocomplete resolve against.
    editHistoryEnsureMounted();
    VectorSvelte.setEditHistory(messageId, msg.edit_history, mergeEmojiTags(msg.emoji_tags, equippedEmojiTags()));
    VectorSvelte.flushSync();

    // Find the message bubble (p element) for positioning
    const msgBubble = targetElement.closest('.dmsg');
    const rect = msgBubble ? msgBubble.getBoundingClientRect() : targetElement.getBoundingClientRect();

    // Reset position and show popup to measure its actual dimensions
    popup.style.top = '0';
    popup.style.left = '0';
    popup.style.visibility = 'hidden';
    popup.style.display = 'block';

    // Force layout recalculation then measure
    const popupHeight = popup.getBoundingClientRect().height;
    const popupWidth = popup.getBoundingClientRect().width;

    // Position above or below the bubble depending on space
    let top = rect.top - popupHeight - 4;
    const showBelow = top < 10;
    if (showBelow) {
        top = rect.bottom + 4;
    }

    // Above: latest (bottom) fades first, oldest (top) last; below: the reverse.
    VectorSvelte.setEditHistoryBelow(showBelow);

    // Align horizontally with the bubble edge, keep within viewport
    let left = rect.left;
    left = Math.max(10, Math.min(left, window.innerWidth - popupWidth - 10));

    popup.style.left = `${left}px`;
    popup.style.top = `${top}px`;
    popup.style.visibility = 'visible';

    // Scroll to show the current (latest) entry
    content.scrollTop = content.scrollHeight;
}

/**
 * Hide the edit history popup
 */
function hideEditHistory() {
    const popup = document.getElementById('edit-history-popup');
    if (popup) {
        popup.style.display = 'none';
    }
    strCurrentEditHistoryMsgId = '';
    VectorSvelte.clearEditHistory();
}

let editHistoryMounted = false;
function editHistoryEnsureMounted() {
    if (editHistoryMounted) return;
    editHistoryMounted = true;
    VectorSvelte.mountEditHistory(document.getElementById('edit-history-content'), {
        h: { renderEmoji: (node, tags) => { renderCustomEmojiShortcodes(node, tags); twemojify(node); } },
    });
}

/**
 * Open a chat with a particular contact
 * @param {string} contact
 */
// System events (wallpaper/membership changes) are synthesized app-data
// events stored apart from the message-views pagination (kind 30078,
// distinguished by a `d` tag, so the kind-filtered message window skips them).
// To give them the SAME on-demand windowing as messages, the full set is
// fetched once into this side buffer, then revealed into the message cache
// only as far back as the loaded message window reaches — and progressively
// as the user scrolls older messages into view.
const _systemEventBuffer = new Map(); // chatId -> sorted array of system-event msg objects

/** Reveal buffered system events that fall within the currently-loaded
 *  message window into the event cache. Returns the newly-revealed ones so
 *  the caller can hand them to updateChat. Dedup-safe via cache.addEvent. */
function revealSystemEventsInWindow(chatId) {
    const buffer = _systemEventBuffer.get(chatId);
    if (!buffer || !buffer.length) return [];

    // Lower bound of the loaded window = oldest real (non-system) message in
    // the cache. Below that, messages haven't been paged in yet, so their
    // system events stay hidden. Once every message is loaded, the bound drops
    // away and the remaining (oldest) system events reveal too.
    const stats = eventCache.getStats(chatId);
    let bound = -Infinity;
    if (!stats?.isFullyLoaded) {
        const loaded = eventCache.getEventsRef(chatId) || [];
        let oldestReal = Infinity;
        for (const m of loaded) {
            if (!m.system_event && m.at < oldestReal) oldestReal = m.at;
        }
        if (oldestReal !== Infinity) bound = oldestReal;
    }

    const revealed = [];
    for (const sm of buffer) {
        if (sm.at >= bound && eventCache.addEvent(chatId, sm)) {
            revealed.push(sm);
        }
    }
    return revealed;
}

/**
 * Show/hide the "start of the channel" marker for an empty Community (no messages or system events
 * yet). Discord-style placeholder so a fresh/quiet community never opens to a blank void; removed
 * the moment any content (a message or a "X joined" event) lands. Idempotent — safe to call on every
 * render/content change for the open chat.
 */
function refreshChatEmptyState() {
    const existing = document.getElementById('chat-empty-state');
    const chat = strOpenChat ? arrChats.find(c => c.id === strOpenChat) : null;
    const isEmptyCommunity = !!chat && chat.chat_type === 'Community' && (!chat.messages || chat.messages.length === 0);
    if (isEmptyCommunity) {
        if (!existing && domChatMessages) {
            const name = chat.metadata?.custom_fields?.name || 'this community';
            const el = document.createElement('div');
            el.id = 'chat-empty-state';
            el.className = 'chat-empty-state';
            // .icon is position:absolute → wrap it in a relative, sized span (same pattern as elsewhere).
            el.innerHTML = `<div class="chat-empty-state-icon"><span class="icon icon-users-multi"></span></div>`
                + `<h3>Welcome to ${escapeHtml(name)}</h3>`
                + `<p>This is the very beginning of the channel — say hello! 👋</p>`;
            domChatMessages.appendChild(el);
        }
    } else if (existing) {
        existing.remove();
    }
}

async function openChat(contact) {
    // Safety net: a navigate-away mid-resolve clears this in jumpToUnread's finally,
    // but unfreeze the window on any chat open in case a path slipped through.
    _unreadJumpResolving = false;
    // A command composer belongs to the chat it was opened in.
    if (commandCtrl) commandCtrl.exitComposer();
    // A direct chat-to-chat jump never passes through closeChat: stash the
    // outgoing chat's draft here so it doesn't bleed into the new one.
    if (strOpenChat && strOpenChat !== contact) {
        if (strCurrentEditMessageId) cancelEdit();
        stashComposerDraft();
    }
    pushBack('chat', closeChat);
    // Abandon a wallpaper preview staged in a different chat so its edit
    // overlay doesn't leak onto this header.
    if (wallpaperPreviewState && wallpaperPreviewState.chatId !== contact) {
        const stale = wallpaperPreviewState;
        wallpaperPreviewState = null;
        setWallpaperPreviewBarVisible(false);
        invoke('cancel_wallpaper_preview', { chatId: stale.chatId }).catch(() => {});
    }
    // Display the Chat UI
    navbarSelect('chat-btn');
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domChatNew.style.display = 'none';
    domCreateGroup.style.display = 'none';
    domChats.style.display = 'none';
    domGroupOverview.style.display = 'none';
    // Hide the Settings/Invites tabs too — a chat opened from inside one of them (deep-link join,
    // notification tap) must fully take over, not paint underneath the still-visible menu.
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';
    // Jumping to a chat (e.g. mini-profile "Send Message" from the member list) closes Group
    // Details for good — drop its back entry so back-nav doesn't land on a dead re-hide step.
    // Same for the two composers: opening a conversation abandons them, so leaving their
    // entries behind would send back-nav to a panel that is no longer on screen.
    popBack('group-overview');
    popBack('create-group');
    popBack('new-chat');
    domChat.style.display = '';
    // Match the fade transition the navbar/account tabs use for visual cohesion.
    domChat.classList.add('fadein-anim');
    domChat.addEventListener('animationend', () => domChat.classList.remove('fadein-anim'), { once: true });
    domSettingsBtn.style.display = 'none';

    // Hide the Navbar
    domNavbar.style.display = `none`;

    // Clear existing messages so they're fully re-rendered (picks up state changes like blocking)
    ensureMessageList();
    VectorSvelte.clearWindow();
    VectorSvelte.flushSync();
    windowTopId = windowBottomId = null;
    // Only reset revealed blocked messages when switching to a different chat
    if (strOpenChat !== contact) revealedBlockedMessages.clear();

    // Pins: resolve this chat's pin context (v2 community channels only) —
    // shows/hides the header pin button and closes a stale drawer.
    pinsOnChatOpened(contact);

    // Warm up GIF server connection early (non-blocking)
    preconnectGifServer();

    // Get the chat (could be DM or Group)
    const chat = arrChats.find(c => c.id === contact);
    // A community channel id with no chat = a community we no longer hold (e.g. torn down mid-open by a
    // removal). Bail to the chatlist rather than render a phantom — updateChat(undefined) would throw and
    // jam the render loop. (DMs legitimately open with no prior chat, so this only guards channel ids.)
    if (!chat && !contact.startsWith('npub1')) {
        console.warn('openChat: no chat for community channel, returning to list:', contact);
        return openChatlist();
    }
    const isGroup = chatIsGroup(chat);
    const profile = !isGroup ? getProfile(contact) : null;
    strOpenChat = contact;
    wsSyncOpenChat();
    updateSelfDestructIndicator(contact);
    // Warm the command-bot snapshot so the attachment menu's Commands item
    // (bot-chats only) is ready by the time the panel opens.
    if (commandCtrl && commandCtrl.hasBots) commandCtrl.hasBots(contact);
    // Snapshot last_read BEFORE the open-time markAsRead — the divider needs
    // the stale value to find the boundary, but we still want to advance
    // chat.last_read so the OS badge clears immediately on entering the chat.
    const lastReadOnOpen = chat?.last_read || '';
    const unreadOnOpen = chat?.unread || 0;   // snapshot before the open-time markAsRead zeroes it
    // Opening IS reading — release any explicit mark-unread latch.
    clearChatUnreadLatch(chat?.id);
    if (chat?.messages?.length) {
        const latestNonMine = findLatestContactMessage(chat.messages);
        if (latestNonMine) markAsRead(chat, latestNonMine);
    }

    // Apply the chat's wallpaper to the layer before any messages render,
    // so the first paint already shows the bg + filter.
    applyChatWallpaper(contact, chat?.wallpaper_path || '', chat?.wallpaper_blur, chat?.wallpaper_dim, chat?.wallpaper_ts);

    // Render the header SYNCHRONOUSLY using whatever in-memory data we have,
    // so the user sees the contact name + avatar the instant the chat panel
    // appears — no more black flash while async cache/DB loads run.
    setChatHeader(chat);

    // Pre-paint: synchronously render in-memory messages so the chat has
    // content the moment the panel reveals. The subsequent eventCache load
    // layers any newer/older messages on top via updateChat's dedup guard
    // (each msg id is checked against the existing DOM before re-rendering).
    if (chat?.messages?.length) {
        const preBatch = chat.messages.slice(-proceduralScrollState.messagesPerBatch);
        // Fire-and-forget — updateChat's DOM building is synchronous; only
        // its mark-as-read tail is async, which we don't need to wait on.
        updateChat(chat, preBatch, profile, false);
    }

    // Queue profile sync for DMs (on-demand refresh when opening)
    if (!isGroup && contact) {
        invoke('queue_profile_sync', {
            npub: contact,
            priority: 'high',
            forceRefresh: false
        }).catch(err => console.error('Failed to queue DM profile sync:', err));
    }

    // Clear any existing auto-scroll timer
    if (chatOpenAutoScrollTimer) {
        clearTimeout(chatOpenAutoScrollTimer);
        chatOpenAutoScrollTimer = null;
    }

    // Record when the chat was opened
    chatOpenTimestamp = Date.now();

    // Chat-open paths render the latest messages and scroll-to-bottom,
    // so the user starts pinned. Reset here in case the previous chat
    // was scrolled up. Also wipes the unread badge + divider from the
    // prior chat — re-entering a chat counts as "I've read up to here".
    chatPinnedToBottom = true;
    _userScrolledAway = false;   // fresh open lands at the live tail; release any prior latch
    clearUnreadBelow();
    clearUnreadDivider();
    syncBackendActiveChat();

    // After 100ms, stop auto-scrolling on media loads
    chatOpenAutoScrollTimer = setTimeout(() => {
        chatOpenTimestamp = 0; // Reset timestamp to disable auto-scrolling
        chatOpenAutoScrollTimer = null;
    }, 100);

    // Load events from cache (on-demand loading)
    // This uses the LRU event cache for efficient memory management
    // Load events from cache (will fetch from DB if not cached)
    const initialMessages = await eventCache.loadInitialEvents(
        contact,
        proceduralScrollState.messagesPerBatch
    );

    // Merge any historical PIVX payments — helper in pivx.js
    await mergePivxPaymentsIntoChat(contact, initialMessages);

    // No on-open Community sync: NIP-17-parity means catch-up happens at boot (sync_communities_boot)
    // and on relay reconnect, with realtime delivering everything in between. Opening a channel reads
    // DB history; older pages load on scroll-up.

    // Load system events (wallpaper/membership changes). They're fetched in
    // full but buffered — only the ones inside the initially-loaded message
    // window are revealed now; the rest surface as the user scrolls older
    // messages into view (same on-demand windowing as messages).
    try {
        const systemEvents = await invoke('get_system_events', { conversationId: contact });
        // Index once: dedup-vs-loaded-messages and the known-profile check were each O(n) per
        // system event (O(systemEvents * messages) + O(systemEvents * profiles)) on chat open.
        const initialMsgIds = new Set(initialMessages.map(m => m.id));
        const knownProfileIds = new Set(arrProfiles.map(p => p.id));
        const buffer = (systemEvents || [])
            .filter(event => !initialMsgIds.has(event.id))
            .map(event => ({
                id: event.id,
                at: event.at,
                // Rebuild from the actor's CURRENT cached name rather than the npub-baked
                // stored content. Fetch unknown profiles so the next open resolves them.
                content: (() => {
                    const np = event.member_npub;
                    if (np && !knownProfileIds.has(np) && !strangerProfileRequested.has(np)) {
                        strangerProfileRequested.add(np);
                        invoke('load_profile', { npub: np }).catch(() => {});
                    }
                    return systemEventContent(event.event_type, np);
                })(),
                mine: false,
                attachments: [],
                system_event: {
                    event_type: event.event_type,
                    member_npub: event.member_npub,
                },
            }))
            .sort((a, b) => a.at - b.at);
        _systemEventBuffer.set(contact, buffer);
        // revealSystemEventsInWindow adds into the cache array, which is
        // aliased to initialMessages — re-sort so the pre-paint renders them
        // in chronological order.
        if (revealSystemEventsInWindow(contact).length > 0) {
            initialMessages.sort((a, b) => a.at - b.at);
        }
    } catch (e) {
        console.warn('Failed to load system events:', e);
    }

    // Get cache stats for procedural scroll
    const cacheStats = eventCache.getStats(contact);
    const totalMessages = cacheStats?.totalInDb || initialMessages.length;

    // Update the chat object's messages array for compatibility
    // (Some parts of the code still reference chat.messages)
    if (chat) {
        chat.messages = initialMessages;
    }

    // Initialize procedural scroll state with actual counts
    initProceduralScrollWithCache(contact, initialMessages.length, totalMessages);

    // DOM windowing: render only the newest MAX rows so a large cached array
    // (prior scroll-up loads still in the cache from a previous open) doesn't
    // flood the DOM on reopen. renderWindow clears + renders the slice and sets
    // the window anchors. Falls through to the legacy full render when disabled.
    if (CHAT_WINDOW_ENABLED && initialMessages.length > MAX_WINDOW_ROWS) {
        await renderWindow(initialMessages.length - MAX_WINDOW_ROWS, initialMessages.length);
        scrollToBottom(domChatMessages, false);
    } else {
        await updateChat(chat, initialMessages, profile, true);
        // Anchor the window to the freshly-rendered tail so isAtDataBottom() and
        // the scroll-extend paths have valid anchors.
        if (CHAT_WINDOW_ENABLED) _windowReseatAnchorsFromDom();
    }
    // Initial open lands on the newest message — the window bottom IS the live tail.
    if (CHAT_WINDOW_ENABLED) windowAtTail = true;
    // A chat open is where the most media resolves at once, and every one of
    // those loads grows the content below the fold — hold the bottom until the
    // layout settles instead of finishing short of the newest message.
    holdChatBottom();
    refreshChatEmptyState(); // empty community → show the "start of channel" marker

    // Drop a "New" divider above the first non-mine message after
    // `last_read`. Only fires when last_read matches a loaded message —
    // stale markers (id drift, deleted msg) would otherwise stick the
    // divider above the latest contact on every reopen.
    if (initialMessages.length > 0 && lastReadOnOpen) {
        const idx = initialMessages.findIndex(m => m.id === lastReadOnOpen);
        if (idx >= 0) {
            let firstUnread = null;
            for (let i = idx + 1; i < initialMessages.length; i++) {
                const m = initialMessages[i];
                if (m.system_event) continue;
                if (!m.mine) { firstUnread = m; break; }
            }
            if (firstUnread) {
                const node = document.getElementById(firstUnread.id);
                if (node) {
                    insertUnreadDivider(node);
                    // The divider lands above a message after updateChat already
                    // pinned to bottom, so its height bumps us off. Re-pin.
                    compensateChatScrollForResize();
                }
            }
        }
    }

    // Offer the jump pill ONLY when the read boundary is genuinely OFF-SCREEN — last_read is older
    // than the opened page (not in initialMessages). If last_read is within the loaded window, the
    // unread is already on screen (handled by the divider, or trivially visible) and a jump button
    // would point at nothing. This also stops a stray pill for an own trailing message.
    const lastReadIdx = lastReadOnOpen ? initialMessages.findIndex(m => m.id === lastReadOnOpen) : -1;
    if (unreadOnOpen > 0 && lastReadOnOpen && lastReadIdx < 0 && initialMessages.length > 0) {
        showUnreadJumpPill(unreadOnOpen, lastReadOnOpen);
    } else {
        hideUnreadJumpPill();
    }

    // Short messages can leave the fixed-count initial load not overflowing the viewport — no
    // scrollbar, so the user can't page older or reach the unread frontier. Fill to overflow.
    ensureChatScrollable();

    // If the user is blocked (DM only), disable the chat input and show a system message
    const isBlockedChat = !isGroup && profile?.is_blocked;
    // Dissolved community: the backend seals it (no new events accepted), so disable the
    // composer the same way a blocked DM does and mark the timeline's end.
    const isDissolvedChat = chatIsDissolved(chat);
    // Remove any previous blocked / dissolved notice before (re-)evaluating
    document.getElementById('blocked-notice')?.remove();
    document.getElementById('dissolved-notice')?.remove();
    if (isBlockedChat) {
        VectorSvelte.setLock('blocked', 'Unblock to send messages');
        VectorSvelte.flushSync();
        // Append a system-style blocked notice at the bottom of the chat
        const blockedNotice = insertSystemEvent('Blocked — You won\'t receive new messages from them');
        blockedNotice.id = 'blocked-notice';
        blockedNotice.style.marginBottom = '20px';
        domChatMessages.appendChild(blockedNotice);
    } else if (isDissolvedChat) {
        applyDissolvedChatUI(chat);
    } else {
        VectorSvelte.setLock(null);
        VectorSvelte.flushSync();
        // Restore this chat's draft (runtime-only). Skip when the input still
        // holds live text — a same-chat re-open must not clobber typing.
        if (domChatMessageInput.value === '') {
            domChatMessageInput.value = chatDrafts.get(contact) || '';
            autoResizeChatInput();
        }
        // Programmatic value sets fire no 'input' event; derive mic/send here.
        syncSendMicToInput();
    }

    // last_read is not advanced on open — the divider needs the stale value
    // to anchor above the first missed message. closeChat / msg-while-pinned
    // / onFocusChanged are the catch-up signals.

    // Update the back button notification dot

    // Focus chat input on desktop (mobile keyboards are intrusive)
    if (!platformFeatures.is_mobile && !isBlockedChat && !isDissolvedChat) {
        domChatMessageInput.focus();
    }

    // Inbound share: if the user picked this chat to share into, attach now.
    if (pendingShareToSend) {
        const share = pendingShareToSend;
        pendingShareToSend = null;
        if (share.uris && share.uris.length) {
            if (share.uris.length > 1) {
                console.warn(`[Share] ${share.uris.length} files shared; sending the first (multi-file is a follow-up)`);
            }
            // content:// URIs are read + cached immediately by openFilePreview,
            // then the user captions/confirms the send in the preview UI.
            openFilePreview(share.uris[0], contact, '').catch(e => console.error('[Share] preview failed:', e));
        } else if (share.text) {
            domChatMessageInput.value = share.text;
            // A programmatic value set doesn't fire 'input', so derive mic/send here.
            syncSendMicToInput();
            autoResizeChatInput();
            domChatMessageInput.focus();
        }
    }
}

/** Inbound share awaiting a chat selection (set when another app shares into Vector). */
let pendingShareToSend = null;

/** Consume a pending inbound share and route it to the chat picker. The backend's get_pending_share
 *  atomically take()s, so the cold-start poll, the live event, and the resume hook can all call this
 *  and the share is handled exactly once, whichever fires first. */
async function consumePendingShare() {
    try {
        const share = await invoke('get_pending_share');
        if (share) await handleIncomingShare(share);
    } catch (e) {
        console.error('[Share] consume failed:', e);
    }
}

/**
 * Handle a file/text share received from another app. Drops the user into the
 * chat list; opening a chat then attaches the share (see openChat tail).
 */
async function handleIncomingShare(payload) {
    if (!payload || ((!payload.uris || !payload.uris.length) && !payload.text)) return;
    pendingShareToSend = payload;
    // If a chat was left open (e.g. the app was backgrounded mid-chat, then
    // foregrounded by the share), tear it down — closeChat() returns to the
    // list. Otherwise just show the list. Either way we land on a clean chat
    // list for picking a destination.
    if (strOpenChat) {
        await closeChat();
    } else {
        await openChatlist();
    }
    showToast('Choose a chat to forward to');
}

/**
 * Open the dialog for starting a new chat
 */
function openNewChat() {
    // The two composers are alternatives, not layers. Left up, the other one stays
    // behind this pane with its back entry intact, so closing this one reveals it
    // instead of returning to the list. Not closeCreateGroup(): that navigates to
    // the chat list, which is where we are leaving.
    popBack('create-group');
    domCreateGroup.style.display = 'none';

    pushBack('new-chat', closeChat);
    // Display the UI
    domChatNew.style.display = '';
    domChats.style.display = 'none';
    domChat.style.display = 'none';

    // Hide the Navbar
    domNavbar.style.display = 'none';
}

/**
 * Closes the current chat, taking the user back to the chat list
 */
async function closeChat() {
    popBack('chat');
    popBack('new-chat');
    pinsOnChatClosed();
    // Stop all audio engine playback (voice messages, music, etc.)
    invoke('audio_stop_all').catch(() => {});

    // Clear any auto-scroll timer
    if (chatOpenAutoScrollTimer) {
        clearTimeout(chatOpenAutoScrollTimer);
        chatOpenAutoScrollTimer = null;
    }

    // Attempt to completely release memory (force garbage collection...) of in-chat media
    while (domChatMessages.firstElementChild) {
        const domChild = domChatMessages.firstElementChild;

        // For media (images, audio, video); we ensure they're fully unloaded
        const domMedias = domChild?.querySelectorAll('img, audio, video');
        for (const domMedia of domMedias) {
            // Streamable media (audio + video) should be paused, then force-unloaded
            if (domMedia instanceof HTMLMediaElement) {
                domMedia.pause();
                domMedia.removeAttribute('src'); // Better than setting to empty string
                domMedia.load();
            }
            // Static media (images) should simply be unloaded
            if (domMedia instanceof HTMLImageElement) {
                if (domMedia.src.startsWith('blob:')) {
                    URL.revokeObjectURL(domMedia.src);
                }
                domMedia.removeAttribute('src');
            }
        }

        // Now we explicitly drop them
        domChild.remove();
    }

    // Only catch up last_read on close when the user is actually at the
    // bottom. Marking on close while scrolled up would lie about messages the
    // user never scrolled down to see — the OS badge has to stay accurate.
    if (strOpenChat && chatPinnedToBottom) {
        const closedChat = arrChats.find(c => c.id === strOpenChat);
        if (closedChat?.messages?.length) {
            const lastContactMsg = findLatestContactMessage(closedChat.messages);
            if (lastContactMsg) {
                markAsRead(closedChat, lastContactMsg);
            }
        }
    }

    // Drop the divider ref so the next openChat starts from a clean slate.
    clearUnreadDivider();

    // Drop any in-flight wallpaper preview so the new chat doesn't inherit
    // a stale confirm bar or a foreign preview file on the background.
    if (wallpaperPreviewState) {
        const stalePreview = wallpaperPreviewState;
        wallpaperPreviewState = null;
        setWallpaperPreviewBarVisible(false);
        invoke('cancel_wallpaper_preview', { chatId: stalePreview.chatId })
            .catch(() => { /* best-effort cleanup */ });
    }
    // Strip the wallpaper from the layer so it doesn't flash through
    // during the chat list re-render.
    applyChatWallpaper(strOpenChat, '', 0, 100);

    // Trim the event cache for this chat to free memory
    // (keeps max 100 events, removes older ones loaded during scroll)
    if (strOpenChat) {
        eventCache.trimConversation(strOpenChat);
    }

    // Reset the chat UI
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domGroupOverview.style.display = 'none';
    domSettingsBtn.style.display = '';
    domChatNew.style.display = 'none';
    domChat.style.display = 'none';
    // Stash the unsent text as this chat's draft and clear the composer, so
    // the next open restores its own draft with a coherent mic/send state.
    if (strCurrentEditMessageId) cancelEdit();
    stashComposerDraft();
    strOpenChat = "";
    wsSyncOpenChat();
    previousChatBeforeProfile = ""; // Clear when closing chat
    nLastTypingIndicator = 0;
    syncBackendActiveChat();
    
    // Reset procedural scroll state
    resetProceduralScroll();
    
    // Hide the back button notification dot when closing chat

    // Display the Navbar
    domNavbar.style.display = ``;

    // Cancel any ongoing replies or selections
    strCurrentReactionReference = "";
    strCurrentReplyReference = "";
    cancelReply();

    // Navigate back to chat list with animation
    await openChatlist();

    openChatChanged();

    // Ensure the chat list re-adjusts to fit
    adjustSize();
}

/**
 * The timestamp we sent our last typing indicator
 * 
 * Ensure this is wiped when the chat is closed!
 */
let nLastTypingIndicator = 0;

const strOriginalInputPlaceholder = domChatMessageInput.getAttribute('placeholder');
// The composer's chrome is one reconciler over these elements (index.html keeps the
// markup, the editor is never touched); modules set state instead of poking buttons.
VectorSvelte.mountComposerChrome({
    els: {
        box: domChatMessageBox, input: domChatMessageInput,
        file: domChatMessageInputFile, cancel: domChatMessageInputCancel, emoji: domChatMessageInputEmoji,
        voice: domChatMessageInputVoice, send: domChatMessageInputSend,
        replyName: domChatReplyBarName, replySnippet: domChatReplyBarSnippet, replyCancel: domChatReplyBarCancel,
    },
    // Lazy: the helpers live in scripts that load after this one evaluates.
    h: {
        twemojify: (el) => twemojify(el),
        renderCustomEmojiShortcodes: (el, tags) => renderCustomEmojiShortcodes(el, tags),
        placeholder: strOriginalInputPlaceholder || 'Enter message...',
    },
});
VectorSvelte.mountCommandComposer({
    editor: domChatMessageInput,
    strip: document.getElementById('chat-command-bar'),
    onCancel: () => { if (commandCtrl) commandCtrl.exitComposer(); },
});
VectorSvelte.mountChatHeader({
    els: { avatar: domChatHeaderAvatarContainer, name: domChatContact, status: domChatContactStatus, menu: document.getElementById('chat-menu-btn'), backDot: domChatBackNotificationDot },
    // Lazy: the helpers live in scripts that load after this one evaluates.
    h: {
        myNpub: () => strPubkey,
        getChat: (id) => arrChats.find(c => c.id === id),
        getProfile: (npub) => getProfile(npub),
        getName: (x) => getName(x),
        getProfileAvatarSrc: (p) => getProfileAvatarSrc(p),
        createAvatarImg: (src, size, group) => createAvatarImg(src, size, group),
        twemojify: (el) => twemojify(el),
        renderCustomEmojiShortcodes: (el, tags) => renderCustomEmojiShortcodes(el, tags),
        convertFileSrc: (p) => convertFileSrc(p),
        isGroup: (chat) => chatIsGroup(chat),
        communityChatTitle: (chat) => communityChatTitle(chat),
        typingText: (chat) => generateTypingText(chat),
        memberSubtext: (cid) => communityMemberSubtext(cid),
        menuCount: (chat) => buildChatMenuItems(chat).length,
        openProfile: (profile) => { previousChatBeforeProfile = strOpenChat; openProfile(profile); },
        openCommunity: (chat) => openCommunityDetails(chat),
        chats: () => arrChats,
        backDotWanted: () => chatBackDotWanted(),
    },
});
VectorSvelte.mountComposerPopups({
    anchor: domChatMessageBox,
    // Lazy: the helpers live in scripts that load after this one evaluates.
    h: {
        bindCachedEmojiImg: (img, url, kind) => bindCachedEmojiImg(img, url, kind),
        twemojiUrl: (emoji) => emojiToTwemojiUrl(emoji),
    },
});

/**
 * Auto-resize the chat input textarea based on content.
 * Expands up to max-height defined in CSS (150px), then scrolls.
 * Only expands when content actually needs more space (multi-line).
 */
let _chatInputHeight = 0;

/**
 * The composer is a contenteditable, so it sizes itself between the min- and
 * max-height in CSS — no measure-and-set pass, which on a contenteditable
 * fights the growth it is trying to measure. This only reacts to a height
 * change, keeping the chat pinned the way the old resize did.
 */
function autoResizeChatInput() {
    const node = domChatMessageInput.el || domChatMessageInput;
    // A textarea has no intrinsic auto-grow, so the fallback still has to measure
    // and set. The rich composer sizes itself between its min- and max-height.
    if (!domChatMessageInput.el) {
        const computed = window.getComputedStyle(node);
        const padding = (parseFloat(computed.paddingTop) || 10.5) + (parseFloat(computed.paddingBottom) || 10.5);
        const singleLine = (parseFloat(computed.lineHeight) || 24) + padding;
        node.style.overflowY = 'hidden';
        node.style.height = '0';
        const needed = node.scrollHeight;
        if (needed > singleLine) {
            node.style.height = (needed - padding) + 'px';
            node.style.overflowY = 'auto';
        } else {
            node.style.height = '';
        }
    }
    const h = node.offsetHeight;
    if (h === _chatInputHeight) return;
    _chatInputHeight = h;
    softChatScroll();
}

/** Undo what autoResizeChatInput set, after the input is cleared. */
function resetChatInputSize() {
    // Legacy only. The rich composer is sized by the stylesheet, and an inline
    // overflow-y outranks it permanently — one send and a long draft could
    // never scroll again.
    if (domChatMessageInput.el) return;
    domChatMessageInput.style.height = '';
    domChatMessageInput.style.overflowY = 'hidden';
}

/**
 * Resize certain tricky components (i.e: the Chat Box) on window resizes.
 * 
 * This can also be re-called when some components are spawned, since they can
 * affect the height and width of other components, too.
 */
function adjustSize() {
    // Chat List: resize the list to fit within the screen after the upper Account area
    // Note: no idea why the `- 50px` is needed below, magic numbers, I guess.
    // Widescreen sizes the list by flex instead: this math treats the list as the
    // viewport, and would read the full-height rail as a viewport-tall navbar.
    if (!wsActive()) {
        const nNewChatBtnHeight = domChatNewDM?.getBoundingClientRect().height || 0;
        const nNavbarHeight = domNavbar.getBoundingClientRect().height;
        domChatList.style.maxHeight = (window.innerHeight - (domChatList.offsetTop + nNewChatBtnHeight + nNavbarHeight)) + 50 + 'px';
    }

    // Re-calculate chat input size on window resize (text may reflow)
    autoResizeChatInput();

    // If the chat is open, and they've not significantly scrolled up: auto-scroll down to correct against container resizes
    softChatScroll();
}

/**
 * Scrolls the chat to the bottom if the user has not already scrolled upwards substantially.
 * 
 * This is used to correct against container resizes, i.e: if an image loads, or a message is received.
 */
/**
 * Tracks whether the user wants to be pinned to the bottom of the chat.
 *
 * Only flipped by *user-initiated* scrolls — wheel, touch, keyboard. Pure
 * scroll events from a programmatic scrollTo, or from layout reflow as
 * media loads in, are ignored. This separation is what makes the chat
 * "self-heal" during chat-open: every async load fires softChatScroll,
 * which re-snaps to bottom; the resulting scroll event would normally
 * confuse a snapshot-based check during transitional layout, but here
 * it never sees a recent user-input timestamp and so leaves pinned=true
 * alone.
 *
 * Initial true: chat-open paths scroll to bottom synchronously, so the
 * user starts pinned by definition.
 */
let chatPinnedToBottom = true;

// "Window active" = the user can actually see the chat (window focused on
// desktop, page visible on mobile). Real-time arrivals must NOT auto-mark
// as read while the user is tabbed out; the catch-up fires when activity
// resumes (handled in setup_listeners).
let windowFocused = true;
let documentVisible = typeof document !== 'undefined' ? !document.hidden : true;
function isWindowActive() { return windowFocused && documentVisible; }

/** Tell the backend which chat the user is actively watching, so inbound
 *  messages in that chat auto-mark as read on arrival. Bumps badge counts
 *  in lock-step with our FE markAsRead — without this the on_dm_received
 *  task can race ahead and tick the dock badge before markAsRead lands. */
let _lastReportedActiveChat = '__init__';
function syncBackendActiveChat() {
    const id = (strOpenChat && chatPinnedToBottom && isWindowActive()) ? strOpenChat : null;
    if (id === _lastReportedActiveChat) return;
    _lastReportedActiveChat = id;
    invoke('set_active_chat', { chatId: id }).catch(() => { /* best-effort */ });
}

const PIN_THRESHOLD_PX = 80;

// Intent-aware pin. PIN_THRESHOLD_PX alone makes the pin purely positional, so a
// user resting just under the threshold gets re-snapped to the bottom by every
// auto-scroll. The latch records that the USER scrolled up and KEEPS the pin
// released until they return to the true bottom — a slight scroll-up sticks.
const BOTTOM_EPSILON_PX = 6;        // "true bottom" tolerance for clearing the latch
const PROGRAMMATIC_SCROLL_MS = 120; // suppress user-scroll-up detection just after an app scroll
let lastScrollTop = 0;
// Content height at the previous pin evaluation, so growth beneath the view can be
// told apart from the user moving away from the bottom.
let _lastPinEvalHeight = 0;
let _userScrolledAway = false;
/** Swap a rendered message row without moving the reader.
 *
 *  `replaceWith` removes before it inserts, so the list is briefly shorter and
 *  the engine clamps `scrollTop` down by the row's height. Put it back, and mark
 *  the move as ours: a re-render is never the reader leaving the page. */
function replaceMessageRow(domMsg, fresh) {
    const s = domChatMessages;
    const top = s ? s.scrollTop : 0;
    beginProgrammaticScroll();
    domMsg.replaceWith(fresh);
    if (s && s.scrollTop !== top) {
        s.scrollTop = top;
        beginProgrammaticScroll();
    }
}

let _programmaticScrollUntil = 0;
/** Mark a short window during which scroll events are the app's own (not the
 *  user). Call immediately before any programmatic scrollTop change so a drop-top
 *  compensation (scrollTop -= droppedHeight) isn't misread as a user scroll-up. */
function beginProgrammaticScroll() {
    _programmaticScrollUntil = Date.now() + PROGRAMMATIC_SCROLL_MS;
    // Re-baseline so the very next scroll event's delta is measured from the
    // post-jump position, not the pre-jump one.
    if (domChatMessages) lastScrollTop = domChatMessages.scrollTop;
}

let unreadBelowCount = 0;
let unreadDividerEl = null;
const domChatScrollReturnBadge = document.getElementById('chat-scroll-return-badge');

/**
 * Insert (or reuse) the "New" divider relative to the given message element.
 * Persists for the chat session — only the first unread message gets a
 * divider; later messages just stack under it. Cleared by openChat()
 * (close + re-enter) and by sending a message.
 *
 * `anchorAfter=false` (default) inserts the divider BEFORE the row (it sits
 * above that row). `anchorAfter=true` inserts it AFTER the row (the divider
 * sits below the anchor, i.e. above the NEXT row). The manual scroll-up path
 * anchors AFTER the last-read row: the boundary row is older than the first
 * unread, so it survives the scroll-up bottom-trim longer, and "after last_read"
 * = "above the first new message" regardless of who sent it.
 */
function insertUnreadDivider(anchorEl, anchorAfter = false) {
    if (unreadDividerEl || !anchorEl?.parentNode || !anchorEl.id) return;
    // The list island renders the divider beside its target row; the element and
    // its anchor mode are kept for the callers that measure it.
    VectorSvelte.setDivider(anchorEl.id, anchorAfter);
    VectorSvelte.flushSync();
    const p = domChatMessages.querySelector(':scope > .unread-divider');
    if (p) { p._targetId = anchorEl.id; p._anchorAfter = anchorAfter; unreadDividerEl = p; }
}
function clearUnreadDivider() {
    VectorSvelte.clearDivider();
    VectorSvelte.flushSync();
    unreadDividerEl = null;
}
function setUnreadBelow(n) {
    unreadBelowCount = Math.max(0, n);
    if (!domChatScrollReturnBadge) return;
    if (unreadBelowCount > 0) {
        domChatScrollReturnBadge.textContent = unreadBelowCount > 99 ? '99+' : String(unreadBelowCount);
        domChatScrollReturnBadge.classList.add('visible');
    } else {
        domChatScrollReturnBadge.textContent = '';
        domChatScrollReturnBadge.classList.remove('visible');
    }
}
function incrementUnreadBelow() { setUnreadBelow(unreadBelowCount + 1); }
function clearUnreadBelow() { setUnreadBelow(0); }

/**
 * Recompute chatPinnedToBottom. Intent-aware: a USER scroll-up (scrollTop
 * decreased on a non-programmatic event) releases the pin and latches it
 * released until the user returns to the true bottom. The app's own scrolls
 * (guarded by beginProgrammaticScroll) never trip the latch.
 */
function handleChatScrollIntent() {
    if (!strOpenChat || !domChatMessages) return;
    const scrollTop = domChatMessages.scrollTop;
    const scrollHeight = domChatMessages.scrollHeight;
    const pxFromBottom = scrollHeight - scrollTop - domChatMessages.clientHeight;
    const isProgrammatic = Date.now() < _programmaticScrollUntil;
    // Content grew and the view did NOT move up: the world got taller beneath us,
    // which is never the user leaving the bottom.
    const prevHeight = _lastPinEvalHeight;
    _lastPinEvalHeight = scrollHeight;
    const grewUnderUs = scrollHeight > prevHeight && scrollTop >= lastScrollTop;

    // User scrolled UP (and it wasn't us) → release and latch until they're
    // back at the true bottom. Drop-top compensations move scrollTop up too,
    // but they're wrapped in beginProgrammaticScroll, so isProgrammatic gates them out.
    //
    // A SHRINK clamp is not a scroll-up: switching to a shorter chat (or hiding
    // rows) makes the engine pull scrollTop down on its own, outside any
    // programmatic window. That latched "the user left" on chat-open, and the
    // latch outlives the open — every later scroll-to-bottom then refused to run.
    // A clamp always lands at the bottom, which is what separates it from a real
    // scroll-up (that leaves real distance below).
    const clampedByShrink = scrollHeight < prevHeight && pxFromBottom < BOTTOM_EPSILON_PX;
    if (!isProgrammatic && scrollTop < lastScrollTop - 1 && !clampedByShrink) {
        _userScrolledAway = true;
    }
    // Returned to the true bottom → allow re-pin (genuine "glue to the live tail").
    if (pxFromBottom < BOTTOM_EPSILON_PX) {
        _userScrolledAway = false;
    }
    lastScrollTop = scrollTop;

    // Manually scrolled up toward the unread boundary (instead of clicking the pill): reveal the
    // "New" divider as the boundary loads, and retire the pill + mark caught up once it's in view.
    revealUnreadFrontierIfReached();

    const wasPinned = chatPinnedToBottom;
    // Only the USER leaves the bottom. Distance alone used to release the pin, so
    // media resolving after a chat-open — which drops hundreds of px in below the
    // fold in one frame — blew past PIN_THRESHOLD_PX and silently released it,
    // after which every scroll-to-bottom path refused to run and the chat sat
    // short forever. So the pin also survives while the app is actively holding
    // the bottom, and whenever the content simply grew beneath a view that didn't
    // move up. A seek moves the view up, so it still releases normally.
    chatPinnedToBottom =
        (pxFromBottom < PIN_THRESHOLD_PX || chatBottomHoldPending() || (wasPinned && grewUnderUs))
        && !_userScrolledAway;
    // User scrolled themselves back into pin range — clear the badge and
    // advance last_read so the OS unread indicator reflects reality. The
    // divider stays put until the chat is closed.
    if (!wasPinned && chatPinnedToBottom) {
        clearUnreadBelow();
        const currentChat = getChat(strOpenChat);
        if (currentChat?.messages?.length) {
            const latestNonMine = findLatestContactMessage(currentChat.messages);
            if (latestNonMine) markAsRead(currentChat, latestNonMine);
        }
    }
    if (wasPinned !== chatPinnedToBottom) syncBackendActiveChat();
}

function softChatScroll() {
    if (!strOpenChat) return;
    if (!chatPinnedToBottom) return;
    // Windowing: the pin only drives scrolling in the NEWEST window. Windowed away from the live
    // tail, scrolling to the DOM bottom would trip windowExtendNewer → re-render → re-scroll, an
    // infinite down-window cascade. Stay put; the ↓ button is the way back to "now".
    if (CHAT_WINDOW_ENABLED && !isAtDataBottom()) return;
    scrollToBottom(domChatMessages, false);
    // Whatever prompted this (a media load, a new message) may keep growing the
    // content below us for a few frames — ride it down.
    holdChatBottom();
}

window.onresize = adjustSize;

/** Wire the chat view chrome: back, bookmarks, header menu, wallpaper, scroll, new chat, reply bar. */
async function wireChatUi() {
    domChatBackBtn.onclick = closeChat;
    domChatBookmarksBtn.onclick = () => {
        openChat(strPubkey);
    };
    domChatNewBackBtn.onclick = closeChat;

    // Chat-header overflow menu — dropdown of chat-scoped actions. Currently
    // hosts "Change Wallpaper" for DM chats. Group chats don't get wallpapers
    // by design, so the option only renders when the open chat is a DM.
    const domChatMenuBtn = document.getElementById('chat-menu-btn');
    if (domChatMenuBtn) {
        domChatMenuBtn.addEventListener('click', (e) => {
            e.stopPropagation();
            const rect = domChatMenuBtn.getBoundingClientRect();
            const items = buildChatMenuItems(getChat(strOpenChat));
            if (!items.length) return;
            showContextMenu({ x: rect.right, y: rect.bottom + 4, items });
        });
    }

    // Self-Destruct Timer: right-click / long-press the Send button, plus the
    // active-timer clock indicator injected next to the composer.
    setupSelfDestructComposer();

    // Wallpaper edit UI — header Cancel/Save overlay, bottom sliders.
    const wallpaperEditSave = document.getElementById('wallpaper-edit-save-btn');
    const wallpaperEditCancel = document.getElementById('wallpaper-edit-cancel-btn');
    const wallpaperBlurSlider = document.getElementById('wallpaper-blur-slider');
    const wallpaperDimSlider = document.getElementById('wallpaper-dim-slider');
    if (wallpaperEditSave) {
        wallpaperEditSave.onclick = () => confirmWallpaperChange();
    }
    if (wallpaperEditCancel) {
        wallpaperEditCancel.onclick = () => cancelWallpaperChange();
    }
    if (wallpaperBlurSlider) {
        wallpaperBlurSlider.addEventListener('input', onWallpaperSliderInput);
    }
    if (wallpaperDimSlider) {
        wallpaperDimSlider.addEventListener('input', onWallpaperSliderInput);
    }

    // Add scroll event listener for procedural message loading + intent tracking
    let scrollTimeout;
    domChatMessages.addEventListener('scroll', () => {
        handleChatScrollIntent();
        if (scrollTimeout) clearTimeout(scrollTimeout);
        scrollTimeout = setTimeout(() => {
            handleProceduralScroll();
        }, 100);
    });
    domChatNewStartBtn.onclick = () => {
        const inputValue = domChatNewInput.value.trim();
        domChatNewInput.value = ``;
        // Same parser as the QR scanner: invites join, npubs DM, and any
        // URL wrapper around either is ignored.
        const parsed = parseContactInput(inputValue);
        if (parsed?.kind === 'invite') {
            previewAndJoinCommunityLink(parsed.url);
            return;
        }
        // No extractable npub falls through raw — openChat owns rejection
        openChat(parsed?.npub || inputValue);
    };
    domChatNewInput.onkeydown = async (evt) => {
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            domChatNewStartBtn.click();
        }
    };
    domChatNewInput.addEventListener('input', function() {
        domChatNewStartBtn.style.display = this.value.length > 0 ? '' : 'none';
    });

    // Tooltip for help icon
    document.querySelector('.chat-new-help-link').addEventListener('mouseenter', function() {
    showGlobalTooltip('Visit the Vector Privacy Docs', this);
    });
    document.querySelector('.chat-new-help-link').addEventListener('mouseleave', hideGlobalTooltip);

    domChatMessageInputCancel.onclick = () => {
        // Cancel edit mode if active, otherwise cancel reply
        if (strCurrentEditMessageId) {
            cancelEdit();
        } else {
            cancelReply();
        }
    };

    domChatReplyBarCancel.onclick = () => cancelReply();

    // Tapping the reply bar's content jumps to the message being replied to,
    // like tapping an inline reply quote. The cancel button keeps its own handler.
    const domChatReplyBar = document.getElementById('chat-reply-bar');
    if (domChatReplyBar) {
        domChatReplyBar.addEventListener('click', (e) => {
            if (e.target.closest('#chat-reply-bar-cancel')) return;
            jumpToMessage(strCurrentReplyReference);
        });
    }

    // Hook up a scroll handler in the chat to display UI elements at certain scroll depths
    createScrollHandler(domChatMessages, domChatMessagesScrollReturnBtn, {
        threshold: 500,
        isPinned: () => chatPinnedToBottom,
        onClick: clearUnreadBelow,
        // With windowing, newer messages can live below the rendered window even
        // when the DOM is "at its bottom" — keep the button up whenever we're not
        // viewing the live tail so the user can always get back to "now".
        shouldForceVisible: () => CHAT_WINDOW_ENABLED && !isAtDataBottom(),
        // Inverse: at the live tail, force-hide so a media-reflow scroll can't strand the button on.
        shouldForceHidden: () => CHAT_WINDOW_ENABLED && isAtDataBottom(),
        // Click must reach the true data bottom. When windowed away, re-render the
        // newest window + pin; otherwise fall through to the default scrollTo.
        onJumpToBottom: () => {
            chatPinnedToBottom = true;
            _userScrolledAway = false;   // explicit return to "now" releases the latch
            if (CHAT_WINDOW_ENABLED && !isAtDataBottom()) {
                windowJumpToBottom();   // re-renders newest MAX window, pins, clears badge
                return true;
            }
            syncBackendActiveChat();
            return false;
        },
    });
}
