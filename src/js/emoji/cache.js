// Emoji and pack-icon bytes come through the Rust cache: a raw Blossom URL never lands on an <img src>.
// One global scope: loads before picker.js and shares its globals.

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
const _emojiFailReason = new Map();   // url → why the cache last failed (drives the inline tap-to-explain)

/** Map the internal cache error to a friendly, accurate reason an emoji wouldn't load. */
function emojiUnavailableMessage(reason) {
    const r = (reason || '').toLowerCase();
    if (r.includes('too large')) return "This emoji is over the 1 MB size limit.<br><br>Ask the emoji creator to compress their emoji!";
    if (r.includes('404') || r.includes('not found')) return "This emoji is no longer available (it may have been deleted).";
    if (r.includes('invalid') || r.includes('corrupt')) return "This emoji file is invalid or corrupted.";
    if (r.includes('blocked')) return "This emoji was blocked for security (its host resolves to a private address).";
    return reason
        ? `This emoji couldn't be loaded (${escapeHtml(reason)}).`
        : "This emoji couldn't be loaded (the host may be offline, or the file was removed).";
}

/** A failed inline custom emoji becomes a tappable `:shortcode:` chip that explains why it's missing. */
function makeUnavailableEmojiChip(shortcode, reason) {
    const span = document.createElement('span');
    span.className = 'emoji-unavailable';
    span.textContent = `:${shortcode}:`;
    span.title = 'Emoji unavailable (tap for details)';
    span.addEventListener('click', () => {
        popupConfirm('Emoji unavailable', emojiUnavailableMessage(reason), true, '', 'vector_warning.svg');
    });
    return span;
}

function _isCacheableEmojiUrl(url) {
    return typeof url === 'string' && url.startsWith('https://');
}

/**
 * Drop every memoized emoji path and re-resolve all emoji <img>s on screen.
 * Called after the disk cache is cleared: the memos (and any rendered srcs)
 * point at deleted files, and left alone they'd short-circuit the re-download
 * forever, leaving broken images until a full reload.
 */
function reloadCachedEmojiImgs() {
    _emojiCacheMemo.clear();
    _emojiFailReason.clear();
    document.querySelectorAll('img[data-cache-token]').forEach(img => {
        bindCachedEmojiImg(img, img.dataset.cacheToken, img.dataset.cacheKind || 'emoji');
    });
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
                _emojiFailReason.set(url, String(e && e.message ? e.message : e));
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
            onUnavailable(img, _emojiFailReason.get(url) || '');
        } else {
            // Conservative default: blank the img only. Containers (pack tabs, logos, the editor's
            // logo button) must survive a failed icon. Emoji grids that should HIDE a failed cell pass
            // an explicit onUnavailable (browse grid → cell.remove; canvas panel → _compact).
            img.removeAttribute('src');
        }
    };
    if (!_isCacheableEmojiUrl(url)) {
        delete img.dataset.cacheToken;
        delete img.dataset.cacheKind;
        unavailable();
        return;
    }
    // Token guard against the re-bind race: if this same <img> gets
    // rebound to a different URL before our async resolve lands, the
    // stale `.then` would overwrite the newer src. Reused elements
    // (e.g. the naming-overlay preview cycling through a batch) are the
    // common offenders. The kind rides along so a cache-clear rebind
    // resolves into the same cache subdir.
    img.dataset.cacheToken = url;
    img.dataset.cacheKind = kind;
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
