/**
 * Emoji + GIF picker — the floating panel that appears above the chat input.
 *
 * Two modes share one container: emoji (default) and GIF (Gifverse API). The
 * mode toggle, search box, and dispatch logic all live here. Emoji data and
 * persistence (recent / shortcodes) live in emoji.js.
 *
 * The panel is also used as a reaction picker: openReactionPicker(msgId) (the message
 * toolbar's React) makes the next emoji selection a reaction to that message instead
 * of an insert. `strCurrentReactionReference` tracks the target message id meanwhile.
 *
 * Cross-file dependencies (resolved at call time via classic-script scope):
 *   - emoji.js   — arrEmojis, searchEmojis, getMostUsedEmojis
 *   - twemoji    — twemojify
 *   - main.js    — domChatMessageInput; the box's buttons via composerEls(),
 *                  closeAttachmentPanel, platformFeatures, arrChats
 *   - miniapps-panel.js — closeMiniAppLaunchDialog
 *   - message-row.js    — dmsgReactOptimistic
 */

// The root is the component's; its visibility drives the Android back stack and dismisses
// the shared tooltip on keyboard-driven closes (no mouse leaves, so no mouseleave).
VectorSvelte.onPickerVisibility((visible) => {
    if (visible) {
        pushBack('emoji-picker', () => {
            VectorSvelte.setPickerVisible(false);
            VectorSvelte.setPickerBottom('');
        });
    } else {
        hideEmojiTooltip();
        popBack('emoji-picker');
        // A close lands back in emoji mode with the composer's face at rest; the GIF page
        // reloads on the next open.
        setPickerMode(PICKER_MODE_EMOJI);
        trendingGifsLoaded = false;
        VectorSvelte.setEmojiIcon('smile');
    }
});

// The panel's chrome is a component over the root. The hosts it hands back are what the
// scroll spy, rail follow, jump-scroll and keyboard paths work on; everything the
// component needs from the app arrives through one bag.
const _pickerEls = { search: null, sidebar: null, main: null, recents: null, all: null, results: null, gif: null, creatorGrid: null };
/** @type {HTMLInputElement} */
let emojiSearch = null;
const _cropperEls = { stage: null, img: null, box: null, preview: null, cancel: null, ok: null };

const _pickerPanelHelpers = {
    mounted: (els) => { Object.assign(_pickerEls, els); emojiSearch = els.search; },
    searchInput: (e) => _onSearchInput(e),
    // The root's own click routing: pack emoji first (capture, it stops the event), then
    // the stock grids' spans and images.
    rootClickCapture: (e) => _onPackEmojiClick(e),
    rootClick: (e) => { _onEmojiSpanClick(e); _onEmojiImgClick(e); },
    searchKeydown: (e) => _onSearchKeydown(e),
    setMode: (mode) => setPickerMode(mode === 'gif' ? PICKER_MODE_GIF : PICKER_MODE_EMOJI),
    railClick: (e) => _onRailClick(e),
    // Grabbing the rail wins over an in-flight follow: never animate against the user's own finger.
    railStop: () => _railStop(),
    mainClick: (e) => {
        const header = e.target.closest('.emoji-section-header');
        if (!header) return;
        e.stopPropagation();
        header.parentElement.classList.toggle('collapsed');
    },
    mainScroll: () => _onMainScroll(),
    gifClick: (e) => _onGifGridClick(e),
    gifScroll: () => _onGifGridScroll(),
    /**
     * PickerIslandHelpers: the rail (PackSidebar), the pack sections (PackSections) and the
     * stock grids. `mountGrid` is the one deliberately imperative leaf: a pack section is ONE
     * canvas drawn from a shared decoded-frame cache, so a pack of 2,000 emoji costs one node
     * and one compositor layer; a keyed each over cells tripled the DOM and stalled every
     * hover for seconds.
     * @typedef {Object} PickerIslandHelpers
     * @property {(pack: object) => string} deadMessage
     * @property {() => boolean} isLinux
     * @property {(el: Element, pack: object) => void} packMenu       long-press menu on a section header
     * @property {(pack: object) => void} unsubscribe
     * @property {(pack: object) => number} sectionHeight            the section's fixed height in px, for content-visibility
     * @property {(section: Element, pack: object) => () => void} mountGrid   appends the pack's canvas; returns its teardown
     * @property {() => void} afterRender                            the sections are in the DOM: arm the canvases, calibrate chrome
     * @property {(img: HTMLImageElement, url: string, kind: string) => void} bindCachedImg
     * @property {(pack: object, x: number, y: number) => void} showTabMenu
     * @property {() => void} closeMenu
     * @property {(fromId: string, toId: string, isBefore: boolean) => void} reorderPack
     * @property {(pack: object) => boolean} packIsDead
     * @property {(pack: object) => string} packInitial
     * @property {(id?: string) => void} openCreator
     * @property {(el: Element) => void} twemojify
     * @property {(e: object) => string} stockTitle
     * @property {() => HTMLElement|null} scrollRoot        the panel's scroller, for the grids' observers
     * @property {() => object[]} recents
     * @property {() => object[]} all
     * @property {(q: string) => object[]} search
     * @property {(item: object, el: Element, placeholder: string|null) => void} loadMedia   a GIF tile's media with format fallback
     */
    islands: {
        deadMessage: (pack) => deadPackMessage(pack),
        isLinux: () => platformFeatures?.os === 'linux',
        packMenu: (el, pack) => attachLongPressContextMenu(el, (x, y) => _showPackTabMenu(pack, x, y)),
        unsubscribe: (pack) => _unsubscribePackFromMenu(pack),
        sectionHeight: (pack) => _packSectionHeightPx(pack),
        mountGrid: (section, pack) => _mountPackCanvasGrid(section, pack),
        afterRender: () => _afterPackSectionsRender(),
        bindCachedImg: (img, url, kind) => bindCachedEmojiImg(img, url, kind),
        showTabMenu: (pack, x, y) => _showPackTabMenu(pack, x, y),
        closeMenu: () => hideContextMenu(),
        reorderPack: (fromId, toId, isBefore) => _applyPackTabReorder(fromId, toId, isBefore),
        packIsDead: (pack) => packIsDead(pack),
        packInitial: (pack) => _packTitleInitial(pack),
        openCreator: (id) => openEmojiPackCreator(id),
        twemojify: (el) => twemojify(el),
        stockTitle: (e) => stockEmojiTitle(e),
        scrollRoot: () => _pickerEls.main,
        recents: () => getMostUsedEmojis().slice(0, 24).concat(getMostUsedCustomEmojis(24))
            .sort((a, b) => (b.used || 0) - (a.used || 0)).slice(0, 24),
        all: () => arrEmojis,
        search: (q) => searchEmojis(q).filter(e => e.name.toLowerCase().includes(q))
            .concat(searchCustomEmojis(q)).sort((a, b) => (a.score || 0) - (b.score || 0)).slice(0, 48),
        loadMedia: (item, el, placeholder) => loadGifWithFallback(el, `${GIF_API_BASE}/media`, item.id, item.title, placeholder, 0),
    },
    creator: {
        bindCachedImg: (img, url, kind, onUnavailable) => bindCachedEmojiImg(img, url, kind, onUnavailable),
        unavailableMessage: (reason) => emojiUnavailableMessage(reason).replace(/<br\s*\/?>/gi, ' '),
        goneMessage: () => _pcGoneMessage(),
        isMobile: () => !!platformFeatures?.is_mobile,
        remove: (idx) => _pcRemoveEmoji(idx),
        // A broken emoji can't be renamed: explain why and how to fix it instead.
        cellClick: (idx, broken) => {
            if (broken) { _pcBrokenEmojiError(_emojiFailReason.get(_pc.emojis[idx]?.url) || ''); return; }
            _pcRenameEmoji(idx);
        },
        cellMenu: (idx, x, y) => _pcCellMenu(idx, x, y),
        reorderEmoji: (from, targetIdx, isBefore) => _pcReorderEmoji(from, targetIdx, isBefore),
        gridMounted: (el) => { _pickerEls.creatorGrid = el; },
        // Native picker + Rust import (content URIs, non-web-safe formats), not <input type=file>.
        pickLogo: async () => { const file = await _pcPickImage(false); if (file) _pcSetLogoFile(file); },
        pickImages: () => _pcPickImage(true),
        addFiles: (files) => { if (files && files.length) _pcAddFiles(files); },
        nameInput: (value) => { _pc.name = value; _pc.dirty = true; },
        done: () => closeEmojiPackCreator(),
        deletePack: () => _pcDelete(),
    },
    overlays: {
        bindCachedImg: (img, url, kind) => bindCachedEmojiImg(img, url, kind),
        errorRetry: () => _pcHideSizeError(),
        confirm: (ok) => _pcConfirmFinish(ok),
        namingInput: () => VectorSvelte.setPickerNamingError(''),
        namingCommit: (raw) => _pcNamingTryCommit(raw),
        namingCancel: () => _pcNamingFinish(null),
        cropperEls: (els) => Object.assign(_cropperEls, els),
    },
};
// Synchronous: the listeners and observers below need the hosts at load.
VectorSvelte.setPickerHandlers(_pickerPanelHelpers);
VectorSvelte.flushSync();
/**
 * The current reaction reference - i.e: a message being reacted to.
 *
 * When empty, emojis are simply injected to the current chat input.
 */
let strCurrentReactionReference = "";

/** When set, non-reaction selections insert via this callback instead of the
 *  chat input — the Status dialog points it at its own field. Owned by the
 *  dialog's lifecycle: it clears the target when it closes. */
let _emojiPanelTarget = null;

/**
 * Open the panel for a dialog-owned input: selections route to `onInsert`,
 * GIFs are hidden (statuses carry text + emoji only), and the panel rises
 * above the dialog overlay. Mirrors openEmojiPanel's deferred-render shape so
 * the open transition starts this frame and content fills in after.
 */

function openEmojiPanelForStatus(onInsert) {
    _emojiPanelTarget = { insert: onInsert };
    setPickerMode(PICKER_MODE_EMOJI);
    VectorSvelte.setPickerAnchor({ statusMode: true, noGifs: true, messageType: false, bottom: '' });
    VectorSvelte.setPickerVisible(true);
    requestAnimationFrame(() => requestAnimationFrame(async () => {
        if (!VectorSvelte.pickerVisible()) return;
        await loadEmojiUsage();
        if (!VectorSvelte.pickerVisible()) return;
        resetEmojiPicker();
        renderEmojiPanel();
        if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
            emojiSearch.focus();
        }
        if (!emojiPacksLoaded) {
            loadEmojiPacks();
        }
        loadEmojiPacks({ refresh: true });
        _attachEmojiPackReveal();
        _rearmVisiblePackCanvases();
    }));
}

/**
 * Opens the Emoji Input Panel
 *
 * The panel always appears in a fixed position at the bottom, regardless of whether
 * it's opened from the message input or a reaction button.
 * @param {MouseEvent?} e - An associated click event
 */
function openEmojiPanel(e) {
    const emojiBtn = VectorSvelte.composerEls().emoji;
    const isDefaultPanel = e.target === emojiBtn || emojiBtn.contains(e.target);

    // Don't close if clicking inside the picker itself. The path is read rather than
    // the target's ancestry: a panel control that unmounts on click is detached by
    // the time this document listener runs.
    const root = VectorSvelte.pickerEls().root;
    if (root.contains(e.target) || (e.composedPath?.() || []).includes(root)) return;

    if (isDefaultPanel && !VectorSvelte.pickerVisible()) _openPanel({ isDefaultPanel: true, reactionId: '' });
    else closeEmojiPanel();
}

/** The toolbar's React: the panel as a reaction picker for one message. */
function openReactionPicker(msgId) {
    if (!VectorSvelte.pickerVisible()) _openPanel({ isDefaultPanel: false, reactionId: msgId });
    else closeEmojiPanel();
}

function _openPanel({ isDefaultPanel, reactionId }) {
    const strReaction = reactionId;
    {
        // Close attachment panel if open
        if (VectorSvelte.attachmentVisible()) {
            closeAttachmentPanel();
        }

        // --- Synchronous: kick off the open transition THIS frame. Keep this
        // block tiny — any heavy work here delays the first "opening" paint, and
        // the open is a compositor transform/opacity transition. The panel is
        // always display:flex (hidden via transform), so it animates in
        // regardless of content, and last session's emoji DOM stays put until the
        // deferred render refreshes it (no empty flash on re-open). ---

        // Read chat-box height while the layout is still clean (before .visible
        // and before the deferred render mutates panel DOM) to avoid a reflow.
        const chatBox = VectorSvelte.composerEls().box;
        const bottomPx = chatBox ? (chatBox.getBoundingClientRect().height + 10) + 'px' : '';

        // A status open may still be exiting; its centred anchor must not carry
        // into this one, and the swap has to settle before `visible` animates.
        if (VectorSvelte.pickerRoot().statusMode) VectorSvelte.setPickerAnchor({ statusMode: false, noGifs: false });
        VectorSvelte.setPickerAnchor({ messageType: true, bottom: bottomPx || '' }, false);
        VectorSvelte.setPickerVisible(true);

        // Swap the emoji button to a wink while open (message input only).
        if (isDefaultPanel) {
            VectorSvelte.setEmojiIcon('wink');
        }
        strCurrentReactionReference = strReaction || '';

        // --- Deferred: the expensive content build (twemoji recents,
        // the ~1.8k-span All grid, pack sidebar/sections + canvas loop) runs
        // AFTER the panel's first visible paint. Double rAF: the outer fires in
        // the same frame the .visible style lands (transition starts), the inner
        // fires the frame after (content fills in, ~16ms into the 300ms slide). ---
        requestAnimationFrame(() => requestAnimationFrame(async () => {
            // Bail if the panel was closed again before this fired.
            if (!VectorSvelte.pickerVisible()) return;

            // Hydrate recents/search ranking from the per-account usage store
            // (fast IPC; also picks up the active account after a swap). Re-check
            // visibility after the await in case the panel closed meanwhile.
            await loadEmojiUsage();
            if (!VectorSvelte.pickerVisible()) return;

            resetEmojiPicker();
            renderEmojiPanel();

            // Focus the search box (desktop only — mobile keyboards are disruptive).
            if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
                emojiSearch.focus();
            }

            // Prefetch GIF data in background (non-blocking)
            prefetchTrendingGifs();

            // Cold-load packs on first open. On reopen the sidebar + sections
            // (and their canvas grids, with frames already decoded) persist in
            // the DOM — the picker is never display:none — so we DON'T re-render.
            // Recreating grids every open was redundant and, worse, spun up fresh
            // IntersectionObservers that compute intersection mid-open-transition
            // (panel transformed off-screen) and wrongly mark visible packs as
            // off-screen. A background refresh re-renders only if packs changed.
            if (!emojiPacksLoaded) {
                loadEmojiPacks();
            }
            loadEmojiPacks({ refresh: true });
            _attachEmojiPackReveal();
            // Re-activate the on-screen pack canvases (the close drained the
            // active set). Persisted grids keep their frames, so this resumes
            // animation immediately.
            _rearmVisiblePackCanvases();
        }));
    }
}

/** Hide + reset the emoji/reaction panel. Extracted from the toggle so other
 *  flows (e.g. a reacted-to message being removed) can dismiss it too. */
function closeEmojiPanel() {
    // Hide and reset the UI - use class instead of inline style
    emojiSearch.value = '';
    // Auto-save any in-progress pack edit before the panel disappears.
    if (_pc.open) closeEmojiPackCreator();
    VectorSvelte.setPickerVisible(false);
    // A dialog-owned insert target ends with the panel; a reopen for the dialog sets it again.
    _emojiPanelTarget = null;
    // The anchor classes outlive the exit: dropping them now retargets the
    // transform mid-flight and the panel leaves at an angle. Re-opening inside
    // the window must not have its own classes stripped by this timer.
    setTimeout(() => {
        if (!VectorSvelte.pickerVisible()) VectorSvelte.setPickerAnchor({ statusMode: false, noGifs: false });
    }, 320);
    VectorSvelte.setPickerBottom('');
    strCurrentReactionReference = '';
    // Drop the canvas rAF so we don't tick under opacity:0.
    _stopPackCanvasLoop();

    // Change the emoji button to the regular face
    VectorSvelte.setEmojiIcon('smile');
}



async function _sharePackToClipboard(pack) {
    try {
        // Share as a vectorapp.io URL — friends without Vector get a
        // working web preview, friends with Vector get the OS-level
        // deep-link interception that pops the Pack Details modal.
        // Relay hints ride the shared naddr; the canonical pack.id stays bare.
        const url = `https://vectorapp.io/emojis/pack/${await _shareNaddr(pack.id)}`;
        await navigator.clipboard.writeText(url);
        showToast('Copied to Clipboard');
        // Close the picker so the user lands back in their chat ready
        // to paste the link they just copied.
        VectorSvelte.setPickerVisible(false);
    } catch (e) {
        console.warn('[emoji-packs] share-copy failed:', e);
        showToast('Failed to Copy');
    }
}

async function _unsubscribePackFromMenu(pack) {
    try {
        // If this is the active theme's pack, seed the theme cache with the copy
        // we already have so it stays pinned with no fetch gap after unsubscribe
        // — it just flips from a subscribed pin to a theme pin, you keep the pack.
        const themeNaddr = THEME_EMOJI_PACKS[_currentThemeName()];
        if (themeNaddr && pack.id === themeNaddr && !_themePackCache[themeNaddr]) {
            // Theme pins are never health-judged; carrying a dead verdict over
            // would grey a pin the health engine no longer tracks.
            _themePackCache[themeNaddr] = { ...pack, is_theme: true, status: 0 };
        }
        await invoke('unsubscribe_emoji_pack', { id: pack.id });
        await loadEmojiPacks();
    } catch (e) {
        console.warn('[emoji-packs] unsubscribe failed:', e);
    }
}

function _showPackTabMenu(pack, x, y) {
    const items = [
        {
            label: 'Share Pack',
            icon: 'share',
            onClick: () => _sharePackToClipboard(pack),
        },
    ];
    // Own packs get an Edit entry (same target as the section-header pencil),
    // above the soft Remove.
    if (pack.is_own) {
        items.push({
            label: 'Edit Pack',
            icon: 'edit',
            onClick: () => openEmojiPackCreator(pack.id),
        });
    }
    // Theme packs are pinned by the active theme, not user subscriptions —
    // there's nothing to "remove". Sharing still applies (real pack + naddr).
    if (!pack.is_theme) {
        items.push({
            // "Remove" is a soft action on every pack — unsubscribes
            // locally + republishes kind 10030 without it. For own packs
            // the file + Nostr event stay in place so re-subscribing
            // later (paste naddr) restores it with the edit pencil.
            // The permanent-delete path (Blossom cleanup + tombstone)
            // lives behind the Edit Pack creator's Delete button.
            label: 'Remove Pack',
            icon: 'x-user',
            danger: true,
            onClick: () => _unsubscribePackFromMenu(pack),
        });
    }
    showContextMenu({ x, y, items });
}

function renderEmojiPackSidebar() {
    _ensureEmojiPickerIslands();
    VectorSvelte.setPickerPacks(arrEmojiPacks);
}

// The rail and the three stock grids are islands inside the panel; they render on the
// first open, not at boot. The pack sections (canvas grids) and every gesture stay here.
let _emojiPickerIslandsMounted = false;
function _ensureEmojiPickerIslands() {
    if (_emojiPickerIslandsMounted) return;
    _emojiPickerIslandsMounted = true;
    VectorSvelte.setPickerReady();
}

// Apply the drop: reorder `arrEmojiPacks` optimistically + repaint, then
// persist the full order (marker included) so it syncs. The theme-slot tab
// maps to the `theme_slot` token; real tabs map to their pack id.
function _applyPackTabReorder(fromId, toId, isBefore) {
    if (fromId === toId) return;

    const arr = arrEmojiPacks.slice();
    const fromIdx = arr.findIndex(p => p.id === fromId);
    if (fromIdx === -1) return;
    const [moved] = arr.splice(fromIdx, 1);
    const toIdx = arr.findIndex(p => p.id === toId);
    if (toIdx === -1) return;
    let insertAt = toIdx + (isBefore ? 0 : 1);
    if (insertAt < 0) insertAt = 0;
    if (insertAt > arr.length) insertAt = arr.length;
    arr.splice(insertAt, 0, moved);

    arrEmojiPacks = arr;
    _lastPacksSignature = null;   // force a repaint past the idempotence guard
    renderEmojiPackSidebar();
    renderEmojiPackSections();

    const orderedIds = arrEmojiPacks.map(p => (p._isThemeSlot ? 'theme_slot' : p.id));
    invoke('reorder_emoji_packs', { orderedIds })
        .catch(e => console.error('reorder_emoji_packs failed:', e));
}

/**
 * Replace `:shortcode:` occurrences inside `rootEl` with `<img>` elements
 * sourced from the message's `emoji_tags` array. Walks text nodes so
 * existing markup (links, mentions, code blocks) is preserved.
 *
 * Phase 1 contract: `emojiTags` is the array attached to an inbound
 * message rumor via NIP-30 tags. Missing or empty → noop, shortcode
 * stays literal.
 *
 * @param {HTMLElement} rootEl
 * @param {Array<{shortcode:string,url:string}>|undefined} emojiTags
 */
function renderCustomEmojiShortcodes(rootEl, emojiTags) {
    if (!rootEl || !emojiTags || !emojiTags.length) return;
    const map = new Map();
    for (const t of emojiTags) {
        if (t && t.shortcode && t.url) map.set(t.shortcode, t.url);
    }
    if (!map.size) return;

    // `~` matches the disambiguation separator (`:love~2:`).
    const pattern = /:([a-zA-Z0-9_~-]+):/g;
    const walker = document.createTreeWalker(rootEl, NodeFilter.SHOW_TEXT, {
        acceptNode(node) {
            // Skip text inside pack-emoji spans and code blocks — those
            // already carry their own colon-bracketed labels.
            let p = node.parentElement;
            while (p && p !== rootEl) {
                if (p.tagName === 'CODE' || p.tagName === 'PRE') return NodeFilter.FILTER_REJECT;
                if (p.classList && p.classList.contains('emoji-pack-emoji')) return NodeFilter.FILTER_REJECT;
                p = p.parentElement;
            }
            // A `g` regex's test() resumes from the PREVIOUS node's lastIndex —
            // when twemoji has split the text into segments, that silently
            // rejects every other node holding a shortcode.
            pattern.lastIndex = 0;
            return pattern.test(node.nodeValue) ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
        }
    });

    const matches = [];
    let n;
    while ((n = walker.nextNode())) matches.push(n);

    for (const textNode of matches) {
        const original = textNode.nodeValue;
        const frag = document.createDocumentFragment();
        let lastIndex = 0;
        pattern.lastIndex = 0;
        let m;
        while ((m = pattern.exec(original)) !== null) {
            const url = map.get(m[1]);
            if (!url) continue;
            if (m.index > lastIndex) {
                frag.appendChild(document.createTextNode(original.slice(lastIndex, m.index)));
            }
            const shortcode = m[1];
            const img = document.createElement('img');
            // Share twemoji's `emoji` class so custom emojis inherit the exact
            // same per-context sizing rules; `custom-emoji-inline` adds only the
            // custom-specific bits (fill for non-square images).
            img.className = 'emoji custom-emoji-inline';
            img.alt = `:${shortcode}:`;
            img.dataset.emojiTooltip = `:${shortcode}:`;
            frag.appendChild(img);
            // Unavailable emoji (404 / removed / over the 1 MB cap) → a tappable `:shortcode:` chip
            // that explains why, so the message still reads coherently and the reason is discoverable.
            bindCachedEmojiImg(img, url, 'emoji', (el, reason) => {
                el.replaceWith(makeUnavailableEmojiChip(shortcode, reason));
            });
            lastIndex = m.index + m[0].length;
        }
        if (lastIndex === 0) continue;
        if (lastIndex < original.length) {
            frag.appendChild(document.createTextNode(original.slice(lastIndex)));
        }
        textNode.parentNode.replaceChild(frag, textNode);
    }
}

// ============================================================================
// Pack-section reveal fade (chatlist pattern)
// ============================================================================
//
// `content-visibility: auto` makes the browser skip off-screen pack
// sections entirely. When they scroll back in, we play a 180ms opacity
// fade so the reveal feels intentional instead of popping.
//
// Critical: the fade ONLY plays on the off→on transition. Initial render
// (the section was just inserted, never went off-screen) and re-render
// (pack subscription changes) must NOT animate — the chatlist solves
// this with a `data-cv-was-off` flag set the first time the section is
// observed off-screen; without it, animations would fire every time the
// pack list updates and every time the picker opens.
//
// Two paths: `contentvisibilityautostatechange` (Chromium) is the
// preferred trigger; IntersectionObserver is the WebKit fallback.

let _emojiPackRevealAttached = false;
let _emojiPackRevealObserver = null;
let _emojiPackCvEventSeen = false;

function _emojiPackReveal_trigger(el) {
    // remove → reflow → add restarts the CSS animation on the same element.
    el.classList.remove('cv-revealed');
    void el.offsetWidth;
    el.classList.add('cv-revealed');
}

function _emojiPackReveal_observeAll(main) {
    if (!_emojiPackRevealObserver) return;
    main.querySelectorAll('.emoji-pack-section').forEach(el => {
        _emojiPackRevealObserver.observe(el);
    });
}

function _attachEmojiPackReveal() {
    if (_emojiPackRevealAttached) return;
    const main = _pickerEls.main;
    if (!main) return;
    _emojiPackRevealAttached = true;

    main.addEventListener('contentvisibilityautostatechange', (e) => {
        _emojiPackCvEventSeen = true;
        const target = e.target;
        if (!(target instanceof HTMLElement)) return;
        if (!target.classList.contains('emoji-pack-section')) return;
        if (e.skipped) {
            target.dataset.cvWasOff = '1';
            target.classList.remove('cv-revealed');
        } else if (target.dataset.cvWasOff === '1') {
            _emojiPackReveal_trigger(target);
        }
        // Else: initial render after insertion → no animation.
    });

    _emojiPackRevealObserver = new IntersectionObserver((entries) => {
        if (_emojiPackCvEventSeen) return; // Chromium path is driving things.
        for (const entry of entries) {
            const el = entry.target;
            if (!(el instanceof HTMLElement)) continue;
            if (!el.classList.contains('emoji-pack-section')) continue;
            if (entry.isIntersecting) {
                if (el.dataset.cvWasOff === '1') {
                    _emojiPackReveal_trigger(el);
                }
            } else {
                el.dataset.cvWasOff = '1';
            }
        }
    }, { root: main, threshold: 0 });

    _emojiPackReveal_observeAll(main);

    // Pack sections come and go on subscribe/unsubscribe — re-observe new ones.
    const mo = new MutationObserver((mutations) => {
        for (const m of mutations) {
            for (const node of m.addedNodes) {
                if (node instanceof HTMLElement
                    && node.classList.contains('emoji-pack-section')
                    && _emojiPackRevealObserver) {
                    _emojiPackRevealObserver.observe(node);
                }
            }
        }
    });
    mo.observe(main, { childList: true, subtree: true });
}

function _handlePackEmojiSelect(pack, emoji, keepOpen = false) {
    if (!emoji) return;
    // Insert the disambiguated code (`love~2`) so duplicate-name emojis resolve
    // to the exact image the user clicked.
    const code = emoji.dispCode || emoji.shortcode;
    if (strCurrentReactionReference) {
        const literal = `:${code}:`;
        if (!_userAlreadyReacted(literal)) {
            _sendCustomEmojiReaction(code, emoji.url);
        }
        // Shift keeps the panel open for rapid multi-react (mirrors compose
        // multi-insert), until the message hits its reaction display ceiling.
        if (!keepOpen || _reactionRowAtCapacity(strCurrentReactionReference)) VectorSvelte.setPickerVisible(false);
        return;
    }
    insertAtCursor(`:${code}:`, true);
    // Shift-click keeps the panel open for rapid multi-insert (Discord-style).
    if (!keepOpen) VectorSvelte.setPickerVisible(false);
    if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
        if (!_emojiPanelTarget) domChatMessageInput.focus();
    }
}

/**
 * Returns true when the current user has already reacted to the message
 * referenced by `strCurrentReactionReference` with `emoji`. Used to
 * suppress duplicate reactions — Nostr lets you publish more than one,
 * but we surface each emoji once per author, so the second send would
 * silently no-op anyway. Better to block it up front and close the picker.
 */
function _userAlreadyReacted(emoji) {
    if (!strCurrentReactionReference || !emoji) return false;
    for (const cChat of arrChats) {
        const cMsg = cChat.messages.find(m => m.id === strCurrentReactionReference);
        if (!cMsg || !cMsg.reactions) continue;
        return cMsg.reactions.some(r => r.emoji === emoji && r.author_id === strPubkey);
    }
    return false;
}

/** Send a NIP-30 custom-emoji reaction. Reuses the existing
 *  `react_to_message` invoke with the `emoji_url` parameter so the
 *  backend attaches an `["emoji", code, url]` tag — any spec-aware
 *  client renders the image instead of the literal `:shortcode:`. */
function _sendCustomEmojiReaction(shortcode, url) {
    if (!shortcode || !url || !strCurrentReactionReference) return;
    for (const cChat of arrChats) {
        const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
        if (!cMsg) continue;
        const strReceiverPubkey = cChat.id;

        // Provisional first: the chip shows the image right away.
        const retract = dmsgReactOptimistic(cMsg.id, `:${shortcode}:`, url);
        reactToMessageRouted(strCurrentReactionReference, strReceiverPubkey, `:${shortcode}:`, url)
            .catch(() => { if (retract) retract(); });
    }
}

// Header-chrome height (padding + logo/title row) of a pack section, above its
// canvas grid. Seeded from CSS (4px×2 padding + 18px logo) and recalibrated
// from a real rendered header each render so the intrinsic-size estimates below
// are pixel-accurate. Constant across packs — the header is structurally
// identical regardless of emoji count.
let _packSectionChromePx = 26;

// True rendered height of a pack section: header chrome + a 6-column canvas
// grid (PACK_CANVAS_CELL_PX per row). Excludes emoji already known-unavailable
// so the row count matches what the grid actually draws.
function _packSectionHeightPx(pack) {
    const usable = Array.isArray(pack.emojis)
        ? pack.emojis.filter(e => !_emojiFailReason.has(e.url)).length
        : 0;
    const rows = Math.max(1, Math.ceil(usable / 6));
    return _packSectionChromePx + PACK_CANVAS_CELL_PX * rows;
}

/** The sections are an island over the packs order; this arms what is on screen. */
function renderEmojiPackSections() {
    _ensureEmojiPickerIslands();
    VectorSvelte.flushSync();
    _afterPackSectionsRender();
}

/**
 * One canvas grid per section: a direct child of the section (the grid sizes itself from
 * its parent and the section hosts the cell tooltip). Returns the teardown.
 */
function _mountPackCanvasGrid(section, pack) {
    const grid = new PackCanvasGrid(pack);
    _packCanvasGrids.set(pack.id, grid);
    section.appendChild(grid.canvas);
    if (_pickerEls.main) grid.attachVisibilityObserver(_pickerEls.main);
    return () => {
        grid.destroy();
        if (_packCanvasGrids.get(pack.id) === grid) _packCanvasGrids.delete(pack.id);
    };
}

function _afterPackSectionsRender() {
    const main = _pickerEls.main;
    if (!main) return;
    // Arm the on-screen packs deterministically rather than waiting on the IO's first
    // callback (unreliable when this runs mid-open-transition).
    _rearmVisiblePackCanvases();
    // Calibrate the header-chrome constant from a real rendered header; the sections'
    // intrinsic sizes re-derive from it, keeping jump-scroll pixel-accurate across themes.
    requestAnimationFrame(() => {
        const header = main.querySelector('.emoji-pack-section .emoji-section-header');
        const measured = header ? header.offsetHeight : 0;
        if (measured <= 0 || measured === _packSectionChromePx) return;
        _packSectionChromePx = measured;
        VectorSvelte.bumpPickerChrome();
    });
}

/**
 * Discord-style tooltip for a stock emoji: the canonical `:shortcode:`,
 * never the keyword soup in `name` (that field exists for search only).
 */
function stockEmojiTitle(e) {
    return e.shortcode ? `:${e.shortcode}:` : (e.display || e.name || '');
}

/** Recents re-derive from usage; the full grid rendered once at mount. */
function renderEmojiPanel() {
    _ensureEmojiPickerIslands();
    VectorSvelte.bumpPickerRecents();
}

// The panel's query clears; the sections come back.
function resetEmojiPicker() {
    emojiSearch.value = '';
    VectorSvelte.setPickerQuery('');
}

// Emoji search: the results grid derives from the query and the sections step aside.
// GIF search is debounced against the API.
function _onSearchInput(e) {
    if (pickerMode === PICKER_MODE_GIF) {
        clearTimeout(gifSearchTimeout);
        gifSearchTimeout = setTimeout(() => {
            searchGifs(e.target.value);
        }, 300);
        return;
    }
    const search = e.target.value.toLowerCase();
    if (search) VectorSvelte.setPickerQuery(search);
    else resetEmojiPicker();
}

// Scroll-spy for the rail: the highlight tracked clicks only, so scrolling the
// emoji panel left it pointing at whatever was last tapped.
//
// Suppressed briefly after a tab click — that scroll is `behavior: 'smooth'`, and
// following it would strobe the highlight through every section it passes on the
// way to the one the user actually picked.
let _emojiSpyMuteUntil = 0;
let _emojiSpyFrame = 0;

// Rail follow: ease toward a target scrollTop instead of handing each change to
// `scrollIntoView({ behavior: 'smooth' })`. That restarts a fixed-duration
// animation per section crossed, so a continuous scroll came out as a series of
// discrete lurches. Lerping toward a target that can move mid-flight reads as one
// fluid motion however fast the panel is scrolled.
const _RAIL_EDGE_PAD_PX = 14;
const _RAIL_LERP_PER_FRAME = 0.16;
let _railTarget = null;
let _railRaf = 0;
let _railLastTs = 0;

function _railStop() {
    if (_railRaf) cancelAnimationFrame(_railRaf);
    _railRaf = 0;
    _railTarget = null;
}

function _railStep(ts) {
    const rail = _pickerEls.sidebar;
    if (!rail || _railTarget === null) { _railStop(); return; }
    // Frame-rate independent: the same glide on a 120Hz panel as on 60Hz.
    const dt = _railLastTs ? Math.min(ts - _railLastTs, 64) : 16.67;
    _railLastTs = ts;
    const k = 1 - Math.pow(1 - _RAIL_LERP_PER_FRAME, dt / 16.67);

    const delta = _railTarget - rail.scrollTop;
    if (Math.abs(delta) < 0.5) {
        rail.scrollTop = _railTarget;
        _railStop();
        return;
    }
    rail.scrollTop += delta * k;
    _railRaf = requestAnimationFrame(_railStep);
}

// Minimal move that brings `tab` fully into the rail with a margin — the old
// `block: 'nearest'` + `scroll-padding` behaviour, computed here now that we own
// the animation, so the inset lives in one place instead of two.
function _railFollow(tab) {
    const rail = _pickerEls.sidebar;
    if (!rail) return;
    const max = rail.scrollHeight - rail.clientHeight;
    if (max <= 0) return;
    const top = tab.offsetTop - _RAIL_EDGE_PAD_PX;
    const bottom = tab.offsetTop + tab.offsetHeight + _RAIL_EDGE_PAD_PX;
    const from = _railTarget ?? rail.scrollTop;

    let target = from;
    if (top < from) target = top;
    else if (bottom > from + rail.clientHeight) target = bottom - rail.clientHeight;
    target = Math.max(0, Math.min(target, max));
    if (Math.abs(target - rail.scrollTop) < 0.5) return;

    _railTarget = target;
    if (!_railRaf) {
        _railLastTs = 0;
        _railRaf = requestAnimationFrame(_railStep);
    }
}

function _tabForSection(section) {
    if (section.classList.contains('emoji-pack-section')) {
        const id = section.dataset.packId;
        return id
            ? _pickerEls.sidebar.querySelector(`.emoji-pack-tab[data-pack-id="${CSS.escape(id)}"]`)
            : null;
    }
    // Stock sections are `#emoji-<category>` against `[data-category]` tabs.
    const category = section.id?.startsWith('emoji-') ? section.id.slice(6) : '';
    return category
        ? _pickerEls.sidebar.querySelector(`.emoji-category-btn[data-category="${CSS.escape(category)}"]`)
        : null;
}

function _syncActiveSectionTab() {
    if (Date.now() < _emojiSpyMuteUntil) return;
    const main = _pickerEls.main;
    // Creator mode hides every section, so there's nothing to track.
    if (!main || _pc.open) return;
    const sections = [...main.querySelectorAll('.emoji-section')]
        .filter(s => !s.hidden && s.offsetParent !== null);
    if (!sections.length) return;

    // Detector line at the panel's CENTRE, not its top edge. Against the top, a
    // one-pixel sliver of the outgoing section still counted as current while the
    // next one filled the whole view. Sections stack contiguously, so the last one
    // whose top is at or above the midpoint is the one occupying the centre — and
    // it degrades correctly at the ends, where no section spans the midpoint.
    const rect = main.getBoundingClientRect();
    const mid = rect.top + rect.height / 2;
    let current = sections[0];
    for (const s of sections) {
        if (s.getBoundingClientRect().top <= mid) current = s;
        else break;
    }

    const tab = _tabForSection(current);
    if (!tab) return;
    const key = tab.dataset.category || tab.dataset.packId;
    if (!key || VectorSvelte.pickerState().active === key) return;
    VectorSvelte.setPickerActive(key);
    // The rail scrolls too, so a pack scrolled past off-rail would highlight
    // invisibly. Only moves when the tab isn't comfortably in view.
    _railFollow(tab);
}

function _onMainScroll() {
    if (_emojiSpyFrame) return;
    _emojiSpyFrame = requestAnimationFrame(() => {
        _emojiSpyFrame = 0;
        _syncActiveSectionTab();
    });
}

// One delegated handler for the stock tabs and the pack tabs alike.
async function _onRailClick(e) {
    const btn = e.target.closest('.emoji-category-btn');
    if (!btn) return;
    e.stopPropagation();

    // A just-completed drag-reorder fires a trailing click — swallow it so the
    // dragged tab doesn't also jump-scroll to its section.
    if (btn.dataset.suppressClick === '1') {
        delete btn.dataset.suppressClick;
        return;
    }

    // "+" creator tab — enter creator mode (handled in its own listener too,
    // but stopPropagation here keeps the active-tab toggle from cycling).
    if (btn.classList.contains('emoji-pack-tab-create')) return;

    // Switching out of creator view: auto-save first. A failed save keeps the
    // creator open (emojis preserved), so don't switch tabs out from under it.
    if (_pc.open && !(await closeEmojiPackCreator())) return;

    VectorSvelte.setPickerActive(btn.dataset.category || btn.dataset.packId);

    let section = null;
    if (btn.dataset.category) {
        section = _pickerEls.main.querySelector(`#emoji-${btn.dataset.category}`);
    } else if (btn.dataset.packId) {
        section = _pickerEls.main.querySelector(
            `.emoji-pack-section[data-pack-id="${CSS.escape(btn.dataset.packId)}"]`,
        );
    }
    // Pixel-accurate contain-intrinsic-size on every pack section (see
    // renderEmojiPackSections) means nothing resizes under the scroll, so a
    // single smooth scroll lands on target — no post-jump correction needed.
    if (section) {
        // Hold the spy off while the smooth scroll travels, so the highlight
        // stays on the tab that was clicked instead of chasing the animation.
        _emojiSpyMuteUntil = Date.now() + 700;
        section.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }
}

/**
 * Insert text at the cursor position in the chat input
 * If no selection, inserts at cursor. If text is selected, replaces it.
 * @param {string} text - The text to insert
 * @param {boolean} autoSpace - If true, adds spaces around inserted text when adjacent to non-whitespace
 */
function insertAtCursor(text, autoSpace = false) {
    // Dialog-owned target (Status dialog) intercepts the insert wholesale.
    if (_emojiPanelTarget) {
        _emojiPanelTarget.insert(text);
        return;
    }
    const input = domChatMessageInput;
    const start = input.selectionStart;
    const end = input.selectionEnd;
    const value = input.value;

    // Auto-space: add spaces if inserting next to non-whitespace characters
    let prefix = '';
    let suffix = '';
    if (autoSpace) {
        const charBefore = start > 0 ? value[start - 1] : '';
        const charAfter = end < value.length ? value[end] : '';
        if (charBefore && !/\s/.test(charBefore)) prefix = ' ';
        if (charAfter && !/\s/.test(charAfter)) suffix = ' ';
    }

    const before = value.substring(0, start);
    const after = value.substring(end);
    const insertText = prefix + text + suffix;
    input.value = before + insertText + after;
    // Move cursor to end of inserted text
    const newPos = start + insertText.length;
    input.setSelectionRange(newPos, newPos);
    // Trigger input event to update send/mic button state
    input.dispatchEvent(new Event('input', { bubbles: true }));
}

// Pack-emoji selection — Phase 1 inserts the `:shortcode:` literal so
// recipients with the same pack subscribed still see the right emoji
// (their renderer resolves the shortcode against their own pack list).
// Phase 2 will add the NIP-30 `emoji` tag to the outbound rumor so
// rendering doesn't depend on the recipient already having the pack.
function _onPackEmojiClick(e) {
    const span = e.target.closest('.emoji-pack-emoji');
    if (!span) return;
    e.stopPropagation();
    const shortcode = span.dataset.packShortcode;
    if (!shortcode) return;

    if (strCurrentReactionReference) {
        const url = span.dataset.packUrl;
        const literal = `:${shortcode}:`;
        if (!_userAlreadyReacted(literal)) {
            _sendCustomEmojiReaction(shortcode, url);
        }
        // Shift keeps the panel open for rapid multi-react (mirrors compose
        // multi-insert), until the message hits its reaction display ceiling.
        if (!e.shiftKey || _reactionRowAtCapacity(strCurrentReactionReference)) VectorSvelte.setPickerVisible(false);
        return;
    }
    insertAtCursor(`:${shortcode}:`, true);
    // Shift-click keeps the panel open for rapid multi-insert (Discord-style).
    if (!e.shiftKey) VectorSvelte.setPickerVisible(false);
    if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
        if (!_emojiPanelTarget) domChatMessageInput.focus();
    }
}

// Emoji selection handler
function _onEmojiSpanClick(e) {
    if (e.target.tagName === 'SPAN' && e.target.parentElement.classList.contains('emoji-grid')) {
        const char = e.target.dataset.emoji;
        const cEmoji = arrEmojis.find(e => e.emoji === char);

        if (cEmoji) {

            // Handle the emoji selection
            if (strCurrentReactionReference) {
                if (_userAlreadyReacted(cEmoji.emoji)) {
                    // Already reacted with this one — just dismiss the picker.
                } else {
                    // Reaction handling
                    for (const cChat of arrChats) {
                        const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
                        if (!cMsg) continue;

                        const strReceiverPubkey = cChat.id;
                        const retract = dmsgReactOptimistic(cMsg.id, cEmoji.emoji, null);
                        reactToMessageRouted(strCurrentReactionReference, strReceiverPubkey, cEmoji.emoji)
                            .catch(() => { if (retract) retract(); });
                    }
                }
            } else {
                // Add to message input at cursor position (with auto-spacing)
                insertAtCursor(cEmoji.emoji, true);
            }

            // Shift keeps the panel open for rapid multi-insert AND multi-react
            // (Discord-style); release shift, Escape, or click away to dismiss.
            // A reaction that fills the display ceiling also closes.
            if (!e.shiftKey || (strCurrentReactionReference && _reactionRowAtCapacity(strCurrentReactionReference))) {
                VectorSvelte.setPickerVisible(false);
            }
            // Focus chat input (desktop only - mobile keyboards are disruptive)
            if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
                if (!_emojiPanelTarget) domChatMessageInput.focus();
            }
        }
    }
}

// When hitting Enter on the emoji search - choose the first emoji/GIF
async function _onSearchKeydown(e) {
    if ((e.code === 'Enter' || e.code === 'NumpadEnter')) {
        e.preventDefault();

        // Handle GIF mode - select first GIF
        if (pickerMode === PICKER_MODE_GIF) {
            const firstGif = _pickerEls.gif.querySelector('.gif-item');
            if (firstGif && firstGif.dataset.gifId) {
                selectGif(firstGif.dataset.gifId);
            }
            return;
        }

        // Find the first emoji in search results or recent emojis
        let emojiElement;
        if (emojiSearch.value) {
            emojiElement = _pickerEls.results.querySelector('span:first-child');
        } else {
            emojiElement = _pickerEls.recents.querySelector('span:first-child');
        }

        if (!emojiElement) return;

        // Custom (pack) emoji at the first position — mirror the pack-click path
        // (insert the :shortcode: literal, or send a custom reaction). Stock lookup
        // below keys on the emoji char, which a custom cell doesn't carry, so handle
        // it here first.
        const packShortcode = emojiElement.dataset.packShortcode;
        if (packShortcode) {
            if (strCurrentReactionReference && _userAlreadyReacted(`:${packShortcode}:`)) {
                // already reacted — just dismiss below
            } else if (strCurrentReactionReference) {
                _sendCustomEmojiReaction(packShortcode, emojiElement.dataset.packUrl);
            } else {
                insertAtCursor(`:${packShortcode}:`, true);
            }
            emojiSearch.value = '';
            VectorSvelte.setPickerVisible(false);
            strCurrentReactionReference = '';
            VectorSvelte.setEmojiIcon('smile');
            if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
                if (!_emojiPanelTarget) domChatMessageInput.focus();
            }
            return;
        }

        // Register the selection in the emoji-dex
        const cEmoji = arrEmojis.find(a => a.emoji === emojiElement.dataset.emoji);
        if (!cEmoji) return;


        // If this is a Reaction - use the original reaction handling
        if (strCurrentReactionReference && _userAlreadyReacted(cEmoji.emoji)) {
            // Already reacted with this emoji — silently dismiss below.
        } else if (strCurrentReactionReference) {
            // Grab the referred message to find it's chat pubkey
            for (const cChat of arrChats) {
                const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
                if (!cMsg) continue;

                // Found the message!
                const strReceiverPubkey = cChat.id;

                // Provisional first: instant feedback, no wait for the network echo.
                const retract = dmsgReactOptimistic(cMsg.id, cEmoji.emoji, null);
                // Send the Reaction to the network (protocol-agnostic)
                reactToMessageRouted(strCurrentReactionReference, strReceiverPubkey, cEmoji.emoji)
                    .catch(() => { if (retract) retract(); });
            }
        } else {
            // Add to message input at cursor position (with auto-spacing)
            insertAtCursor(cEmoji.emoji, true);
        }

        // Reset the UI state - use class instead of inline style
        emojiSearch.value = '';
        VectorSvelte.setPickerVisible(false);
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        VectorSvelte.setEmojiIcon('smile');

        // Bring the focus back to the chat (desktop only - mobile keyboards are disruptive)
        if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
            if (!_emojiPanelTarget) domChatMessageInput.focus();
        }
    } else if (e.code === 'Escape') {
        // Close the Mini App launch dialog if open
        if (VectorSvelte.launchDialog.state().active) {
            closeMiniAppLaunchDialog();
            return;
        }

        // Close the emoji dialog - use class instead of inline style
        emojiSearch.value = '';
        VectorSvelte.setPickerVisible(false);
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        VectorSvelte.setEmojiIcon('smile');

        // Close the attachment panel if open
        if (VectorSvelte.attachmentVisible()) {
            closeAttachmentPanel();
        }

        // Bring the focus back to the chat (desktop only - mobile keyboards are disruptive)
        if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
            if (!_emojiPanelTarget) domChatMessageInput.focus();
        }
    }
}

// Emoji selection
function _onEmojiImgClick(e) {
    if (e.target.tagName === 'IMG') {
        // Register the click in the emoji-dex
        const cEmoji = arrEmojis.find(a => a.emoji === e.target.alt);
        if (!cEmoji) return; // not a stock emoji IMG (pack image, logo, etc.)

        // If this is a Reaction - let's send it! (Skip if the user has
        // already reacted with this emoji; one reaction per emoji per author.)
        if (strCurrentReactionReference && !_userAlreadyReacted(cEmoji.emoji)) {
            // Grab the referred message to find it's chat pubkey
            for (const cChat of arrChats) {
                const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
                if (!cMsg) continue;

                // Found the message!
                const strReceiverPubkey = cChat.id;

                // Provisional first: instant feedback, no wait for the network echo.
                const retract = dmsgReactOptimistic(cMsg.id, cEmoji.emoji, null);
                // Send the Reaction to the network (protocol-agnostic)
                reactToMessageRouted(strCurrentReactionReference, strReceiverPubkey, cEmoji.emoji)
                    .catch(() => { if (retract) retract(); });
            }
        }
    }
}
