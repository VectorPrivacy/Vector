<script>
    // Drag or click to seek through the whole chat; the wheel scrolls the list as usual.
    import { chatScrollbar, chatScrollbarHandlers } from '../lib/scrollbar.svelte.js';

    const bar = chatScrollbar();
    const h = () => chatScrollbarHandlers();
    const MIN_THUMB = 32;

    const thumbH = $derived(Math.min(bar.trackHeight, Math.max(MIN_THUMB, bar.size * bar.trackHeight)));
    const thumbY = $derived((bar.trackHeight - thumbH) * bar.top);

    let grab = -1;   // the pointer's offset into the thumb while dragging
    function at(e, node) {
        const y = e.clientY - node.getBoundingClientRect().top - grab;
        return Math.max(0, Math.min(1, y / Math.max(1, bar.trackHeight - thumbH)));
    }
    function down(e) {
        if (e.button !== 0) return;
        const node = e.currentTarget;
        const y = e.clientY - node.getBoundingClientRect().top;
        // On the thumb: hold it where it was taken. On the track: centre it under the pointer.
        grab = y >= thumbY && y <= thumbY + thumbH ? y - thumbY : thumbH / 2;
        node.setPointerCapture(e.pointerId);
        e.preventDefault();
        h().seek?.(at(e, node), false);
    }
    function move(e) {
        if (grab < 0) return;
        h().seek?.(at(e, e.currentTarget), false);
    }
    function up(e) {
        if (grab < 0) return;
        h().seek?.(at(e, e.currentTarget), true);
        grab = -1;
    }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="chat-scrollbar" class:shown={bar.shown} class:dragging={bar.dragging} aria-hidden="true"
     style:top="{bar.trackTop}px" style:height="{bar.trackHeight}px"
     onpointerdown={down} onpointermove={move} onpointerup={up} onpointercancel={up}
     onwheel={(e) => h().wheel?.(e)}>
    <div class="chat-scrollbar-thumb" style:height="{thumbH}px" style:transform="translateY({thumbY}px)"></div>
    {#if bar.dragging && bar.label}
        <div class="chat-scrollbar-label" style:transform="translateY({thumbY + thumbH / 2}px)">{bar.label}</div>
    {/if}
</div>
