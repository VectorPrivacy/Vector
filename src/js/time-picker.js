/**
 * `@time`: type it, then a phrase ("tomorrow 5pm", "friday 9:30", "in 2 hours"), and
 * pick how it should read. The pick becomes a `<t:UNIX:STYLE>` code that everyone sees
 * in their own zone; core sends it as fixed UTC text plus a time span.
 *
 *   const ctrl = initTimeSelector(textarea);
 *   ctrl.isOpen() → the panel is up and owns Enter/Tab/arrows/Escape
 */

// The order the styles are offered in, the everyday ones first.
const TIME_STYLE_ORDER = ['f', 'F', 'R', 't', 'T', 'd', 'D', 's', 'S'];
const TIME_STYLE_NAMES = {
    f: 'Date and time', F: 'Full date and time', R: 'Relative', t: 'Time', T: 'Time with seconds',
    d: 'Short date', D: 'Long date', s: 'Short date and time', S: 'Short date, time with seconds',
};

// eslint-disable-next-line no-unused-vars
function initTimeSelector(textarea) {
    let open = false;
    let active = 0;
    let trigger = -1;
    let unix = null;
    let rowCount = 0;
    let dismissed = -1;     // the trigger an Escape closed, until the draft moves past it

    /** The `@time` the caret is in: its start and what's typed after it, or null. */
    function find() {
        const val = textarea.value;
        const caret = textarea.selectionStart;
        const lineStart = val.lastIndexOf('\n', caret - 1) + 1;
        const line = val.slice(lineStart, caret);
        let found = null;
        for (const m of line.matchAll(/(^|\s)@time(?=\s|$)/gi)) found = m;
        if (!found) return null;
        const at = lineStart + found.index + found[1].length;
        const query = val.slice(at + 5, caret);
        return query.length > 60 ? null : { at, query };
    }

    function hide() {
        if (open) VectorSvelte.closePopup('time');
        open = false;
        active = 0;
        rowCount = 0;
    }

    function render() {
        const f = find();
        if (!f || f.at === dismissed) { hide(); return; }
        if (f.at !== trigger) active = 0;
        trigger = f.at;
        unix = ttParse(f.query);
        const rows = unix === null ? [] : TIME_STYLE_ORDER.map((style) => ({
            style, name: TIME_STYLE_NAMES[style], preview: ttFormat(unix, style),
        }));
        rowCount = rows.length;
        if (active >= rowCount) active = 0;
        open = true;
        VectorSvelte.openPopup('time', {
            query: f.query.trim(),
            full: unix === null ? '' : ttFull(unix),
            rows,
            active,
            pick: (i) => select(rows[i].style),
        });
    }

    function select(style) {
        // Read again: the caret may have moved since the rows were drawn.
        const f = find();
        unix = f && ttParse(f.query);
        if (!f || unix === null) return;
        const val = textarea.value;
        const caret = textarea.selectionStart;
        const insert = `<t:${unix}:${style}> `;
        textarea.value = val.slice(0, f.at) + insert + val.slice(caret);
        const pos = f.at + insert.length;
        textarea.setSelectionRange(pos, pos);
        hide();
        textarea.dispatchEvent(new Event('input', { bubbles: true }));
        textarea.focus();
    }

    function onInput() {
        if (dismissed >= 0 && !textarea.value.slice(dismissed).toLowerCase().startsWith('@time')) dismissed = -1;
        render();
    }

    function onKeyDown(e) {
        // A key another picker already took (the mention list's "time" row) isn't ours.
        if (!open || e.defaultPrevented) return;
        if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            dismissed = trigger;
            hide();
            return;
        }
        if (!rowCount) {
            // An unread phrase holds the send: it would go out as the literal `@time …`.
            if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); e.stopPropagation(); }
            return;
        }
        if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            active = (active + (e.key === 'ArrowDown' ? 1 : -1) + rowCount) % rowCount;
            render();
        } else if ((e.key === 'Enter' && !e.shiftKey) || e.key === 'Tab') {
            e.preventDefault();
            e.stopPropagation();
            select(TIME_STYLE_ORDER[active]);
        }
    }

    function onBlur() {
        setTimeout(() => { if (open && document.activeElement !== textarea) hide(); }, 150);
    }

    // The caret moving through the phrase changes what it says.
    function onCaret(e) {
        if (open && (e.type === 'click' || /^(Arrow(Left|Right)|Home|End)$/.test(e.key))) render();
    }

    textarea.addEventListener('input', onInput);
    textarea.addEventListener('keydown', onKeyDown);
    textarea.addEventListener('keyup', onCaret);
    textarea.addEventListener('click', onCaret);
    textarea.addEventListener('blur', onBlur);

    return {
        isOpen() { return open; },
        destroy() {
            textarea.removeEventListener('input', onInput);
            textarea.removeEventListener('keydown', onKeyDown);
            textarea.removeEventListener('keyup', onCaret);
            textarea.removeEventListener('click', onCaret);
            textarea.removeEventListener('blur', onBlur);
            hide();
        },
    };
}
