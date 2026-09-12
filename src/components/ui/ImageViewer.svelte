<script>
    // Fullscreen image viewer. A press on the backdrop closes unless it turned into a
    // drag; the image's own gestures belong to previewer.js through the handlers bag.
    import { imageViewerState, imageViewerEls, imageViewerHandlers } from '../lib/imageviewer.svelte.js';
    const v = imageViewerState();
    const els = imageViewerEls();
    const h = () => imageViewerHandlers();

    let pressX = 0, pressY = 0, dragged = false;
    function pressStart(x, y) { pressX = x; pressY = y; dragged = false; }
    function pressMove(x, y) { if (Math.hypot(x - pressX, y - pressY) > 5) dragged = true; }
    function pressEnd() { if (!dragged) h().close?.(); }
    function bindContainer(node) { els.container = node; return { destroy() { els.container = null; } }; }
    function bindImage(node) { els.image = node; return { destroy() { els.image = null; } }; }
</script>

{#if v.open}
    <div class="image-viewer-overlay" class:active={v.active} style="display: flex;">
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="image-viewer-container" use:bindContainer
             onwheel={(e) => h().wheel?.(e)}
             onmousedown={(e) => { if (e.target === e.currentTarget) pressStart(e.clientX, e.clientY); }}
             onmousemove={(e) => { if (e.target === e.currentTarget) pressMove(e.clientX, e.clientY); }}
             onmouseup={(e) => { if (e.target === e.currentTarget) pressEnd(); }}
             ontouchstart={(e) => { if (e.target === e.currentTarget && e.touches.length === 1) pressStart(e.touches[0].clientX, e.touches[0].clientY); }}
             ontouchmove={(e) => { if (e.target === e.currentTarget && e.touches.length === 1) pressMove(e.touches[0].clientX, e.touches[0].clientY); }}
             ontouchend={(e) => { if (e.target === e.currentTarget) pressEnd(); }}>
            <!-- The image is the zoom and pan gesture surface. -->
            <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
            <img class="image-viewer-image" class:zoomed={v.zoomed} class:no-anim={v.settling || v.noAnim} class:dragging={v.dragging} draggable="false" alt=""
                 src={v.src} use:bindImage style:transform={v.transform} style:visibility={v.settling ? 'hidden' : null}
                 onload={() => h().load?.()} onerror={() => h().error?.()}
                 onmousedown={(e) => h().mouseDown?.(e)}
                 ontouchstart={(e) => h().touchStart?.(e)} ontouchmove={(e) => h().touchMove?.(e)} ontouchend={(e) => h().touchEnd?.(e)}>
        </div>
        <button class="image-viewer-close" aria-label="Close" onclick={() => h().close?.()}></button>
        <div class="image-viewer-controls">
            <button class="image-viewer-ctrl-btn" aria-label="Rotate image" onclick={() => h().rotate?.()}>
                <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M2 10C2 10 4.00498 7.26822 5.63384 5.63824C7.26269 4.00827 9.5136 3 12 3C16.9706 3 21 7.02944 21 12C21 16.9706 16.9706 21 12 21C7.89691 21 4.43511 18.2543 3.35177 14.5M2 10V4M2 10H8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
            </button>
        </div>
        <div class="image-viewer-zoom-info" class:visible={v.zoom.visible}>{v.zoom.text}</div>
        <div class="image-viewer-tip" class:visible={v.tip.visible}>{v.tip.text}</div>
    </div>
{/if}
