<script>
    // Archived Chats: a row tucked above the list. Reaching the top never shows it; a
    // second, deliberate pull past the top does: a drag begun at the top, or a fresh
    // wheel gesture begun there, against some tension. Tapping it unfolds the chats.
    import { flushSync } from 'svelte';
    import { shellElements } from '../lib/shell.svelte.js';
    import { chatVersion } from '../lib/signals.svelte.js';

    let { h, chats, forced = false, rows } = $props();   // h: ChatlistHelpers (js/render/chatlist/list.js)

    // Finger travel and wheel travel past the top that commit a reveal.
    const DRAG_PX = 110;
    const WHEEL_PX = 160;
    // The row follows the finger at half speed: the tension.
    const DRAG_RESIST = 0.5;
    // Wheel silence that ends a gesture, so the momentum that reached the top never counts.
    const GESTURE_GAP_MS = 220;
    // A pull ends in a lifted finger, which must not also open the chat under it.
    const CLICK_SWALLOW_MS = 400;

    let revealed = $state(false);
    let expanded = $state(false);
    let pull = $state(0);
    let dragging = $state(false);
    let instant = $state(false);
    let rowH = $state(0);
    let rowEl = $state(null);

    const shown = $derived(forced || revealed);
    const height = $derived(shown ? rowH : Math.min(rowH, pull));
    const names = $derived(chats.map((c) => h.getName(c.id)).join(', '));
    // A chat marked unread inside the archive still lights the unread dots, so the row says where.
    const unread = $derived(chats.some((c) => { chatVersion(c.id); return h.computeListRowUnreadCount(c) > 0; }));

    $effect(() => { if (!shown) expanded = false; });

    // Shown outright because nothing else was listed: when a chat arrives, stay open rather than vanish.
    let wasForced = false;
    $effect(() => {
        if (wasForced && !forced) revealed = true;
        wasForced = forced;
    });

    // The row's full stride, margin included, follows the layout (widescreen rows are shorter).
    $effect(() => {
        if (!rowEl) return;
        const measure = () => { rowH = rowEl.offsetHeight + (parseFloat(getComputedStyle(rowEl).marginBottom) || 0); };
        measure();
        const ro = new ResizeObserver(measure);
        ro.observe(rowEl);
        return () => ro.disconnect();
    });

    function hide(scroller) {
        const before = scroller.scrollTop;
        instant = true;
        revealed = false;
        flushSync();
        // Chromium's scroll anchoring keeps the rows in place by itself; WebKit has none.
        const kept = before - scroller.scrollTop;
        if (kept < rowH) scroller.scrollTop = Math.max(0, scroller.scrollTop - (rowH - kept));
        requestAnimationFrame(() => { instant = false; });
    }

    // Bound to the list's scroller, which this row lives inside.
    function tension() {
        const scroller = shellElements().chatList;
        if (!scroller) return;

        let startY = 0, armed = false, tuckable = false, travel = 0, pulledAt = 0;
        const atTop = () => scroller.scrollTop <= 0;
        const commit = () => { revealed = true; pull = 0; };
        // A list too short to scroll the row away tucks it with the opposite gesture instead.
        const canTuck = () => revealed && !expanded && !forced && scroller.scrollHeight - scroller.clientHeight < rowH;

        const onTouchStart = (e) => {
            tuckable = e.touches.length === 1 && canTuck();
            armed = !shown && e.touches.length === 1 && atTop();
            startY = e.touches[0]?.clientY ?? 0;
            travel = 0;
        };
        const onTouchMove = (e) => {
            const dy = (e.touches[0]?.clientY ?? startY) - startY;
            if (tuckable && dy < -DRAG_PX / 3) {
                tuckable = false;
                revealed = false;
            }
            if (!armed) return;
            if (dy <= 0 || !atTop()) {
                if (dy < 0 || !atTop()) armed = false;
                pull = 0;
                dragging = false;
                return;
            }
            e.preventDefault();
            dragging = true;
            travel = dy;
            pull = dy * DRAG_RESIST;
        };
        const onTouchEnd = () => {
            if (!armed) return;
            armed = false;
            dragging = false;
            if (travel > 8) pulledAt = performance.now();
            if (travel >= DRAG_PX) commit();
            else pull = 0;
        };
        const onClick = (e) => {
            if (performance.now() - pulledAt < CLICK_SWALLOW_MS) {
                e.stopPropagation();
                e.preventDefault();
            }
        };

        let lastWheel = 0, fromTop = false, wheelTravel = 0, settle = 0;
        const onWheel = (e) => {
            const now = performance.now();
            if (now - lastWheel > GESTURE_GAP_MS) {
                fromTop = atTop();
                wheelTravel = 0;
            }
            lastWheel = now;
            const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? scroller.clientHeight : 1;
            const dy = e.deltaY * unit;
            if (dy > 0 && canTuck()) {
                revealed = false;
                return;
            }
            if (shown || !fromTop) return;
            if (dy > 0 || !atTop()) {
                fromTop = false;
                pull = 0;
                return;
            }
            wheelTravel -= dy;
            pull = rowH * Math.min(1, wheelTravel / WHEEL_PX) * 0.85;
            clearTimeout(settle);
            if (wheelTravel >= WHEEL_PX) commit();
            else settle = setTimeout(() => { if (!revealed) pull = 0; }, GESTURE_GAP_MS);
        };

        // Scrolled past while folded: tuck it away again without moving the rows on screen.
        const onScroll = () => {
            if (revealed && !expanded && !forced && rowH && scroller.scrollTop >= rowH) hide(scroller);
        };

        scroller.addEventListener('touchstart', onTouchStart, { passive: true });
        scroller.addEventListener('touchmove', onTouchMove, { passive: false });
        scroller.addEventListener('touchend', onTouchEnd, { passive: true });
        scroller.addEventListener('touchcancel', onTouchEnd, { passive: true });
        scroller.addEventListener('click', onClick, true);
        scroller.addEventListener('wheel', onWheel, { passive: true });
        scroller.addEventListener('scroll', onScroll, { passive: true });
        return {
            destroy() {
                clearTimeout(settle);
                scroller.removeEventListener('touchstart', onTouchStart);
                scroller.removeEventListener('touchmove', onTouchMove);
                scroller.removeEventListener('touchend', onTouchEnd);
                scroller.removeEventListener('touchcancel', onTouchEnd);
                scroller.removeEventListener('click', onClick, true);
                scroller.removeEventListener('wheel', onWheel);
                scroller.removeEventListener('scroll', onScroll);
            },
        };
    }
</script>

<div class="chatlist-archive-slot" class:settling={!dragging && !instant} style:height="{height}px" use:tension>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="chatlist-contact chatlist-archive" class:expanded bind:this={rowEl}
         style:opacity={shown ? null : Math.min(1, pull / (rowH || 1))}
         onclick={() => { if (shown) expanded = !expanded; }}>
        <div class="chatlist-archive-icon"><span class="icon icon-archive"></span></div>
        <div class="chatlist-contact-preview">
            <div class="chatlist-contact-header">
                <h4 class="cutoff">Archived Chats</h4>
                <span class="chatlist-contact-flags"><span class="chatlist-archive-count" class:has-unread={unread}>{chats.length}</span></span>
            </div>
            <div class="chatlist-contact-line"><p class="cutoff">{names}</p></div>
        </div>
        <div class="chatlist-expander" class:expanded><span class="icon icon-chevron-down"></span></div>
    </div>
</div>
{#if shown && expanded}
    <div class="chatlist-archived">{@render rows()}</div>
{/if}

<!-- No <style>: global styles.css cascades. -->
