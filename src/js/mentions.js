/**
 * Mention Selector Module
 * Discord-inspired @mention autocomplete for chat input.
 *
 * Usage:
 *   const ctrl = initMentionSelector(textarea, candidatesFn);
 *   // ctrl.getMentions()   → [{name, npub}]
 *   // ctrl.clearMentions() → reset tracked mentions
 *   // ctrl.destroy()       → remove DOM + listeners
 */

// eslint-disable-next-line no-unused-vars
function initMentionSelector(textarea, candidatesFn) {
    // --- State ---
    let mentions = [];       // [{name, npub}] tracked for current draft
    let activeIndex = 0;     // keyboard-highlighted row
    let query = '';          // text typed after '@'
    let atStart = -1;        // caret position of the '@' trigger
    let open = false;
    let skipNextInput = false; // suppress re-open after selection

    // --- Helpers ---
    function isVisible() {
        return open;
    }

    function hide() {
        if (open) {
            open = false;
            VectorSvelte.closePopup('mention');
        }
        query = '';
        atStart = -1;
        activeIndex = 0;
        cachedCandidates = null;
    }

    function renderItems(items) {
        if (!items.length) { hide(); return; }
        open = true;
        VectorSvelte.openPopup('mention', { items, active: activeIndex, pick: (i) => selectItem(items[i]) });
    }

    // Cache candidates per selector-open to avoid rebuilding on every keystroke
    let cachedCandidates = null;

    function getFiltered() {
        if (!cachedCandidates) cachedCandidates = candidatesFn();
        const q = query.toLowerCase();
        // Tiered match: name prefix, then name substring, then npub. An npub
        // is 58 chars of bech32 soup that matches almost any short query, so
        // it only fills whatever slots real name matches leave open — a
        // member with no profile name yet (npub-stub entry) is still findable
        // by typing (part of) their npub. Sort is stable, so recency order
        // from the candidate pool holds within each tier.
        const tier = (c) => {
            const n = c.name.toLowerCase();
            if (!q || n.startsWith(q)) return 0;
            if (n.includes(q)) return 1;
            if (c.npub.toLowerCase().includes(q)) return 2;
            return 3;
        };
        return cachedCandidates
            .map(c => [tier(c), c])
            .filter(([t]) => t < 3)
            .sort((a, b) => a[0] - b[0])
            .map(([, c]) => c)
            .slice(0, 5);
    }

    function selectItem(item) {
        // Replace '@query' with '@DisplayName '
        const before = textarea.value.substring(0, atStart);
        const after = textarea.value.substring(textarea.selectionStart);
        const insert = '@' + item.name + ' ';
        textarea.value = before + insert + after;
        // Place caret after inserted text
        const newPos = atStart + insert.length;
        textarea.selectionStart = textarea.selectionEnd = newPos;
        // Track mention
        if (!mentions.find(m => m.npub === item.npub)) {
            mentions.push({ name: item.name, npub: item.npub });
        }
        hide();
        // Fire input event so send-button / auto-resize react
        skipNextInput = true;
        textarea.dispatchEvent(new Event('input', { bubbles: true }));
    }

    // --- Detect '@' trigger on every input ---
    function onInput() {
        if (skipNextInput) { skipNextInput = false; return; }
        const val = textarea.value;
        const caret = textarea.selectionStart;

        // Walk backwards from caret to find an unescaped '@' after whitespace or at pos 0
        let foundAt = -1;
        for (let i = caret - 1; i >= 0; i--) {
            const ch = val[i];
            if (ch === '@') {
                // '@' must be at start or preceded by whitespace/newline
                if (i === 0 || /\s/.test(val[i - 1])) {
                    foundAt = i;
                }
                break;
            }
            if (/\s/.test(ch)) break; // stop at whitespace before finding '@'
        }

        if (foundAt === -1) {
            if (isVisible()) hide();
            return;
        }

        atStart = foundAt;
        query = val.substring(foundAt + 1, caret);

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
        getMentions() { return mentions.slice(); },
        clearMentions() { mentions = []; },
        destroy() {
            textarea.removeEventListener('input', onInput);
            textarea.removeEventListener('keydown', onKeyDown);
            textarea.removeEventListener('blur', onBlur);
            hide();
        }
    };
}
