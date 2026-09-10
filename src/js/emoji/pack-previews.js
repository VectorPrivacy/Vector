// In-chat pack preview cards for naddr mentions, and the pack details modal they open.
// One global scope: loads before picker.js and shares its globals.

// ============================================================================
// In-chat pack preview cards
// ============================================================================

/**
 * In-memory cache of fetched pack previews keyed by the lowercased naddr.
 * Cards re-render frequently (on every chat reopen), so we keep the
 * relay fetch one-shot per naddr per session.
 */
const _packPreviewCache = new Map(); // naddr -> { state: 'loading'|'ok'|'err', pack?, error? }

// Matches an emoji-pack reference in three forms:
//   1. Vector URL:  https://vectorapp.io/emojis/pack/naddr1...  (group 1; with
//                   optional www. + optional trailing .html / slash)
//   2. NIP-21:      nostr:naddr1...                              (group 1)
//   3. Bare:        naddr1...                                    (group 3, with
//                   a required text boundary in group 2 — start/space/bracket/
//                   quote — so a naddr buried in a *foreign* URL path
//                   (njump.me/naddr1…, ditto.pub/naddr1…) is NOT yanked into a
//                   broken pack card. Lookbehind is avoided on purpose: WKWebView
//                   only gained it in Safari 16.4, and a parse-time SyntaxError
//                   would take the whole module down on older WebViews.
// Each match is replaced by the inline preview card so the user never sees the
// underlying string — the preview's Copy button is the only exposed share affordance.
const NADDR_REGEX = /(?:https?:\/\/(?:www\.)?vectorapp\.io\/emojis\/pack\/|nostr:)(naddr1[ac-hj-np-z02-9]{20,})(?:\.html)?\/?|(^|[\s([{<"'])(naddr1[ac-hj-np-z02-9]{20,})/gi;

// NIP-30 emoji sets are kind 30030; any other coordinate kind is not ours.
const KIND_EMOJI_SET = 30030;

const _naddrKindCache = new Map(); // naddr (lowercase) -> kind number | null (null = undecodable)
const _BECH32_CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';

/**
 * Decode the target kind out of a bech32 `naddr1...` (TLV type 3, 4-byte
 * big-endian). Checksum is NOT verified — a corrupt string either fails
 * decode (null) or gets rejected by the backend's strict parse; either
 * way nothing wrong is fetched. Returns null on any malformed input.
 */
function _naddrKind(naddr) {
    const key = naddr.toLowerCase();
    if (_naddrKindCache.has(key)) return _naddrKindCache.get(key);
    let kind = null;
    // Drop the "naddr1" prefix and the 6-char checksum, then regroup the
    // remaining 5-bit words into bytes and walk the TLV entries.
    const data = key.slice(6, -6);
    const bytes = [];
    let acc = 0, bits = 0, valid = data.length > 0;
    for (const ch of data) {
        const v = _BECH32_CHARSET.indexOf(ch);
        if (v === -1) { valid = false; break; }
        acc = (acc << 5) | v;
        bits += 5;
        if (bits >= 8) {
            bits -= 8;
            bytes.push((acc >> bits) & 0xff);
        }
    }
    if (valid) {
        for (let i = 0; i + 2 <= bytes.length;) {
            const t = bytes[i], len = bytes[i + 1];
            if (i + 2 + len > bytes.length) break;
            if (t === 3 && len === 4) {
                kind = ((bytes[i + 2] << 24) | (bytes[i + 3] << 16) | (bytes[i + 4] << 8) | bytes[i + 5]) >>> 0;
                break;
            }
            i += 2 + len;
        }
    }
    _naddrKindCache.set(key, kind);
    return kind;
}

/**
 * The shareable form of a pack naddr: same coordinate, plus relay hints
 * naming the PACK's home (the author's NIP-65 write relays — a subscribed
 * pack does not live on the sharer's relays). The canonical hint-less `id`
 * stays the cache/identity key; hints are minted per-share. Falls back to
 * the bare naddr on any failure.
 */
async function _shareNaddr(naddr) {
    try {
        return await window.__TAURI__.core.invoke('get_pack_share_naddr', { naddr });
    } catch {
        return naddr;
    }
}

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
        // Keep the leading boundary char (group 2 — the space/bracket/quote that
        // preceded a bare naddr); only the naddr itself is removed. Coordinates
        // pointing at any other kind stay in the text verbatim — no card is
        // rendered for them, so stripping would silently eat the content.
        .replace(NADDR_REGEX, (m, urlNaddr, boundary, bareNaddr) => {
            if (_naddrKind(urlNaddr || bareNaddr) !== KIND_EMOJI_SET) return m;
            return boundary ?? '';
        })
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
// Cards mounted per host, so a rebuilt message body can tear its cards down.
const _packPreviewCards = new WeakMap();

function renderEmojiPackPreviews(target, text) {
    if (!text) return;
    NADDR_REGEX.lastIndex = 0;
    const seen = new Set();
    let match;
    while ((match = NADDR_REGEX.exec(text)) !== null) {
        // Group 1 = URL/nostr form, group 3 = bare form.
        const naddr = (match[1] || match[3]).toLowerCase();
        if (seen.has(naddr)) continue;
        seen.add(naddr);
        // Only kind-30030 coordinates are emoji packs; anything else stays plain text.
        if (_naddrKind(naddr) !== KIND_EMOJI_SET) continue;
        const inst = VectorSvelte.mountPackPreviewCard(target, { naddr, h: _packPreviewHelpers });
        const list = _packPreviewCards.get(target) || [];
        list.push(inst);
        _packPreviewCards.set(target, list);
    }
}

/** The host that held cards is going away: release their effects and canvases. */
function destroyEmojiPackPreviews(target) {
    const list = _packPreviewCards.get(target);
    if (!list) return;
    for (const inst of list) VectorSvelte.unmountComponent(inst);
    _packPreviewCards.delete(target);
}

// The in-chat card's thumb grid: the app's canvas grid, laid out from the grid column's
// real width so a narrow card shows fewer, larger thumbs. Rebuilt when the column resizes.
function _packPreviewLayout(width) {
    const cellPx = 32, thumbPx = 28, gapPx = 4;
    // Sized to the grid's 96px clip: three rows, two where the column is too
    // narrow to be worth a third. Never fewer than four across: a cell only
    // grows to fill the row, so three would be three oversized thumbs.
    const rows = width < 240 ? 2 : 3;
    const cols = Math.max(4, Math.floor((width + gapPx) / (cellPx + gapPx)));
    return { cols, rows, cellPx, thumbPx, gapPx };
}
function _mountPackPreviewThumbs(left, pack) {
    const column = left.parentElement;
    let grid = null, key = '';
    const build = () => {
        const width = column?.clientWidth || (window.innerWidth <= 480 ? 200 : 260);
        const l = _packPreviewLayout(width);
        const next = `${l.cols}:${l.rows}`;
        if (next === key) return;
        key = next;
        grid?.destroy();
        left.replaceChildren();
        column?.classList.toggle('is-overflowing', pack.emojis.length > l.cols * (l.rows - 1));
        grid = new PackCanvasGrid(pack, {
            emojis: pack.emojis.slice(0, l.cols * l.rows), cols: l.cols,
            cellPx: l.cellPx, thumbPx: l.thumbPx, gapPx: l.gapPx,
            boxPx: 0, hoverScale: false, selectable: false, isPreview: true,
            ioRootMargin: '200px',
        });
        left.appendChild(grid.canvas);
        grid.attachVisibilityObserver(null);
    };
    build();
    const ro = column && typeof ResizeObserver !== 'undefined' ? new ResizeObserver(build) : null;
    ro?.observe(column);
    return () => { ro?.disconnect(); grid?.destroy(); };
}

const _packPreviewHelpers = {
    resolve: (naddr) => _resolvePackPreview(naddr),
    bindCachedImg: (img, url, kind) => bindCachedEmojiImg(img, url, kind),
    mountPreviewGrid: (left, pack) => _mountPackPreviewThumbs(left, pack),
    // The shareable vectorapp.io URL (a web preview elsewhere, the deep link for Vector users).
    copyShareLink: async (naddr) => {
        const shareUrl = `https://vectorapp.io/emojis/pack/${await _shareNaddr(naddr)}`;
        try { await navigator.clipboard.writeText(shareUrl); return true; }
        catch (err) { console.warn('[emoji-packs] copy share link failed:', err); return false; }
    },
    // The cap is pre-gated so the user sees actionable copy, not the backend's raw error.
    // The transient label stays on screen a beat: a local-DB toggle resolves in a few ms.
    toggle: async (naddr, pack, isSubscribed) => {
        if (!isSubscribed && _userPackCount() >= MAX_EQUIPPED_PACKS) { _pcShowSlotFullError(); return false; }
        const minDelay = new Promise(r => setTimeout(r, 350));
        try {
            const work = isSubscribed
                ? invoke('unsubscribe_emoji_pack', { id: pack.id })
                : invoke('subscribe_emoji_pack', { naddr });
            await Promise.all([work, minDelay]);
            await loadEmojiPacks();
            return true;
        } catch (e) {
            console.warn('[emoji-packs] subscribe toggle failed:', e);
            return false;
        }
    },
    // The grid resolves its canvas height well after the open-scroll: re-pin the view.
    onResized: (card) => { if (domChatMessages?.contains(card)) compensateChatScrollForResize(); },
};

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

// The modal is an island over lib/packdetails.svelte.js in its body-level overlay; this
// side fetches and closes.
function _registerPackDetails() {
    VectorSvelte.setScreen('packDetails', {
        h: {
            bindCachedImg: (img, url, kind, onUnavailable) => bindCachedEmojiImg(img, url, kind, onUnavailable),
            maxDisplay: () => MAX_DISPLAY_EMOJIS_PER_PACK,
            // The same IPCs the in-chat card uses, cap-gated on the equipped-pack limit.
            toggle: async (naddr, pack, isSub) => {
                if (!isSub && _userPackCount() >= MAX_EQUIPPED_PACKS) { _pcShowSlotFullError(); return false; }
                const minDelay = new Promise(r => setTimeout(r, 300));
                try {
                    const work = isSub
                        ? invoke('unsubscribe_emoji_pack', { id: pack.id })
                        : invoke('subscribe_emoji_pack', { naddr });
                    await Promise.all([work, minDelay]);
                    await loadEmojiPacks();
                    if (!isSub) { showToast('Pack equipped'); return 'added'; }
                    return 'removed';
                } catch (e) {
                    console.warn('[pack-details] toggle failed:', e);
                    showToast(String(e) || 'Failed');
                    return false;
                }
            },
        },
    });
}
// Registered once every script is in: the bag calls helpers from files that load later.
document.addEventListener('DOMContentLoaded', _registerPackDetails, { once: true });

async function openPackDetailsModal(naddr) {
    if (!naddr) return;
    VectorSvelte.openPackDetails(naddr);
    try {
        const pack = await invoke('fetch_emoji_pack_by_naddr', { naddr });
        VectorSvelte.resolvePackDetails(naddr, { state: 'ok', pack });
    } catch (err) {
        console.warn('[pack-details] fetch failed:', err);
        VectorSvelte.resolvePackDetails(naddr, { state: 'err', error: String(err) || 'Failed to fetch' });
    }
}

function closePackDetailsModal() {
    VectorSvelte.closePackDetails();
}
