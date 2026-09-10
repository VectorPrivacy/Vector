// Equipped NIP-30 emoji packs: the hydrated array, the theme's pinned pack, tier limits, dead-pack facts.
// One global scope: loads before picker.js and shares its globals.

/**
 * Subscribed + owned NIP-30 emoji packs hydrated from vector-core, plus the
 * active theme's pinned pack at index 0 (see `_composeAndRenderPacks`).
 * Populated by `loadEmojiPacks()` on first picker open; subsequent
 * opens reuse the cached array while a background refresh updates it.
 * `id` is the canonical naddr (no relay hints); there is NO `addr` field.
 * `status`: 0 active, 1 revoked (creator tombstone), 2 missing (durably absent).
 * @type {Array<{id:string,title:string,image_url:string,description:string,emojis:Array<{shortcode:string,url:string}>,is_own:boolean,is_theme?:boolean,updated_at:number,status:number}>}
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
let _themeSlotAnchor = '';         // naddr the theme pack renders AFTER ('' = top); synced marker
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

/**
 * Discord-style duplicate-shortcode disambiguation. When the same `:code:`
 * appears in more than one (displayed) pack with DIFFERENT images, stamp each
 * emoji with a `dispCode` of `code~N` (1-based). The index follows a
 * lexicographic URL sort — the SAME ordering vector-core's
 * `resolve_outbound_emoji_tags` uses — so a `:love~2:` inserted here resolves
 * to the same image on send + on the recipient. Single-image codes keep their
 * bare `code`. Runs over the already-display-capped `packs`, matching the
 * backend's per-pack cap, so the indices agree exactly.
 */
/** A pack the health engine has declared dead: revoked (deterministic
 *  tombstone from its creator) or missing (durably absent from its relays).
 *  Dead packs still render as a greyed section with an explanation, but
 *  their emojis leave recents, search, autocomplete, and disambiguation. */
function packIsDead(pack) {
    return pack && (pack.status === 1 || pack.status === 2);
}

/** Explanation line for a dead pack's section. Plain language (no protocol
 *  jargon), picked deterministically per pack (not per render) so the copy
 *  doesn't shuffle every panel open. */
function deadPackMessage(pack) {
    const revoked = pack.status === 1;
    const msgs = revoked
        ? [
            'The creator has taken this pack down.',
            'This pack was removed by its creator.',
            'The creator of this pack has retired it.',
        ]
        : [
            'This pack is no longer available.',
            'This pack seems to have disappeared.',
            'This pack couldn\'t be found anymore.',
        ];
    let h = 0;
    const id = pack.id || '';
    for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
    return msgs[h % msgs.length];
}

function _assignEmojiDisambig(packs) {
    const urlsByCode = new Map(); // base code -> Set of distinct URLs
    for (const pack of packs) {
        // Dead packs don't claim `~N` slots; a live pack sharing the
        // shortcode gets its clean code back.
        if (packIsDead(pack)) continue;
        if (!Array.isArray(pack.emojis)) continue;
        for (const e of pack.emojis) {
            if (!e || !e.shortcode || !e.url) continue;
            let set = urlsByCode.get(e.shortcode);
            if (!set) { set = new Set(); urlsByCode.set(e.shortcode, set); }
            set.add(e.url);
        }
    }
    // Pre-sort the colliding codes once.
    const orderByCode = new Map();
    for (const [code, set] of urlsByCode) {
        if (set.size > 1) orderByCode.set(code, Array.from(set).sort());
    }
    for (const pack of packs) {
        if (!Array.isArray(pack.emojis)) continue;
        for (const e of pack.emojis) {
            if (!e || !e.shortcode) continue;
            const sorted = orderByCode.get(e.shortcode);
            if (!sorted) { e.dispCode = e.shortcode; continue; }
            const idx = sorted.indexOf(e.url);
            e.dispCode = idx >= 0 ? `${e.shortcode}~${idx + 1}` : e.shortcode;
        }
    }
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
            // Subscribed → a real pack already sitting at its own reorderable
            // position; no marker, drags like any other pack.
            combined = _userEmojiPacks.slice();
        } else {
            // Not subscribed → render the fetched theme pack at the theme-slot
            // marker (no Remove, doesn't use a slot). Tag it so its tab drags as
            // the marker. Until the fetch lands, just the user packs render.
            const themePack = _cachedThemePack();
            if (themePack) {
                themePack._isThemeSlot = true;
                combined = _insertAtThemeSlot(_userEmojiPacks, themePack);
            } else {
                combined = _userEmojiPacks.slice();
            }
        }
    }
    // Keep the send resolver in sync with the (non-subscribed) theme pack so
    // its emoji post as real custom emoji, not plaintext. Cheap + guarded.
    _registerThemeEmoji(_cachedThemePack());
    const sig = _packsSignature(combined);
    if (sig === _lastPacksSignature && emojiPacksLoaded) return;
    _lastPacksSignature = sig;
    arrEmojiPacks = combined;
    _assignEmojiDisambig(arrEmojiPacks);
    emojiPacksLoaded = true;
    renderEmojiPackSidebar();
    renderEmojiPackSections();
}

// Place the (non-subscribed) theme pack at the theme-slot marker: right after
// the pack whose naddr is `_themeSlotAnchor`, or at the top when the anchor is
// empty or names a pack no longer equipped.
function _insertAtThemeSlot(packs, themePack) {
    if (!_themeSlotAnchor) return [themePack, ...packs];
    const idx = packs.findIndex(p => p.id === _themeSlotAnchor);
    if (idx === -1) return [themePack, ...packs];
    return [...packs.slice(0, idx + 1), themePack, ...packs.slice(idx + 1)];
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
 *  Scaled by the effective tier (see `applyTierLimits`); these are `let` so
 *  the tier can lift them at runtime. Pure in-app gates — never used to
 *  slice the loaded/displayed pack list. */
let MAX_EQUIPPED_PACKS = 3;
/** Display-side per-pack emoji cap. Mirrors vector-core's
 *  `MAX_EMOJIS_PER_PACK` (which only constrains own packs). Shared packs
 *  with more emojis are truncated to the first N at load so picker,
 *  search index, and recent-used surfaces all see the same set. Old
 *  reactions referencing emojis past the cap still render via the
 *  per-message `emoji` tags — those don't depend on `arrEmojiPacks`.
 *  Scaled by the effective tier (see `applyTierLimits`). */
let MAX_DISPLAY_EMOJIS_PER_PACK = 30;

/** Per-effective-tier (0-3) caps, mirroring vector-core's emoji_packs tables.
 *  Tier 3 (full premium) is unlimited equipped packs. */
const EQUIPPED_PACKS_BY_TIER = [3, 6, 9, Infinity];
const EMOJIS_PER_PACK_BY_TIER = [30, 30, 60, 90];

/** Lift (or restore) the emoji-pack limits for the account's effective tier
 *  (0-3). Called at boot (get_my_badges) and on the `badges_updated` event.
 *  PC_MAX_EMOJIS is declared later in the file but hoisted, so it's safe to
 *  assign here. In-app gates + the per-pack display cap only — the pack-count
 *  load/render path is never gated on them. */
function applyTierLimits(tier) {
    tier = Math.min(Math.max(tier | 0, 0), 3);
    const newDisplayCap = EMOJIS_PER_PACK_BY_TIER[tier];
    const displayCapChanged = MAX_DISPLAY_EMOJIS_PER_PACK !== newDisplayCap;
    MAX_EQUIPPED_PACKS = EQUIPPED_PACKS_BY_TIER[tier];
    MAX_DISPLAY_EMOJIS_PER_PACK = newDisplayCap;
    PC_MAX_EMOJIS = newDisplayCap;
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

function _packsSignature(packs) {
    // The trailing `T` marks a theme-pinned entry vs. the user's own subscribed
    // copy of the same pack (identical id/updated_at/length), so toggling a
    // subscription to the active theme's pack actually repaints instead of being
    // skipped as "unchanged". `~status` is load-bearing: a health transition
    // (active/revoked/missing) changes NOTHING else — no new event exists,
    // that's what dead means — so without it the grey-out never repaints.
    return packs.map(p => `${p.id}@${p.updated_at}#${p.emojis ? p.emojis.length : 0}${p.is_theme ? 'T' : ''}~${p.status | 0}`).join('|');
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
            if (!Array.isArray(p.emojis)) continue;
            // Defensive URL trim (mirrors the backend) — a stray leading/trailing space breaks the
            // cache fetch, blanking the emoji wherever it's served from cache; clean already-loaded
            // data so it renders without waiting for a re-fetch.
            for (const e of p.emojis) {
                if (typeof e.url === 'string') e.url = e.url.trim();
            }
            if (p.emojis.length > MAX_DISPLAY_EMOJIS_PER_PACK) {
                p.emojis = p.emojis.slice(0, MAX_DISPLAY_EMOJIS_PER_PACK);
            }
        }
        _userEmojiPacks = arr;
        // Theme-slot marker position (naddr the theme pack renders after, or ''
        // for top). Synced via the kind-10030 list; read here so compose can
        // place the pinned theme pack at the user's chosen spot.
        try { _themeSlotAnchor = (await invoke('get_theme_slot_anchor')) || ''; }
        catch (_e) { _themeSlotAnchor = ''; }
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
