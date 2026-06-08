/**
 * Emoji + GIF picker — the floating panel that appears above the chat input.
 *
 * Two modes share one container: emoji (default) and GIF (Gifverse API). The
 * mode toggle, search box, and dispatch logic all live here. Emoji data and
 * persistence (recent / favorites / shortcodes) live in emoji.js.
 *
 * The panel is also used as a reaction picker — when a `.dmsg-react-trigger`
 * element is the click source (set by the floating message toolbar), the next
 * emoji selection is sent as a reaction to the message instead of being
 * inserted into the chat input. `strCurrentReactionReference` tracks the
 * target message id during that mode.
 *
 * Cross-file dependencies (resolved at call time via classic-script scope):
 *   - emoji.js   — arrEmojis, arrFavoriteEmojis, searchEmojis, getMostUsedEmojis,
 *                  addToRecentEmojis, toggleFavoriteEmoji
 *   - twemoji    — twemojify
 *   - main.js    — domChatMessageInput, domChatMessageInputEmoji,
 *                  domChatMessageInputSend, domChatMessageInputFile,
 *                  domAttachmentPanel, domMiniAppLaunchOverlay,
 *                  closeAttachmentPanel, platformFeatures, arrChats
 *   - miniapps-panel.js — closeMiniAppLaunchDialog
 *   - message-row.js    — _dmsgInjectReaction
 */

const picker = document.querySelector('.emoji-picker');

// Dismiss the shared emoji tooltip whenever the picker leaves the
// `visible` state — handles keyboard-driven closes (Enter / Esc),
// where the mouse never moves so the usual mouseleave path doesn't
// fire. One observer covers every close site without sprinkling
// `hideEmojiTooltip()` calls throughout. The same observer drives the
// Android back-stack push/pop so the panel can be dismissed with the
// hardware back button from any of its many open sites.
if (picker) {
    new MutationObserver(() => {
        if (picker.classList.contains('visible')) {
            pushBack('emoji-picker', () => {
                picker.classList.remove('visible');
                picker.style.bottom = '';
                if (typeof domChatMessageInputEmoji !== 'undefined' && domChatMessageInputEmoji) {
                    domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;
                }
            });
        } else {
            hideEmojiTooltip();
            popBack('emoji-picker');
        }
    }).observe(picker, { attributes: true, attributeFilter: ['class'] });
}

/** @type {HTMLInputElement} */
const emojiSearch = document.getElementById('emoji-search-input');
/**
 * The current reaction reference - i.e: a message being reacted to.
 *
 * When empty, emojis are simply injected to the current chat input.
 */
let strCurrentReactionReference = "";

/**
 * Opens the Emoji Input Panel
 *
 * The panel always appears in a fixed position at the bottom, regardless of whether
 * it's opened from the message input or a reaction button.
 * @param {MouseEvent?} e - An associated click event
 */
function openEmojiPanel(e) {
    const isDefaultPanel = e.target === domChatMessageInputEmoji || domChatMessageInputEmoji.contains(e.target);

    // Don't close if clicking inside the picker itself
    if (picker.contains(e.target)) return;

    // Open or Close the panel depending on it's state
    // `dmsg-react-trigger` is the synthetic class added by the floating
    // toolbar's _dmsgOpenReactionPicker — see message-toolbar.js. The class
    // exists solely as a routing token between the toolbar and this handler.
    const strReaction = e.target.classList.contains('dmsg-react-trigger') ? e.target.parentElement.parentElement.id : '';
    const fClickedInputOrReaction = isDefaultPanel || strReaction;
    if (fClickedInputOrReaction && !picker.classList.contains('visible')) {
        // Close attachment panel if open
        if (domAttachmentPanel.classList.contains('visible')) {
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
        const chatBox = document.getElementById('chat-box');
        const bottomPx = chatBox ? (chatBox.getBoundingClientRect().height + 10) + 'px' : '';

        picker.classList.add('visible');
        picker.classList.add('emoji-picker-message-type');
        if (bottomPx) picker.style.bottom = bottomPx;
        picker.style.top = '';
        picker.style.left = '';
        picker.style.right = '';

        // Swap the emoji button to a wink while open (message input only).
        if (isDefaultPanel) {
            domChatMessageInputEmoji.innerHTML = `<span class="icon icon-wink-face"></span>`;
        }
        strCurrentReactionReference = strReaction || '';

        // --- Deferred: the expensive content build (twemoji recents/favorites,
        // the ~1.8k-span All grid, pack sidebar/sections + canvas loop) runs
        // AFTER the panel's first visible paint. Double rAF: the outer fires in
        // the same frame the .visible style lands (transition starts), the inner
        // fires the frame after (content fills in, ~16ms into the 300ms slide). ---
        requestAnimationFrame(() => requestAnimationFrame(() => {
            // Bail if the panel was closed again before this fired.
            if (!picker.classList.contains('visible')) return;

            resetEmojiPicker();
            renderEmojiPanel();
            initCollapsibleSections();

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
    } else {
        // Hide and reset the UI - use class instead of inline style
        emojiSearch.value = '';
        // Auto-save any in-progress pack edit before the panel disappears.
        if (_pc.open) closeEmojiPackCreator();
        picker.classList.remove('visible');
        picker.style.bottom = ''; // Reset to CSS default
        strCurrentReactionReference = '';
        // Drop the canvas rAF so we don't tick under opacity:0.
        _stopPackCanvasLoop();

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;
    }
}

let emojiLazyLoadObserver = null;
const EMOJI_CHUNK_SIZE = 36; // 6 columns x 6 rows

/**
 * Subscribed + owned NIP-30 emoji packs hydrated from vector-core, plus the
 * active theme's pinned pack at index 0 (see `_composeAndRenderPacks`).
 * Populated by `loadEmojiPacks()` on first picker open; subsequent
 * opens reuse the cached array while a background refresh updates it.
 * `id` is the canonical naddr (no relay hints); there is NO `addr` field.
 * @type {Array<{id:string,title:string,image_url:string,description:string,emojis:Array<{shortcode:string,url:string}>,is_own:boolean,is_theme?:boolean,updated_at:number}>}
 */
let arrEmojiPacks = [];
let emojiPacksLoaded = false;
let _lastPacksSignature = '';
let _lastRefreshAt = 0;
const PACK_REFRESH_TTL_MS = 60_000;

// ----- Theme emoji packs ------------------------------------------------------
// Hardcoded per-theme "pinned" packs. The active theme's pack (if any) renders
// FIRST in the picker, ahead of the user's own/subscribed packs. It's injected
// at render time only — never written to the user's kind-10030 subscription
// list, and it doesn't occupy an equip slot. Add naddrs here as theme packs
// ship; themes absent from this map simply have no pinned pack.
const THEME_EMOJI_PACKS = {
    vector: 'naddr1qqxx7mt2f945yvj0fdg8x3czyzu0jtnpuuw5tp4wdlnfwrmnm58ahzgpp9vfut6p00h5gkam4ykg6qcyqqq82nsstfm4j',
};
let _userEmojiPacks = [];          // backend (own + subscribed) packs, pre-theme-merge
const _themePackCache = {};        // naddr -> pack | null (fetched; null = none/failed)
const _themePackFetching = {};     // naddr -> in-flight Promise (dedupe concurrent fetches)

function _currentThemeName() {
    // applyTheme() sets exactly one `<name>-theme` class on <body>.
    const cls = Array.from(document.body.classList).find(c => c.endsWith('-theme'));
    return cls ? cls.slice(0, -'-theme'.length) : 'vector';
}

// Count packs that occupy a user equip slot (theme packs are free/pinned).
function _userPackCount() {
    return Array.isArray(arrEmojiPacks) ? arrEmojiPacks.filter(p => !p.is_theme).length : 0;
}

function _cachedThemePack() {
    const naddr = THEME_EMOJI_PACKS[_currentThemeName()];
    if (!naddr) return null;
    return _themePackCache[naddr] || null;
}

// Register the active theme pack's emoji with the send resolver so its
// shortcodes get NIP-30 tags even when the pack isn't a real subscription.
// (Subscribed packs already resolve via the DB; the theme pack doesn't, so it
// would otherwise post as literal `:shortcode:`.) Pass the cached theme pack,
// or null to clear. Guarded so we only invoke when the pack actually changes.
let _registeredThemeEmojiId = null;
function _registerThemeEmoji(pack) {
    const id = pack ? pack.id : '';
    if (id === _registeredThemeEmojiId) return;
    _registeredThemeEmojiId = id;
    invoke('set_theme_emoji_pack', { emojis: pack ? pack.emojis : [] }).catch(() => {});
}

async function _fetchThemePack(naddr, force = false) {
    if (!force && _themePackCache[naddr]) return _themePackCache[naddr];   // cached success
    if (_themePackFetching[naddr]) return _themePackFetching[naddr];
    const p = (async () => {
        try {
            // Cache-first: returns the persisted copy instantly across sessions
            // (and refreshes in the background), so the pinned theme pack paints
            // without a per-session relay round-trip.
            const pack = await invoke('get_theme_emoji_pack', { naddr });
            if (pack && Array.isArray(pack.emojis) && pack.emojis.length) {
                if (pack.emojis.length > MAX_DISPLAY_EMOJIS_PER_PACK) {
                    pack.emojis = pack.emojis.slice(0, MAX_DISPLAY_EMOJIS_PER_PACK);
                }
                pack.is_theme = true;
                _themePackCache[naddr] = pack;   // only successes are cached
                return pack;
            }
            return null;
        } catch (e) {
            // Don't cache the failure — a transient miss (e.g. relays not
            // connected on first open) should retry on the next open.
            console.warn('[theme-pack] fetch failed:', e);
            return null;
        } finally {
            delete _themePackFetching[naddr];
        }
    })();
    _themePackFetching[naddr] = p;
    return p;
}

// Merge the active theme's pinned pack (pinned first, de-duped against the
// user's list) and repaint. Idempotent via the packs signature.
function _composeAndRenderPacks() {
    const themeNaddr = THEME_EMOJI_PACKS[_currentThemeName()];
    let combined;
    if (!themeNaddr) {
        combined = _userEmojiPacks.slice();
    } else {
        // The theme naddr is canonical (no relay hints), so it equals the
        // backend's `pack.id`. If the user is already subscribed to that pack we
        // can pin THEIR copy immediately — no fetch needed, so the picker opens
        // already-pinned instead of reordering after the network lands. The
        // pinned copy keeps Remove + counts as a real subscription; we never add
        // a separate theme entry, so it can't double up.
        const subIdx = _userEmojiPacks.findIndex(p => p.id === themeNaddr);
        if (subIdx !== -1) {
            const rest = _userEmojiPacks.filter((_, i) => i !== subIdx);
            combined = [_userEmojiPacks[subIdx], ...rest];
        } else {
            // Not subscribed → pin the fetched theme pack once we have it (no
            // Remove, doesn't use a slot). Until the fetch lands, just the user
            // packs render and the pack appears at the top when ready.
            const themePack = _cachedThemePack();
            combined = themePack ? [themePack, ..._userEmojiPacks] : _userEmojiPacks.slice();
        }
    }
    // Keep the send resolver in sync with the (non-subscribed) theme pack so
    // its emoji post as real custom emoji, not plaintext. Cheap + guarded.
    _registerThemeEmoji(_cachedThemePack());
    const sig = _packsSignature(combined);
    if (sig === _lastPacksSignature && emojiPacksLoaded) return;
    _lastPacksSignature = sig;
    arrEmojiPacks = combined;
    emojiPacksLoaded = true;
    const activeAddr = document.querySelector('.emoji-pack-tab.active')?.dataset.packId;
    renderEmojiPackSidebar();
    renderEmojiPackSections();
    if (activeAddr) {
        const tab = document.querySelector(`.emoji-pack-tab[data-pack-id="${CSS.escape(activeAddr)}"]`);
        if (tab) tab.classList.add('active');
    }
    _refreshPackPreviewButtons();
}

// Ensure the active theme's pack is fetched, then recompose so it appears.
async function _ensureThemePack(refresh = false) {
    const naddr = THEME_EMOJI_PACKS[_currentThemeName()];
    if (!naddr) return;
    // Subscribed → the user's own copy is pinned and `refresh_emoji_packs`
    // already pulls its edits/deletions; nothing to fetch here.
    if (_userEmojiPacks.some(p => p.id === naddr)) return;
    // Cold path: fetch only if we don't have it. Refresh path: re-fetch to pick
    // up pack edits / removal (debounced by loadEmojiPacks' PACK_REFRESH_TTL_MS).
    if (!refresh && _themePackCache[naddr]) return;
    const fresh = await _fetchThemePack(naddr, refresh);
    if (refresh && !fresh && _themePackCache[naddr]) {
        // A refresh came back empty for a pack we had — edited-to-empty or
        // removed; drop the cached copy so it stops showing. (Self-healing: a
        // later cold/refresh fetch re-adds it if it was only a transient miss.)
        delete _themePackCache[naddr];
    }
    _composeAndRenderPacks();
}

// Called when the user switches theme (see settings.js setTheme).
function refreshEmojiPacksForTheme() {
    if (!emojiPacksLoaded) return;   // picker not opened yet — next open handles it
    _composeAndRenderPacks();        // swap to the new theme's cached pack (or none)
    _ensureThemePack();              // fetch the new theme's pack if not cached yet
}

/** Max packs a user can have equipped at once. Mirrors vector-core's
 *  `MAX_EQUIPPED_PACKS`. Frontend pre-gates the create + subscribe
 *  buttons so the backend never sees a request it's just going to reject.
 *  Raised by the Vector badge (see `applyBadgeLimits`); these are `let` so
 *  the badge can lift them at runtime. Pure in-app gates — never used to
 *  slice the loaded/displayed pack list. */
let MAX_EQUIPPED_PACKS = 3;
/** Display-side per-pack emoji cap. Mirrors vector-core's
 *  `MAX_EMOJIS_PER_PACK` (which only constrains own packs). Shared packs
 *  with more emojis are truncated to the first N at load so picker,
 *  search index, and recent-used surfaces all see the same set. Old
 *  reactions referencing emojis past the cap still render via the
 *  per-message `emoji` tags — those don't depend on `arrEmojiPacks`.
 *  Raised by the Vector badge (see `applyBadgeLimits`). */
let MAX_DISPLAY_EMOJIS_PER_PACK = 30;

/** Lift (or restore) the emoji-pack limits based on whether we hold the
 *  Vector badge. Called at boot (get_my_badges) and on the `badges_updated`
 *  event. PC_MAX_EMOJIS is declared later in the file but hoisted, so it's
 *  safe to assign here. These are in-app gates + the per-pack display cap
 *  only — the pack-count load/render path is never gated on them. */
function applyBadgeLimits(hasVectorBadge) {
    const newDisplayCap = hasVectorBadge ? 100 : 30;
    const displayCapChanged = MAX_DISPLAY_EMOJIS_PER_PACK !== newDisplayCap;
    MAX_EQUIPPED_PACKS = hasVectorBadge ? 100 : 3;
    MAX_DISPLAY_EMOJIS_PER_PACK = newDisplayCap;
    PC_MAX_EMOJIS = hasVectorBadge ? 100 : 30;
    // If the display cap changes after packs were already loaded, they were
    // truncated in place to the old cap and the signature cache would block a
    // plain reload. Reset the signature and reload so packs re-truncate to the
    // new cap (the backend still has the full emoji lists locally).
    if (displayCapChanged && emojiPacksLoaded) {
        _lastPacksSignature = '';
        // The theme pack was truncated + cached at the old cap; drop it so it
        // re-fetches at the new cap alongside the user packs.
        for (const k of Object.keys(_themePackCache)) delete _themePackCache[k];
        loadEmojiPacks();
    }
}

// =============================================================================
// Emoji & pack-icon URL caching (NEVER load raw Blossom URLs in the webview).
// =============================================================================
//
// All emoji / pack-icon image bytes go through the Rust process via
// `get_or_cache_image`. This buys two things:
//   1. Strong on-disk cache keyed on URL — load once, reuse forever (until
//      the URL itself changes, i.e. a new Blossom hash).
//   2. Tor protection — the webview never fetches an HTTPS Blossom URL, so
//      a Tor-routed Rust client can't be bypassed by an `<img src="https://…">`.
//
// `_emojiCacheMemo` holds resolved url→localPath strings so repeat renders
// of the same emoji skip the IPC round-trip entirely. `_emojiCacheInflight`
// dedupes concurrent requests for the same URL (e.g. a chat with 50
// `:lol:` shortcodes only fires one IPC).
const _emojiCacheMemo = new Map();    // url → cached local fs path
const _emojiCacheInflight = new Map(); // url → Promise<path|null>

function _isCacheableEmojiUrl(url) {
    return typeof url === 'string' && url.startsWith('https://');
}

/** Returns the memoized local path for `url`, or null if not yet cached
 *  in this session. Synchronous — safe to call from render-fast paths. */
function cachedEmojiPath(url) {
    return _emojiCacheMemo.get(url) || null;
}

/** Async fetch + cache. Returns a `convertFileSrc(...)` URL ready to use
 *  as an `img.src`, or null on failure. `kind` is 'emoji' or
 *  'emoji_pack_icon' (chooses which subdir Rust caches into for stats /
 *  selective clearing — both flow through the same SSRF-guarded download
 *  pipeline). */
async function cacheEmojiSrc(url, kind = 'emoji') {
    if (!_isCacheableEmojiUrl(url)) return null;
    const memo = _emojiCacheMemo.get(url);
    if (memo) return convertFileSrc(memo);
    let inflight = _emojiCacheInflight.get(url);
    if (!inflight) {
        inflight = (async () => {
            try {
                const path = await invoke('get_or_cache_image', { url, imageType: kind });
                if (path) _emojiCacheMemo.set(url, path);
                return path;
            } catch (e) {
                console.warn('[emoji-cache] failed:', url, e);
                return null;
            } finally {
                _emojiCacheInflight.delete(url);
            }
        })();
        _emojiCacheInflight.set(url, inflight);
    }
    const path = await inflight;
    return path ? convertFileSrc(path) : null;
}

// 1x1 transparent GIF. Used as the placeholder src during the loading
// phase so the WebView never paints its broken-image glyph behind the
// shimmer — an <img> with no src renders that glyph on Android WebView.
const TRANSPARENT_PIXEL = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';

/** Hook an `<img>` to cached bytes. Holds a transparent pixel (never a raw
 *  URL, never a broken-image glyph) while the shimmer plays, then swaps to
 *  convertFileSrc(path) when Rust resolves. Memoized hits flip src
 *  synchronously. `onUnavailable(img)` fires when the bytes can't be had
 *  (uncacheable URL or a failed/404 download) so the caller can substitute
 *  a context-appropriate fallback (shortcode text, twemoji glyph, etc.). */
function bindCachedEmojiImg(img, url, kind = 'emoji', onUnavailable = null) {
    const unavailable = () => {
        img.classList.remove('emoji-img-loading');
        if (typeof onUnavailable === 'function') {
            onUnavailable(img);
        } else {
            img.removeAttribute('src');
        }
    };
    if (!_isCacheableEmojiUrl(url)) {
        delete img.dataset.cacheToken;
        unavailable();
        return;
    }
    // Token guard against the re-bind race: if this same <img> gets
    // rebound to a different URL before our async resolve lands, the
    // stale `.then` would overwrite the newer src. Reused elements
    // (e.g. the naming-overlay preview cycling through a batch) are the
    // common offenders.
    img.dataset.cacheToken = url;
    const memo = _emojiCacheMemo.get(url);
    if (memo) {
        img.src = convertFileSrc(memo);
        img.classList.remove('emoji-img-loading');
        return;
    }
    // No bytes yet — transparent placeholder + shimmer until the cache
    // resolves. The class (and placeholder) drop as soon as we have bytes.
    img.src = TRANSPARENT_PIXEL;
    img.classList.add('emoji-img-loading');
    cacheEmojiSrc(url, kind).then(src => {
        if (img.dataset.cacheToken !== url) return; // superseded
        if (!src) {
            unavailable();
            return;
        }
        img.src = src;
        img.classList.remove('emoji-img-loading');
    });
}

function _packsSignature(packs) {
    // The trailing `T` marks a theme-pinned entry vs. the user's own subscribed
    // copy of the same pack (identical id/updated_at/length), so toggling a
    // subscription to the active theme's pack actually repaints instead of being
    // skipped as "unchanged".
    return packs.map(p => `${p.id}@${p.updated_at}#${p.emojis ? p.emojis.length : 0}${p.is_theme ? 'T' : ''}`).join('|');
}

async function loadEmojiPacks({ refresh = false } = {}) {
    if (refresh && Date.now() - _lastRefreshAt < PACK_REFRESH_TTL_MS) {
        // Skip the relay round-trip — local mirror is still fresh.
        // The first open of every panel session still calls the read-only
        // path (refresh:false), so initial render isn't gated.
        return;
    }
    try {
        const packs = await invoke(refresh ? 'refresh_emoji_packs' : 'list_emoji_packs');
        if (refresh) _lastRefreshAt = Date.now();
        const arr = Array.isArray(packs) ? packs : [];
        // Truncate every pack's emoji list to the display cap. Shared
        // packs may exceed the limit (creator's choice), but we surface
        // the first N uniformly across picker, search, and recents.
        for (const p of arr) {
            if (Array.isArray(p.emojis) && p.emojis.length > MAX_DISPLAY_EMOJIS_PER_PACK) {
                p.emojis = p.emojis.slice(0, MAX_DISPLAY_EMOJIS_PER_PACK);
            }
        }
        _userEmojiPacks = arr;
        // Render now (theme pack prepended if already cached); the signature
        // check inside guards against needless repaints. In-chat pack preview
        // cards are swept by the composer too (each Add/Remove button carries
        // its pack id, so the refresh is local + cheap).
        _composeAndRenderPacks();
        // Pinned theme pack: fetch on cold open, re-fetch on the (rate-limited)
        // refresh path so edits/removals reflect. Skipped entirely when the user
        // is subscribed — `refresh_emoji_packs` covers that copy.
        _ensureThemePack(refresh);
    } catch (e) {
        console.warn('[emoji-packs] load failed:', e);
    }
}

function _packTitleInitial(pack) {
    const t = (pack.title || pack.identifier || '?').trim();
    return (t.charAt(0) || '?').toUpperCase();
}

function _escapeAttr(s) {
    return String(s).replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// ----- Custom emoji usage counters --------------------------------------------
// Stock emojis track `used` directly on `arrEmojis` entries; custom emojis
// don't live there, so we keep our own shortcode→count map in localStorage.
// Used to promote frequently-used custom emojis ahead of stock unicode in
// the search merge.

const CUSTOM_EMOJI_USAGE_KEY = 'vector_custom_emoji_usage';
let _customEmojiUsageCache = null;

function _customEmojiUsageMap() {
    if (_customEmojiUsageCache) return _customEmojiUsageCache;
    try {
        const raw = localStorage.getItem(CUSTOM_EMOJI_USAGE_KEY);
        _customEmojiUsageCache = raw ? JSON.parse(raw) : {};
    } catch (_) {
        _customEmojiUsageCache = {};
    }
    return _customEmojiUsageCache;
}

function _customEmojiUsage(shortcode) {
    return _customEmojiUsageMap()[shortcode] || 0;
}

function bumpCustomEmojiUsage(shortcode) {
    if (!shortcode) return;
    const map = _customEmojiUsageMap();
    map[shortcode] = (map[shortcode] || 0) + 1;
    try {
        localStorage.setItem(CUSTOM_EMOJI_USAGE_KEY, JSON.stringify(map));
    } catch (_) {}
}

/** Custom-emoji equivalent of `getMostUsedEmojis()`. Hydrates each
 *  shortcode in the usage map against `arrEmojiPacks` so the caller
 *  gets `{ isCustom, shortcode, url, used }` rows ready to merge with
 *  stock emoji recents. Customs not present in any subscribed pack are
 *  dropped silently (subscription was removed; the recents shouldn't
 *  point at packs that no longer exist). */
function getMostUsedCustomEmojis(limit) {
    const map = _customEmojiUsageMap();
    if (!arrEmojiPacks || !arrEmojiPacks.length) return [];
    const out = [];
    const seen = new Set();
    for (const shortcode of Object.keys(map)) {
        const used = map[shortcode];
        if (!used || used <= 0) continue;
        if (seen.has(shortcode)) continue;
        for (const pack of arrEmojiPacks) {
            if (!pack.emojis) continue;
            const match = pack.emojis.find(e => e.shortcode === shortcode);
            if (match) {
                seen.add(shortcode);
                out.push({ isCustom: true, shortcode, url: match.url, used });
                break;
            }
        }
    }
    out.sort((a, b) => b.used - a.used);
    return typeof limit === 'number' ? out.slice(0, limit) : out;
}

/**
 * Match every subscribed pack's emoji shortcodes against `query` and tag
 * each result with a `matchTier`:
 *   0 = exact shortcode match (top of every list)
 *   1 = prefix match (mid)
 *   2 = inner substring (bottom)
 * The picker + shortcode autocomplete use this tier to interleave custom
 * results with stock results properly — a substring custom match must
 * NOT outrank an exact stock unicode match like "kiss".
 */
function searchCustomEmojis(query) {
    if (!query || !Array.isArray(arrEmojiPacks) || !arrEmojiPacks.length) return [];
    const q = String(query).toLowerCase().replace(/^:|:$/g, '');
    if (!q) return [];
    // Base scores chosen to slot into searchEmojis' score scale so a unified
    // sort puts custom + stock on equal footing:
    //   -2.0 exact shortcode  (matches stock shortcode-exact)
    //   -1.5 prefix           (matches stock shortcode-prefix)
    //    0.3 substring        (between stock word-starts-with 0.1 and fuzzy 0.5)
    // Personal usage subtracts via the same USAGE_SCORE_WEIGHT.
    const out = [];
    const seen = new Set();
    for (const pack of arrEmojiPacks) {
        if (!pack.emojis) continue;
        for (const e of pack.emojis) {
            const sc = e.shortcode.toLowerCase();
            let baseScore;
            if (sc === q) baseScore = -2.0;
            else if (sc.startsWith(q)) baseScore = -1.5;
            else if (sc.includes(q)) baseScore = 0.3;
            else continue;
            if (seen.has(sc)) continue;
            seen.add(sc);
            const used = _customEmojiUsage(e.shortcode);
            const weight = (typeof USAGE_SCORE_WEIGHT === 'number') ? USAGE_SCORE_WEIGHT : 0.2;
            out.push({
                isCustom: true,
                shortcode: e.shortcode,
                url: e.url,
                name: e.shortcode,
                packTitle: pack.title || pack.identifier,
                used,
                score: baseScore - used * weight,
            });
        }
    }
    out.sort((a, b) => a.score - b.score || a.shortcode.length - b.shortcode.length);
    return out;
}

async function _sharePackToClipboard(pack) {
    try {
        // Share as a vectorapp.io URL — friends without Vector get a
        // working web preview, friends with Vector get the OS-level
        // deep-link interception that pops the Pack Details modal.
        // pack.id IS the naddr; no IPC round-trip needed to derive it.
        const url = `https://vectorapp.io/emojis/pack/${pack.id}`;
        await navigator.clipboard.writeText(url);
        if (typeof showToast === 'function') showToast('Copied to Clipboard');
        // Close the picker so the user lands back in their chat ready
        // to paste the link they just copied.
        picker.classList.remove('visible');
    } catch (e) {
        console.warn('[emoji-packs] share-copy failed:', e);
        if (typeof showToast === 'function') showToast('Failed to Copy');
    }
}

async function _unsubscribePackFromMenu(pack) {
    try {
        // If this is the active theme's pack, seed the theme cache with the copy
        // we already have so it stays pinned with no fetch gap after unsubscribe
        // — it just flips from a subscribed pin to a theme pin, you keep the pack.
        const themeNaddr = THEME_EMOJI_PACKS[_currentThemeName()];
        if (themeNaddr && pack.id === themeNaddr && !_themePackCache[themeNaddr]) {
            _themePackCache[themeNaddr] = { ...pack, is_theme: true };
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
    const sidebar = document.querySelector('.emoji-sidebar');
    if (!sidebar) return;
    sidebar.querySelectorAll('.emoji-pack-tab, .emoji-pack-tab-create').forEach(el => el.remove());

    for (const pack of arrEmojiPacks) {
        const btn = document.createElement('button');
        btn.className = 'emoji-category-btn emoji-pack-tab';
        btn.dataset.packId = pack.id;
        btn.title = pack.title || pack.identifier;
        if (pack.image_url) {
            const tabImg = document.createElement('img');
            tabImg.alt = '';
            bindCachedEmojiImg(tabImg, pack.image_url, 'emoji_pack_icon');
            btn.appendChild(tabImg);
        } else {
            btn.classList.add('emoji-pack-tab-letter');
            // <div> not <span>: the `.emoji-picker span` rule (line 3401 of
            // styles.css) forces every span in the picker to a 30x30 circle,
            // which destroys our inner plate dimensions.
            const plate = document.createElement('div');
            plate.className = 'emoji-pack-tab-letter-plate';
            plate.textContent = _packTitleInitial(pack);
            btn.appendChild(plate);
        }
        // Right-click on desktop, long-press on Android — both open the
        // Share / Remove menu.
        attachLongPressContextMenu(btn, (x, y) => _showPackTabMenu(pack, x, y));
        sidebar.appendChild(btn);
    }

    // "+" creator slot — always last so it stays the natural "add another"
    // affordance regardless of how many packs the user has.
    const createBtn = document.createElement('button');
    createBtn.className = 'emoji-category-btn emoji-pack-tab-create';
    createBtn.title = 'Create new pack';
    createBtn.innerHTML = '<span class="icon icon-plus-circle"></span>';
    createBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        openEmojiPackCreator();
    });
    sidebar.appendChild(createBtn);
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

    const pattern = /:([a-zA-Z0-9_-]+):/g;
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
            // Deleted/404 emoji → fall back to the literal `:shortcode:`
            // text so the message still reads coherently.
            bindCachedEmojiImg(img, url, 'emoji', (el) => {
                el.replaceWith(document.createTextNode(`:${shortcode}:`));
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
// In-chat pack preview cards
// ============================================================================

/**
 * In-memory cache of fetched pack previews keyed by the lowercased naddr.
 * Cards re-render frequently (on every chat reopen), so we keep the
 * relay fetch one-shot per naddr per session.
 */
const _packPreviewCache = new Map(); // naddr -> { state: 'loading'|'ok'|'err', pack?, error? }

// Matches an emoji-pack reference in three forms, all collapsing to the
// same naddr capture group:
//   1. Bare:        naddr1...
//   2. NIP-21:      nostr:naddr1...
//   3. Vector URL:  https://vectorapp.io/emojis/pack/naddr1...  (with
//                   optional www. + optional trailing .html / slash)
// Each form is replaced by the inline preview card so the user never
// sees the underlying string — the preview's Copy button is the only
// exposed share affordance.
const NADDR_REGEX = /(?:https?:\/\/(?:www\.)?vectorapp\.io\/emojis\/pack\/|nostr:)?(naddr1[ac-hj-np-z02-9]{20,})(?:\.html)?\/?/gi;

/**
 * Strip every emoji-pack reference (bare naddr, `nostr:` URI, or
 * vectorapp.io share URL) from `text` so the in-chat preview card
 * carries the share affordance and the raw text doesn't double up as
 * wall-of-text. Collapses the whitespace left behind so trailing /
 * leading spaces don't leak.
 */
function stripEmojiPackNaddrs(text) {
    if (!text) return text;
    return text
        .replace(NADDR_REGEX, '')
        .replace(/[ \t]{2,}/g, ' ')
        .replace(/\n{3,}/g, '\n\n')
        .trim();
}

/**
 * Find every naddr (emoji pack candidate) inside `text` and resolve via
 * backend. Resolved packs are appended as preview cards under `target`.
 * Cards build immediately in a "loading" state to reserve layout and
 * fade to the resolved content on completion.
 *
 * @param {HTMLElement} target — append target (usually `.dmsg-content`)
 * @param {string} text — message body text
 */
function renderEmojiPackPreviews(target, text) {
    if (!text) return;
    NADDR_REGEX.lastIndex = 0;
    const seen = new Set();
    let match;
    while ((match = NADDR_REGEX.exec(text)) !== null) {
        const naddr = match[1].toLowerCase();
        if (seen.has(naddr)) continue;
        seen.add(naddr);
        const card = _buildPackPreviewCard(naddr);
        target.appendChild(card);
    }
}

function _buildPackPreviewCard(naddr) {
    const card = document.createElement('div');
    card.className = 'emoji-pack-preview';
    card.dataset.naddr = naddr;

    const left = document.createElement('div');
    left.className = 'emoji-pack-preview-grid';
    card.appendChild(left);

    const right = document.createElement('div');
    right.className = 'emoji-pack-preview-meta';
    card.appendChild(right);

    // Skeleton placeholders fill both columns while the relay fetch
    // runs — same shape + dimensions as the resolved content so the
    // card doesn't reflow when it lands. Shimmer animation on each
    // sub-element keeps the loading state alive.
    card.classList.add('is-loading');
    left.innerHTML = `
        <div class="pack-skel pack-skel-thumb"></div>
        <div class="pack-skel pack-skel-thumb"></div>
        <div class="pack-skel pack-skel-thumb"></div>
        <div class="pack-skel pack-skel-thumb"></div>
        <div class="pack-skel pack-skel-thumb"></div>
        <div class="pack-skel pack-skel-thumb"></div>
    `;
    right.innerHTML = `
        <div class="emoji-pack-preview-title-row">
            <div class="pack-skel pack-skel-logo"></div>
            <div class="pack-skel pack-skel-title"></div>
        </div>
        <div class="pack-skel pack-skel-sub"></div>
        <div class="pack-skel pack-skel-actions"></div>
    `;

    _resolvePackPreview(naddr).then(result => {
        _fillPackPreviewCard(card, result);
    });

    return card;
}

// Cached error results expire after this many ms — long enough to
// coalesce burst re-renders of the same message (reactions land, edits,
// etc.) but short enough that a chat reopen after a slow relay gets a
// fresh attempt instead of inheriting the stale "Pack Unavailable".
const PACK_PREVIEW_ERR_TTL_MS = 10_000;

async function _resolvePackPreview(naddr) {
    const cached = _packPreviewCache.get(naddr);
    if (cached) {
        if (cached.state === 'loading') return cached.promise;
        if (cached.state === 'ok') return cached;
        if (cached.state === 'err') {
            const age = Date.now() - (cached.at || 0);
            if (age < PACK_PREVIEW_ERR_TTL_MS) return cached;
            // Stale error — fall through and refetch.
        }
    }

    const promise = (async () => {
        try {
            const pack = await invoke('fetch_emoji_pack_by_naddr', { naddr });
            const result = { state: 'ok', pack };
            _packPreviewCache.set(naddr, result);
            return result;
        } catch (e) {
            const result = { state: 'err', error: String(e), at: Date.now() };
            _packPreviewCache.set(naddr, result);
            return result;
        }
    })();
    _packPreviewCache.set(naddr, { state: 'loading', promise });
    return promise;
}

function _isPackSubscribed(id) {
    // Theme packs are pinned, not user subscriptions — don't report them here.
    return Array.isArray(arrEmojiPacks) && arrEmojiPacks.some(p => p.id === id && !p.is_theme);
}

// ============================================================================
// Pack Details modal — opened via vector://emojis/pack/<naddr> deep link or
// from in-app entry points (share-pack copy, etc.). Reuses the same fetch
// cache + subscription helpers as the in-chat preview card; the difference
// is just the chrome (global overlay vs inline card).
// ============================================================================

let _packDetailsCurrentNaddr = null;
let _packDetailsBusy = false;

async function openPackDetailsModal(naddr) {
    if (!naddr) return;
    const overlay = document.getElementById('pack-details-overlay');
    const body = document.getElementById('pack-details-body');
    if (!overlay || !body) return;

    _packDetailsCurrentNaddr = naddr;
    overlay.hidden = false;
    body.innerHTML = `
        <div class="pack-details-loading">
            <div class="pack-details-spinner"></div>
            <p class="pack-details-loading-text">Loading pack…</p>
        </div>
    `;

    try {
        const pack = await invoke('fetch_emoji_pack_by_naddr', { naddr });
        // Naddr may have changed in the time the IPC was in flight (user
        // closed + reopened the modal with a different link). Bail.
        if (_packDetailsCurrentNaddr !== naddr) return;
        _renderPackDetails(naddr, pack);
    } catch (err) {
        if (_packDetailsCurrentNaddr !== naddr) return;
        console.warn('[pack-details] fetch failed:', err);
        body.innerHTML = `
            <div class="pack-details-error">
                <p class="pack-details-error-title">Pack unavailable</p>
                <p class="pack-details-error-detail">${_escapeAttr(String(err) || 'Failed to fetch')}</p>
            </div>
        `;
    }
}

function closePackDetailsModal() {
    const overlay = document.getElementById('pack-details-overlay');
    if (overlay) overlay.hidden = true;
    _packDetailsCurrentNaddr = null;
    _packDetailsBusy = false;
}

function _renderPackDetails(naddr, pack) {
    const body = document.getElementById('pack-details-body');
    if (!body || !pack) return;

    const emojis = Array.isArray(pack.emojis) ? pack.emojis : [];
    const displayEmojis = emojis.slice(0, MAX_DISPLAY_EMOJIS_PER_PACK);
    const title = _escapeAttr(pack.title || pack.identifier || 'Untitled');
    const fallbackChar = (pack.title || pack.identifier || '?').trim().charAt(0).toUpperCase();
    const isSub = _isPackSubscribed(pack.id);

    body.innerHTML = `
        <div class="pack-details-header">
            <div class="pack-details-logo" id="pack-details-logo">
                <span class="pack-details-logo-fallback">${_escapeAttr(fallbackChar)}</span>
            </div>
            <div class="pack-details-title-block">
                <h3 class="pack-details-title">${title}</h3>
                <div class="pack-details-meta">
                    <span class="emoji-count">${emojis.length}</span> Emoji${emojis.length === 1 ? '' : 's'}
                </div>
            </div>
        </div>
        ${pack.description ? `<p class="pack-details-desc">${_escapeAttr(pack.description)}</p>` : ''}
        <div class="pack-details-grid" id="pack-details-grid"></div>
        <button type="button" class="pack-details-action ${isSub ? 'is-subscribed' : ''}" id="pack-details-action">
            ${isSub ? 'Remove Pack' : 'Add Pack'}
        </button>
    `;

    // Logo — route through the emoji cache so Blossom URLs never hit the
    // webview directly.
    if (pack.image_url) {
        const logo = document.getElementById('pack-details-logo');
        const fallback = logo.querySelector('.pack-details-logo-fallback');
        const img = document.createElement('img');
        img.alt = '';
        bindCachedEmojiImg(img, pack.image_url, 'emoji_pack_icon');
        img.addEventListener('load', () => {
            fallback && fallback.remove();
            logo.style.backgroundColor = 'transparent';
        }, { once: true });
        logo.appendChild(img);
    }

    // Thumbnail grid.
    const grid = document.getElementById('pack-details-grid');
    if (displayEmojis.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'pack-details-empty';
        empty.textContent = 'Empty pack';
        grid.appendChild(empty);
    } else {
        for (const e of displayEmojis) {
            const cell = document.createElement('div');
            cell.className = 'pack-details-thumb';
            const img = document.createElement('img');
            img.alt = `:${e.shortcode}:`;
            img.dataset.emojiTooltip = `:${e.shortcode}:`;
            bindCachedEmojiImg(img, e.url, 'emoji');
            cell.appendChild(img);
            grid.appendChild(cell);
        }
    }

    // Action button — subscribe / unsubscribe via the same IPCs the
    // in-chat preview uses. Cap-gate on the equipped-pack limit.
    const actionBtn = document.getElementById('pack-details-action');
    actionBtn.addEventListener('click', () => _onPackDetailsAction(pack));
}

async function _onPackDetailsAction(pack) {
    if (_packDetailsBusy) return;
    const btn = document.getElementById('pack-details-action');
    if (!btn) return;
    const isSub = _isPackSubscribed(pack.id);

    if (!isSub && _userPackCount() >= MAX_EQUIPPED_PACKS) {
        _pcShowSlotFullError();
        return;
    }

    _packDetailsBusy = true;
    btn.disabled = true;
    const origText = btn.textContent.trim();
    btn.textContent = isSub ? 'Removing…' : 'Adding…';

    const minDelay = new Promise(r => setTimeout(r, 300));
    try {
        const work = isSub
            ? invoke('unsubscribe_emoji_pack', { id: pack.id })
            : invoke('subscribe_emoji_pack', { naddr: _packDetailsCurrentNaddr });
        await Promise.all([work, minDelay]);
        await loadEmojiPacks();
        if (!isSub) {
            // Successful add — close the modal and confirm via toast so
            // the user lands back in their normal flow.
            closePackDetailsModal();
            if (typeof showToast === 'function') showToast('Pack equipped');
            return;
        }
        // Successful remove — flip button state in-place.
        btn.classList.add('is-subscribed');
        btn.classList.remove('is-subscribed');
        btn.textContent = 'Add Pack';
    } catch (e) {
        console.warn('[pack-details] toggle failed:', e);
        btn.textContent = origText;
        if (typeof showToast === 'function') showToast(String(e) || 'Failed');
    } finally {
        btn.disabled = false;
        _packDetailsBusy = false;
    }
}

// Wire close interactions once at module load.
(function _initPackDetailsModal() {
    const overlay = document.getElementById('pack-details-overlay');
    if (!overlay) return;
    const closeBtn = document.getElementById('pack-details-close');
    closeBtn.addEventListener('click', closePackDetailsModal);
    // Backdrop dismiss — only when the click landed on the overlay itself,
    // not when it bubbled up from card content.
    overlay.addEventListener('click', (e) => {
        if (e.target === overlay) closePackDetailsModal();
    });
    document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && !overlay.hidden) closePackDetailsModal();
    });
})();

function _fillPackPreviewCard(card, result) {
    if (!card.isConnected) return;
    card.classList.remove('is-loading');

    const left = card.querySelector('.emoji-pack-preview-grid');
    const right = card.querySelector('.emoji-pack-preview-meta');
    left.innerHTML = '';
    right.innerHTML = '';

    if (result.state === 'err') {
        card.classList.add('is-error');
        right.innerHTML = `<div class="emoji-pack-preview-title-row"><span class="emoji-pack-preview-title">Pack unavailable</span></div><div class="emoji-pack-preview-desc">${_escapeAttr(result.error || 'Failed to fetch')}</div>`;
        return;
    }

    const pack = result.pack;
    card.dataset.packId = pack.id;

    // Up to three rows × six columns of thumbnails. The fade gradient
    // only kicks in once the third row is starting — smaller packs sit
    // flush so we don't fake "more below" when there isn't any.
    const thumbs = pack.emojis.slice(0, 18);
    for (const e of thumbs) {
        const img = document.createElement('img');
        bindCachedEmojiImg(img, e.url, 'emoji');
        img.alt = `:${e.shortcode}:`;
        img.dataset.emojiTooltip = `:${e.shortcode}:`;
        img.className = 'emoji-pack-preview-thumb';
        left.appendChild(img);
    }
    if (thumbs.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'emoji-pack-preview-empty';
        empty.textContent = 'Empty pack';
        left.appendChild(empty);
    } else if (pack.emojis.length > 12) {
        left.classList.add('is-overflowing');
    }

    const titleRow = document.createElement('div');
    titleRow.className = 'emoji-pack-preview-title-row';
    if (pack.image_url) {
        const logo = document.createElement('img');
        logo.className = 'emoji-pack-preview-logo';
        bindCachedEmojiImg(logo, pack.image_url, 'emoji_pack_icon');
        logo.alt = '';
        titleRow.appendChild(logo);
    }
    const title = document.createElement('span');
    title.className = 'emoji-pack-preview-title';
    title.textContent = pack.title || pack.identifier;
    titleRow.appendChild(title);
    right.appendChild(titleRow);

    const sub = document.createElement('div');
    sub.className = 'emoji-pack-preview-sub';
    sub.textContent = `${pack.emojis.length} emoji${pack.emojis.length === 1 ? '' : 's'}`;
    right.appendChild(sub);

    if (pack.description) {
        const desc = document.createElement('div');
        desc.className = 'emoji-pack-preview-desc';
        desc.textContent = pack.description;
        right.appendChild(desc);
    }

    const actions = document.createElement('div');
    actions.className = 'emoji-pack-preview-actions';

    const copyBtn = document.createElement('button');
    copyBtn.className = 'btn emoji-pack-preview-copy';
    copyBtn.title = 'Copy share link';
    copyBtn.innerHTML = '<span class="icon icon-copy"></span>';
    copyBtn.addEventListener('click', (ev) => {
        ev.stopPropagation();
        _onPackPreviewCopyClick(copyBtn, card);
    });
    actions.appendChild(copyBtn);

    const btn = document.createElement('button');
    btn.className = 'btn emoji-pack-preview-add';
    _setPackPreviewButtonState(btn, pack, _isPackSubscribed(pack.id));
    btn.addEventListener('click', async (ev) => {
        ev.stopPropagation();
        await _onPackPreviewButtonClick(btn, card);
    });
    actions.appendChild(btn);

    right.appendChild(actions);
}

function _onPackPreviewCopyClick(btn, card) {
    const naddr = card.dataset.naddr;
    if (!naddr) return;
    // Copy the shareable vectorapp.io URL (matches the "Share Pack" action),
    // not the bare naddr — the URL gives a web preview on other platforms and
    // the OS deep-link interception for Vector users.
    const shareUrl = `https://vectorapp.io/emojis/pack/${naddr}`;
    navigator.clipboard.writeText(shareUrl).then(() => {
        const icon = btn.querySelector('.icon');
        if (!icon) return;
        icon.classList.remove('icon-copy');
        icon.classList.add('icon-check');
        setTimeout(() => {
            icon.classList.remove('icon-check');
            icon.classList.add('icon-copy');
        }, 1500);
    }).catch(err => {
        console.warn('[emoji-packs] copy share link failed:', err);
    });
}

function _setPackPreviewButtonState(btn, pack, isSubscribed) {
    btn.dataset.packId = pack.id;
    if (isSubscribed) {
        btn.classList.add('is-subscribed');
        btn.textContent = 'Remove';
    } else {
        btn.classList.remove('is-subscribed');
        btn.textContent = 'Add Pack';
    }
}

/** Sweep every in-chat pack preview card's Add/Remove button and re-sync
 *  its label to current subscription state. Called whenever
 *  `arrEmojiPacks` mutates so the inline cards don't drift after a
 *  right-click "Remove Pack" or a sidebar subscribe action. */
function _refreshPackPreviewButtons() {
    document.querySelectorAll('.emoji-pack-preview-add[data-pack-id]')
        .forEach(btn => {
            const id = btn.dataset.packId;
            if (!id) return;
            // Cheap: only re-paint label/class. We don't have the full
            // pack object here but `_setPackPreviewButtonState` only
            // reads `pack.id` from it.
            _setPackPreviewButtonState(btn, { id }, _isPackSubscribed(id));
        });
}

async function _onPackPreviewButtonClick(btn, card) {
    const id = btn.dataset.packId;
    const naddr = card.dataset.naddr;
    if (!id || btn.disabled) return;
    const isSubscribed = btn.classList.contains('is-subscribed');
    // Pre-gate the cap so users see actionable copy instead of the
    // backend's raw error. Subscribing to a pack we already have
    // (idempotent) doesn't count, but that path goes through the
    // "Remove" branch anyway.
    if (!isSubscribed && _userPackCount() >= MAX_EQUIPPED_PACKS) {
        _pcShowSlotFullError();
        return;
    }
    btn.disabled = true;
    const original = btn.textContent;
    btn.textContent = isSubscribed ? 'Removing…' : 'Adding…';

    // Minimum on-screen time for the transient state — local-DB toggles
    // resolve in a few ms otherwise and the label change is invisible.
    const minDelay = new Promise(r => setTimeout(r, 350));
    try {
        const work = isSubscribed
            ? invoke('unsubscribe_emoji_pack', { id })
            : invoke('subscribe_emoji_pack', { naddr });
        await Promise.all([work, minDelay]);
        await loadEmojiPacks();
        const cached = _packPreviewCache.get(naddr);
        const pack = cached && cached.state === 'ok' ? cached.pack : null;
        if (pack) {
            _setPackPreviewButtonState(btn, pack, _isPackSubscribed(pack.id));
        }
    } catch (e) {
        console.warn('[emoji-packs] subscribe toggle failed:', e);
        btn.textContent = original;
    } finally {
        btn.disabled = false;
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
    const main = document.querySelector('.emoji-main');
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
    mo.observe(main, { childList: true });
}

/**
 * Attach `<img>` elements to pre-laid-out emoji cells in small batches,
 * one batch per animation frame. The grid keeps its final geometry from
 * the moment cells are appended (empty spans are still 36×36 inside the
 * grid template), so this only spreads decode + compositor work, never
 * triggers a layout shift. Used as the fallback path when ImageDecoder
 * isn't available (older WKWebView / WebView2).
 */
function _hydrateEmojiCellsStaggered(cells) {
    if (!cells.length) return;
    let i = 0;
    const BATCH = 8;
    function step() {
        const end = Math.min(i + BATCH, cells.length);
        for (; i < end; i++) {
            const { span, emoji } = cells[i];
            if (!span.isConnected) continue;
            const img = document.createElement('img');
            img.alt = `:${emoji.shortcode}:`;
            bindCachedEmojiImg(img, emoji.url, 'emoji');
            span.appendChild(img);
        }
        if (i < cells.length) requestAnimationFrame(step);
    }
    requestAnimationFrame(step);
}

// ============================================================================
// Canvas-batched pack rendering
// ============================================================================
//
// Each pack section gets one `<canvas>` that draws every emoji from a
// shared decoded-frame cache. One compositor layer per section instead
// of one per emoji, and animation is driven by a single requestAnimationFrame
// — not by the browser's native animated-image pipeline. This is what
// Discord (on Electron/Chromium) gets for free; on WKWebView we build it.
//
// Decoding: ImageDecoder (WebCodecs) extracts each frame as an
// ImageBitmap, resized down to 56×56 so memory stays bounded
// (54 emojis × ~30 frames × ~12KB ≈ 20MB). Frames are cached globally by
// URL so reopening the panel reuses the decode.
//
// Activation: IntersectionObserver toggles each section's slot in the
// global active set; the rAF loop self-terminates when the set is empty.
// Panel hide manually drains the set so we don't keep ticking under
// opacity:0. Single shared rAF for ALL pack sections in the picker.

// WKWebView doesn't ship the WebCodecs `ImageDecoder` API, so frame
// decoding runs in Rust via a Tauri command and ships a single
// PNG spritesheet per emoji over IPC. Frontend just slices the loaded
// `<img>` into the canvas via drawImage(src, sx, sy, sw, sh, ...).
const PACK_CANVAS_AVAILABLE = typeof invoke === 'function';
const PACK_CANVAS_THUMB_PX = 28;
/** Row stride. Slightly taller than the thumb's 28+pad to give rows a
 *  bit of breathing room between them, matching the stock grid's 4px
 *  gap visually without introducing real gaps inside the canvas. */
const PACK_CANVAS_CELL_PX = 38;
/** Visible hover/active highlight box — always square, sized to the
 *  row height minus margin so vertical gaps between rows stay clean. */
const PACK_CANVAS_BOX_PX = 34;
/** Horizontal gap between cells. Mirrors stock `.emoji-grid { gap: 4px }`
 *  so the canvas column positions land at the same x-coordinates as the
 *  native grid's columns — otherwise canvas cells are slightly wider
 *  and the first-column thumb visibly drifts right of stock's first emoji. */
const PACK_CANVAS_GAP_PX = 4;
/** Hover scale + animation duration — mirrors stock CSS
 *  `transition: all 0.2s ease; .emoji-grid span:hover { transform: scale(1.125) }`.
 *  Each cell tweens independently so quick mouse-overs don't snap. */
const PACK_CANVAS_HOVER_SCALE = 1.125;
const PACK_CANVAS_HOVER_MS = 200;
const PACK_CANVAS_TOOLTIP_DELAY_MS = 350;

// ============================================================================
// Shared Vector tooltip (used by canvas cells, preview thumbs, inline emoji)
// ============================================================================
//
// One global tooltip element + show/hide/schedule helpers. Two ways in:
//   1. `data-emoji-tooltip="..."` on any element → handled automatically
//      via document-level mouseover/mouseout delegation
//   2. Direct API (`scheduleEmojiTooltipAt`) → used by the canvas, which
//      has no per-cell DOM, just coordinates
//
// All callers share the same tooltip node so simultaneously-hovered
// elements can't double up, and the styling stays consistent.

let _emojiTooltipEl = null;
let _emojiTooltipTimer = null;
let _emojiTooltipCurrentAnchor = null;

/** Touch-only mobiles synthesise mouseover/mouseout on tap, which would
 *  flash the tooltip on every emoji selection — suppress on mobile.
 *  `platformFeatures.is_mobile` is Vector's canonical desktop/mobile split
 *  (set up in main.js at boot) and is used by every other hover-only path. */
function _supportsHoverTooltip() {
    return typeof platformFeatures !== 'undefined' && platformFeatures !== null && !platformFeatures.is_mobile;
}

function _ensureEmojiTooltip() {
    if (_emojiTooltipEl) return _emojiTooltipEl;
    _emojiTooltipEl = document.createElement('div');
    _emojiTooltipEl.className = 'emoji-pack-canvas-tooltip';
    document.body.appendChild(_emojiTooltipEl);
    return _emojiTooltipEl;
}

function showEmojiTooltipAt(text, x, y) {
    const el = _ensureEmojiTooltip();
    el.textContent = text;
    el.style.left = `${x}px`;
    el.style.top = `${y}px`;
    el.classList.add('is-visible');
}

function hideEmojiTooltip() {
    if (_emojiTooltipTimer) {
        clearTimeout(_emojiTooltipTimer);
        _emojiTooltipTimer = null;
    }
    _emojiTooltipCurrentAnchor = null;
    if (_emojiTooltipEl) _emojiTooltipEl.classList.remove('is-visible');
}

function scheduleEmojiTooltipAt(text, x, y, delay = PACK_CANVAS_TOOLTIP_DELAY_MS) {
    if (!_supportsHoverTooltip()) return;
    if (_emojiTooltipTimer) clearTimeout(_emojiTooltipTimer);
    _emojiTooltipTimer = setTimeout(() => {
        _emojiTooltipTimer = null;
        showEmojiTooltipAt(text, x, y);
    }, delay);
}

// Event-delegation path for any element carrying a `data-emoji-tooltip`
// attribute (preview thumbs, inline custom emoji <img>s, anything else
// that wants the same look). Skipped at handler time when mobile.
document.addEventListener('mouseover', (e) => {
    if (!_supportsHoverTooltip()) return;
    const el = e.target.closest && e.target.closest('[data-emoji-tooltip]');
    if (!el || el === _emojiTooltipCurrentAnchor) return;
    _emojiTooltipCurrentAnchor = el;
    const text = el.dataset.emojiTooltip;
    if (!text) return;
    const rect = el.getBoundingClientRect();
    scheduleEmojiTooltipAt(text, rect.left + rect.width / 2, rect.top);
}, true);

document.addEventListener('mouseout', (e) => {
    if (!_supportsHoverTooltip()) return;
    const el = e.target.closest && e.target.closest('[data-emoji-tooltip]');
    if (!el) return;
    // mouseout fires when crossing into children too — only react when we
    // actually leave the tooltipped element.
    const related = e.relatedTarget;
    if (related && el.contains(related)) return;
    if (related && related.closest && related.closest('[data-emoji-tooltip]') === el) return;
    hideEmojiTooltip();
}, true);

// Mobile tap-tooltip — desktop uses hover, mobile users tap to see the
// shortcode. Any other interaction (tap elsewhere, scroll, swipe, key)
// dismisses. Bypasses the hover suppression check since mobile WebViews
// synthesize mouseover-on-tap, but the document-level click listener
// runs *after* those so a stale tooltip from a synthesized hover would
// just get re-shown here anyway.
function _isMobileTouchEnv() {
    return typeof platformFeatures !== 'undefined' && platformFeatures.is_mobile;
}

document.addEventListener('click', (e) => {
    if (!_isMobileTouchEnv()) return;
    const el = e.target.closest && e.target.closest('[data-emoji-tooltip]');
    if (!el) {
        // Tap landed elsewhere — dismiss any active tooltip.
        if (_emojiTooltipCurrentAnchor) hideEmojiTooltip();
        return;
    }
    // Toggle: tapping the same emoji again hides; tapping a different
    // tooltipped emoji moves the tooltip to it.
    if (_emojiTooltipCurrentAnchor === el) {
        hideEmojiTooltip();
        return;
    }
    const text = el.dataset.emojiTooltip;
    if (!text) return;
    _emojiTooltipCurrentAnchor = el;
    const rect = el.getBoundingClientRect();
    showEmojiTooltipAt(text, rect.left + rect.width / 2, rect.top);
}, true);

// Any of these dismiss a mobile-tap tooltip — anything that "isn't
// looking at this emoji anymore" should drop it. Desktop hover takes
// care of its own dismissal via mouseout above.
document.addEventListener('touchmove', () => {
    if (_isMobileTouchEnv() && _emojiTooltipCurrentAnchor) hideEmojiTooltip();
}, { passive: true });
document.addEventListener('scroll', () => {
    if (_isMobileTouchEnv() && _emojiTooltipCurrentAnchor) hideEmojiTooltip();
}, true);
document.addEventListener('keydown', () => {
    if (_isMobileTouchEnv() && _emojiTooltipCurrentAnchor) hideEmojiTooltip();
});

/** url → Promise<{img: HTMLImageElement, frameCount, frameSize, durations: number[]} | null> */
const _packEmojiSheetCache = new Map();

async function decodePackEmojiFrames(url) {
    if (_packEmojiSheetCache.has(url)) return _packEmojiSheetCache.get(url);
    const promise = (async () => {
        try {
            const sheet = await invoke('decode_animated_emoji', { url });
            if (!sheet || !sheet.png_base64) return null;
            const img = new Image();
            img.src = `data:image/png;base64,${sheet.png_base64}`;
            // decode() blocks until the PNG is fully ready to draw — avoids
            // a flash of placeholder when the canvas calls drawImage().
            if (typeof img.decode === 'function') {
                try { await img.decode(); } catch (_e) {}
            } else {
                await new Promise((res, rej) => {
                    img.onload = res;
                    img.onerror = rej;
                });
            }
            return {
                img,
                frameCount: sheet.frame_count,
                frameSize: sheet.frame_size,
                durations: sheet.frame_durations_ms || [],
            };
        } catch (e) {
            console.warn('[emoji-packs] frame decode failed:', url, e);
            return null;
        }
    })();
    _packEmojiSheetCache.set(url, promise);
    return promise;
}

const _activeCanvasSections = new Set();
let _packCanvasRafHandle = null;
let _packCanvasLastTick = 0;

function _packCanvasTick(now) {
    const dt = _packCanvasLastTick ? Math.min(now - _packCanvasLastTick, 100) : 0;
    _packCanvasLastTick = now;
    let anyActive = false;
    for (const section of _activeCanvasSections) {
        if (section._advance(dt)) anyActive = true;
    }
    // Pause when nothing needs animating (all visible packs static + idle).
    // Sections stay in the active set; hover / frame-load / re-intersect restart
    // the loop. Avoids a 60fps wakeup while the panel sits open on static packs.
    if (_activeCanvasSections.size === 0 || !anyActive) {
        _packCanvasRafHandle = null;
        _packCanvasLastTick = 0;
        return;
    }
    _packCanvasRafHandle = requestAnimationFrame(_packCanvasTick);
}

function _startPackCanvasLoop() {
    if (_packCanvasRafHandle) return;
    _packCanvasLastTick = 0;
    _packCanvasRafHandle = requestAnimationFrame(_packCanvasTick);
}

/** Force-stop the loop and clear active sections — used when the picker
 *  closes so we don't keep ticking under opacity:0. */
function _stopPackCanvasLoop() {
    _activeCanvasSections.clear();
    if (_packCanvasRafHandle) {
        cancelAnimationFrame(_packCanvasRafHandle);
        _packCanvasRafHandle = null;
    }
}

/** Re-arm the pack canvases currently on-screen in `.emoji-main` after a panel
 *  reopen: decode their frames (lazily — off-screen packs are left untouched)
 *  and resume the loop. Uses a direct geometry check instead of leaning on the
 *  IntersectionObserver, which doesn't reliably re-fire across the panel's
 *  hide/show (the close drains the active set, but the observed intersection
 *  state never changed). The IO still handles subsequent scrolling. */
function _rearmVisiblePackCanvases() {
    const main = document.querySelector('.emoji-main');
    if (!main || _packCanvasGrids.size === 0) return;
    // main + each canvas share the panel's transform, so this viewport-space
    // overlap test stays correct even mid open-transition.
    const mainRect = main.getBoundingClientRect();
    let any = false;
    for (const grid of _packCanvasGrids.values()) {
        const r = grid.canvas.getBoundingClientRect();
        if (r.height > 0 && r.bottom > mainRect.top && r.top < mainRect.bottom) {
            grid._requestFrames();
            _activeCanvasSections.add(grid);
            any = true;
        }
    }
    if (any) _startPackCanvasLoop();
}

function _drawRoundedRect(ctx, x, y, w, h, r) {
    ctx.beginPath();
    ctx.moveTo(x + r, y);
    ctx.lineTo(x + w - r, y);
    ctx.quadraticCurveTo(x + w, y, x + w, y + r);
    ctx.lineTo(x + w, y + h - r);
    ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
    ctx.lineTo(x + r, y + h);
    ctx.quadraticCurveTo(x, y + h, x, y + h - r);
    ctx.lineTo(x, y + r);
    ctx.quadraticCurveTo(x, y, x + r, y);
    ctx.closePath();
}

class PackCanvasGrid {
    constructor(pack) {
        this.pack = pack;
        this.cols = 6;
        this.rows = Math.ceil(pack.emojis.length / this.cols) || 1;
        this.dpr = Math.min(window.devicePixelRatio || 1, 2);
        // Cell dimensions are computed from the parent's width at attach
        // time so the canvas matches the native grid's `repeat(6, 1fr)`
        // layout instead of bunching to a fixed 216px in the centre.
        this.cellW = PACK_CANVAS_CELL_PX;
        this.cellH = PACK_CANVAS_CELL_PX;

        const canvas = document.createElement('canvas');
        canvas.className = 'emoji-pack-canvas';
        canvas.dataset.packId = pack.id;
        canvas.style.display = 'block';
        canvas.style.width = '100%';
        this.canvas = canvas;
        this.ctx = canvas.getContext('2d');

        this.frames = new Array(pack.emojis.length);
        this.cellState = pack.emojis.map(() => ({
            frame: 0,
            elapsed: 0,
            // Hover scale state — per-cell so concurrent enter/leave
            // animations on different cells coexist without snapping.
            scale: 1,
            scaleTarget: 1,
            scaleFrom: 1,
            scaleStart: 0,
        }));
        this.hoveredIndex = -1;
        this.dirty = new Set();
        this._io = null;
        this._ro = null;

        // Animation scheduler. Skip the per-cell scan on ticks where no frame is
        // due and no hover tween is running; `_nextDue` is ms until the soonest
        // frame flip across loaded animated cells (Infinity = nothing animated).
        // `_accumDt` banks elapsed time across skipped ticks so the eventual scan
        // advances by the real elapsed time. Starts at 0 so the first ticks scan
        // until frames load + settle.
        this._nextDue = 0;
        this._accumDt = 0;
        this._hasTween = false;

        // Tooltip drives the shared singleton (defined at module scope so
        // canvas cells, preview thumbs and inline custom emoji all share
        // one node + one show/hide timer).
        this._tooltipPendingIdx = -1;

        for (let i = 0; i < pack.emojis.length; i++) this.dirty.add(i);

        this._installEvents();
        // Frames are decoded lazily on first visibility (see
        // attachVisibilityObserver) — an off-screen pack decodes nothing.
        this._framesRequested = false;
    }

    _resize() {
        const parent = this.canvas.parentElement;
        if (!parent) return;
        const cssWidth = parent.clientWidth;
        if (cssWidth <= 0) return;
        // Match stock grid's `gap: 4px` so column positions align.
        const cellW = (cssWidth - PACK_CANVAS_GAP_PX * (this.cols - 1)) / this.cols;
        // Row stride stays compact (matches stock grid's vertical rhythm)
        // even when cells get wider — keeps the panel scannable.
        const cellH = PACK_CANVAS_CELL_PX;
        const cssHeight = cellH * this.rows;
        if (Math.abs(cellW - this.cellW) < 0.5 && cellH === this.cellH) return;
        this.canvas.style.height = cssHeight + 'px';
        this.canvas.width = Math.round(cssWidth * this.dpr);
        this.canvas.height = Math.round(cssHeight * this.dpr);
        this.cellW = cellW;
        this.cellH = cellH;
        // setTransform resets any prior scale (canvas resize clears the
        // transform too, but be explicit to avoid surprises).
        this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
        this.ctx.imageSmoothingEnabled = true;
        // 'low' is visually identical at a 28px thumb (≈1:1 at dpr 2) but far
        // cheaper than 'high' on the downscale path, which runs every drawn frame.
        this.ctx.imageSmoothingQuality = 'low';
        for (let i = 0; i < this.pack.emojis.length; i++) this.dirty.add(i);
        this._render();
    }

    _installEvents() {
        this.canvas.addEventListener('mousemove', (e) => {
            const idx = this._cellAtEvent(e);
            this._setHoverCell(idx);
            this.canvas.style.cursor = idx >= 0 ? 'pointer' : '';
        });
        this.canvas.addEventListener('mouseleave', () => {
            this._setHoverCell(-1);
            this.canvas.style.cursor = '';
        });
        this.canvas.addEventListener('click', (e) => {
            const idx = this._cellAtEvent(e);
            if (idx < 0) return;
            e.stopPropagation();
            _handlePackEmojiSelect(this.pack, this.pack.emojis[idx]);
        });
    }

    _setHoverCell(idx) {
        if (idx === this.hoveredIndex) return;
        const now = performance.now();
        if (this.hoveredIndex >= 0) {
            const prev = this.cellState[this.hoveredIndex];
            prev.scaleFrom = prev.scale;
            prev.scaleTarget = 1;
            prev.scaleStart = now;
            this.dirty.add(this.hoveredIndex);
        }
        this.hoveredIndex = idx;
        if (idx >= 0) {
            const cur = this.cellState[idx];
            cur.scaleFrom = cur.scale;
            cur.scaleTarget = PACK_CANVAS_HOVER_SCALE;
            cur.scaleStart = now;
            this.dirty.add(idx);
        }
        this._scheduleTooltip(idx);
        // A hover tween needs per-frame ticks — flag it so _advance won't skip,
        // ensure this section is active, and (re)start the loop if it went idle.
        this._hasTween = true;
        _activeCanvasSections.add(this);
        _startPackCanvasLoop();
    }

    _scheduleTooltip(idx) {
        this._tooltipPendingIdx = idx;
        if (idx < 0) {
            hideEmojiTooltip();
            return;
        }
        const emoji = this.pack.emojis[idx];
        if (!emoji) return;
        const rect = this.canvas.getBoundingClientRect();
        const col = idx % this.cols;
        const row = (idx / this.cols) | 0;
        const cx = rect.left + col * (this.cellW + PACK_CANVAS_GAP_PX) + this.cellW / 2;
        const cy = rect.top + row * this.cellH;
        scheduleEmojiTooltipAt(`:${emoji.shortcode}:`, cx, cy);
    }

    _cellAtEvent(e) {
        const rect = this.canvas.getBoundingClientRect();
        const x = e.clientX - rect.left;
        const y = e.clientY - rect.top;
        const stride = this.cellW + PACK_CANVAS_GAP_PX;
        const col = Math.floor(x / stride);
        const row = Math.floor(y / this.cellH);
        if (col < 0 || col >= this.cols || row < 0 || row >= this.rows) return -1;
        // Reject clicks that landed in the gap itself rather than on a cell.
        if ((x - col * stride) > this.cellW) return -1;
        const idx = row * this.cols + col;
        return idx < this.pack.emojis.length ? idx : -1;
    }

    // Decode this section's frames once, the first time it becomes visible.
    _requestFrames() {
        if (this._framesRequested) return;
        this._framesRequested = true;
        this._loadFrames();
    }

    _loadFrames() {
        for (let i = 0; i < this.pack.emojis.length; i++) {
            const url = this.pack.emojis[i].url;
            const idx = i;
            decodePackEmojiFrames(url).then((sheet) => {
                this.frames[idx] = sheet || null;
                this.dirty.add(idx);
                this._render();
                // A newly-loaded animated emoji needs the scheduler to re-evaluate
                // and the loop to resume if it had gone idle while static.
                this._nextDue = 0;
                if (_activeCanvasSections.has(this)) _startPackCanvasLoop();
            });
        }
    }

    attachVisibilityObserver(root) {
        // Size against parent now that we're in the DOM, then keep
        // tracking width changes (window resize, picker layout shifts).
        this._resize();
        if (typeof ResizeObserver !== 'undefined' && this.canvas.parentElement) {
            this._ro = new ResizeObserver(() => this._resize());
            this._ro.observe(this.canvas.parentElement);
        }
        if (typeof IntersectionObserver === 'undefined') {
            this._requestFrames();   // no IO to gate on — decode now
            _activeCanvasSections.add(this);
            _startPackCanvasLoop();
            return;
        }
        this._io = new IntersectionObserver(entries => {
            for (const entry of entries) {
                if (entry.isIntersecting) {
                    this._requestFrames();   // lazy decode: only once the pack is actually on-screen
                    _activeCanvasSections.add(this);
                    _startPackCanvasLoop();
                } else {
                    _activeCanvasSections.delete(this);
                }
            }
        }, { root: root || null, threshold: 0 });
        this._io.observe(this.canvas);
    }

    // Returns whether this section still needs ticking (animated frames pending,
    // a hover tween in flight, or frames still loading). The shared loop pauses
    // itself when every active section returns false.
    _advance(dt) {
        // Settled + nothing animated → no work until a hover / frame-load wakes us.
        if (!this._hasTween && this._nextDue === Infinity) return false;
        this._accumDt += Math.max(dt, 0);
        // Nothing due yet and no tween → bank the time and wait (loop stays alive
        // but this is O(1), not a full per-cell scan).
        if (!this._hasTween && this._accumDt < this._nextDue) return true;

        const effDt = this._accumDt;   // banked elapsed time (per-tick dt already clamped upstream)
        this._accumDt = 0;
        const now = performance.now();
        const n = this.pack.emojis.length;
        let nextDue = Infinity;
        let hasTween = false;

        for (let i = 0; i < n; i++) {
            const cell = this.cellState[i];
            const sheet = this.frames[i];

            if (sheet === undefined) {
                // Still loading — re-scan promptly so we start it the moment it lands.
                nextDue = 0;
            } else if (sheet && sheet.frameCount >= 2 && effDt > 0) {
                cell.elapsed += effDt;
                let dur = sheet.durations[cell.frame] || 100;
                while (cell.elapsed >= dur) {
                    cell.elapsed -= dur;
                    cell.frame = (cell.frame + 1) % sheet.frameCount;
                    dur = sheet.durations[cell.frame] || 100;
                    this.dirty.add(i);
                }
                const rem = dur - cell.elapsed;
                if (rem < nextDue) nextDue = rem;
            }

            // Hover scale tween. ease-out cubic gives the same "quick
            // start, soft settle" feel as CSS `ease` for short anims.
            if (cell.scale !== cell.scaleTarget) {
                const t = Math.min((now - cell.scaleStart) / PACK_CANVAS_HOVER_MS, 1);
                const eased = 1 - Math.pow(1 - t, 3);
                cell.scale = t >= 1
                    ? cell.scaleTarget
                    : cell.scaleFrom + (cell.scaleTarget - cell.scaleFrom) * eased;
                this.dirty.add(i);
                if (cell.scale !== cell.scaleTarget) hasTween = true;
            }
        }

        this._nextDue = nextDue;
        this._hasTween = hasTween;
        this._render();
        return hasTween || nextDue !== Infinity;
    }

    _render() {
        if (this.dirty.size === 0) return;
        const ctx = this.ctx;
        const thumb = PACK_CANVAS_THUMB_PX;
        const insetX = (this.cellW - thumb) / 2;
        const insetY = (this.cellH - thumb) / 2;
        for (const i of this.dirty) {
            const col = i % this.cols;
            const row = (i / this.cols) | 0;
            const x = col * (this.cellW + PACK_CANVAS_GAP_PX);
            const y = row * this.cellH;
            const cell = this.cellState[i];
            const cx = x + this.cellW / 2;
            const cy = y + this.cellH / 2;

            ctx.clearRect(x, y, this.cellW + PACK_CANVAS_GAP_PX, this.cellH + PACK_CANVAS_GAP_PX);

            // Scale around the cell centre so the thumb grows in place
            // instead of drifting toward a corner.
            const scaled = cell.scale !== 1;
            if (scaled) {
                ctx.save();
                ctx.translate(cx, cy);
                ctx.scale(cell.scale, cell.scale);
                ctx.translate(-cx, -cy);
            }

            // Hover highlight — alpha eases in/out with the scale tween
            // (the bg-color side of stock's `transition: all`).
            const bgProgress = Math.max(0, Math.min(1,
                (cell.scale - 1) / (PACK_CANVAS_HOVER_SCALE - 1),
            ));
            if (bgProgress > 0.01) {
                const boxX = x + (this.cellW - PACK_CANVAS_BOX_PX) / 2;
                const boxY = y + (this.cellH - PACK_CANVAS_BOX_PX) / 2;
                ctx.fillStyle = `rgba(255, 255, 255, ${0.10 * bgProgress})`;
                _drawRoundedRect(ctx, boxX, boxY, PACK_CANVAS_BOX_PX, PACK_CANVAS_BOX_PX, 6);
                ctx.fill();
            }

            const sheet = this.frames[i];
            if (sheet && sheet.img) {
                // Source rect: vertical strip of frames, each `frameSize`
                // pixels tall, indexed by the current frame.
                const sy = cell.frame * sheet.frameSize;
                ctx.drawImage(
                    sheet.img,
                    0, sy, sheet.frameSize, sheet.frameSize,
                    x + insetX, y + insetY, thumb, thumb,
                );
            } else if (sheet === undefined) {
                // Loading placeholder — same visual weight as a filled cell
                // so the layout doesn't shift when frames resolve.
                ctx.fillStyle = 'rgba(255, 255, 255, 0.03)';
                _drawRoundedRect(ctx, x + insetX, y + insetY, thumb, thumb, 4);
                ctx.fill();
            }

            if (scaled) ctx.restore();
        }
        this.dirty.clear();
    }

    destroy() {
        _activeCanvasSections.delete(this);
        if (this._io) this._io.disconnect();
        if (this._ro) this._ro.disconnect();
        // The shared tooltip belongs to the module — only hide it if it
        // happens to be showing this section's cell. Don't remove the node.
        hideEmojiTooltip();
    }
}

/** Tracks every active canvas grid by pack address so re-renders can
 *  destroy obsolete observers. */
const _packCanvasGrids = new Map();

function _destroyAllPackCanvasGrids() {
    for (const g of _packCanvasGrids.values()) g.destroy();
    _packCanvasGrids.clear();
}

// ============================================================================
// Pack Creator — in-panel view
// ============================================================================
//
// Replaces the emoji grid sections inside `.emoji-main` while editing one
// of the user's own packs. Uploads + the kind 30030 publish are deferred
// until the user exits the creator (clicks a sidebar tab, hits the close
// pencil, or closes the picker) so an abandoned edit costs zero network.

// Pack-creator per-pack emoji cap. `let` so the Vector badge can raise it
// at runtime (see `applyBadgeLimits`).
let PC_MAX_EMOJIS = 30;
const PC_MAX_FILE_BYTES = 256 * 1024;

const _pc = {
    open: false,
    mode: 'create',          // 'create' | 'edit'
    editingId: null,
    editingIdentifier: null,
    name: '',
    logoUrl: '',             // existing remote URL (edit mode)
    logoFile: null,          // File pending upload
    logoBlobUrl: '',
    emojis: [],              // Array<{ shortcode, url?, file?, blobUrl? }>
    // Remote URLs queued for Blossom cleanup at next save. Populated by
    // _pcRemoveEmoji (emoji × badge) and _pcSetLogoFile (logo replace).
    // Cleared after a successful publish; persists across save failures
    // so retries don't leak the orphan.
    pendingBlobDeletes: [],
    saving: false,
    dirty: false,            // tracks unsaved changes for auto-save on exit
};

function openEmojiPackCreator(id) {
    const editingPack = id ? arrEmojiPacks.find(p => p.id === id) : null;
    // Creating a new pack while at the equipped-pack cap would be
    // rejected by the backend — surface the same overlay we use for
    // other publish failures so the user gets actionable feedback.
    // Editing an existing own pack is always fine (no slot change).
    if (!editingPack && _userPackCount() >= MAX_EQUIPPED_PACKS) {
        _pcShowSlotFullError();
        return;
    }

    _pcRevokeBlobUrls();
    _pc.open = true;
    _pc.saving = false;
    _pc.dirty = false;
    _pc.logoFile = null;
    _pc.logoBlobUrl = '';
    _pc.pendingBlobDeletes = [];
    if (editingPack) {
        _pc.mode = 'edit';
        _pc.editingId = editingPack.id;
        _pc.editingIdentifier = editingPack.identifier;
        _pc.name = editingPack.title || '';
        _pc.logoUrl = editingPack.image_url || '';
        _pc.emojis = editingPack.emojis.map(e => ({ shortcode: e.shortcode, url: e.url }));
    } else {
        _pc.mode = 'create';
        _pc.editingId = null;
        _pc.editingIdentifier = null;
        _pc.name = '';
        _pc.logoUrl = '';
        _pc.emojis = [];
    }
    _pcShowView(true);
    _pcSyncDom();
    // Reset the scroll container to the top — without this, opening
    // edit mode for a pack while the picker was already scrolled to
    // that pack's section leaves the creator showing its grid bottom
    // instead of the pack title.
    const main = document.querySelector('.emoji-main');
    if (main) main.scrollTop = 0;
    // Focus the title for immediate-edit affordance — but only on
    // desktop. On mobile the focus pops the soft keyboard, which lands
    // on top of the creator UI and is jarring when the user might not
    // even want to edit the name (e.g. just opened to add emojis).
    const nameInput = document.getElementById('emoji-creator-name');
    const isMobile = typeof platformFeatures !== 'undefined' && platformFeatures.is_mobile;
    if (nameInput && !isMobile) setTimeout(() => nameInput.focus(), 50);
}

/** Saves pending edits (if dirty) and switches back to the normal
 *  emoji-section view. Safe to call multiple times. */
async function closeEmojiPackCreator() {
    if (!_pc.open) return;
    if (_pc.dirty && !_pc.saving) {
        await _pcSave();
    }
    _pc.open = false;
    _pcShowView(false);
    _pcRevokeBlobUrls();
    // Save can fail (network/relay error) — _pc.emojis still holds entries
    // whose blobUrl strings we just revoked. Clear them so a re-open
    // starting from cached _pc state doesn't render dead <img> sources.
    if (_pc.dirty) {
        _pc.emojis = [];
        _pc.logoFile = null;
    }
}

function _pcShowView(showCreator) {
    const main = document.querySelector('.emoji-main');
    if (!main) return;
    const creator = document.getElementById('emoji-creator');
    if (!creator) return;
    // Toggle: hide all `.emoji-section`s when in creator mode, show only
    // `#emoji-creator`. Existing search/section-collapse logic is paused
    // implicitly because none of those elements receive layout.
    main.querySelectorAll('.emoji-section').forEach(el => {
        el.style.display = showCreator ? 'none' : '';
    });
    creator.hidden = !showCreator;
}

function _pcRevokeBlobUrls() {
    if (_pc.logoBlobUrl) {
        try { URL.revokeObjectURL(_pc.logoBlobUrl); } catch (_e) {}
        _pc.logoBlobUrl = '';
    }
    for (const e of _pc.emojis) {
        if (e.blobUrl) {
            try { URL.revokeObjectURL(e.blobUrl); } catch (_e) {}
            e.blobUrl = '';
        }
    }
}

function _pcSyncDom() {
    document.getElementById('emoji-creator-name').value = _pc.name;
    document.getElementById('emoji-creator-delete').hidden = _pc.mode !== 'edit';
    _pcRenderLogo();
    _pcRenderGrid();
}

function _pcRenderLogo() {
    const btn = document.getElementById('emoji-creator-logo');
    if (!btn) return;
    btn.innerHTML = '';
    if (_pc.logoBlobUrl) {
        // Local preview of an in-progress upload — blob: URL is safe.
        const img = document.createElement('img');
        img.alt = '';
        img.src = _pc.logoBlobUrl;
        btn.appendChild(img);
        btn.classList.add('has-image');
    } else if (_pc.logoUrl) {
        // Existing remote logo — route through the cache.
        const img = document.createElement('img');
        img.alt = '';
        bindCachedEmojiImg(img, _pc.logoUrl, 'emoji_pack_icon');
        btn.appendChild(img);
        btn.classList.add('has-image');
    } else {
        btn.innerHTML = '<span class="icon icon-image"></span>';
        btn.classList.remove('has-image');
    }
}

function _pcRenderGrid() {
    const grid = document.getElementById('emoji-creator-grid');
    const count = document.getElementById('emoji-creator-count');
    if (!grid || !count) return;
    grid.innerHTML = '';
    count.textContent = `(${_pc.emojis.length}/${PC_MAX_EMOJIS})`;

    const isMobile = typeof platformFeatures !== 'undefined' && platformFeatures.is_mobile;

    _pc.emojis.forEach((e, idx) => {
        const cell = document.createElement('div');
        cell.className = 'emoji-creator-cell';
        cell.dataset.idx = String(idx);

        const img = document.createElement('img');
        // Local blob URL (freshly-uploaded, never hit the network) is safe
        // to assign directly. Remote URLs (loaded from a saved pack) route
        // through the cache helper so the webview never fetches Blossom.
        if (e.blobUrl) {
            img.src = e.blobUrl;
        } else {
            bindCachedEmojiImg(img, e.url, 'emoji');
        }
        img.alt = `:${e.shortcode}:`;
        img.draggable = false;
        cell.appendChild(img);

        // Hover-revealed × button is desktop-only; on mobile there's no
        // hover so we surface delete through the long-press context menu
        // below instead.
        if (!isMobile) {
            const removeBtn = document.createElement('button');
            removeBtn.type = 'button';
            removeBtn.className = 'emoji-creator-cell-remove';
            removeBtn.setAttribute('aria-label', 'Remove emoji');
            removeBtn.innerHTML = '<span class="icon icon-x"></span>';
            removeBtn.addEventListener('click', (ev) => {
                ev.stopPropagation();
                _pcRemoveEmoji(idx);
            });
            cell.appendChild(removeBtn);
        }

        // JS-managed hover state. CSS `:hover` is unreliable during
        // pointer-driven drags in WKWebView — mouseleave events for
        // cells the cursor crossed during a drag never fire, leaving
        // multiple cells visually "hovered" after drop. The drag handler
        // toggles `_pcDragActive` so we don't even add the class during
        // a reorder.
        cell.addEventListener('mouseenter', () => {
            if (_pcDragActive) return;
            cell.classList.add('is-hovered');
        });
        cell.addEventListener('mouseleave', () => {
            cell.classList.remove('is-hovered');
        });

        // Edit shortcode on click — but only when the pointerdown→pointerup
        // sequence was a tap, not a drag (see _pcInstallReorderHandlers).
        cell.addEventListener('click', (ev) => {
            if (ev.target.closest('.emoji-creator-cell-remove')) return;
            if (cell.dataset.suppressClick === '1') {
                delete cell.dataset.suppressClick;
                return;
            }
            _pcRenameEmoji(idx);
        });
        cell.title = `:${e.shortcode}:`;

        // Right-click (desktop) + long-press (mobile) context menu.
        // Mobile users have no hover-× to reach, so this is the only
        // path to delete on touch. The reorder handler's drag threshold
        // (move > _PC_DRAG_THRESHOLD_PX) and the long-press tolerance
        // (8px) don't fight — moving past either cancels both gestures.
        if (typeof attachLongPressContextMenu === 'function') {
            attachLongPressContextMenu(cell, (x, y) => {
                if (typeof showContextMenu !== 'function') return;
                showContextMenu({
                    x, y,
                    items: [
                        { label: 'Rename Emoji', icon: 'edit',  onClick: () => _pcRenameEmoji(idx) },
                        { label: 'Delete Emoji', icon: 'trash', danger: true, onClick: () => _pcRemoveEmoji(idx) },
                    ],
                });
            });
        }

        _pcInstallReorderHandlers(cell, idx);
        grid.appendChild(cell);
    });

    // Bottom dropzone label adapts: first-emoji onboarding tone, normal,
    // or cap-reached.
    const dz = document.getElementById('emoji-creator-dropzone');
    if (dz) {
        const atCap = _pc.emojis.length >= PC_MAX_EMOJIS;
        dz.classList.toggle('is-disabled', atCap);
        const label = dz.querySelector('.emoji-creator-dropzone-label');
        if (atCap) {
            label.textContent = `Maximum ${PC_MAX_EMOJIS} reached`;
        } else if (_pc.emojis.length === 0) {
            label.textContent = 'Upload Emoji';
        } else {
            label.textContent = 'Upload Emoji';
        }
    }
}

function _pcSanitizeShortcode(s) {
    return String(s).replace(/[^a-zA-Z0-9_-]/g, '').slice(0, 22);
}

function _pcShortcodeFromFilename(name) {
    const base = String(name || 'emoji').replace(/\.[^.]+$/, '');
    let sc = _pcSanitizeShortcode(base);
    if (!sc) sc = 'emoji';
    const seen = new Set(_pc.emojis.map(e => e.shortcode));
    if (!seen.has(sc)) return sc;
    let i = 2;
    while (seen.has(`${sc}_${i}`)) i++;
    return `${sc}_${i}`;
}

function _pcClearDropMarkers() {
    const grid = document.getElementById('emoji-creator-grid');
    if (!grid) return;
    grid.querySelectorAll('.drop-before, .drop-after').forEach(c => {
        c.classList.remove('drop-before', 'drop-after');
    });
}

// Reorder via pointer events. We can't use the HTML5 drag API because
// Tauri's `dragDropEnabled: true` swallows DOM drag events at the native
// layer (it's needed for the OS file-drop → onDragDropEvent pipeline).
const _PC_DRAG_THRESHOLD_PX = 6;
let _pcDragActive = false;
function _pcInstallReorderHandlers(cell, idx) {
    cell.addEventListener('pointerdown', (ev) => {
        if (ev.button !== 0) return;
        if (ev.target.closest('.emoji-creator-cell-remove')) return;
        const startX = ev.clientX;
        const startY = ev.clientY;
        let dragging = false;
        let ghost = null;
        let ghostOffsetX = 0;
        let ghostOffsetY = 0;

        const onMove = (mv) => {
            if (!dragging) {
                if (Math.hypot(mv.clientX - startX, mv.clientY - startY) < _PC_DRAG_THRESHOLD_PX) return;
                dragging = true;
                _pcDragActive = true;
                cell.classList.add('is-dragging');
                // Clear any stuck hover state — we own this class now,
                // so a drag start is the right moment to normalize it.
                const grid = document.getElementById('emoji-creator-grid');
                if (grid) {
                    grid.querySelectorAll('.is-hovered').forEach(c =>
                        c.classList.remove('is-hovered'));
                }
                const rect = cell.getBoundingClientRect();
                ghost = cell.cloneNode(true);
                // Drop transient state from the clone so it reads as a static
                // preview: kill the remove button, the hover-only chrome, and
                // any nested pointer-capturing behaviour.
                ghost.classList.add('emoji-creator-cell-ghost');
                ghost.classList.remove('is-dragging');
                const ghostRemove = ghost.querySelector('.emoji-creator-cell-remove');
                if (ghostRemove) ghostRemove.remove();
                ghost.style.position = 'fixed';
                ghost.style.left = `${rect.left}px`;
                ghost.style.top = `${rect.top}px`;
                ghost.style.width = `${rect.width}px`;
                ghost.style.height = `${rect.height}px`;
                ghost.style.pointerEvents = 'none';
                ghost.style.zIndex = '2200';
                document.body.appendChild(ghost);
                // Ghost is scaled (transform: scale 0.6) around its center,
                // so centering the unscaled box on the cursor keeps the
                // visible thumbnail anchored under the pointer regardless of
                // where the user grabbed the cell.
                ghostOffsetX = rect.width / 2;
                ghostOffsetY = rect.height / 2;
            }
            if (ghost) {
                ghost.style.left = `${mv.clientX - ghostOffsetX}px`;
                ghost.style.top  = `${mv.clientY - ghostOffsetY}px`;
            }
            _pcUpdateDropTarget(mv.clientX, mv.clientY);
        };

        const onUp = (up) => {
            window.removeEventListener('pointermove', onMove);
            window.removeEventListener('pointerup', onUp);
            window.removeEventListener('pointercancel', onUp);
            if (!dragging) return;
            cell.dataset.suppressClick = '1';
            cell.classList.remove('is-dragging');
            _pcDragActive = false;
            if (ghost) ghost.remove();
            const target = _pcResolveDropTarget(up.clientX, up.clientY);
            _pcClearDropMarkers();
            if (!target) return;
            const { targetIdx, isBefore } = target;
            let to = targetIdx + (isBefore ? 0 : 1);
            if (idx === to || idx === to - 1) return;
            const [moved] = _pc.emojis.splice(idx, 1);
            if (idx < to) to--;
            if (to < 0) to = 0;
            if (to > _pc.emojis.length) to = _pc.emojis.length;
            _pc.emojis.splice(to, 0, moved);
            _pc.dirty = true;
            _pcRenderGrid();
        };

        window.addEventListener('pointermove', onMove);
        window.addEventListener('pointerup', onUp);
        window.addEventListener('pointercancel', onUp);
    });
}

function _pcResolveDropTarget(x, y) {
    const grid = document.getElementById('emoji-creator-grid');
    if (!grid) return null;
    // Scope to cells that carry an index — skips the "+" add-cell, which
    // sits in the grid but has no data-idx.
    const cells = grid.querySelectorAll('.emoji-creator-cell[data-idx]');
    if (cells.length === 0) return null;

    // Confine to the grid bounds so drops on the dropzone / footer
    // don't snap-attach to a phantom slot.
    const gridRect = grid.getBoundingClientRect();
    if (x < gridRect.left || x > gridRect.right) return null;
    if (y < gridRect.top || y > gridRect.bottom) return null;

    // Pick the cell whose centre is closest to the pointer. Covers
    // direct hits, the 4px inter-cell gutters, and inter-row gaps with
    // one rule. isBefore/after is determined by the pointer's x vs the
    // chosen cell's horizontal midpoint.
    let bestCell = null;
    let bestDist = Infinity;
    for (const c of cells) {
        const r = c.getBoundingClientRect();
        const cx = r.left + r.width / 2;
        const cy = r.top + r.height / 2;
        const dx = x - cx;
        const dy = y - cy;
        const d = dx * dx + dy * dy;
        if (d < bestDist) { bestDist = d; bestCell = c; }
    }
    if (!bestCell) return null;
    const r = bestCell.getBoundingClientRect();
    const targetIdx = parseInt(bestCell.dataset.idx, 10);
    if (Number.isNaN(targetIdx)) return null;
    return { targetIdx, isBefore: x < r.left + r.width / 2 };
}

function _pcUpdateDropTarget(x, y) {
    _pcClearDropMarkers();
    const t = _pcResolveDropTarget(x, y);
    if (!t) return;
    const grid = document.getElementById('emoji-creator-grid');
    const cell = grid.querySelector(`.emoji-creator-cell[data-idx="${t.targetIdx}"]`);
    if (!cell) return;
    cell.classList.add(t.isBefore ? 'drop-before' : 'drop-after');
}

function _pcRemoveEmoji(idx) {
    const e = _pc.emojis[idx];
    if (e && e.blobUrl) {
        try { URL.revokeObjectURL(e.blobUrl); } catch (_err) {}
    }
    // If the emoji was already on Blossom (came from a previously
    // published pack), queue its URL for cleanup at next save so the
    // file doesn't linger after the pack republishes without it.
    if (e && e.url && !e.file) {
        _pc.pendingBlobDeletes.push(e.url);
    }
    _pc.emojis.splice(idx, 1);
    _pc.dirty = true;
    _pcRenderGrid();
}

async function _pcRenameEmoji(idx) {
    const e = _pc.emojis[idx];
    if (!e) return;
    const input = document.getElementById('emoji-pack-creator-naming-input');
    if (input) input.dataset.ownIdx = String(idx);
    const next = await _pcShowNaming(
        { src: e.blobUrl || e.url, initial: e.shortcode },
        'edit',
    );
    if (input) delete input.dataset.ownIdx;
    if (next == null || next === e.shortcode) return;
    _pc.emojis[idx].shortcode = next;
    _pc.dirty = true;
    _pcRenderGrid();
}

// Accept anything tagged image/* OR with a known extension — browser
// MIME detection is unreliable for renamed files; the backend's magic-
// bytes check is the final word.
function _pcIsSupportedImage(file) {
    if (file.type && file.type.startsWith('image/')) return true;
    const name = (file.name || '').toLowerCase();
    return /\.(png|jpe?g|gif|webp)$/.test(name);
}

/** Load the file as an Image() to read its natural dimensions. Resolves
 *  with `{ ok: true, width, height }` for valid images, `{ ok: false }`
 *  when the file can't be decoded (treat as a format-reject). The blob
 *  URL is scoped to this check — revoked before resolve, so it doesn't
 *  leak into the editor's render path. */
function _pcReadImageDims(file) {
    return new Promise((resolve) => {
        const url = URL.createObjectURL(file);
        const img = new Image();
        img.onload = () => {
            const out = { ok: true, width: img.naturalWidth, height: img.naturalHeight };
            URL.revokeObjectURL(url);
            resolve(out);
        };
        img.onerror = () => {
            URL.revokeObjectURL(url);
            resolve({ ok: false });
        };
        img.src = url;
    });
}

function _pcShowSquareError() {
    _pcShowError(
        'Emojis Must Be Square!',
        'Please crop your emoji to equal width and height before uploading.',
        { title: 'Square Images Only.', buttonText: 'GOT IT' },
    );
}

async function _pcAddFiles(fileList) {
    if (!fileList || !fileList.length) return;
    let rejectedFormat = false;
    let rejectedSize = false;
    let rejectedSquare = false;
    const justAdded = [];
    for (const file of fileList) {
        if (_pc.emojis.length >= PC_MAX_EMOJIS) break;
        if (!_pcIsSupportedImage(file)) { rejectedFormat = true; continue; }
        if (file.size > PC_MAX_FILE_BYTES) { rejectedSize = true; continue; }
        // Square-only — Vector renders foreign clients' non-square
        // emojis stretched-to-fit (object-fit: fill), but we won't
        // author distorted emojis ourselves. The dim check decodes a
        // copy via Image() so animated formats (GIF / WebP) report the
        // canvas dimensions, not frame-by-frame.
        const dims = await _pcReadImageDims(file);
        if (!dims.ok) { rejectedFormat = true; continue; }
        let workingFile = file;
        if (dims.width !== dims.height) {
            // Static formats get the in-panel cropper. Animated formats
            // (GIF / animated WebP) keep the square-or-reject path
            // because the backend doesn't yet round-trip their timing.
            if (!_pcIsCroppableImage(file)) { rejectedSquare = true; continue; }
            const cropped = await _pcShowCropper(file);
            if (!cropped) continue; // user cancelled — silent skip
            workingFile = cropped;
        }
        const entry = {
            shortcode: _pcShortcodeFromFilename(workingFile.name),
            file: workingFile,
            blobUrl: URL.createObjectURL(workingFile),
        };
        _pc.emojis.push(entry);
        justAdded.push(entry);
    }
    if (justAdded.length > 0) _pc.dirty = true;
    _pcRenderGrid();
    // Reject precedence (most actionable first): size → square →
    // format. Only one overlay surfaces per batch.
    if (rejectedSize) _pcShowSizeError();
    else if (rejectedSquare) _pcShowSquareError();
    else if (rejectedFormat) _pcShowFormatError();
    if (justAdded.length > 0) _pcQueueNamingForEntries(justAdded);
}

/** Walk freshly-added emoji entries, popping the naming overlay for each.
 *  Tracks by entry reference (not index) so removal/reorder mid-queue
 *  doesn't shift the target out from under us. Skipping keeps the
 *  auto-generated shortcode the file landed with. */
async function _pcQueueNamingForEntries(entries) {
    const input = document.getElementById('emoji-pack-creator-naming-input');
    for (let i = 0; i < entries.length; i++) {
        if (!_pc.open) return; // panel closed mid-queue
        const entry = entries[i];
        const idx = _pc.emojis.indexOf(entry);
        if (idx < 0) continue; // user already removed it via the × badge
        if (input) input.dataset.ownIdx = String(idx);
        const next = await _pcShowNaming({
            src: entry.blobUrl || entry.url,
            initial: entry.shortcode,
            batch: { current: i + 1, total: entries.length },
        }, 'create');
        if (input) delete input.dataset.ownIdx;
        if (next != null && next !== entry.shortcode) {
            entry.shortcode = next;
            _pc.dirty = true;
            _pcRenderGrid();
        }
    }
}

// Native-drop bridge: Tauri's window-level `onDragDropEvent` hands us
// absolute filesystem paths, never DOM File objects, so the panel's own
// `drop` listeners never fire. `convertFileSrc` exposes each path as a
// fetchable URL — slurp the bytes into a Blob, wrap it as a File so the
// existing validator + uploader code paths stay unchanged.
async function _pcAddPaths(paths) {
    if (!Array.isArray(paths) || paths.length === 0) return;
    const { convertFileSrc } = window.__TAURI__.core;
    const files = [];
    let failed = 0;
    for (const p of paths) {
        try {
            const res = await fetch(convertFileSrc(p));
            if (!res.ok) { failed++; continue; }
            const blob = await res.blob();
            const name = p.split(/[\\/]/).pop() || 'emoji';
            // Preserve the blob's real type — defaulting to image/png would
            // mask a non-image (e.g. .toml) and bypass the format filter.
            files.push(new File([blob], name, { type: blob.type || '' }));
        } catch (e) {
            failed++;
            console.warn('[emoji-pack-creator] failed to read', p, e);
        }
    }
    // Total failure → user gets the same overlay treatment as a bad
    // format/size, so the drop never looks silently ignored.
    if (files.length === 0 && failed > 0) {
        _pcShowError('Oops! Couldn’t Read That!',
            failed === 1 ? 'The file couldn’t be opened.' :
                           `${failed} files couldn’t be opened.`);
        return;
    }
    _pcAddFiles(files);
}

function isEmojiPackCreatorOpen() {
    return _pc.open === true;
}

async function _pcSetLogoFile(file) {
    if (!file) return;
    if (!_pcIsSupportedImage(file)) { _pcShowFormatError(); return; }
    if (file.size > PC_MAX_FILE_BYTES) { _pcShowSizeError(); return; }
    // Logos must be square too — they render in the same stretch-to-fit
    // way as emojis everywhere downstream.
    const dims = await _pcReadImageDims(file);
    if (!dims.ok) { _pcShowFormatError(); return; }
    let workingFile = file;
    if (dims.width !== dims.height) {
        if (!_pcIsCroppableImage(file)) { _pcShowSquareError(); return; }
        const cropped = await _pcShowCropper(file);
        if (!cropped) return; // user cancelled
        workingFile = cropped;
    }
    if (_pc.logoBlobUrl) {
        try { URL.revokeObjectURL(_pc.logoBlobUrl); } catch (_e) {}
    }
    // If a remote logo was already published, queue its URL for Blossom
    // cleanup at next save — otherwise the replaced logo would orphan.
    if (_pc.logoUrl) {
        _pc.pendingBlobDeletes.push(_pc.logoUrl);
    }
    _pc.logoFile = workingFile;
    _pc.logoBlobUrl = URL.createObjectURL(workingFile);
    _pc.logoUrl = '';
    _pc.dirty = true;
    _pcRenderLogo();
}

// In-panel naming overlay — Promise-based so it composes with both the
// upload queue (await per-emoji) and cell-click editing. Resolves to the
// new shortcode on Save, `null` on Skip/Cancel/Esc.
let _pcNamingResolver = null;

function _pcShowNaming({ src, initial, batch }, mode = 'create') {
    return new Promise((resolve) => {
        // Late-arriving prompt while one is already open — coalesce so
        // the caller doesn't deadlock waiting on a never-resolved promise.
        // `null` is this primitive's "no decision" sentinel (skip /
        // cancel / esc all resolve to null); the user didn't actively
        // press Cancel, the prompt was superseded.
        if (_pcNamingResolver) _pcNamingResolver(null);
        _pcNamingResolver = resolve;

        const overlay  = document.getElementById('emoji-pack-creator-naming');
        const preview  = document.getElementById('emoji-pack-creator-naming-preview');
        const input    = document.getElementById('emoji-pack-creator-naming-input');
        const batchEl  = document.getElementById('emoji-pack-creator-naming-batch');
        const errEl    = document.getElementById('emoji-pack-creator-naming-error');
        const skipBtn  = document.getElementById('emoji-pack-creator-naming-skip');
        const titleEl  = document.getElementById('emoji-pack-creator-naming-title');

        // `src` is either a local blob: URL (safe — never touched the
        // network) or a remote Blossom URL (must route through the cache).
        // Clear leftover state from any previous bind on this reused
        // <img>: a stale cacheToken would let an in-flight .then from a
        // prior remote bind overwrite our blob src once it resolves,
        // and a stale `emoji-img-loading` class would paint the mint
        // placeholder over the actual emoji.
        if (typeof src === 'string' && src.startsWith('blob:')) {
            delete preview.dataset.cacheToken;
            preview.classList.remove('emoji-img-loading');
            preview.src = src;
        } else {
            bindCachedEmojiImg(preview, src, 'emoji');
        }
        input.value = initial || '';
        input.classList.remove('is-invalid');
        errEl.hidden = true;
        errEl.textContent = '';
        skipBtn.textContent = mode === 'edit' ? 'CANCEL' : 'SKIP';
        titleEl.textContent = mode === 'edit' ? 'Rename Emoji' : 'Name This Emoji';
        if (batch && batch.total > 1) {
            batchEl.textContent = `${batch.current} of ${batch.total}`;
            batchEl.hidden = false;
        } else {
            batchEl.hidden = true;
        }
        overlay.hidden = false;
        setTimeout(() => { input.focus(); input.select(); }, 30);
    });
}

function _pcNamingFinish(value) {
    const overlay = document.getElementById('emoji-pack-creator-naming');
    if (overlay) overlay.hidden = true;
    const r = _pcNamingResolver;
    _pcNamingResolver = null;
    if (r) r(value);
}

/** Display the in-panel error overlay. `opts.title` overrides the
 *  default "Please Try Again." headline (some errors aren't retryable
 *  — e.g. equipped-pack cap), and `opts.buttonText` overrides the
 *  default "TRY AGAIN" CTA so it can read "GOT IT" for accept-only
 *  states. Also forces the picker visible — callers fire this from
 *  the modal / chat preview where the picker might be closed, and a
 *  hidden picker means a hidden overlay. */
function _pcShowError(pretitle, detail, opts = {}) {
    const overlay = document.getElementById('emoji-pack-creator-error');
    if (!overlay) return;
    // Ensure the picker is visible so the in-panel overlay actually
    // surfaces. Without this, an error triggered from the deep-link
    // modal / in-chat preview would sit invisible until the user
    // opens the picker themselves.
    if (typeof picker !== 'undefined' && picker && !picker.classList.contains('visible')) {
        picker.classList.add('visible');
        picker.classList.add('emoji-picker-message-type');
    }
    const pre = document.getElementById('emoji-pack-creator-error-pretitle');
    const det = document.getElementById('emoji-pack-creator-error-detail');
    const titleEl = document.getElementById('emoji-pack-creator-error-title');
    const btn = document.getElementById('emoji-pack-creator-error-retry');
    if (pre) pre.textContent = pretitle;
    if (det) det.textContent = detail;
    if (titleEl) titleEl.textContent = opts.title || 'Please Try Again.';
    if (btn) btn.textContent = opts.buttonText || 'TRY AGAIN';
    overlay.hidden = false;
}
function _pcShowSizeError() {
    _pcShowError('Oops! File Size Exceeded!', 'File Size must be under 256Kb.');
}
function _pcShowFormatError() {
    _pcShowError('Oops! Unsupported Format!', 'Use PNG, JPG, GIF, or WebP.');
}
function _pcShowSlotFullError() {
    _pcShowError(
        'Pack Slots Full!',
        `Vector supports up to ${MAX_EQUIPPED_PACKS} equipped packs. Remove one to add another.`,
        { title: 'Remove a Pack First.', buttonText: 'GOT IT' },
    );
}
function _pcHideSizeError() {
    const overlay = document.getElementById('emoji-pack-creator-error');
    if (overlay) overlay.hidden = true;
}

/** Whether a file is eligible for the cropper. All supported image
 *  formats route through the backend: static formats decode + re-encode
 *  in place, animated formats (GIF, animated WebP) crop every frame and
 *  preserve per-frame durations. */
function _pcIsCroppableImage(file) {
    const t = (file.type || '').toLowerCase();
    return t === 'image/png'
        || t === 'image/jpeg'
        || t === 'image/jpg'
        || t === 'image/gif'
        || t === 'image/webp';
}

// Pure display-pixel floor so the crop box's "middle" stays grabbable
// (move-drag) even when the box is small. The 14px corner dots already
// eat 7px from each edge — below ~36px the dots crowd the middle out.
// This floor is display-only; source-pixel output has no minimum.
const PC_CROP_MIN_DISP = 36;

/** In-panel square cropper. Resolves to a freshly-encoded File (same
 *  mime as input) on confirm, or null on cancel. Caller is expected to
 *  have already vetted the file is `_pcIsCroppableImage`. */
function _pcShowCropper(file) {
    return new Promise((resolve) => {
        const overlay = document.getElementById('emoji-pack-creator-cropper');
        const stage   = document.getElementById('emoji-pack-creator-cropper-stage');
        const img     = document.getElementById('emoji-pack-creator-cropper-img');
        const box     = document.getElementById('emoji-pack-creator-cropper-box');
        const preview = document.getElementById('emoji-pack-creator-cropper-preview');
        const cancel  = document.getElementById('emoji-pack-creator-cropper-cancel');
        const ok      = document.getElementById('emoji-pack-creator-cropper-ok');
        if (!overlay || !stage || !img || !box || !preview || !cancel || !ok) { resolve(null); return; }

        const blobUrl = URL.createObjectURL(file);
        let srcW = 0, srcH = 0;
        // Display rect of the image inside the stage (stage-local coords).
        let imgRect = { left: 0, top: 0, width: 0, height: 0 };
        // Crop box in stage-local display pixels.
        let crop = { x: 0, y: 0, size: 0 };
        // Minimum crop edge in *display* pixels. Updates with imgRect.
        let minDisp = 0;

        const cleanupListeners = () => {
            stage.removeEventListener('pointerdown', onStageDown);
            stage.removeEventListener('pointermove', onPointerMove);
            stage.removeEventListener('pointerup',   onPointerUp);
            stage.removeEventListener('pointercancel', onPointerUp);
            box.removeEventListener('pointerdown', onBoxDown);
            for (const h of box.querySelectorAll('.epcc-handle')) {
                h.removeEventListener('pointerdown', onHandleDown);
            }
            cancel.removeEventListener('click', onCancel);
            ok.removeEventListener('click',     onOk);
            document.removeEventListener('keydown', onKey, true);
        };
        const finish = (result) => {
            cleanupListeners();
            URL.revokeObjectURL(blobUrl);
            overlay.hidden = true;
            img.removeAttribute('src');
            // Drop the preview's background-image so CSS doesn't pin
            // the (revoked) blob URL alive in the layout tree.
            preview.style.backgroundImage = '';
            ok.disabled = false;
            resolve(result);
        };
        const onCancel = () => finish(null);
        const onOk = async () => {
            // Convert display-space crop to source-pixel crop.
            const scale = srcW / imgRect.width;
            const srcX = Math.max(0, Math.round((crop.x - imgRect.left) * scale));
            const srcY = Math.max(0, Math.round((crop.y - imgRect.top)  * scale));
            const srcSize = Math.max(1, Math.min(
                srcW - srcX,
                srcH - srcY,
                Math.round(crop.size * scale),
            ));
            ok.disabled = true;
            try {
                const buf = new Uint8Array(await file.arrayBuffer());
                const out = await invoke('emoji_crop_and_reencode', {
                    input: {
                        bytes: Array.from(buf),
                        mime:  file.type,
                        x:     srcX,
                        y:     srcY,
                        w:     srcSize,
                        h:     srcSize,
                    },
                });
                const cropped = new File(
                    [new Uint8Array(out)],
                    file.name,
                    { type: file.type },
                );
                finish(cropped);
            } catch (err) {
                console.warn('[cropper] backend rejected crop:', err);
                ok.disabled = false;
                _pcShowError('Oops! Couldn’t Crop That!',
                    typeof err === 'string' ? err : 'Please try a different image.');
                finish(null);
            }
        };

        // --- pointer interaction --------------------------------------
        let mode = null;          // 'move' | 'resize'
        let start = null;         // anchor state at pointerdown
        let activePointerId = null;
        // Stage origin in viewport coords, captured once at layout time
        // so pointer math doesn't pay a getBoundingClientRect() per
        // pointermove (forces a sync layout read).
        let stageOriginX = 0, stageOriginY = 0;
        let previewSize = 48;

        const applyBox = () => {
            box.style.left   = `${crop.x}px`;
            box.style.top    = `${crop.y}px`;
            box.style.width  = `${crop.size}px`;
            box.style.height = `${crop.size}px`;
            // Live preview: scale the full source image so the cropped
            // region fills the preview chip exactly, then offset so the
            // crop's top-left lands at (0,0) of the chip. backgroundImage
            // is set once in onload — only size/position varies here.
            if (crop.size > 0 && imgRect.width > 0) {
                const k = previewSize / crop.size;
                preview.style.backgroundSize     = `${imgRect.width  * k}px ${imgRect.height * k}px`;
                preview.style.backgroundPosition = `${(imgRect.left - crop.x) * k}px ${(imgRect.top - crop.y) * k}px`;
            }
        };
        const clampToImg = () => {
            // Size first, then position.
            const maxEdge = Math.min(imgRect.width, imgRect.height);
            crop.size = Math.max(minDisp, Math.min(maxEdge, crop.size));
            crop.x = Math.max(imgRect.left, Math.min(imgRect.left + imgRect.width  - crop.size, crop.x));
            crop.y = Math.max(imgRect.top,  Math.min(imgRect.top  + imgRect.height - crop.size, crop.y));
        };

        const onBoxDown = (e) => {
            if (mode || imgRect.width === 0) return;
            // Stop propagation so the stage's own pointerdown doesn't
            // also fire and reset mode mid-gesture. Capture lives on
            // stage so subsequent moves still route correctly.
            e.stopPropagation();
            activePointerId = e.pointerId;
            try { stage.setPointerCapture(e.pointerId); } catch {}
            mode = 'move';
            const px = e.clientX - stageOriginX;
            const py = e.clientY - stageOriginY;
            start = { px, py, cx: crop.x, cy: crop.y };
            e.preventDefault();
        };
        const onStageDown = (e) => {
            if (mode || imgRect.width === 0) return;
            // Marquee: pointerdown on the stage background or the
            // image itself (NOT box/handle/preview) starts a fresh
            // resize anchored at the pointer location. Only fire when
            // the press lands inside the image rect — drawing from the
            // letterbox empty space is confusing.
            if (e.target !== stage && e.target !== img) return;
            const px = e.clientX - stageOriginX;
            const py = e.clientY - stageOriginY;
            if (px < imgRect.left || px > imgRect.left + imgRect.width)  return;
            if (py < imgRect.top  || py > imgRect.top  + imgRect.height) return;
            activePointerId = e.pointerId;
            try { stage.setPointerCapture(e.pointerId); } catch {}
            mode = 'resize';
            start = { anchorX: px, anchorY: py };
            e.preventDefault();
        };
        const onPointerMove = (e) => {
            if (!mode || e.pointerId !== activePointerId) return;
            const px = e.clientX - stageOriginX;
            const py = e.clientY - stageOriginY;
            if (mode === 'move') {
                crop.x = start.cx + (px - start.px);
                crop.y = start.cy + (py - start.py);
            } else if (mode === 'resize') {
                // Anchor stays fixed; the dragged corner tracks the
                // pointer with a 1:1 aspect lock. Size = max(|dx|, |dy|).
                // Used by both corner-handle resizes (anchor = opposite
                // corner) and marquee draws (anchor = pointerdown point).
                const ax = start.anchorX;
                const ay = start.anchorY;
                const dx = Math.abs(px - ax);
                const dy = Math.abs(py - ay);
                let size = Math.max(dx, dy);
                // Clamp size to the image extent in the direction we're
                // growing so the box never escapes the image rect.
                const maxX = (px >= ax) ? (imgRect.left + imgRect.width  - ax) : (ax - imgRect.left);
                const maxY = (py >= ay) ? (imgRect.top  + imgRect.height - ay) : (ay - imgRect.top);
                size = Math.min(size, maxX, maxY);
                size = Math.max(minDisp, size);
                crop.size = size;
                crop.x = (px >= ax) ? ax : ax - size;
                crop.y = (py >= ay) ? ay : ay - size;
            }
            clampToImg();
            applyBox();
        };
        const onPointerUp = (e) => {
            if (e.pointerId !== activePointerId) return;
            try { stage.releasePointerCapture(e.pointerId); } catch {}
            mode = null;
            start = null;
            activePointerId = null;
        };
        const onHandleDown = (e) => {
            if (mode || imgRect.width === 0) return;
            e.stopPropagation();
            activePointerId = e.pointerId;
            try { stage.setPointerCapture(e.pointerId); } catch {}
            mode = 'resize';
            const handle = e.currentTarget.dataset.handle;
            // Anchor = opposite corner of the box, in stage coords.
            const ax = (handle === 'tl' || handle === 'bl') ? (crop.x + crop.size) : crop.x;
            const ay = (handle === 'tl' || handle === 'tr') ? (crop.y + crop.size) : crop.y;
            start = { anchorX: ax, anchorY: ay };
            e.preventDefault();
        };
        const onKey = (e) => {
            // Capture-phase + stopPropagation so the picker's global
            // Escape handler (closes the whole panel) doesn't fire on
            // top of ours.
            if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                finish(null);
            } else if (e.key === 'Enter' && !ok.disabled) {
                e.preventDefault();
                e.stopPropagation();
                onOk();
            }
        };

        // --- show + layout --------------------------------------------
        img.onload = () => {
            srcW = img.naturalWidth;
            srcH = img.naturalHeight;
            // Stage size is fixed by CSS — read it after the overlay
            // is visible so getBoundingClientRect returns real px.
            // Origin is cached so pointer math doesn't sync-read layout
            // every move.
            const sr = stage.getBoundingClientRect();
            const stageW = sr.width;
            const stageH = sr.height;
            stageOriginX = sr.left;
            stageOriginY = sr.top;
            previewSize  = preview.offsetWidth || 48;
            // Letterbox-fit *inside* a small inset so the corner handles
            // (positioned at -7px from the box edge) never overflow the
            // stage and get clipped by `overflow: hidden`. Handle radius
            // is 7px + a px of breathing room.
            const HANDLE_INSET = 10;
            const fitW = Math.max(1, stageW - HANDLE_INSET * 2);
            const fitH = Math.max(1, stageH - HANDLE_INSET * 2);
            const scale = Math.min(fitW / srcW, fitH / srcH);
            const dispW = Math.round(srcW * scale);
            const dispH = Math.round(srcH * scale);
            imgRect = {
                left: Math.round((stageW - dispW) / 2),
                top:  Math.round((stageH - dispH) / 2),
                width:  dispW,
                height: dispH,
            };
            img.style.left   = `${imgRect.left}px`;
            img.style.top    = `${imgRect.top}px`;
            img.style.width  = `${imgRect.width}px`;
            img.style.height = `${imgRect.height}px`;

            // Initial crop: largest centered square inside the image.
            const initEdge = Math.min(imgRect.width, imgRect.height);
            crop.size = initEdge;
            crop.x = imgRect.left + (imgRect.width  - initEdge) / 2;
            crop.y = imgRect.top  + (imgRect.height - initEdge) / 2;
            minDisp = PC_CROP_MIN_DISP;
            // Set backgroundImage once — only size/position vary per
            // pointermove inside applyBox.
            preview.style.backgroundImage = `url("${blobUrl}")`;
            clampToImg();
            applyBox();
        };

        // Wire handlers + show.
        stage.addEventListener('pointerdown', onStageDown);
        stage.addEventListener('pointermove', onPointerMove);
        stage.addEventListener('pointerup',   onPointerUp);
        stage.addEventListener('pointercancel', onPointerUp);
        box.addEventListener('pointerdown', onBoxDown);
        for (const h of box.querySelectorAll('.epcc-handle')) {
            h.addEventListener('pointerdown', onHandleDown);
        }
        cancel.addEventListener('click', onCancel);
        ok.addEventListener('click',     onOk);
        document.addEventListener('keydown', onKey, true);

        overlay.hidden = false;
        img.src = blobUrl;
    });
}

/** In-panel confirm overlay. Generalised question modal in the same
 *  visual family as the error / naming / progress overlays. Returns a
 *  Promise that resolves to true (Continue) or false (Cancel / Esc).
 *  Use this instead of `popupConfirm` from inside the creator —
 *  popupConfirm lives outside .emoji-picker so any click on it triggers
 *  the picker's outside-click-close handler.
 *
 *  Options: { title, detail?, icon? (file name in /icons/), tone?
 *  ('default'|'danger'), confirmText?, cancelText? }. */
let _pcConfirmResolver = null;

function _pcShowConfirm(opts = {}) {
    return new Promise((resolve) => {
        // Coalesce a stale resolver so the caller can't deadlock. `false`
        // is this primitive's "no decision" sentinel — the user didn't
        // explicitly press Cancel, the prompt was just superseded.
        if (_pcConfirmResolver) _pcConfirmResolver(false);
        _pcConfirmResolver = resolve;

        const overlay  = document.getElementById('emoji-pack-creator-confirm');
        const titleEl  = document.getElementById('emoji-pack-creator-confirm-title');
        const detailEl = document.getElementById('emoji-pack-creator-confirm-detail');
        const iconEl   = document.getElementById('emoji-pack-creator-confirm-icon');
        const okBtn    = document.getElementById('emoji-pack-creator-confirm-ok');
        const cancelBtn = document.getElementById('emoji-pack-creator-confirm-cancel');

        titleEl.textContent = opts.title || 'Are you sure?';
        detailEl.textContent = opts.detail || '';
        detailEl.hidden = !opts.detail;
        if (opts.icon) {
            iconEl.src = `/icons/${opts.icon}`;
            iconEl.hidden = false;
        } else {
            iconEl.hidden = true;
        }
        okBtn.textContent = opts.confirmText || 'CONTINUE';
        cancelBtn.textContent = opts.cancelText || 'CANCEL';
        overlay.dataset.tone = opts.tone || 'default';
        overlay.hidden = false;
        setTimeout(() => { okBtn.focus(); }, 30);
    });
}

function _pcConfirmFinish(value) {
    const overlay = document.getElementById('emoji-pack-creator-confirm');
    if (overlay) overlay.hidden = true;
    const r = _pcConfirmResolver;
    _pcConfirmResolver = null;
    if (r) r(value);
}

/** Full-panel progress overlay for long ops (pack delete in particular,
 *  where one slow Blossom server can stall for ~30s and per-cell rings
 *  alone leave too much unexplained dead time). */
function _pcShowProgress(title, detail) {
    const overlay = document.getElementById('emoji-pack-creator-progress');
    const titleEl = document.getElementById('emoji-pack-creator-progress-title');
    const detailEl = document.getElementById('emoji-pack-creator-progress-detail');
    if (titleEl) titleEl.textContent = title;
    if (detailEl) detailEl.textContent = detail || '';
    if (overlay) overlay.hidden = false;
}
function _pcSetProgressDetail(detail) {
    const detailEl = document.getElementById('emoji-pack-creator-progress-detail');
    if (detailEl) detailEl.textContent = detail || '';
}
function _pcHideProgress() {
    const overlay = document.getElementById('emoji-pack-creator-progress');
    if (overlay) overlay.hidden = true;
}

/** Per-cell busy state painter. State: 'pending' | 'uploading' | 'deleting'
 *  | null (clear). Cells are referenced by their current data-idx in the
 *  DOM. Renders a dimmed overlay + progress ring without disturbing the
 *  underlying cell DOM, so a parallel re-render (e.g. shortcode tweak)
 *  doesn't strand the overlay. */
function _pcSetCellBusy(idx, state) {
    const grid = document.getElementById('emoji-creator-grid');
    if (!grid) return;
    const cell = grid.querySelector(`.emoji-creator-cell[data-idx="${idx}"]`);
    if (!cell) return;
    let overlay = cell.querySelector('.emoji-creator-cell-busy');
    if (!state) {
        if (overlay) overlay.remove();
        return;
    }
    if (!overlay) {
        overlay = document.createElement('div');
        overlay.className = 'emoji-creator-cell-busy';
        overlay.innerHTML = '<div class="emoji-creator-cell-busy-ring"></div>';
        cell.appendChild(overlay);
    }
    overlay.classList.remove('is-pending', 'is-uploading', 'is-deleting');
    overlay.classList.add(`is-${state}`);
}

function _pcClearAllBusy() {
    const grid = document.getElementById('emoji-creator-grid');
    if (!grid) return;
    grid.querySelectorAll('.emoji-creator-cell-busy').forEach(o => o.remove());
}

async function _pcUploadFile(file, kind = 'emoji') {
    const buf = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buf));
    return invoke('emoji_pack_upload_image', {
        bytes,
        mime: file.type || 'application/octet-stream',
        kind,
    });
}

/** Persist current state to relays + DB. Called on exit when dirty,
 *  not on every keystroke (would publish a new kind 30030 per stroke). */
async function _pcSave() {
    if (_pc.saving) return;
    // .slice(0, 26) catches legacy packs whose titles predate the 26-char
    // cap — the input's maxlength only constrains new typing, not values
    // we hydrated into the field from an existing pack.
    const name = (document.getElementById('emoji-creator-name').value || '').trim().slice(0, 26);
    if (!name || !_pc.emojis.length) {
        // Empty pack — drop the in-progress edit silently. Better than
        // publishing a useless empty/no-name set the user clearly bailed on.
        _pc.dirty = false;
        return;
    }

    const seenCodes = new Set();
    const sanitized = [];
    _pc.emojis.forEach((e, originalIdx) => {
        const sc = _pcSanitizeShortcode(e.shortcode);
        if (!sc || seenCodes.has(sc)) return;
        seenCodes.add(sc);
        // Preserve the original _pc.emojis index so the upload loop can
        // paint the cell's busy state without searching by reference.
        sanitized.push({ ...e, shortcode: sc, originalIdx });
    });
    if (!sanitized.length) { _pc.dirty = false; return; }

    // Defense-in-depth: only forward `editingIdentifier` when the matching
    // pack is still in `arrEmojiPacks` AND owned by the current user.
    // Protects against a malformed pack list (or stale state after an
    // account swap) tricking publish into overwriting a stranger's pack.
    let safeIdentifier = null;
    if (_pc.editingId && _pc.editingIdentifier) {
        const owned = arrEmojiPacks.find(p =>
            p.id === _pc.editingId && p.is_own === true);
        if (owned && owned.identifier === _pc.editingIdentifier) {
            safeIdentifier = _pc.editingIdentifier;
        }
    }

    _pc.saving = true;
    _pcSetSavingChrome(true);

    // Pre-paint queue: every entry that still needs an upload (has a
    // local File, no remote URL yet) gets a "pending" overlay so the
    // user sees the batch lined up before the first upload completes.
    for (const e of sanitized) {
        if (e.file) _pcSetCellBusy(e.originalIdx, 'pending');
    }

    try {
        let logoUrl = _pc.logoUrl || '';
        if (_pc.logoFile) logoUrl = await _pcUploadFile(_pc.logoFile, 'emoji_pack_icon');

        const emojis = [];
        for (const e of sanitized) {
            let url = e.url;
            if (e.file) {
                _pcSetCellBusy(e.originalIdx, 'uploading');
                url = await _pcUploadFile(e.file, 'emoji');
                _pcSetCellBusy(e.originalIdx, null);
            }
            if (!url) continue;
            emojis.push({ shortcode: e.shortcode, url });
        }

        await invoke('emoji_pack_create', {
            input: {
                identifier: safeIdentifier,
                title: name,
                image_url: logoUrl || null,
                description: null,
                emojis,
            },
        });

        // Pack is published — now drain any blob-cleanup queue (emojis
        // removed via × badge, or a replaced logo). Parallel since these
        // are best-effort and the cells are already gone from the UI.
        // Cleared only on success; a failed publish leaves the queue
        // intact so the next save attempt still gets to clean up.
        // Fire-and-forget: a single hung Blossom DELETE shouldn't freeze
        // the creator with `_pc.saving=true`. The queue has been moved
        // out of `_pc.pendingBlobDeletes` so re-entrance is safe. Also
        // evict each URL from the emoji cache memo on success so a stale
        // local path can't outlive its deleted Blossom file.
        if (_pc.pendingBlobDeletes.length > 0) {
            const queue = _pc.pendingBlobDeletes.slice();
            _pc.pendingBlobDeletes = [];
            Promise.allSettled(queue.map(async url => {
                try {
                    await invoke('emoji_pack_delete_blob', { url });
                    _emojiCacheMemo.delete(url);
                } catch (err) {
                    console.warn('[emoji-pack-creator] orphan blob delete:', err);
                }
            })).catch(() => { /* allSettled never rejects */ });
        }

        await loadEmojiPacks();
        _pc.dirty = false;
    } catch (e) {
        console.warn('[emoji-pack-creator] save failed:', e);
        // Leave dirty=true so the next close-attempt retries; user can
        // also tweak something and exit again.
    } finally {
        // Clear any lingering busy overlays — success or failure, the
        // upload phase is over.
        _pcClearAllBusy();
        _pc.saving = false;
        _pcSetSavingChrome(false);
    }
}

function _pcSetSavingChrome(on) {
    const done = document.getElementById('emoji-creator-done');
    if (done) {
        done.disabled = on;
        // Swap the Save-and-exit pencil for a spinner while the publish
        // is in flight. The inner span gets re-classed in place so the
        // button keeps its size + listener wiring.
        const iconSpan = done.querySelector('.icon');
        if (iconSpan) {
            iconSpan.classList.toggle('icon-edit', !on);
            iconSpan.classList.toggle('emoji-creator-done-spinner', on);
        }
    }
    const del = document.getElementById('emoji-creator-delete');
    if (del) del.disabled = on;
    const name = document.getElementById('emoji-creator-name');
    if (name) name.disabled = on;
    const dz = document.getElementById('emoji-creator-dropzone');
    if (dz) dz.classList.toggle('is-disabled', on || _pc.emojis.length >= PC_MAX_EMOJIS);
    // Freeze cell interactions during the save: drag-to-reorder, hover
    // ×, click-to-rename — all paused so the publish flight can't be
    // raced by an edit. CSS owns the visual + pointer-events block via
    // the `.is-saving` class on the grid.
    const grid = document.getElementById('emoji-creator-grid');
    if (grid) grid.classList.toggle('is-saving', on);
}

async function _pcDelete() {
    if (!_pc.editingId || _pc.saving) return;
    // In-panel confirm — popupConfirm lives outside .emoji-picker and
    // any click on it would trip the outside-close handler, snapping
    // the picker shut mid-flow.
    const ok = await _pcShowConfirm({
        title: 'Delete This Pack?',
        detail: 'This action permanently removes the emoji files from your media servers and the pack from Nostr. This action cannot be undone.',
        icon: 'vector_warning.svg',
        tone: 'danger',
        confirmText: 'DELETE',
    });
    if (!ok) return;
    _pc.saving = true;
    _pcSetSavingChrome(true);

    // Snapshot URLs to delete before any state mutation. We use the
    // current _pc.emojis (what's on screen) rather than arrEmojiPacks
    // because the user may have removed cells in this edit session that
    // haven't been re-published yet — those files should still die.
    const cellOps = _pc.emojis
        .map((e, idx) => ({ idx, url: e.url }))
        .filter(op => Boolean(op.url));
    const logoUrl = _pc.logoUrl || '';
    // Any URLs queued for cleanup (× removals + replaced logo) get
    // swept here too so a "delete pack" run after an unsaved edit still
    // tears down those orphans.
    const pendingExtras = _pc.pendingBlobDeletes.slice();
    _pc.pendingBlobDeletes = [];

    // Full-panel overlay survives the whole flow so the user gets a
    // continuous "Deleting…" signal even while a single slow Blossom
    // server hangs the per-cell ring for ~30s.
    const totalBlobs = cellOps.length + (logoUrl ? 1 : 0) + pendingExtras.length;
    _pcShowProgress('Deleting Pack', totalBlobs > 0
        ? `Removing ${totalBlobs} file${totalBlobs === 1 ? '' : 's'} from media servers…`
        : 'Removing from Nostr…');

    try {
        // Layer 1 — Blossom blob deletes. Sequential so each cell's
        // ring spins for the duration of its actual request, giving the
        // user a real (not faked) progress signal.
        let done = 0;
        // Evict each URL from the JS memo on a successful Blossom delete
        // so the memo can't outlive its server-side data. Local cached
        // files persist (Rust only deletes Blossom-side), so subsequent
        // renders re-IPC to `get_or_cache_image` which returns the still-
        // valid local path — minor cost for cleaner bookkeeping.
        for (const op of cellOps) {
            done++;
            _pcSetProgressDetail(`Deleting emoji ${done} of ${totalBlobs} from media servers…`);
            _pcSetCellBusy(op.idx, 'deleting');
            try {
                await invoke('emoji_pack_delete_blob', { url: op.url });
                _emojiCacheMemo.delete(op.url);
            } catch (err) { console.warn('[emoji-pack-creator] blob delete:', err); }
            _pcSetCellBusy(op.idx, null);
        }
        // Logo file (best-effort, no per-cell anchor).
        if (logoUrl) {
            done++;
            _pcSetProgressDetail(`Deleting pack icon (${done} of ${totalBlobs})…`);
            try {
                await invoke('emoji_pack_delete_blob', { url: logoUrl });
                _emojiCacheMemo.delete(logoUrl);
            } catch (err) { console.warn('[emoji-pack-creator] logo delete:', err); }
        }
        // Drain any orphan URLs queued before the user pressed Delete.
        if (pendingExtras.length > 0) {
            _pcSetProgressDetail(`Cleaning up ${pendingExtras.length} orphan file${pendingExtras.length === 1 ? '' : 's'}…`);
            const results = await Promise.allSettled(pendingExtras.map(url =>
                invoke('emoji_pack_delete_blob', { url })));
            results.forEach((r, i) => {
                if (r.status === 'fulfilled') _emojiCacheMemo.delete(pendingExtras[i]);
            });
        }
        // Layer 2 — Nostr tombstone + local DB cleanup + 10030 republish.
        _pcSetProgressDetail('Removing pack from Nostr…');
        await invoke('emoji_pack_delete', { id: _pc.editingId });
        _pcSetProgressDetail('Refreshing your packs…');
        await loadEmojiPacks();
        _pc.dirty = false;
        _pc.open = false;
        _pcHideProgress();
        _pcShowView(false);
        _pcRevokeBlobUrls();
    } catch (e) {
        console.warn('[emoji-pack-creator] delete failed:', e);
        _pcHideProgress();
    } finally {
        _pcClearAllBusy();
        _pc.saving = false;
        _pcSetSavingChrome(false);
    }
}

// ----- Wire up event listeners once at module load --------------------------
(function _initPackCreator() {
    const root = document.getElementById('emoji-creator');
    if (!root) return;

    const nameInput = document.getElementById('emoji-creator-name');
    nameInput.addEventListener('input', () => {
        _pc.name = nameInput.value;
        _pc.dirty = true;
    });

    // Logo upload
    const logoBtn = document.getElementById('emoji-creator-logo');
    const logoInput = document.getElementById('emoji-creator-logo-input');
    logoBtn.addEventListener('click', () => logoInput.click());
    logoInput.addEventListener('change', (e) => {
        const file = e.target.files && e.target.files[0];
        if (file) _pcSetLogoFile(file);
        logoInput.value = '';
    });

    // Bottom dropzone + drag/drop + file picker.
    const dz = document.getElementById('emoji-creator-dropzone');
    const filesInput = document.getElementById('emoji-creator-files');
    dz.addEventListener('click', () => {
        if (dz.classList.contains('is-disabled')) return;
        filesInput.click();
    });
    filesInput.addEventListener('change', (e) => {
        _pcAddFiles(e.target.files);
        filesInput.value = '';
    });
    dz.addEventListener('dragover', (e) => {
        e.preventDefault();
        dz.classList.add('is-dragover');
    });
    dz.addEventListener('dragleave', () => dz.classList.remove('is-dragover'));
    dz.addEventListener('drop', (e) => {
        e.preventDefault();
        dz.classList.remove('is-dragover');
        if (dz.classList.contains('is-disabled')) return;
        _pcAddFiles(e.dataTransfer && e.dataTransfer.files);
    });

    // Done (top-right pencil) saves + exits.
    document.getElementById('emoji-creator-done').addEventListener('click', closeEmojiPackCreator);
    document.getElementById('emoji-creator-delete').addEventListener('click', _pcDelete);

    const errBtn = document.getElementById('emoji-pack-creator-error-retry');
    if (errBtn) errBtn.addEventListener('click', _pcHideSizeError);

    // Naming overlay wiring.
    const namingInput = document.getElementById('emoji-pack-creator-naming-input');
    const namingErr   = document.getElementById('emoji-pack-creator-naming-error');
    const namingSave  = document.getElementById('emoji-pack-creator-naming-save');
    const namingSkip  = document.getElementById('emoji-pack-creator-naming-skip');
    const tryCommit = () => {
        const raw = namingInput.value;
        const sc = _pcSanitizeShortcode(raw);
        if (!sc) {
            namingInput.classList.add('is-invalid');
            namingErr.textContent = 'Letters, numbers, and underscores only.';
            namingErr.hidden = false;
            return;
        }
        // Reject collisions with other emojis in this pack — saving an
        // identical shortcode would silently drop one of them at publish
        // (_pcSave dedups). Caller passes its own index via dataset so we
        // can let the user "keep their own" without flagging it.
        const ownIdx = namingInput.dataset.ownIdx;
        const conflict = _pc.emojis.some((e, i) =>
            String(i) !== ownIdx && _pcSanitizeShortcode(e.shortcode) === sc);
        if (conflict) {
            namingInput.classList.add('is-invalid');
            namingErr.textContent = `:${sc}: is already used in this pack.`;
            namingErr.hidden = false;
            return;
        }
        _pcNamingFinish(sc);
    };
    namingInput.addEventListener('input', () => {
        namingInput.classList.remove('is-invalid');
        namingErr.hidden = true;
    });
    namingInput.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') { e.preventDefault(); tryCommit(); }
        else if (e.key === 'Escape') { e.preventDefault(); _pcNamingFinish(null); }
    });
    namingSave.addEventListener('click', tryCommit);
    namingSkip.addEventListener('click', () => _pcNamingFinish(null));

    // Confirm overlay wiring. Enter / Escape route through the active
    // resolver so the in-panel modal feels native — and only when the
    // overlay is the one expecting input (resolver present).
    const confirmOk     = document.getElementById('emoji-pack-creator-confirm-ok');
    const confirmCancel = document.getElementById('emoji-pack-creator-confirm-cancel');
    confirmOk.addEventListener('click', () => _pcConfirmFinish(true));
    confirmCancel.addEventListener('click', () => _pcConfirmFinish(false));
    document.addEventListener('keydown', (e) => {
        if (!_pcConfirmResolver) return;
        if (e.key === 'Enter') { e.preventDefault(); _pcConfirmFinish(true); }
        else if (e.key === 'Escape') { e.preventDefault(); _pcConfirmFinish(false); }
    });
})();

function _handlePackEmojiSelect(pack, emoji) {
    if (!emoji) return;
    bumpCustomEmojiUsage(emoji.shortcode);
    if (strCurrentReactionReference) {
        const literal = `:${emoji.shortcode}:`;
        if (!_userAlreadyReacted(literal)) {
            _sendCustomEmojiReaction(emoji.shortcode, emoji.url);
        }
        picker.classList.remove('visible');
        return;
    }
    insertAtCursor(`:${emoji.shortcode}:`, true);
    picker.classList.remove('visible');
    if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
        domChatMessageInput.focus();
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

        // Local decoy chip — shows the image right away. The renderer
        // for inbound reactions picks up the `emoji` tag and replaces
        // the `:shortcode:` text similarly.
        const spanReaction = document.createElement('span');
        spanReaction.classList.add('reaction');
        spanReaction.dataset.emoji = `:${shortcode}:`;
        spanReaction.dataset.emojiUrl = url;
        spanReaction.dataset.msgId = strCurrentReactionReference;
        spanReaction.dataset.reacted = 'true';
        const img = document.createElement('img');
        img.alt = `:${shortcode}:`;
        img.className = 'reaction-custom-emoji';
        spanReaction.appendChild(img);
        spanReaction.appendChild(document.createTextNode(' 1'));
        // Deleted/404 custom emoji → fall back to a twemoji'd question mark
        // so the reaction chip stays a recognisable glyph, not a gap.
        bindCachedEmojiImg(img, url, 'emoji', (el) => {
            el.replaceWith(document.createTextNode('❓'));
            twemojify(spanReaction);
        });

        const divMessage = document.getElementById(cMsg.id);
        _dmsgInjectReaction(divMessage, spanReaction);

        reactToMessageRouted(strCurrentReactionReference, strReceiverPubkey, `:${shortcode}:`, url);
    }
}

function renderEmojiPackSections() {
    const main = document.querySelector('.emoji-main');
    if (!main) return;
    main.querySelectorAll('.emoji-pack-section').forEach(el => el.remove());
    _destroyAllPackCanvasGrids();

    // Pack sections slot between Favorites and All so custom emojis read
    // as a promoted tier, even though their sidebar tabs sit at the bottom.
    const anchor = document.getElementById('emoji-all');

    for (const pack of arrEmojiPacks) {
        const section = document.createElement('div');
        section.className = 'emoji-section emoji-pack-section';
        section.dataset.packId = pack.id;

        const header = document.createElement('div');
        header.className = 'emoji-section-header';
        const editPencil = pack.is_own
            ? `<button type="button" class="emoji-pack-edit-pencil" data-pack-id="${_escapeAttr(pack.id)}" aria-label="Edit pack" title="Edit Pack"><span class="icon icon-edit"></span></button>`
            : '';
        // Order: [logo][title][count][collapse-arrow] ... [edit-if-own].
        // The pencil uses `margin-left: auto` (in CSS) to float right;
        // no spacer element required.
        const emojiCount = Array.isArray(pack.emojis) ? pack.emojis.length : 0;
        header.innerHTML = `<span class="header-text">${_escapeAttr(pack.title || pack.identifier)}</span><span class="emoji-pack-count">(${emojiCount})</span><span class="icon icon-chevron-down"></span>${editPencil}`;
        // Logo is a separate Element so we can bind it through the URL
        // cache (raw Blossom URL never lands on <img src>).
        if (pack.image_url) {
            const logo = document.createElement('img');
            logo.className = 'emoji-pack-logo';
            logo.alt = '';
            bindCachedEmojiImg(logo, pack.image_url, 'emoji_pack_icon');
            header.insertBefore(logo, header.firstChild);
        }
        const pencil = header.querySelector('.emoji-pack-edit-pencil');
        if (pencil) {
            pencil.addEventListener('click', (ev) => {
                ev.stopPropagation();
                openEmojiPackCreator(pack.id);
            });
        }
        // Right-click on desktop / long-press on Android, on either the
        // title text or the logo image — count, chevron, and pencil keep
        // their stock browser context menu (or their own behaviour).
        const fireMenu = (x, y) => _showPackTabMenu(pack, x, y);
        const titleEl = header.querySelector('.header-text');
        if (titleEl) attachLongPressContextMenu(titleEl, fireMenu);
        const logoEl = header.querySelector('.emoji-pack-logo');
        if (logoEl) attachLongPressContextMenu(logoEl, fireMenu);
        section.appendChild(header);

        if (PACK_CANVAS_AVAILABLE) {
            // Canvas-batched path: one compositor layer per section instead
            // of one per emoji. Activation is gated by IntersectionObserver
            // so off-screen pack sections don't tick.
            const grid = new PackCanvasGrid(pack);
            _packCanvasGrids.set(pack.id, grid);
            section.appendChild(grid.canvas);
        } else {
            // Fallback: native <img> grid (older WKWebView / WebView2).
            const grid = document.createElement('div');
            grid.className = 'emoji-grid emoji-pack-grid';
            const pendingCells = [];
            for (const emoji of pack.emojis) {
                const span = document.createElement('span');
                span.className = 'emoji-pack-emoji';
                span.dataset.packShortcode = emoji.shortcode;
                span.dataset.packUrl = emoji.url;
                span.dataset.packId = pack.id;
                span.title = `:${emoji.shortcode}:`;
                grid.appendChild(span);
                pendingCells.push({ span, emoji });
            }
            _hydrateEmojiCellsStaggered(pendingCells);
            section.appendChild(grid);
        }

        if (anchor) {
            main.insertBefore(section, anchor);
        } else {
            main.appendChild(section);
        }
    }

    // Attach visibility observers after the section is in the DOM so
    // IntersectionObserver can compute geometry against `.emoji-main`.
    if (PACK_CANVAS_AVAILABLE) {
        for (const grid of _packCanvasGrids.values()) {
            grid.attachVisibilityObserver(main);
        }
        // Arm the on-screen packs deterministically rather than waiting on the
        // IO's first callback (unreliable when this runs mid-open-transition).
        _rearmVisiblePackCanvases();
    }
}

/**
 * Renders the Recently Used emojis immediately, then renders
 * the All Emojis grid after the last recent emoji image loads.
 */
function renderEmojiPanel() {
    const recentsGrid = document.getElementById('emoji-recents-grid');
    const allGrid = document.getElementById('emoji-all-grid');

    recentsGrid.innerHTML = '';
    allGrid.innerHTML = '';

    // Build Recently Used with DocumentFragment. Stock + custom merge
    // by `.used` — frequent picks on either side rise together.
    const recentsFragment = document.createDocumentFragment();
    const stockRecents = getMostUsedEmojis().slice(0, 24);
    const customRecents = getMostUsedCustomEmojis(24);
    const mergedRecents = stockRecents.concat(customRecents)
        .sort((a, b) => (b.used || 0) - (a.used || 0))
        .slice(0, 24);

    mergedRecents.forEach(item => {
        if (item.isCustom) {
            const span = document.createElement('span');
            span.className = 'emoji-pack-emoji';
            span.dataset.packShortcode = item.shortcode;
            span.dataset.packUrl = item.url;
            span.dataset.emojiTooltip = `:${item.shortcode}:`;
            const img = document.createElement('img');
            bindCachedEmojiImg(img, item.url, 'emoji');
            img.alt = `:${item.shortcode}:`;
            span.appendChild(img);
            recentsFragment.appendChild(span);
        } else {
            const span = document.createElement('span');
            span.textContent = item.emoji;
            span.title = item.name;
            recentsFragment.appendChild(span);
        }
    });

    recentsGrid.appendChild(recentsFragment);
    twemojify(recentsGrid);

    // Find the last <img> in recents and wait for it to load
    const lastRecentImage = recentsGrid.lastElementChild?.querySelector('img');

    if (lastRecentImage) {
        lastRecentImage.addEventListener('load', () => {
            renderAllEmojisGrid(allGrid);
        }, { once: true });
    } else {
        // No recent emojis, render all grid immediately
        renderAllEmojisGrid(allGrid);
    }

    // Load favorites section
    loadFavoritesSection();
}

/**
 * Renders the full All Emojis grid with lazy twemojification.
 * All spans are created upfront (cheap text nodes) to preserve scroll height,
 * but twemojify() is only called on chunks as they scroll into view.
 * The first span of each chunk is observed — no sentinel elements needed.
 */
function renderAllEmojisGrid(allGrid) {
    // Disconnect any previous observer (handles re-opens)
    if (emojiLazyLoadObserver) {
        emojiLazyLoadObserver.disconnect();
        emojiLazyLoadObserver = null;
    }

    const allFragment = document.createDocumentFragment();
    const chunkLeaders = [];

    arrEmojis.forEach((emoji, i) => {
        const span = document.createElement('span');
        span.textContent = emoji.emoji;
        span.title = emoji.name;
        allFragment.appendChild(span);

        // Mark the first span of each chunk as the observation target
        if (i % EMOJI_CHUNK_SIZE === 0) {
            span.dataset.chunkIndex = chunkLeaders.length;
            chunkLeaders.push(span);
        }
    });

    allGrid.appendChild(allFragment);

    // Build an array of all emoji spans (excluding non-emoji children) for slicing
    const allSpans = Array.from(allGrid.querySelectorAll('span[title]'));

    // Set up IntersectionObserver on the scroll container
    const scrollContainer = document.querySelector('.emoji-main');
    emojiLazyLoadObserver = new IntersectionObserver((entries) => {
        entries.forEach(entry => {
            if (entry.isIntersecting) {
                const leader = entry.target;
                if (!leader.dataset.twemojified) {
                    leader.dataset.twemojified = '1';
                    const idx = Number(leader.dataset.chunkIndex);
                    const start = idx * EMOJI_CHUNK_SIZE;
                    const end = Math.min(start + EMOJI_CHUNK_SIZE, allSpans.length);
                    for (let i = start; i < end; i++) {
                        twemojify(allSpans[i]);
                    }
                }
                emojiLazyLoadObserver.unobserve(leader);
            }
        });
    }, {
        root: scrollContainer,
        rootMargin: '0px 0px 200px 0px'
    });

    // Observe the first span of each chunk
    chunkLeaders.forEach(leader => {
        emojiLazyLoadObserver.observe(leader);
    });
}

/**
 * Loads the favorites section separately.
 */
function loadFavoritesSection() {
    const favoritesGrid = document.getElementById('emoji-favorites-grid');
    favoritesGrid.innerHTML = '';

    const favoritesFragment = document.createDocumentFragment();
    arrFavoriteEmojis.slice(0, 24).forEach(emoji => {
        const span = document.createElement('span');
        span.textContent = emoji.emoji;
        span.title = emoji.name;
        favoritesFragment.appendChild(span);
    });

    favoritesGrid.appendChild(favoritesFragment);
    twemojify(favoritesGrid);
}

function loadEmojiSections() {
    // Legacy function - now calls renderEmojiPanel
    renderEmojiPanel();

    // Initialize collapsible sections after loading emojis
    initCollapsibleSections();
}

// Track if we've already initialized to prevent duplicates
let collapsiblesInitialized = false;

function initCollapsibleSections() {
    // `.emoji-main` is static, so this delegated listener only needs attaching
    // once — without the guard every panel open stacked another copy.
    if (collapsiblesInitialized) return;
    collapsiblesInitialized = true;
    document.querySelector('.emoji-main').addEventListener('click', (e) => {
        const header = e.target.closest('.emoji-section-header');
        if (!header) return;
        e.stopPropagation();
        const section = header.parentElement;
        section.classList.toggle('collapsed');
    });
}

// Function to reset emoji picker state
function resetEmojiPicker() {
    // Clear search input
    emojiSearch.value = '';

    // Show all sections — but don't override `hidden` attributes;
    // inline display beats the UA `[hidden]` rule, so sections we
    // deliberately hide (e.g. favorites while unimplemented) would
    // reappear after a search clear.
    document.querySelectorAll('.emoji-section').forEach(section => {
        if (section.hidden) return;
        section.style.display = 'block';
    });

    // Remove search results container
    const existingResults = document.getElementById('emoji-search-results-container');
    if (existingResults) {
        existingResults.remove();
    }
}

// Update the emoji search event listener
emojiSearch.addEventListener('input', (e) => {
    // Skip emoji search if in GIF mode (GIF search is handled separately)
    if (pickerMode === PICKER_MODE_GIF) return;

    const search = e.target.value.toLowerCase();

    if (search) {
        // Hide all sections and show search results
        document.querySelectorAll('.emoji-section').forEach(section => {
            section.style.display = 'none';
        });

        // Unified merge: both searchEmojis and searchCustomEmojis return
        // `score` baked-in with personal usage. Sort all by score so a
        // heavily-used stock or custom emoji rises identically.
        const stockResults = searchEmojis(search).filter(emoji =>
            emoji.name.toLowerCase().includes(search)
        );
        const allCustom = searchCustomEmojis(search);

        const resultsContainer = document.createElement('div');
        resultsContainer.className = 'emoji-section';
        resultsContainer.id = 'emoji-search-results-container';
        resultsContainer.innerHTML = `
            <div class="emoji-section-header">
                <span class="header-text">Search Results</span>
            </div>
            <div class="emoji-grid" id="emoji-search-results"></div>
        `;

        const existingResults = document.getElementById('emoji-search-results-container');
        if (existingResults) {
            existingResults.remove();
        }

        document.querySelector('.emoji-main').prepend(resultsContainer);

        const resultsGrid = document.getElementById('emoji-search-results');
        resultsGrid.innerHTML = '';

        const totalCap = 48;
        const merged = stockResults.concat(allCustom)
            .sort((a, b) => (a.score || 0) - (b.score || 0))
            .slice(0, totalCap);

        const renderCustomCell = (item) => {
            // `.emoji-pack-emoji` re-uses the existing pack-emoji click
            // handler, which inserts `:shortcode:` literal into the input.
            const span = document.createElement('span');
            span.className = 'emoji-pack-emoji';
            span.dataset.packShortcode = item.shortcode;
            span.dataset.packUrl = item.url;
            span.dataset.emojiTooltip = `:${item.shortcode}:`;
            const img = document.createElement('img');
            bindCachedEmojiImg(img, item.url, 'emoji');
            img.alt = `:${item.shortcode}:`;
            span.appendChild(img);
            return span;
        };
        const renderStockCell = (emoji) => {
            const span = document.createElement('span');
            span.textContent = emoji.emoji;
            span.title = emoji.name;
            return span;
        };

        for (const item of merged) {
            resultsGrid.appendChild(
                item.isCustom ? renderCustomCell(item) : renderStockCell(item),
            );
        }

        twemojify(resultsGrid);
    } else {
        resetEmojiPicker();
    }
});

// Delegated category-button click handler. Single source of truth for
// both stock tabs (rendered at parse time) and pack tabs (appended at
// runtime); the bare forEach binding only saw the stock three.
document.querySelector('.emoji-sidebar').addEventListener('click', async (e) => {
    const btn = e.target.closest('.emoji-category-btn');
    if (!btn) return;
    e.stopPropagation();

    // "+" creator tab — enter creator mode (handled in its own listener too,
    // but stopPropagation here keeps the active-tab toggle from cycling).
    if (btn.classList.contains('emoji-pack-tab-create')) return;

    // Switching out of creator view: auto-save first.
    if (_pc.open) await closeEmojiPackCreator();

    document.querySelectorAll('.emoji-category-btn').forEach(b => {
        b.classList.toggle('active', b === btn);
    });

    let section = null;
    if (btn.dataset.category) {
        section = document.getElementById(`emoji-${btn.dataset.category}`);
    } else if (btn.dataset.packId) {
        section = document.querySelector(
            `.emoji-pack-section[data-pack-id="${CSS.escape(btn.dataset.packId)}"]`,
        );
    }
    if (section) section.scrollIntoView({ behavior: 'smooth', block: 'start' });
});

/**
 * Insert text at the cursor position in the chat input
 * If no selection, inserts at cursor. If text is selected, replaces it.
 * @param {string} text - The text to insert
 * @param {boolean} autoSpace - If true, adds spaces around inserted text when adjacent to non-whitespace
 */
function insertAtCursor(text, autoSpace = false) {
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
picker.addEventListener('click', (e) => {
    const span = e.target.closest('.emoji-pack-emoji');
    if (!span) return;
    e.stopPropagation();
    const shortcode = span.dataset.packShortcode;
    if (!shortcode) return;

    bumpCustomEmojiUsage(shortcode);
    if (strCurrentReactionReference) {
        const url = span.dataset.packUrl;
        const literal = `:${shortcode}:`;
        if (!_userAlreadyReacted(literal)) {
            _sendCustomEmojiReaction(shortcode, url);
        }
        picker.classList.remove('visible');
        return;
    }
    insertAtCursor(`:${shortcode}:`, true);
    picker.classList.remove('visible');
    if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
        domChatMessageInput.focus();
    }
}, true);

// Emoji selection handler
picker.addEventListener('click', (e) => {
    if (e.target.tagName === 'SPAN' && e.target.parentElement.classList.contains('emoji-grid')) {
        const emoji = e.target.getAttribute('title');
        const cEmoji = arrEmojis.find(e => e.name === emoji);

        if (cEmoji) {
            // Register usage
            cEmoji.used++;
            addToRecentEmojis(cEmoji);

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
                        const spanReaction = document.createElement('span');
                        spanReaction.classList.add('reaction');
                        spanReaction.dataset.emoji = cEmoji.emoji;
                        spanReaction.dataset.msgId = strCurrentReactionReference;
                        spanReaction.dataset.reacted = 'true';
                        spanReaction.textContent = `${cEmoji.emoji} 1`;
                        twemojify(spanReaction);

                        const divMessage = document.getElementById(cMsg.id);
                        _dmsgInjectReaction(divMessage, spanReaction);
                        reactToMessageRouted(strCurrentReactionReference, strReceiverPubkey, cEmoji.emoji);
                    }
                }
            } else {
                // Add to message input at cursor position (with auto-spacing)
                insertAtCursor(cEmoji.emoji, true);
            }

            // Close the picker - use class instead of inline style
            picker.classList.remove('visible');
            // Focus chat input (desktop only - mobile keyboards are disruptive)
            if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
                domChatMessageInput.focus();
            }
        }
    }
});

// When hitting Enter on the emoji search - choose the first emoji/GIF
emojiSearch.onkeydown = async (e) => {
    if ((e.code === 'Enter' || e.code === 'NumpadEnter')) {
        e.preventDefault();

        // Handle GIF mode - select first GIF
        if (pickerMode === PICKER_MODE_GIF) {
            const firstGif = document.querySelector('#gif-grid .gif-item');
            if (firstGif && firstGif.dataset.gifId) {
                selectGif(firstGif.dataset.gifId);
            }
            return;
        }

        // Find the first emoji in search results or recent emojis
        let emojiElement;
        if (emojiSearch.value) {
            emojiElement = document.querySelector('#emoji-search-results span:first-child');
        } else {
            emojiElement = document.querySelector('#emoji-recents-grid span:first-child');
        }

        if (!emojiElement) return;

        // Register the selection in the emoji-dex
        const cEmoji = arrEmojis.find(a => a.name === emojiElement.getAttribute('title'));
        if (!cEmoji) return;

        cEmoji.used++;
        addToRecentEmojis(cEmoji);

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

                // Add a 'decoy' reaction for good UX (no waiting for the network to register the reaction)
                const spanReaction = document.createElement('span');
                spanReaction.classList.add('reaction');
                spanReaction.dataset.emoji = cEmoji.emoji;
                spanReaction.dataset.msgId = strCurrentReactionReference;
                spanReaction.dataset.reacted = 'true';
                spanReaction.textContent = `${cEmoji.emoji} 1`;
                twemojify(spanReaction);

                // Inject the decoy chip into .dmsg-reactions, bumping count if the emoji
                // already exists or appending a new chip otherwise.
                const divMessage = document.getElementById(cMsg.id);
                _dmsgInjectReaction(divMessage, spanReaction);

                // Send the Reaction to the network (protocol-agnostic)
                reactToMessageRouted(strCurrentReactionReference, strReceiverPubkey, cEmoji.emoji);
            }
        } else {
            // Add to message input at cursor position (with auto-spacing)
            insertAtCursor(cEmoji.emoji, true);
        }

        // Reset the UI state - use class instead of inline style
        emojiSearch.value = '';
        picker.classList.remove('visible');
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;

        // Bring the focus back to the chat (desktop only - mobile keyboards are disruptive)
        if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
            domChatMessageInput.focus();
        }
    } else if (e.code === 'Escape') {
        // Close the Mini App launch dialog if open
        if (domMiniAppLaunchOverlay.classList.contains('active')) {
            closeMiniAppLaunchDialog();
            return;
        }

        // Close the emoji dialog - use class instead of inline style
        emojiSearch.value = '';
        picker.classList.remove('visible');
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;

        // Close the attachment panel if open
        if (domAttachmentPanel.classList.contains('visible')) {
            closeAttachmentPanel();
        }

        // Bring the focus back to the chat (desktop only - mobile keyboards are disruptive)
        if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
            domChatMessageInput.focus();
        }
    }
};

// Add contextmenu event for right-click to favorite
picker.addEventListener('contextmenu', (e) => {
    if (e.target.tagName === 'SPAN' && e.target.parentElement.classList.contains('emoji-grid')) {
        e.preventDefault();
        const emoji = e.target.textContent;
        const emojiData = arrEmojis.find(e => e.emoji === emoji);

        if (emojiData) {
            const added = toggleFavoriteEmoji(emojiData);
            if (added) {
                // Visual feedback for adding to favorites
                e.target.style.transform = 'scale(1.3)';
                e.target.style.backgroundColor = 'rgba(255, 215, 0, 0.3)';
                setTimeout(() => {
                    e.target.style.transform = '';
                    e.target.style.backgroundColor = '';
                }, 500);
            }
        }
    }
});

// Emoji selection
picker.addEventListener('click', (e) => {
    if (e.target.tagName === 'IMG') {
        // Register the click in the emoji-dex
        const cEmoji = arrEmojis.find(a => a.emoji === e.target.alt);
        if (!cEmoji) return; // not a stock emoji IMG (pack image, logo, etc.)
        cEmoji.used++;

        // If this is a Reaction - let's send it! (Skip if the user has
        // already reacted with this emoji; one reaction per emoji per author.)
        if (strCurrentReactionReference && !_userAlreadyReacted(cEmoji.emoji)) {
            // Grab the referred message to find it's chat pubkey
            for (const cChat of arrChats) {
                const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
                if (!cMsg) continue;

                // Found the message!
                const strReceiverPubkey = cChat.id;

                // Add a 'decoy' reaction for good UX (no waiting for the network to register the reaction)
                const spanReaction = document.createElement('span');
                spanReaction.classList.add('reaction');
                spanReaction.dataset.emoji = cEmoji.emoji;
                spanReaction.dataset.msgId = strCurrentReactionReference;
                spanReaction.dataset.reacted = 'true';
                spanReaction.textContent = `${cEmoji.emoji} 1`;
                twemojify(spanReaction);

                // Inject the decoy chip into .dmsg-reactions, bumping count if the emoji
                // already exists or appending a new chip otherwise.
                const divMessage = document.getElementById(cMsg.id);
                _dmsgInjectReaction(divMessage, spanReaction);

                // Send the Reaction to the network (protocol-agnostic)
                reactToMessageRouted(strCurrentReactionReference, strReceiverPubkey, cEmoji.emoji);
            }
        }
    }
});

// ==================== GIF PICKER ====================

const gifPickerContent = document.querySelector('.gif-picker-content');
const emojiPickerContent = document.querySelector('.emoji-picker-content');
const gifGrid = document.getElementById('gif-grid');
const gifLoading = document.getElementById('gif-loading');
const pickerModeButtons = document.querySelectorAll('.picker-mode-btn');

/** Picker mode enum for fast comparison */
const PICKER_MODE_EMOJI = 0;
const PICKER_MODE_GIF = 1;

/** Current picker mode */
let pickerMode = PICKER_MODE_EMOJI;

/** GIF search debounce timer */
let gifSearchTimeout = null;

/** Track if trending GIFs have been loaded */
let trendingGifsLoaded = false;

/** IntersectionObserver for lazy loading GIF previews */
let gifLazyLoadObserver = null;

/** Cached trending GIFs data and timestamp */
let cachedTrendingGifs = null;
let cachedTrendingTimestamp = 0;
const GIF_CACHE_TTL = 5 * 60 * 1000; // 5 minutes

/** Maximum number of cached search queries (LRU eviction) */
const GIF_SEARCH_CACHE_MAX_SIZE = 10;

/** Cached search results (LRU-style) */
const gifSearchCache = new Map();

/** GIFGalaxy API base URL */
const GIF_API_BASE = 'https://gifverse.net';

/** Preconnect link element for GIFGalaxy */
let gifPreconnectLink = null;

/** Pagination state for GIFs */
// Use smaller page size on macOS (WebKit) due to autoplay limitations
const gifPageSize = navigator.userAgent.includes('Mac') && !navigator.userAgent.includes('Firefox') ? 6 : 12;
let gifCurrentOffset = 0;
let gifHasMore = true;
let gifIsLoadingMore = false;
let gifCurrentMode = 'trending'; // 'trending' or 'search'
let gifCurrentQuery = '';

/**
 * AbortController for the in-flight GIF request (trending / search / load-more).
 * Each new request aborts the previous so a slow trending response can't
 * overwrite a faster search response, and a slow search response for an old
 * query can't overwrite the current one. Severs the underlying HTTP request,
 * which also frees bandwidth on slow connections.
 */
let gifFetchController = null;

/**
 * Shows skeleton/ghost placeholders in the GIF grid while loading
 * @param {number} count - Number of skeleton items to show
 */
function showGifSkeletons(count) {
    gifGrid.innerHTML = '';
    const fragment = document.createDocumentFragment();
    for (let i = 0; i < count; i++) {
        const skeleton = document.createElement('div');
        skeleton.className = 'gif-item gif-skeleton';
        fragment.appendChild(skeleton);
    }
    gifGrid.appendChild(fragment);
}

/**
 * Establish early connection to GIF API server
 * Called when opening a chat to warm up connection before user needs GIFs
 */
function preconnectGifServer() {
    if (gifPreconnectLink) return; // Already connected
    gifPreconnectLink = document.createElement('link');
    gifPreconnectLink.rel = 'preconnect';
    gifPreconnectLink.href = 'https://gifverse.net';
    gifPreconnectLink.crossOrigin = 'anonymous';
    document.head.appendChild(gifPreconnectLink);
}

/**
 * Prefetch trending GIFs in background when emoji panel opens
 * Caches the API response for instant display
 */
function prefetchTrendingGifs() {
    // Skip if cache is still fresh
    if (cachedTrendingGifs && Date.now() - cachedTrendingTimestamp < GIF_CACHE_TTL) {
        return;
    }

    // Prefetch trending in background (use dynamic page size)
    fetch(`${GIF_API_BASE}/api/v1/trending?limit=${gifPageSize}&offset=0&sort=popular`)
        .then(res => res.json())
        .then(data => {
            if (data.results && data.results.length > 0) {
                cachedTrendingGifs = data.results;
                cachedTrendingTimestamp = Date.now();
            }
        })
        .catch(() => {}); // Silently fail - this is just an optimization
}

// ===== ThumbHash Decoder (Rust Backend) =====
// Uses efficient Rust-based decoder with LRU caching
/** @type {Map<string, string>} Cache of decoded thumbhash -> data URL */
const thumbhashCache = new Map();
/** Maximum cached thumbhash entries */
const THUMBHASH_CACHE_MAX_SIZE = 200;

/**
 * Get a decoded thumbhash data URL from cache, or decode via Rust backend
 * Returns cached value synchronously if available, otherwise triggers async decode
 * @param {string} thumbhash - The base91-encoded thumbhash string
 * @returns {string|null} - Cached data URL or null (will be decoded async)
 */
function getCachedThumbhash(thumbhash) {
    if (!thumbhash) return null;
    return thumbhashCache.get(thumbhash) || null;
}

/**
 * Pre-decode thumbhashes for a batch of GIFs using the Rust backend
 * Results are cached for synchronous access during rendering
 * @param {Array<{th?: string}>} gifs - Array of GIF objects with optional thumbhash field 'th'
 */
async function predecodeThumbhashes(gifs) {
    const uncached = gifs.filter(g => g.th && !thumbhashCache.has(g.th));
    if (uncached.length === 0) return;

    // Decode in parallel (Rust backend is efficient)
    const decodePromises = uncached.map(async (gif) => {
        try {
            const dataUrl = await invoke('decode_thumbhash', {
                thumbhash: gif.th
            });
            if (dataUrl && dataUrl.startsWith('data:')) {
                // LRU eviction if cache is full
                if (thumbhashCache.size >= THUMBHASH_CACHE_MAX_SIZE) {
                    const firstKey = thumbhashCache.keys().next().value;
                    thumbhashCache.delete(firstKey);
                }
                thumbhashCache.set(gif.th, dataUrl);
            }
        } catch {
            // Silently fail - thumbhash is just for placeholder
        }
    });

    await Promise.all(decodePromises);
}

// ===== Video Format Detection =====
// Detect best supported video format for GIF previews (AV1 > WebM > MP4 > GIF)
// Also builds a fallback chain starting from the best supported format
const gifFormatFallbackChain = (() => {
    const video = document.createElement('video');
    const allFormats = [
        { ext: 'video.av1', type: 'video', test: 'video/mp4; codecs="av01.0.05M.08"' },
        { ext: 'video.webm', type: 'video', test: 'video/webm; codecs="vp9"' },
        { ext: 'video.mp4', type: 'video', test: 'video/mp4; codecs="avc1.42E01E"' },
        { ext: 'original.gif', type: 'image', test: null } // Always supported
    ];

    // Find the first supported format and build chain from there
    let startIndex = allFormats.findIndex(f =>
        f.test === null || video.canPlayType(f.test) === 'probably' || video.canPlayType(f.test) === 'maybe'
    );
    if (startIndex === -1) startIndex = allFormats.length - 1; // Fallback to GIF

    return allFormats.slice(startIndex);
})();
const gifPreviewFormat = gifFormatFallbackChain[0];

/**
 * Switches between emoji and GIF picker modes
 * @param {number} mode - PICKER_MODE_EMOJI or PICKER_MODE_GIF
 */
function setPickerMode(mode) {
    pickerMode = mode;

    // Update button states (data-mode uses strings for readability)
    const modeStr = mode === PICKER_MODE_GIF ? 'gif' : 'emoji';
    pickerModeButtons.forEach(btn => {
        btn.classList.toggle('active', btn.dataset.mode === modeStr);
    });

    if (mode === PICKER_MODE_EMOJI) {
        emojiPickerContent.style.display = '';
        gifPickerContent.style.display = 'none';
        emojiSearch.placeholder = 'Search Emojis...';
    } else {
        emojiPickerContent.style.display = 'none';
        gifPickerContent.style.display = 'flex';
        emojiSearch.placeholder = 'Search GIFs...';

        // Load trending GIFs if not already loaded
        if (!trendingGifsLoaded) {
            loadTrendingGifs();
        }
    }

    // Auto-focus search box on desktop only (mobile keyboards are intrusive)
    // Only focus if the picker is actually visible to avoid stealing focus
    if (!platformFeatures.is_mobile && picker.classList.contains('visible')) {
        emojiSearch.focus();
    }
}

/**
 * Fetches trending GIFs from GIFGalaxy API
 * Uses cached data if available and fresh
 */
async function loadTrendingGifs() {
    // Cancel any in-flight GIF request — a stale response must not overwrite
    // whatever we're about to show.
    if (gifFetchController) gifFetchController.abort();
    gifFetchController = new AbortController();
    const signal = gifFetchController.signal;

    // Reset pagination state
    gifCurrentOffset = 0;
    gifHasMore = true;
    gifIsLoadingMore = false;
    gifCurrentMode = 'trending';
    gifCurrentQuery = '';

    // Use cached data if fresh (only for first page)
    if (cachedTrendingGifs && Date.now() - cachedTrendingTimestamp < GIF_CACHE_TTL) {
        gifGrid.innerHTML = '';
        // Thumbhashes should already be cached, but ensure they are
        await predecodeThumbhashes(cachedTrendingGifs);
        if (signal.aborted) return;
        renderGifs(cachedTrendingGifs, false);
        gifCurrentOffset = cachedTrendingGifs.length;
        gifHasMore = cachedTrendingGifs.length >= gifPageSize;
        trendingGifsLoaded = true;
        gifLoading.style.display = 'none';
        return;
    }

    // Show skeleton placeholders while fetching
    showGifSkeletons(gifPageSize);

    try {
        const response = await fetch(`${GIF_API_BASE}/api/v1/trending?limit=${gifPageSize}&offset=0&sort=popular`, { signal });
        const data = await response.json();
        if (signal.aborted) return;

        if (data.results && data.results.length > 0) {
            // Cache the results
            cachedTrendingGifs = data.results;
            cachedTrendingTimestamp = Date.now();
            // Pre-decode thumbhashes before rendering
            await predecodeThumbhashes(data.results);
            if (signal.aborted) return;
            renderGifs(data.results, false);
            gifCurrentOffset = data.results.length;
            gifHasMore = data.results.length >= gifPageSize;
            trendingGifsLoaded = true;
        } else {
            showGifEmptyState('No trending GIFs found');
            gifHasMore = false;
        }
    } catch (error) {
        if (error.name === 'AbortError' || signal.aborted) return;
        console.error('[GIF] Failed to load trending:', error);
        showGifEmptyState('Failed to load GIFs');
        gifHasMore = false;
    } finally {
        // Don't hide loading if a newer request has taken over — it owns the UI.
        if (!signal.aborted) gifLoading.style.display = 'none';
    }
}

/**
 * Searches GIFs using the GIFGalaxy API
 * @param {string} query - The search query
 */
async function searchGifs(query) {
    if (!query.trim()) {
        // Reset to trending when search is cleared
        trendingGifsLoaded = false;
        return loadTrendingGifs();
    }

    // Cancel any in-flight GIF request (e.g. trending or a previous search).
    if (gifFetchController) gifFetchController.abort();
    gifFetchController = new AbortController();
    const signal = gifFetchController.signal;

    // Reset pagination state for new search
    gifCurrentOffset = 0;
    gifHasMore = true;
    gifIsLoadingMore = false;
    gifCurrentMode = 'search';
    gifCurrentQuery = query.trim();

    const cacheKey = gifCurrentQuery.toLowerCase();

    // Check cache first (only for first page)
    if (gifSearchCache.has(cacheKey)) {
        const cached = gifSearchCache.get(cacheKey);
        // Move to end (most recently used)
        gifSearchCache.delete(cacheKey);
        gifSearchCache.set(cacheKey, cached);
        gifGrid.innerHTML = '';
        // Thumbhashes should already be cached, but ensure they are
        await predecodeThumbhashes(cached);
        if (signal.aborted) return;
        renderGifs(cached, false);
        gifCurrentOffset = cached.length;
        gifHasMore = cached.length >= gifPageSize;
        return;
    }

    // Show skeleton placeholders while fetching
    showGifSkeletons(gifPageSize);

    try {
        const encodedQuery = encodeURIComponent(gifCurrentQuery);
        const response = await fetch(`${GIF_API_BASE}/api/v1/search?q=${encodedQuery}&limit=${gifPageSize}&offset=0&sort=relevant`, { signal });
        const data = await response.json();
        if (signal.aborted) return;

        if (data.results && data.results.length > 0) {
            // Cache the results (LRU eviction if > 10 entries)
            if (gifSearchCache.size >= GIF_SEARCH_CACHE_MAX_SIZE) {
                // Delete oldest entry (first key)
                const oldestKey = gifSearchCache.keys().next().value;
                gifSearchCache.delete(oldestKey);
            }
            gifSearchCache.set(cacheKey, data.results);

            // Pre-decode thumbhashes before rendering
            await predecodeThumbhashes(data.results);
            if (signal.aborted) return;
            renderGifs(data.results, false);
            gifCurrentOffset = data.results.length;
            gifHasMore = data.results.length >= gifPageSize;
        } else {
            const displayQuery = query.length > 32 ? query.slice(0, 32) + '...' : query;
            showGifEmptyState(`No GIFs found for "${displayQuery}"`);
            gifHasMore = false;
        }
    } catch (error) {
        if (error.name === 'AbortError' || signal.aborted) return;
        console.error('[GIF] Search failed:', error);
        showGifEmptyState('Search failed');
        gifHasMore = false;
    } finally {
        if (!signal.aborted) gifLoading.style.display = 'none';
    }
}

/**
 * Loads more GIFs for infinite scroll pagination
 * Appends additional results to the existing grid
 */
async function loadMoreGifs() {
    if (gifIsLoadingMore || !gifHasMore) return;
    gifIsLoadingMore = true;

    // Inherit the current request controller so a fresh search/trending request
    // (which calls .abort()) cancels this load-more too. If no controller exists
    // yet (shouldn't happen in normal flow, but be safe), make one.
    if (!gifFetchController) gifFetchController = new AbortController();
    const signal = gifFetchController.signal;

    // Show skeleton placeholders for the new batch
    const fragment = document.createDocumentFragment();
    for (let i = 0; i < gifPageSize; i++) {
        const skeleton = document.createElement('div');
        skeleton.className = 'gif-item gif-skeleton gif-loading-more';
        fragment.appendChild(skeleton);
    }
    gifGrid.appendChild(fragment);

    try {
        let url;
        if (gifCurrentMode === 'trending') {
            url = `${GIF_API_BASE}/api/v1/trending?limit=${gifPageSize}&offset=${gifCurrentOffset}&sort=popular`;
        } else {
            const encodedQuery = encodeURIComponent(gifCurrentQuery);
            url = `${GIF_API_BASE}/api/v1/search?q=${encodedQuery}&limit=${gifPageSize}&offset=${gifCurrentOffset}&sort=relevant`;
        }

        const response = await fetch(url, { signal });
        const data = await response.json();
        if (signal.aborted) return;

        // Remove skeleton placeholders
        gifGrid.querySelectorAll('.gif-loading-more').forEach(el => el.remove());

        if (data.results && data.results.length > 0) {
            // Pre-decode thumbhashes before rendering
            await predecodeThumbhashes(data.results);
            if (signal.aborted) return;
            renderGifs(data.results, true); // Append mode
            gifCurrentOffset += data.results.length;
            gifHasMore = data.results.length >= gifPageSize;
        } else {
            gifHasMore = false;
        }
    } catch (error) {
        if (error.name === 'AbortError' || signal.aborted) return;
        console.error('[GIF] Failed to load more:', error);
        // Remove skeleton placeholders on error
        gifGrid.querySelectorAll('.gif-loading-more').forEach(el => el.remove());
        gifHasMore = false;
    } finally {
        gifIsLoadingMore = false;
    }
}

/**
 * Renders GIF items to the grid with lazy loading
 * Uses IntersectionObserver to only load visible items + one row ahead
 * @param {Array} gifs - Array of GIF data from API
 * @param {boolean} append - If true, append to existing grid instead of replacing
 */
function renderGifs(gifs, append = false) {
    const mediaUrl = `${GIF_API_BASE}/media`;

    if (!append) {
        // Clean up previous observer when replacing content
        if (gifLazyLoadObserver) {
            gifLazyLoadObserver.disconnect();
        }

        gifGrid.innerHTML = '';

        // Create IntersectionObserver for lazy loading AND play/pause management
        // Keep observing to handle play/pause when items scroll in/out of view
        gifLazyLoadObserver = new IntersectionObserver((entries) => {
            for (const entry of entries) {
                const gifItem = entry.target;

                // Load media if visible and not yet loaded
                if (entry.isIntersecting && !gifItem.dataset.mediaLoaded) {
                    loadGifMedia(gifItem, mediaUrl);
                }

                // Play/pause video based on visibility
                // Only manage videos that have already initialized (data-ready set by canplay)
                const video = gifItem.querySelector('video');
                if (video && video.dataset.ready) {
                    if (entry.isIntersecting) {
                        video.play().catch(() => {});
                    } else {
                        video.pause();
                    }
                }
            }
        }, {
            root: gifGrid,
            rootMargin: '0px 0px 200px 0px',
            threshold: 0.1
        });
    }

    const fragment = document.createDocumentFragment();

    for (const gif of gifs) {
        const gifItem = document.createElement('div');
        gifItem.className = 'gif-item';
        gifItem.dataset.gifId = gif.i;
        gifItem.dataset.gifTitle = gif.ti || '';

        // Create placeholder with thumbhash background
        const placeholder = document.createElement('div');
        placeholder.className = 'gif-placeholder';

        // Apply cached thumbhash as background (pre-decoded via Rust backend)
        if (gif.th) {
            const cachedThumbhash = getCachedThumbhash(gif.th);
            if (cachedThumbhash) {
                placeholder.style.backgroundImage = `url(${cachedThumbhash})`;
                placeholder.style.backgroundSize = 'cover';
            }
        }
        placeholder.innerHTML = '<span class="loading-spinner"></span>';
        gifItem.appendChild(placeholder);

        fragment.appendChild(gifItem);

        // Observe for lazy loading (after appending to fragment)
        gifLazyLoadObserver.observe(gifItem);
    }

    gifGrid.appendChild(fragment);
}

/**
 * Loads the media (video or image) for a GIF item when it becomes visible
 * @param {HTMLElement} gifItem - The GIF item container
 * @param {string} mediaUrl - Base URL for media
 */
function loadGifMedia(gifItem, mediaUrl) {
    // Mark as loaded to prevent re-loading
    gifItem.dataset.mediaLoaded = 'true';

    const gifId = gifItem.dataset.gifId;
    const gifTitle = gifItem.dataset.gifTitle;
    const placeholder = gifItem.querySelector('.gif-placeholder');

    // Try loading with fallback chain
    loadGifWithFallback(gifItem, mediaUrl, gifId, gifTitle, placeholder, 0);
}

/**
 * Attempts to load a GIF using the format at the given index in the fallback chain
 * On error, tries the next format in the chain
 */
function loadGifWithFallback(gifItem, mediaUrl, gifId, gifTitle, placeholder, formatIndex) {
    if (formatIndex >= gifFormatFallbackChain.length) {
        // All formats failed - show error icon
        if (placeholder) placeholder.innerHTML = '<span class="icon icon-image"></span>';
        return;
    }

    const format = gifFormatFallbackChain[formatIndex];

    if (format.type === 'video') {
        // Use video element for efficient formats
        // Use setAttribute for WebKit/Safari compatibility (properties may not work)
        const video = document.createElement('video');
        video.setAttribute('autoplay', '');
        video.setAttribute('loop', '');
        video.setAttribute('muted', '');
        video.setAttribute('playsinline', '');
        video.setAttribute('preload', 'auto');
        // Also set properties for browsers that need them
        video.muted = true;
        video.playsInline = true;
        video.autoplay = true;
        video.loop = true;

        // Use canplay event - fires when enough data to start playing
        video.addEventListener('canplay', () => {
            if (placeholder) placeholder.remove();
            video.dataset.ready = 'true'; // Mark as ready for observer to manage
            // Only auto-play if video is actually visible (not just preloaded in margin)
            // Check if element is in the visible viewport
            const rect = gifItem.getBoundingClientRect();
            const gridRect = gifGrid.getBoundingClientRect();
            const isVisible = rect.top < gridRect.bottom && rect.bottom > gridRect.top;
            if (isVisible) {
                video.play().catch(() => {});
            }
        }, { once: true });

        video.onerror = () => {
            // Try next format in the fallback chain
            video.remove();
            loadGifWithFallback(gifItem, mediaUrl, gifId, gifTitle, placeholder, formatIndex + 1);
        };

        // Set src and explicitly call load() for WebKit
        video.src = `${mediaUrl}/${gifId}/${format.ext}`;
        gifItem.appendChild(video);
        video.load();
    } else {
        // Image format (GIF)
        const img = document.createElement('img');
        img.alt = gifTitle || 'GIF';
        img.src = `${mediaUrl}/${gifId}/${format.ext}`;

        img.onload = () => {
            if (placeholder) placeholder.remove();
        };

        img.onerror = () => {
            // Try next format in the fallback chain (if any)
            img.remove();
            loadGifWithFallback(gifItem, mediaUrl, gifId, gifTitle, placeholder, formatIndex + 1);
        };

        gifItem.appendChild(img);
    }
}

/**
 * Shows an empty state message in the GIF grid
 * @param {string} message - The message to display
 */
function showGifEmptyState(message) {
    const div = document.createElement('div');
    div.className = 'gif-empty-state';
    div.style.gridColumn = '1 / -1';
    const icon = document.createElement('span');
    icon.className = 'icon icon-image';
    const text = document.createElement('span');
    text.textContent = message;
    div.appendChild(icon);
    div.appendChild(text);
    gifGrid.innerHTML = '';
    gifGrid.appendChild(div);
}

/**
 * Handles GIF selection - inserts GIF URL at cursor position
 * If input is empty, auto-sends the GIF. Otherwise, just inserts the URL.
 * @param {string} gifId - The GIF ID
 */
function selectGif(gifId) {
    const gifUrl = `${GIF_API_BASE}/media/${gifId}/original.gif`;
    const wasEmpty = !domChatMessageInput.value.trim();

    // Insert the GIF URL at cursor position (with auto-spacing)
    insertAtCursor(gifUrl, true);

    // Close the picker
    picker.classList.remove('visible');
    picker.style.bottom = '';
    domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;

    // Reset picker state
    emojiSearch.value = '';
    setPickerMode(PICKER_MODE_EMOJI);
    trendingGifsLoaded = false;

    // Focus the input (desktop only)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }

    // Auto-send only if input was empty (just the GIF)
    if (wasEmpty) {
        domChatMessageInputSend.click();
    }
}

// Mode toggle button click handlers
pickerModeButtons.forEach(btn => {
    btn.addEventListener('click', (e) => {
        e.stopPropagation();
        setPickerMode(btn.dataset.mode === 'gif' ? PICKER_MODE_GIF : PICKER_MODE_EMOJI);
    });
});

// GIF grid click handler
gifGrid.addEventListener('click', (e) => {
    e.stopPropagation(); // Prevent bubbling to emoji picker handler
    const gifItem = e.target.closest('.gif-item');
    if (gifItem && gifItem.dataset.gifId) {
        selectGif(gifItem.dataset.gifId);
    }
});

// GIF grid scroll handler for infinite scroll
gifGrid.addEventListener('scroll', () => {
    // Check if scrolled near bottom (within 100px)
    const scrollBottom = gifGrid.scrollHeight - gifGrid.scrollTop - gifGrid.clientHeight;
    if (scrollBottom < 100 && gifHasMore && !gifIsLoadingMore) {
        loadMoreGifs();
    }
});

// Modify the emoji search input to handle GIF search
const originalEmojiSearchHandler = emojiSearch.oninput;
emojiSearch.addEventListener('input', (e) => {
    if (pickerMode === PICKER_MODE_GIF) {
        // Debounce GIF search
        clearTimeout(gifSearchTimeout);
        gifSearchTimeout = setTimeout(() => {
            searchGifs(e.target.value);
        }, 300);
    }
    // Emoji search is handled by the existing handler
});

// Reset picker mode when panel closes
const originalCloseObserver = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
        if (mutation.attributeName === 'class') {
            if (!picker.classList.contains('visible')) {
                // Reset to emoji mode when panel closes
                setPickerMode(PICKER_MODE_EMOJI);
                trendingGifsLoaded = false;
            }
        }
    }
});
originalCloseObserver.observe(picker, { attributes: true });
