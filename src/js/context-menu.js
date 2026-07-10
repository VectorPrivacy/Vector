/**
 * Reusable context menu — singleton appended to <body>, positioned at
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

let _ctxMenuEl = null;
let _ctxMenuVisible = false;
let _ctxMenuDismissedAt = 0; // timestamp of the last outside-tap dismissal

/** True if an outside tap just dismissed a visible menu. Lets an underlying
 *  click handler (e.g. the chatlist open) swallow that same tap, so dismissing
 *  the menu doesn't also activate whatever sat behind it. */
function wasContextMenuJustDismissed() {
    return Date.now() - _ctxMenuDismissedAt < 400;
}

function _ensureContextMenu() {
    if (_ctxMenuEl) return _ctxMenuEl;
    const el = document.createElement('div');
    el.className = 'context-menu';
    el.setAttribute('role', 'menu');
    document.body.appendChild(el);
    _ctxMenuEl = el;
    // Prevent clicks inside from bubbling out and dismissing the menu
    // before our item handlers run.
    el.addEventListener('mousedown', (e) => e.stopPropagation());
    // Android back closes an open menu instead of leaving the screen.
    new MutationObserver(() => {
        if (el.classList.contains('is-visible')) pushBack('context-menu', hideContextMenu);
        else popBack('context-menu');
    }).observe(el, { attributes: true, attributeFilter: ['class'] });
    return el;
}

function hideContextMenu() {
    if (!_ctxMenuVisible) return;
    _ctxMenuVisible = false;
    if (_ctxMenuEl) {
        _ctxMenuEl.classList.remove('is-visible');
    }
}

function showContextMenu({ x, y, items }) {
    if (!Array.isArray(items) || items.length === 0) return;
    const el = _ensureContextMenu();
    el.innerHTML = '';
    for (const item of items) {
        if (item.divider) {
            const div = document.createElement('div');
            div.className = 'context-menu-divider';
            el.appendChild(div);
            continue;
        }
        const row = document.createElement('div');
        row.className = 'context-menu-item';
        if (item.danger) row.classList.add('is-danger');
        row.setAttribute('role', 'menuitem');
        const label = document.createElement('span');
        label.textContent = item.label;
        // Optional dimmed qualifier, e.g. Copy (plain) / Copy (with markdown).
        if (item.hint) {
            const hint = document.createElement('span');
            hint.className = 'context-menu-item-hint';
            hint.textContent = item.hint;
            label.appendChild(hint);
        }
        row.appendChild(label);
        if (item.icon) {
            const icon = document.createElement('span');
            icon.className = `icon icon-${item.icon}`;
            row.appendChild(icon);
        }
        row.addEventListener('click', (e) => {
            e.stopPropagation();
            hideContextMenu();
            try { item.onClick && item.onClick(); }
            catch (err) { console.warn('[context-menu] item handler failed:', err); }
        });
        el.appendChild(row);
    }

    // Position. Render hidden to measure, then clamp to viewport so the
    // menu can never bleed off-screen on long item lists or near edges.
    el.style.left = '0px';
    el.style.top = '0px';
    el.classList.add('is-visible');
    _ctxMenuVisible = true;
    const rect = el.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let nx = x;
    let ny = y;
    if (nx + rect.width > vw - 8)  nx = Math.max(8, vw - rect.width - 8);
    if (ny + rect.height > vh - 8) ny = Math.max(8, y - rect.height); // flip up
    if (nx < 8) nx = 8;
    if (ny < 8) ny = 8;
    el.style.left = `${nx}px`;
    el.style.top = `${ny}px`;
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
