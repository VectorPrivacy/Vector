/**
 * Time suggestions: a draft that ends with "in 6 hours" or "dec 25 at 8pm" is offered as
 * a time everyone reads in their own zone. A chip above the composer takes it on Tab or
 * a tap; Escape or typing on passes. It never takes Enter: the message sends as typed.
 *
 *   const ctrl = initTimeSuggest(textarea, { enabled, busy });
 *   ctrl.takesKey(e) → true when Tab or Escape went to the chip
 */

// eslint-disable-next-line no-unused-vars
function initTimeSuggest(textarea, { enabled = () => true, busy = () => false } = {}) {
    let current = null;          // { start, end, unix, style }
    const dismissed = new Set(); // "start:phrase" the user passed on, for this draft

    const keyOf = (s, val) => `${s.start}:${val.slice(s.start, s.end)}`;
    // A countdown's moment, briefly: the time today, else a short day and date too.
    let shortDate = null;
    function whenAt(unix) {
        const at = new Date(unix * 1000);
        if (at.toDateString() === new Date().toDateString()) return ttFormat(unix, 't');
        shortDate ??= new Intl.DateTimeFormat(undefined, { weekday: 'short', day: 'numeric', month: 'short', hour: 'numeric', minute: '2-digit' });
        return shortDate.format(at);
    }

    function hide() {
        if (current) VectorSvelte.closePopup('timesuggest');
        current = null;
    }

    function check() {
        const val = textarea.value;
        if (!val) dismissed.clear();
        const caret = textarea.selectionStart;
        const found = enabled() && !busy() && caret === textarea.selectionEnd ? ttSuggest(val.slice(0, caret)) : null;
        if (!found || dismissed.has(keyOf(found, val))) { hide(); return; }
        current = found;
        VectorSvelte.openPopup('timesuggest', {
            phrase: val.slice(found.start, found.end),
            preview: found.style === 'R' ? `${ttFormat(found.unix, 'R')} · ${whenAt(found.unix)}` : ttFormat(found.unix, found.style),
            full: ttFull(found.unix, found.style === 'D'),
            countdown: found.style === 'R',
            key: !platformFeatures.is_mobile,
            // On a desktop the chip starts where the phrase does; a phone keeps it at the edge.
            x: !platformFeatures.is_mobile && textarea.xAt ? textarea.xAt(found.start) : null,
            accept,
        });
    }

    function accept() {
        const s = current;
        if (!s) return;
        const val = textarea.value;
        const caret = textarea.selectionStart;
        const code = `<t:${s.unix}:${s.style}>`;
        const rest = val.slice(s.end);
        // A space to type on from, unless the phrase already had what follows it.
        const gap = rest ? '' : ' ';
        textarea.value = val.slice(0, s.start) + code + gap + rest;
        const pos = caret + code.length + gap.length - (s.end - s.start);
        textarea.setSelectionRange(pos, pos);
        hide();
        textarea.dispatchEvent(new Event('input', { bubbles: true }));
        textarea.focus();
    }

    function takesKey(e) {
        // Another panel can take the slot over the chip; the keys are then its.
        if (!current || VectorSvelte.composerPopup().kind !== 'timesuggest') return false;
        if (e.key === 'Tab' && !e.shiftKey) {
            e.preventDefault();
            accept();
            return true;
        }
        if (e.key === 'Escape') {
            e.preventDefault();
            dismissed.add(keyOf(current, textarea.value));
            hide();
            return true;
        }
        return false;
    }

    // The caret moving off the phrase leaves it be.
    function onCaret(e) {
        if (e.type === 'click' || /^(Arrow|Home|End)/.test(e.key)) check();
    }
    function onBlur() {
        setTimeout(() => { if (document.activeElement !== textarea) hide(); }, 150);
    }

    textarea.addEventListener('input', check);
    textarea.addEventListener('keyup', onCaret);
    textarea.addEventListener('click', onCaret);
    textarea.addEventListener('blur', onBlur);

    return {
        takesKey,
        destroy() {
            textarea.removeEventListener('input', check);
            textarea.removeEventListener('keyup', onCaret);
            textarea.removeEventListener('click', onCaret);
            textarea.removeEventListener('blur', onBlur);
            hide();
        },
    };
}
