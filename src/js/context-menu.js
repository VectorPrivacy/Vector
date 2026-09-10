/**
 * Reusable context menu (ui/ContextMenu.svelte over lib/contextmenu), positioned at
 * cursor (right-click) or anchor rect (click). Items are arbitrary; the
 * caller hands in `{ label, icon?, danger?, onClick }`. Auto-closes on
 * outside click, Escape, or item activation.
 *
 * showContextMenu({
 *   x: number, y: number,     // anchor coords; for click-based open use
 *                             //   the button's bounding rect bottom-left
 *   items: [
 *     { label: 'Share', icon: 'share', onClick: () => {...} },
 *     { divider: true },
 *     { label: 'Remove', icon: 'x', danger: true, onClick: () => {...} },
 *   ],
 * });
 */

let _ctxMenuVisible = false;
let _ctxMenuDismissedAt = 0; // timestamp of the last outside-tap dismissal

/** True if an outside tap just dismissed a visible menu. Lets an underlying
 *  click handler (e.g. the chatlist open) swallow that same tap, so dismissing
 *  the menu doesn't also activate whatever sat behind it. */
function wasContextMenuJustDismissed() {
    return Date.now() - _ctxMenuDismissedAt < 400;
}

// The menu is a component; an item's activation closes it before its handler runs.
VectorSvelte.setContextMenuHandlers({
    activate: (item) => {
        hideContextMenu();
        try { item.onClick && item.onClick(); }
        catch (err) { console.warn('[context-menu] item handler failed:', err); }
    },
});

function hideContextMenu() {
    if (!_ctxMenuVisible) return;
    _ctxMenuVisible = false;
    VectorSvelte.setContextMenu({ open: false });
    popBack('context-menu');
}

function showContextMenu({ x, y, items }) {
    if (!Array.isArray(items) || items.length === 0) return;
    // Render at the origin to measure, then clamp to the viewport so the menu can never
    // bleed off-screen on long lists or near edges.
    VectorSvelte.setContextMenu({ items, x: 0, y: 0, open: true });
    VectorSvelte.flushSync();
    _ctxMenuVisible = true;
    // Android back closes an open menu instead of leaving the screen.
    pushBack('context-menu', hideContextMenu);
    const rect = VectorSvelte.contextMenuEls().root.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let nx = x;
    let ny = y;
    if (nx + rect.width > vw - 8)  nx = Math.max(8, vw - rect.width - 8);
    if (ny + rect.height > vh - 8) ny = Math.max(8, y - rect.height); // flip up
    if (nx < 8) nx = 8;
    if (ny < 8) ny = 8;
    VectorSvelte.setContextMenu({ x: nx, y: ny });
}

/**
 * Wire an element to open a context menu on right-click (desktop) AND
 * long-press (touch / Android). The native `contextmenu` event is
 * unreliable on Android WebView — the system often intercepts first
 * (text-selection menu, image context, etc.) — so we install our own
 * 500ms touch-press timer with a small move-tolerance so an accidental
 * scroll doesn't trip it. Mirrors `src/js/reaction.js`'s pattern.
 *
 * @param {HTMLElement} el — trigger element (e.g. pack tab, section header)
 * @param {(x: number, y: number) => void} fireMenu — called with the
 *   trigger coords when the user right-clicks or long-presses.
 */
function attachLongPressContextMenu(el, fireMenu) {
    el.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        e.stopPropagation();
        fireMenu(e.clientX, e.clientY);
    });

    let timer = null;
    let startX = 0, startY = 0;
    const cancel = () => { if (timer) { clearTimeout(timer); timer = null; } };

    el.addEventListener('touchstart', (e) => {
        const t = e.touches && e.touches[0];
        if (!t) return;
        startX = t.clientX;
        startY = t.clientY;
        cancel();
        timer = setTimeout(() => {
            timer = null;
            // preventDefault here so the WebView's selection / context
            // menu doesn't fire on the same gesture.
            try { e.preventDefault(); } catch (_e) {}
            fireMenu(startX, startY);
        }, 500);
    }, { passive: false });

    // Cancel if the finger moves past a small tolerance — that's a
    // scroll / swipe, not a press-and-hold.
    el.addEventListener('touchmove', (e) => {
        const t = e.touches && e.touches[0];
        if (!t) return;
        if (Math.hypot(t.clientX - startX, t.clientY - startY) > 8) cancel();
    });
    el.addEventListener('touchend', cancel);
    el.addEventListener('touchcancel', cancel);
}

// Global dismissal listeners — install once.
(function _installContextMenuDismiss() {
    document.addEventListener('mousedown', () => {
        if (_ctxMenuVisible) _ctxMenuDismissedAt = Date.now();
        hideContextMenu();
    });
    document.addEventListener('scroll', () => hideContextMenu(), true);
    document.addEventListener('keydown', (e) => {
        // Gate on visibility so the listener doesn't fight other Escape
        // handlers (e.g. the in-panel confirm overlay's dismiss) when
        // no menu is up.
        if (e.key === 'Escape' && _ctxMenuVisible) hideContextMenu();
    });
    window.addEventListener('resize', () => hideContextMenu());
    window.addEventListener('blur', () => hideContextMenu());
})();
