// Press arbitration for a list that both scrolls and reorders, as a Svelte action.
//
// One press can mean three things and the only honest disambiguator is "did it move,
// and when": move early → the list scrolls; still at ARM then move → reorder; still at
// MENU → context menu. Without the arm delay the first few pixels of an attempted
// scroll started a drag, so a list long enough to need scrolling could never reach the
// items off-screen. Deliberately not axis-aware: "horizontal means drag, vertical means
// scroll" misfires on the diagonal drift of a real thumb, and press-then-drag is the
// platform convention. Tauri's dragDropEnabled swallows the HTML5 drag events, so this
// is pointer-driven throughout. The pack rail and the pack-creator grid share it so the
// picker teaches one gesture.
const ARM_MS = 180;
const MENU_MS = 500;
const SLOP_PX = 8;          // matches the long-press tolerance for "held still"
const MENU_DRAG_PX = 14;    // menu → drag promotion: larger than DRAG_PX so a lifting finger's wobble keeps the menu
const DRAG_PX = 6;

/**
 * use:reorderable={handlers}
 * @param {HTMLElement} node
 * @param {object} h
 * @param {(x: number, y: number) => void} h.onMenu        long-press / right-click
 * @param {() => void} [h.closeMenu]                       a held menu promoting to a drag
 * @param {(on: boolean) => void} [h.onArm]                a drag became possible (touch hold landed) / stood down
 * @param {(ev: PointerEvent, node: HTMLElement) => void} h.onDragStart
 * @param {(ev: PointerEvent) => void} h.onDragMove
 * @param {(ev: PointerEvent) => void} h.onDragEnd
 * @param {(ev: PointerEvent) => boolean} [h.ignore]       skip the gesture entirely
 */
export function reorderable(node, h) {
    let cur = h;
    let activeTeardown = null;   // the in-flight gesture's window listeners
    const onContextMenu = (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        cur.onMenu(ev.clientX, ev.clientY);
    };
    const onPointerDown = (ev) => {
        if (ev.button !== 0) return;
        if (cur.ignore?.(ev)) return;
        const startX = ev.clientX;
        const startY = ev.clientY;
        const isTouch = ev.pointerType === 'touch';
        let dragging = false;
        let armed = !isTouch;      // a mouse has its own menu button, so it may drag at once
        let claimed = false;       // the gesture belongs to something else (a scroll)
        let menuOpen = false;      // the long-press menu is showing but the finger is still down
        let armTimer = null;
        let menuTimer = null;

        const clearTimers = () => {
            if (armTimer) { clearTimeout(armTimer); armTimer = null; }
            if (menuTimer) { clearTimeout(menuTimer); menuTimer = null; }
        };
        const disarm = () => {
            armed = false;
            cur.onArm?.(false);
            node.style.touchAction = '';   // hand panning back to the browser
        };

        if (isTouch) {
            armTimer = setTimeout(() => {
                armTimer = null;
                armed = true;
                cur.onArm?.(true);
                // Take the gesture from the browser: with `touch-action: pan-y` it would keep
                // panning on vertical movement and ignore preventDefault. Legal to flip now
                // because arming required a still finger, so no pan has begun.
                node.style.touchAction = 'none';
                navigator.vibrate?.(8);   // "you may now drag", without stealing the tap
            }, ARM_MS);
            menuTimer = setTimeout(() => {
                menuTimer = null;
                menuOpen = true;
                node.dataset.suppressClick = '1';
                navigator.vibrate?.(14);
                cur.onMenu(startX, startY);
                // The gesture stays live: the held press may still promote to a drag.
            }, MENU_MS);
        }

        // Swallow the scroll once the gesture is ours. The browser latches pan-y at
        // touchstart, so an un-prevented vertical move in the armed/menu windows lets the
        // native pan claim the gesture and pointercancel us. Non-passive: Android's WebView
        // defaults touchmove to passive, where preventDefault is ignored.
        const onTouchMove = (te) => { if (dragging || armed || menuOpen) te.preventDefault(); };

        const onMove = (mv) => {
            if (!dragging) {
                if (claimed) return;
                const dist = Math.hypot(mv.clientX - startX, mv.clientY - startY);
                if (menuOpen) {
                    if (dist < MENU_DRAG_PX) return;
                    cur.closeMenu?.();
                    menuOpen = false;
                    dragging = true;
                    cur.onDragStart(mv, node);
                    cur.onDragMove(mv);
                    return;
                }
                if (!armed) {
                    // Moved before the hold landed: a scroll. Stand down so the list pans natively.
                    if (dist > SLOP_PX) { claimed = true; clearTimers(); teardown(); }
                    return;
                }
                if (dist < DRAG_PX) return;
                clearTimers();   // moving rules out the long-press menu
                dragging = true;
                cur.onDragStart(mv, node);
            }
            cur.onDragMove(mv);
        };
        function teardown() {
            window.removeEventListener('pointermove', onMove);
            window.removeEventListener('pointerup', onUp);
            window.removeEventListener('pointercancel', onUp);
            window.removeEventListener('touchmove', onTouchMove);
            if (activeTeardown === teardown) activeTeardown = null;
        }
        activeTeardown?.();
        activeTeardown = teardown;
        const onUp = (up) => {
            clearTimers();
            teardown();
            const wasDragging = dragging;
            dragging = false;
            disarm();
            if (wasDragging) cur.onDragEnd(up);
        };
        window.addEventListener('pointermove', onMove);
        window.addEventListener('pointerup', onUp);
        window.addEventListener('pointercancel', onUp);
        window.addEventListener('touchmove', onTouchMove, { passive: false });
    };
    node.addEventListener('contextmenu', onContextMenu);
    node.addEventListener('pointerdown', onPointerDown);
    return {
        update(next) { cur = next; },
        destroy() {
            activeTeardown?.();
            node.removeEventListener('contextmenu', onContextMenu);
            node.removeEventListener('pointerdown', onPointerDown);
        },
    };
}

/**
 * A fixed clone of `node` that rides under the pointer, centred on it: the visible
 * thumbnail stays anchored wherever the user grabbed the item. Gesture-owned, so it is
 * DOM the component never renders. `prune` strips transient chrome from the clone.
 */
export function dragGhost(node, cls, prune) {
    const rect = node.getBoundingClientRect();
    const ghost = node.cloneNode(true);
    ghost.classList.add(cls);
    ghost.classList.remove('is-dragging', 'is-drag-armed');
    prune?.(ghost);
    Object.assign(ghost.style, {
        position: 'fixed', left: `${rect.left}px`, top: `${rect.top}px`,
        width: `${rect.width}px`, height: `${rect.height}px`, pointerEvents: 'none', zIndex: '2200',
    });
    document.body.appendChild(ghost);
    const offX = rect.width / 2;
    const offY = rect.height / 2;
    return {
        move(x, y) { ghost.style.left = `${x - offX}px`; ghost.style.top = `${y - offY}px`; },
        remove() { ghost.remove(); },
    };
}

/**
 * The entry of `items` (each { key, el }) whose centre is nearest to the pointer, and
 * which half of it the pointer is in. `axis: 'y'` measures vertical distance only (a
 * rail); otherwise both, which covers direct hits, gutters and inter-row gaps with one
 * rule. Returns null when `items` is empty.
 */
export function nearestByCentre(items, x, y, axis) {
    let best = null;
    let bestDist = Infinity;
    let bestRect = null;
    for (const it of items) {
        const r = it.el.getBoundingClientRect();
        const dx = axis === 'y' ? 0 : x - (r.left + r.width / 2);
        const dy = y - (r.top + r.height / 2);
        const d = dx * dx + dy * dy;
        if (d < bestDist) { bestDist = d; best = it; bestRect = r; }
    }
    if (!best) return null;
    const before = axis === 'y' ? y < bestRect.top + bestRect.height / 2 : x < bestRect.left + bestRect.width / 2;
    return { key: best.key, el: best.el, before };
}
