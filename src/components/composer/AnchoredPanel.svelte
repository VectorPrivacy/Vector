<script>
    // The positioned shell the three autocomplete panels share: fixed above the
    // composer, clamped to the viewport, faded through the `visible` class so the
    // last content stays painted while it goes. It measures the anchor on every
    // open and window resize; the composer grows as the draft does. A phone's
    // keyboard moves the composer without a window resize, so the visual viewport's
    // changes re-measure too, a frame later, once the page has refit around it.
    // `shrink` sizes to the content (up to the same width) instead of the composer's width;
    // `atX` starts it at that screen x, kept inside the composer. It is placed by its top,
    // its own height above the composer: a home-screen app's innerHeight can shrink with
    // the keyboard while the frame it positions against doesn't, so a bottom offset taken
    // from it lands a keyboard's height too low.
    let { cls, open, anchor, maxWidth = 340, viewportInset = false, shrink = false, atX = null, view, message = false, children } = $props();

    let el;

    function position() {
        const rect = (typeof anchor === 'function' ? anchor() : anchor).getBoundingClientRect();
        const margin = 10;
        const width = Math.min(rect.width, maxWidth, viewportInset ? window.innerWidth - margin * 2 : Infinity);
        const left = Math.max(margin, Math.min(rect.left, window.innerWidth - width - margin));
        el.style.left = left + 'px';
        if (shrink) el.style.maxWidth = width + 'px';
        else el.style.width = width + 'px';
        if (atX != null) {
            const room = Math.min(rect.right, window.innerWidth - margin) - el.offsetWidth;
            el.style.left = Math.max(left, Math.min(atX, room)) + 'px';
        }
        // The room above the anchor, which a keyboard can make shorter than the panel.
        el.style.setProperty('--room', Math.max(120, rect.top - (window.visualViewport?.offsetTop || 0) - 16) + 'px');
        el.style.bottom = 'auto';
        el.style.top = Math.max(margin, rect.top - el.offsetHeight - 6) + 'px';
    }

    let frame = 0;
    function later() {
        cancelAnimationFrame(frame);
        frame = requestAnimationFrame(position);
    }

    $effect(() => {
        if (!open) return;
        view;
        position();
        const vv = window.visualViewport;
        // Its own height changes too (a list filtering, a hint growing): its top follows.
        const ro = new ResizeObserver(later);
        ro.observe(el);
        window.addEventListener('resize', position);
        vv?.addEventListener('resize', later);
        vv?.addEventListener('scroll', later);
        return () => {
            ro.disconnect();
            cancelAnimationFrame(frame);
            window.removeEventListener('resize', position);
            vv?.removeEventListener('resize', later);
            vv?.removeEventListener('scroll', later);
        };
    });
</script>

<div bind:this={el} class="{cls}{message ? ' command-selector--message' : ''}" class:visible={open}>
    {@render children()}
</div>
