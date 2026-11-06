<script>
    // Drag or click to seek through the whole chat, and point to read the date there; the wheel
    // scrolls the list as usual.
    import { untrack } from 'svelte';
    import { chatScrollbar, chatScrollbarHandlers } from '../lib/scrollbar.svelte.js';

    const bar = chatScrollbar();
    const h = () => chatScrollbarHandlers();
    // A finger needs a handle it can find: touch draws a larger one that shows while scrolling.
    const fine = matchMedia('(hover: hover) and (pointer: fine)').matches;
    const MIN_THUMB = fine ? 32 : 40;

    // The thumb measures the view; held by a finger it becomes a grip of one fixed size.
    const thumbH = $derived(Math.min(bar.trackHeight, !fine && bar.dragging ? MIN_THUMB : Math.max(32, bar.size * bar.trackHeight)));
    const thumbY = $derived((bar.trackHeight - thumbH) * bar.top);

    let grab = -1;          // the pointer's offset into the thumb while dragging
    let hovering = $state(false);
    let hoverY = $state(0);
    // On the thumb, hovered or held, the date is where the view is: a press there takes hold, it goes nowhere.
    const onThumb = $derived(hovering && !bar.dragging && hoverY >= thumbY && hoverY <= thumbY + thumbH);
    // Above the track, the mark that more history may still come.
    const onMore = $derived(hovering && !bar.dragging && !!bar.more && hoverY < 0);
    const text = $derived(onMore ? bar.more : onThumb ? bar.here : bar.tip);
    const tipOn = $derived(!!text && (hovering || bar.dragging));
    // Where the date shows, and how; both stay put while it fades out.
    let tipY = $state(0);
    let here = $state(false);
    $effect(() => {
        if (!tipOn) return;
        here = bar.dragging || onThumb;
        tipY = here ? thumbY + thumbH / 2 : onMore ? -8 : hoverY;
    });

    // The mark that more history may come shows only to a pointer within reach of it.
    const MARK_REACH = 100;
    let markEl = $state(null);
    let markNear = $state(false);
    $effect(() => {
        const mark = markEl;
        if (!mark) { markNear = false; return; }
        let frame = 0, px = 0, py = 0;
        const check = () => {
            frame = 0;
            const r = mark.getBoundingClientRect();
            const dx = Math.max(r.left - px, 0, px - r.right), dy = Math.max(r.top - py, 0, py - r.bottom);
            markNear = Math.hypot(dx, dy) <= MARK_REACH;
        };
        const move = (e) => { px = e.clientX; py = e.clientY; if (!frame) frame = requestAnimationFrame(check); };
        const gone = () => { markNear = false; };
        window.addEventListener('pointermove', move, { passive: true });
        document.documentElement.addEventListener('pointerleave', gone);
        window.addEventListener('blur', gone);
        return () => {
            cancelAnimationFrame(frame);
            window.removeEventListener('pointermove', move);
            document.documentElement.removeEventListener('pointerleave', gone);
            window.removeEventListener('blur', gone);
        };
    });

    // The date follows its place with a short ease-out, frame-timed so a slow frame still lands
    // it where it should be. It appears at its place; the loop sleeps once it arrives.
    const FOLLOW_MS = 70;
    let shownY = $state(0);
    let raf = 0, last = 0, wasOn = false;
    function follow(now) {
        raf = 0;
        const d = tipY - shownY;
        if (Math.abs(d) < 0.25) { shownY = tipY; last = 0; return; }
        const dt = last ? Math.min(64, now - last) : 16;
        last = now;
        shownY += d * (1 - Math.exp(-dt / FOLLOW_MS));
        raf = requestAnimationFrame(follow);
    }
    $effect(() => {
        const y = tipY, on = tipOn;
        untrack(() => {
            if (on && !wasOn) shownY = y;
            wasOn = on;
            if (!raf && Math.abs(y - shownY) >= 0.25) { last = 0; raf = requestAnimationFrame(follow); }
        });
    });
    $effect(() => () => cancelAnimationFrame(raf));

    function y(e, node) { return e.clientY - node.getBoundingClientRect().top; }
    /** The place a press at `py` would put the view: the thumb held at `offset`. */
    function at(py, offset) {
        return Math.max(0, Math.min(1, (py - offset) / Math.max(1, bar.trackHeight - thumbH)));
    }
    function down(e) {
        if (e.button !== 0) return;
        const node = e.currentTarget, py = y(e, node);
        // On the thumb: hold it where it was taken. On the track: centre it under the pointer.
        grab = py >= thumbY && py <= thumbY + thumbH ? py - thumbY : thumbH / 2;
        node.setPointerCapture(e.pointerId);
        e.preventDefault();
        h().seek?.(at(py, grab), false);
    }
    function move(e) {
        const node = e.currentTarget, py = y(e, node);
        if (grab >= 0) { h().seek?.(at(py, grab), false); return; }
        hovering = true;
        hoverY = py < 0 ? -1 : Math.min(bar.trackHeight, py);
        if (py >= 0 && (py < thumbY || py > thumbY + thumbH)) h().point?.(at(py, thumbH / 2));
    }
    function up(e) {
        if (grab < 0) return;
        const py = y(e, e.currentTarget);
        h().seek?.(at(py, grab), true);
        hoverY = Math.max(0, Math.min(bar.trackHeight, py));   // let go on the thumb: still on it
        grab = -1;
    }
</script>

<!-- On touch, the date of the view floats at the top of the chat while it scrolls: the thumb
     dragging the handle covers whatever sits beside it. -->
{#if !fine}
    <div class="chat-float-date" class:on={bar.shown && !bar.edgeShown && (bar.active || bar.dragging) && !!(bar.dragging ? bar.tip : bar.here)}
         style:top="{bar.trackTop + 6}px" aria-hidden="true">{bar.dragging ? bar.tip : bar.here}</div>
{/if}
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="chat-scrollbar" class:shown={bar.shown} class:active={bar.active} class:dragging={bar.dragging} aria-hidden="true"
     style:top="{bar.trackTop}px" style:height="{bar.trackHeight}px"
     onpointerdown={down} onpointermove={move} onpointerup={up} onpointercancel={up}
     onpointerleave={() => { hovering = false; }} onwheel={(e) => h().wheel?.(e)}>
    {#if bar.more}<div class="chat-scrollbar-more" class:near={markNear} bind:this={markEl}></div>{/if}
    <div class="chat-scrollbar-thumb" style:height="{thumbH}px" style:transform="translateY({thumbY}px)"></div>
    <div class="chat-scrollbar-label" class:on={tipOn} class:here style:transform="translate3d(0, {shownY}px, 0)">{text}</div>
</div>
