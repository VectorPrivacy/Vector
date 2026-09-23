// Text selection is off for the length of a drag: a pointer sweeping across the page
// otherwise highlights whatever it passes. Drags that capture the pointer are covered on
// their own (the capture events bubble here); the rest take the lock by hand.
let holds = 0;

/** Lock selection until the returned release is called (once; extra calls are no-ops). */
export function lockSelection() {
    if (holds++ === 0) {
        document.body.classList.add('drag-lock');
        // Only a highlight: a collapsed selection is a caret, which the drag must not take.
        const sel = window.getSelection();
        if (sel && !sel.isCollapsed) sel.removeAllRanges();
    }
    let released = false;
    return () => {
        if (released) return;
        released = true;
        if (--holds === 0) document.body.classList.remove('drag-lock');
    };
}

let captured = null;
document.addEventListener('gotpointercapture', () => { captured ??= lockSelection(); });
document.addEventListener('lostpointercapture', () => { captured?.(); captured = null; });
