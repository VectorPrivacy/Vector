// The GIF half of the picker: trending and search over the GIF proxy, thumbhash placeholders, format fallback.
// One global scope: loads before picker.js and shares its globals.

// ==================== GIF PICKER ====================

/** Picker mode enum for fast comparison */
const PICKER_MODE_EMOJI = 0;
const PICKER_MODE_GIF = 1;

/** Current picker mode */
let pickerMode = PICKER_MODE_EMOJI;

/** GIF search debounce timer */
let gifSearchTimeout = null;

/** Track if trending GIFs have been loaded */
let trendingGifsLoaded = false;


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
    VectorSvelte.gifLoading(count);
}

/**
 * Establish early connection to GIF API server
 * Called when opening a chat to warm up connection before user needs GIFs
 *
 * GIFs are the ONE granted exception to "the frontend never fetches remote",
 * Tor included: gifverse.net is Vector's own service, so the WebView talks
 * to it directly even while Tor is on (user-accepted tradeoff).
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

    // The panel swaps its content and placeholder from the mode.
    VectorSvelte.setPanelMode(mode === PICKER_MODE_GIF ? 'gif' : 'emoji');
    if (mode === PICKER_MODE_GIF && !trendingGifsLoaded) loadTrendingGifs();

    // Auto-focus search box on desktop only (mobile keyboards are intrusive)
    // Only focus if the picker is actually visible to avoid stealing focus
    if (!platformFeatures.is_mobile && VectorSvelte.pickerVisible()) {
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
        // Thumbhashes should already be cached, but ensure they are
        await predecodeThumbhashes(cachedTrendingGifs);
        if (signal.aborted) return;
        renderGifs(cachedTrendingGifs, false);
        gifCurrentOffset = cachedTrendingGifs.length;
        gifHasMore = cachedTrendingGifs.length >= gifPageSize;
        trendingGifsLoaded = true;
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

    VectorSvelte.gifLoadingMore(true);

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

        if (data.results && data.results.length > 0) {
            // Pre-decode thumbhashes before rendering
            await predecodeThumbhashes(data.results);
            if (signal.aborted) return;
            renderGifs(data.results, true); // Append mode
            gifCurrentOffset += data.results.length;
            gifHasMore = data.results.length >= gifPageSize;
        } else {
            gifHasMore = false;
            VectorSvelte.gifLoadingMore(false);
        }
    } catch (error) {
        if (error.name === 'AbortError' || signal.aborted) return;
        console.error('[GIF] Failed to load more:', error);
        VectorSvelte.gifLoadingMore(false);
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
    VectorSvelte.gifResults(gifs.map(gif => ({ id: gif.i, title: gif.ti || '', thumb: gif.th ? getCachedThumbhash(gif.th) : null })), append);
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
            const gridRect = _pickerEls.gif.getBoundingClientRect();
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
    VectorSvelte.gifEmpty(message);
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
    VectorSvelte.setPickerVisible(false);
    VectorSvelte.setPickerBottom('');
    VectorSvelte.setEmojiIcon('smile');

    // Reset picker state
    emojiSearch.value = '';
    setPickerMode(PICKER_MODE_EMOJI);
    trendingGifsLoaded = false;

    // Focus the input (desktop only)
    if (!platformFeatures.is_mobile) {
        if (!_emojiPanelTarget) domChatMessageInput.focus();
    }

    // Auto-send only if input was empty (just the GIF)
    if (wasEmpty) {
        VectorSvelte.composerEls().send.click();
    }
}

function _onGifGridClick(e) {
    e.stopPropagation(); // Prevent bubbling to emoji picker handler
    const gifItem = e.target.closest('.gif-item');
    if (gifItem && gifItem.dataset.gifId) {
        selectGif(gifItem.dataset.gifId);
    }
}

// Infinite scroll: near the bottom (within 100px) fetches the next page.
function _onGifGridScroll() {
    const grid = _pickerEls.gif;
    const scrollBottom = grid.scrollHeight - grid.scrollTop - grid.clientHeight;
    if (scrollBottom < 100 && gifHasMore && !gifIsLoadingMore) {
        loadMoreGifs();
    }
}
