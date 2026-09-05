/**
 * Emoji Shortcode Selector Module
 * Discord/Slack-inspired :emoji autocomplete for chat input.
 *
 * Usage:
 *   const ctrl = initEmojiShortcodeSelector(textarea);
 *   // ctrl.isOpen()  → true if panel is visible
 *   // ctrl.destroy() → remove DOM + listeners
 */

// Text emoticon → emoji. Keys are the chars AFTER the ':' trigger (so `:)` → key `)`), since the
// selector already keys off the leading ':'. Gated by the "Emoticon Suggestions" display setting
// (`emoticonSuggestionsEnabled`): when off, these are left as literal text so e.g. `:3` is typeable.
const EMOTICON_MAP = {
    ')': '🙂', '-)': '🙂',
    '(': '🙁', '-(': '🙁', "'(": '😢',
    'D': '😄', '-D': '😄',
    'P': '😛', 'p': '😛', '-P': '😛', '-p': '😛',
    '3': '😺',
    'o': '😮', 'O': '😮', '-o': '😮', '-O': '😮',
    '|': '😐', '-|': '😐',
    '/': '😕', '\\': '😕',
    '*': '😘', '-*': '😘',
};

// Twemoji URL for a stock emoji (delegates to twemoji's own parsing, cached).
const _twemojiUrlCache = {};
function emojiToTwemojiUrl(emoji) {
    if (_twemojiUrlCache[emoji]) return _twemojiUrlCache[emoji];
    const span = document.createElement('span');
    span.textContent = emoji;
    twemoji.parse(span, { callback: (icon) => '/twemoji/svg/' + icon + '.svg' });
    const img = span.querySelector('img');
    const url = img ? img.getAttribute('src') : null;
    if (url) _twemojiUrlCache[emoji] = url;
    return url;
}

// eslint-disable-next-line no-unused-vars
function initEmojiShortcodeSelector(textarea) {
    // --- State ---
    let activeIndex = 0;
    let query = '';
    let colonStart = -1;       // caret position of the ':' trigger
    let open = false;
    let skipNextInput = false;
    let cachedResults = {};    // query → results cache
    let lastItems = [];        // exact list currently rendered (keyboard nav + Enter use this,
                               // since the emoticon path renders a different list than getFiltered)

    // --- Helpers ---
    function isVisible() {
        return open;
    }

    function hide() {
        if (open) {
            open = false;
            VectorSvelte.closePopup('shortcode');
        }
        query = '';
        colonStart = -1;
        activeIndex = 0;
        cachedResults = {};
        lastItems = [];
    }

    function renderItems(items) {
        lastItems = items;
        if (!items.length) { hide(); return; }
        open = true;
        VectorSvelte.openPopup('shortcode', {
            header: query.length ? 'Emojis' : 'Recently Used',
            items,
            active: activeIndex,
            pick: (i) => selectItem(items[i]),
        });
    }

    function getFiltered() {
        // Don't cache empty-query results — used counts can change between views
        if (!query.length) {
            // Merge stock + custom recents (the picker's Recently Used does the
            // same) — a user whose recents are mostly custom emojis would
            // otherwise see an empty/stock-only list on a bare `:`.
            return getMostUsedEmojis().concat(getMostUsedCustomEmojis(5))
                .sort((a, b) => (b.used || 0) - (a.used || 0))
                .slice(0, 5);
        }
        if (cachedResults[query]) return cachedResults[query];
        // Unified merge by score. Both stock and custom bake personal
        // usage into their score (`searchEmojis` + `searchCustomEmojis`
        // share the USAGE_SCORE_WEIGHT constant), so frequently-picked
        // emojis on either side climb the autocomplete identically.
        const allCustom = typeof searchCustomEmojis === 'function'
            ? searchCustomEmojis(query)
            : [];
        const stockResults = searchEmojis(query);
        const results = stockResults.concat(allCustom)
            .sort((a, b) => (a.score || 0) - (b.score || 0))
            .slice(0, 5);
        cachedResults[query] = results;
        return results;
    }

    function selectItem(item) {
        // Replace ':query' (or ':query:') with emoji + space. Custom
        // emojis insert the `:shortcode:` literal — the send pipeline
        // resolves it against the user's subscribed packs and attaches
        // the NIP-30 tag.
        const before = textarea.value.substring(0, colonStart);
        const after = textarea.value.substring(textarea.selectionStart);
        const insert = item.isCustom
            ? `:${item.shortcode}: `
            : item.emoji + ' ';
        textarea.value = before + insert + after;
        const newPos = colonStart + insert.length;
        textarea.selectionStart = textarea.selectionEnd = newPos;
        // Increment usage counter so frequently-used customs (and stock)
        // rise in the search ranking on future opens.
        if (item.isCustom) {
            if (typeof bumpCustomEmojiUsage === 'function') {
                bumpCustomEmojiUsage(item.shortcode);
            }
        }
        const canonical = !item.isCustom
            && typeof arrEmojis !== 'undefined'
            && arrEmojis.find(e => e.emoji === item.emoji);
        if (canonical) {
            canonical.used++;
            if (typeof addToRecentEmojis === 'function') addToRecentEmojis(canonical);
        }
        hide();
        skipNextInput = true;
        textarea.dispatchEvent(new Event('input', { bubbles: true }));
    }

    // --- Detect ':' trigger on every input ---
    function onInput() {
        if (skipNextInput) { skipNextInput = false; return; }
        const val = textarea.value;
        const caret = textarea.selectionStart;

        // Walk backwards from caret to find a ':' after whitespace or at pos 0
        let foundColon = -1;
        for (let i = caret - 1; i >= 0; i--) {
            const ch = val[i];
            if (ch === ':') {
                if (i === 0 || /\s/.test(val[i - 1])) {
                    foundColon = i;
                }
                break;
            }
            if (/\s/.test(ch)) break; // stop at whitespace before finding ':'
        }

        if (foundColon === -1) {
            if (isVisible()) hide();
            return;
        }

        colonStart = foundColon;
        let q = val.substring(foundColon + 1, caret);
        // Strip trailing ':' if user typed e.g. ':cat:'
        if (q.endsWith(':')) q = q.slice(0, -1);
        query = q;

        // Text emoticon (`:)`, `:D`, `:3`, …): suggest the matching emoji on top — unless the user
        // disabled it, in which case leave it as literal text (don't hijack `:3` etc.).
        if (Object.prototype.hasOwnProperty.call(EMOTICON_MAP, query)) {
            if (typeof emoticonSuggestionsEnabled !== 'undefined' && !emoticonSuggestionsEnabled) {
                if (isVisible()) hide();
                return;
            }
            const char = EMOTICON_MAP[query];
            const canonical = (typeof arrEmojis !== 'undefined' && arrEmojis.find(e => e.emoji === char))
                || { emoji: char, name: char };
            const rest = getFiltered().filter(it => it.emoji !== char);
            activeIndex = 0;
            renderItems([canonical, ...rest].slice(0, 5));
            return;
        }

        const items = getFiltered();
        activeIndex = 0;
        renderItems(items);
    }

    // --- Keyboard navigation ---
    function onKeyDown(e) {
        if (!isVisible()) return;
        const items = lastItems;
        if (!items.length) return;

        if (e.key === 'ArrowDown') {
            e.preventDefault();
            activeIndex = (activeIndex + 1) % items.length;
            renderItems(items);
        } else if (e.key === 'ArrowUp') {
            e.preventDefault();
            activeIndex = (activeIndex - 1 + items.length) % items.length;
            renderItems(items);
        } else if (e.key === 'Enter' || e.key === 'Tab') {
            e.preventDefault();
            e.stopPropagation();
            selectItem(items[activeIndex]);
        } else if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            hide();
        }
    }

    // Hide when textarea loses focus (delayed so mousedown on panel fires first)
    function onBlur() {
        setTimeout(() => { if (isVisible()) hide(); }, 150);
    }

    // --- Bind listeners ---
    textarea.addEventListener('input', onInput);
    textarea.addEventListener('keydown', onKeyDown);
    textarea.addEventListener('blur', onBlur);

    // --- Public API ---
    return {
        isOpen() { return isVisible(); },
        destroy() {
            textarea.removeEventListener('input', onInput);
            textarea.removeEventListener('keydown', onKeyDown);
            textarea.removeEventListener('blur', onBlur);
            hide();
        }
    };
}
