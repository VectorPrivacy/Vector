/**
 * Mention Selector Module
 * Discord-inspired @mention autocomplete for chat input.
 *
 * Usage:
 *   const ctrl = initMentionSelector(textarea, candidatesFn, anchorEl);
 *   // ctrl.getMentions()   → [{name, npub}]
 *   // ctrl.clearMentions() → reset tracked mentions
 *   // ctrl.destroy()       → remove DOM + listeners
 */

// eslint-disable-next-line no-unused-vars
function initMentionSelector(textarea, candidatesFn, anchorEl) {
    // --- State ---
    let mentions = [];       // [{name, npub}] tracked for current draft
    let activeIndex = 0;     // keyboard-highlighted row
    let query = '';          // text typed after '@'
    let atStart = -1;        // caret position of the '@' trigger
    let panel = null;        // DOM element
    let skipNextInput = false; // suppress re-open after selection

    // --- Create selector panel ---
    function createPanel() {
        const el = document.createElement('div');
        el.className = 'mention-selector';
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
        atStart = -1;
        activeIndex = 0;
        cachedCandidates = null;
    }

    function position() {
        const rect = anchorEl.getBoundingClientRect();
        const margin = 10;
        const width = Math.min(rect.width, 340);
        // Clamp horizontally so it never touches the edges
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
        header.className = 'mention-selector-header';
        header.textContent = 'Members';
        panel.appendChild(header);

        // Items with staggered animation
        items.forEach((item, i) => {
            const row = document.createElement('div');
            row.className = 'mention-item' + (i === activeIndex ? ' active' : '');
            row.style.animationDelay = (i * 30) + 'ms';

            const img = document.createElement('img');
            img.src = item.avatarSrc || 'icons/user-placeholder.svg';
            img.alt = '';
            row.appendChild(img);

            const name = document.createElement('span');
            name.className = 'mention-item-name';
            name.textContent = item.name;
            row.appendChild(name);

            row.addEventListener('mousedown', (e) => {
                e.preventDefault(); // keep textarea focus
                selectItem(item);
            });
            panel.appendChild(row);
        });

        position();
        show();
    }

    // Cache candidates per selector-open to avoid rebuilding on every keystroke
    let cachedCandidates = null;

    function getFiltered() {
        if (!cachedCandidates) cachedCandidates = candidatesFn();
        const q = query.toLowerCase();
        return cachedCandidates
            .filter(c => c.name.toLowerCase().includes(q))
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
            if (panel.parentNode) panel.parentNode.removeChild(panel);
        }
    };
}
