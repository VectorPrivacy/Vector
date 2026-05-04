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
/** @type {HTMLInputElement} */
const emojiSearch = document.getElementById('emoji-search-input');
const emojiSearchIcon = document.querySelector('.emoji-search-icon');
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

        // Reset the emoji picker state first
        resetEmojiPicker();

        // Load emoji sections with optimized rendering
        renderEmojiPanel();

        // Display the picker - use class instead of inline style
        picker.classList.add('visible');

        // Always use the same fixed position (bottom-up) for both message input and reactions
        picker.classList.add('emoji-picker-message-type');

        // Position emoji picker dynamically above the chat-box (for both input and reactions)
        const chatBox = document.getElementById('chat-box');
        if (chatBox) {
            const chatBoxHeight = chatBox.getBoundingClientRect().height;
            picker.style.bottom = (chatBoxHeight + 10) + 'px'; // 10px gap above chat-box
        }

        // Clear any other positioning styles to ensure CSS fixed positioning takes effect
        picker.style.top = '';
        picker.style.left = '';
        picker.style.right = '';

        // Change the emoji button to a wink while the panel is open (only for message input)
        if (isDefaultPanel) {
            domChatMessageInputEmoji.innerHTML = `<span class="icon icon-wink-face"></span>`;
        }

        // If this is a Reaction, let's cache the Reference ID
        if (strReaction) {
            strCurrentReactionReference = strReaction;
        } else {
            strCurrentReactionReference = '';
        }

        // Focus on the emoji search box for easy searching (desktop only - mobile keyboards are disruptive)
        if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
            emojiSearch.focus();
        }

        // Prefetch GIF data in background (non-blocking)
        prefetchTrendingGifs();
    } else {
        // Hide and reset the UI - use class instead of inline style
        emojiSearch.value = '';
        picker.classList.remove('visible');
        picker.style.bottom = ''; // Reset to CSS default
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;
    }
}

let emojiLazyLoadObserver = null;
const EMOJI_CHUNK_SIZE = 36; // 6 columns x 6 rows

/**
 * Renders the Recently Used emojis immediately, then renders
 * the All Emojis grid after the last recent emoji image loads.
 */
function renderEmojiPanel() {
    const recentsGrid = document.getElementById('emoji-recents-grid');
    const allGrid = document.getElementById('emoji-all-grid');

    recentsGrid.innerHTML = '';
    allGrid.innerHTML = '';

    // Build Recently Used with DocumentFragment
    const recentsFragment = document.createDocumentFragment();
    const recentEmojis = getMostUsedEmojis().slice(0, 24);

    recentEmojis.forEach(emoji => {
        const span = document.createElement('span');
        span.textContent = emoji.emoji;
        span.title = emoji.name;
        recentsFragment.appendChild(span);
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
    if (collapsiblesInitialized) return; // Prevent duplicate initialization

    document.querySelectorAll('.emoji-section-header').forEach(header => {
        header.addEventListener('click', (e) => {
            e.stopPropagation(); // Prevent closing the picker
            const section = header.parentElement;
            section.classList.toggle('collapsed');
        });
    });

    collapsiblesInitialized = true;
}

// Function to reset emoji picker state
function resetEmojiPicker() {
    // Clear search input
    emojiSearch.value = '';

    // Restore search icon opacity
    if (emojiSearchIcon) emojiSearchIcon.style.opacity = '';

    // Show all sections
    document.querySelectorAll('.emoji-section').forEach(section => {
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
         if (emojiSearchIcon) emojiSearchIcon.style.opacity = '0';
        // Hide all sections and show search results
        document.querySelectorAll('.emoji-section').forEach(section => {
            section.style.display = 'none';
        });

        const results = searchEmojis(search);
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

        // STRICT FILTERING: Only show emojis that contain the search term in their name
        const filteredResults = results.filter(emoji =>
            emoji.name.toLowerCase().includes(search)
        );

        filteredResults.slice(0, 48).forEach(emoji => {
            const span = document.createElement('span');
            span.textContent = emoji.emoji;
            span.title = emoji.name;
            resultsGrid.appendChild(span);
        });

        twemojify(resultsGrid);
    } else {
        if (emojiSearchIcon) emojiSearchIcon.style.opacity = ''; // Restore opacity when cleared
        resetEmojiPicker();
    }
});

// Update the category button click handler
document.querySelectorAll('.emoji-category-btn').forEach(btn => {
    btn.addEventListener('click', (e) => {
        e.stopPropagation(); // Prevent closing the picker
        const category = btn.dataset.category;

        // Update active state
        document.querySelectorAll('.emoji-category-btn').forEach(b => {
            b.classList.toggle('active', b === btn);
        });

        // Scroll to the selected section
        const section = document.getElementById(`emoji-${category}`);
        section.scrollIntoView({ behavior: 'smooth', block: 'start' });
    });
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
                    invoke('react_to_message', { referenceId: strCurrentReactionReference, chatId: strReceiverPubkey, emoji: cEmoji.emoji });
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
        if (strCurrentReactionReference) {
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
                invoke('react_to_message', { referenceId: strCurrentReactionReference, chatId: strReceiverPubkey, emoji: cEmoji.emoji });
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
        cEmoji.used++;

        // If this is a Reaction - let's send it!
        if (strCurrentReactionReference) {
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
                invoke('react_to_message', { referenceId: strCurrentReactionReference, chatId: strReceiverPubkey, emoji: cEmoji.emoji });
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
