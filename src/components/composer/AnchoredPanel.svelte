<script>
    // The positioned shell the three autocomplete panels share: fixed above the
    // composer, clamped to the viewport, faded through the `visible` class so the
    // last content stays painted while it goes. It measures the anchor on every
    // open and window resize; the composer grows as the draft does. A phone's
    // keyboard moves the composer without a window resize, so the visual viewport's
    // changes re-measure too, a frame later, once the page has refit around it.
    let { cls, open, anchor, maxWidth = 340, viewportInset = false, view, message = false, children } = $props();

    let el;

    function position() {
        const rect = (typeof anchor === 'function' ? anchor() : anchor).getBoundingClientRect();
        const margin = 10;
        const width = Math.min(rect.width, maxWidth, viewportInset ? window.innerWidth - margin * 2 : Infinity);
        const left = Math.max(margin, Math.min(rect.left, window.innerWidth - width - margin));
        el.style.left = left + 'px';
        el.style.bottom = (window.innerHeight - rect.top + 6) + 'px';
        el.style.width = width + 'px';
        // The room above the anchor, which a keyboard can make shorter than the panel.
        el.style.setProperty('--room', Math.max(120, rect.top - (window.visualViewport?.offsetTop || 0) - 16) + 'px');
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
        window.addEventListener('resize', position);
        vv?.addEventListener('resize', later);
        vv?.addEventListener('scroll', later);
        return () => {
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
