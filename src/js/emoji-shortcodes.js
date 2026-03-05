/**
 * Emoji Shortcode Selector Module
 * Discord/Slack-inspired :emoji autocomplete for chat input.
 *
 * Usage:
 *   const ctrl = initEmojiShortcodeSelector(textarea, anchorEl);
 *   // ctrl.isOpen()  → true if panel is visible
 *   // ctrl.destroy() → remove DOM + listeners
 */

// eslint-disable-next-line no-unused-vars
function initEmojiShortcodeSelector(textarea, anchorEl) {
    // --- State ---
    let activeIndex = 0;
    let query = '';
    let colonStart = -1;       // caret position of the ':' trigger
    let panel = null;
    let skipNextInput = false;
    let cachedResults = {};    // query → results cache

    // --- Twemoji URL helper (delegates to twemoji's own parsing, cached) ---
    const twemojiUrlCache = {};
    function emojiToTwemojiUrl(emoji) {
        if (twemojiUrlCache[emoji]) return twemojiUrlCache[emoji];
        const span = document.createElement('span');
        span.textContent = emoji;
        twemoji.parse(span, { callback: (icon) => '/twemoji/svg/' + icon + '.svg' });
        const img = span.querySelector('img');
        const url = img ? img.getAttribute('src') : null;
        if (url) twemojiUrlCache[emoji] = url;
        return url;
    }

    // --- Short label from full name ---
    function shortLabel(name) {
        const words = name.split(' ');
        return words.slice(0, 3).join(' ');
    }

    // --- Create selector panel ---
    function createPanel() {
        const el = document.createElement('div');
        el.className = 'emoji-shortcode-selector';
        document.body.appendChild(el);
        return el;
    }
    panel = createPanel();

    // --- Helpers ---
    function isVisible() {
        return panel.classList.contains('visible');
    }

    function show() {
        if (!isVisible()) {
            panel.classList.add('visible');
        }
    }

    function hide() {
        panel.classList.remove('visible');
        query = '';
        colonStart = -1;
        activeIndex = 0;
        cachedResults = {};
    }

    function position() {
        const rect = anchorEl.getBoundingClientRect();
        const margin = 10;
        const width = Math.min(rect.width, 340);
        const left = Math.max(margin, Math.min(rect.left, window.innerWidth - width - margin));
        panel.style.left = left + 'px';
        panel.style.bottom = (window.innerHeight - rect.top + 6) + 'px';
        panel.style.width = width + 'px';
    }

    function renderItems(items) {
        panel.innerHTML = '';
        if (!items.length) { hide(); return; }

        // Header
        const header = document.createElement('div');
        header.className = 'emoji-shortcode-header';
        header.textContent = query.length ? 'Emojis' : 'Recently Used';
        panel.appendChild(header);

        // Items with staggered animation
        items.forEach((item, i) => {
            const row = document.createElement('div');
            row.className = 'emoji-shortcode-item' + (i === activeIndex ? ' active' : '');
            row.style.animationDelay = (i * 30) + 'ms';

            const twemojiSrc = emojiToTwemojiUrl(item.emoji);
            if (twemojiSrc) {
                const img = document.createElement('img');
                img.src = twemojiSrc;
                img.alt = item.emoji;
                row.appendChild(img);
            } else {
                const fallback = document.createElement('span');
                fallback.className = 'emoji-shortcode-item-fallback';
                fallback.textContent = item.emoji;
                row.appendChild(fallback);
            }

            const label = document.createElement('span');
            label.className = 'emoji-shortcode-item-label';
            label.textContent = shortLabel(item.name);
            row.appendChild(label);

            row.addEventListener('mousedown', (e) => {
                e.preventDefault(); // keep textarea focus
                selectItem(item);
            });
            panel.appendChild(row);
        });

        position();
        show();
    }

    function getFiltered() {
        // Don't cache empty-query results — used counts can change between views
        if (!query.length) {
            return (typeof getMostUsedEmojis === 'function' ? getMostUsedEmojis() : []).slice(0, 5);
        }
        if (cachedResults[query]) return cachedResults[query];
        const results = searchEmojis(query).slice(0, 5);
        cachedResults[query] = results;
        return results;
    }

    function selectItem(item) {
        // Replace ':query' (or ':query:') with emoji + space
        const before = textarea.value.substring(0, colonStart);
        const after = textarea.value.substring(textarea.selectionStart);
        const insert = item.emoji + ' ';
        textarea.value = before + insert + after;
        const newPos = colonStart + insert.length;
        textarea.selectionStart = textarea.selectionEnd = newPos;
        // Increment usage counter on the canonical arrEmojis entry (matches emoji panel behavior)
        const canonical = typeof arrEmojis !== 'undefined' && arrEmojis.find(e => e.emoji === item.emoji);
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

        const items = getFiltered();
        activeIndex = 0;
        renderItems(items);
    }

    // --- Keyboard navigation ---
    function onKeyDown(e) {
        if (!isVisible()) return;
        const items = getFiltered();
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
            if (panel.parentNode) panel.parentNode.removeChild(panel);
        }
    };
}
