<script>
    // The positioned shell the three autocomplete panels share: fixed above the
    // composer, clamped to the viewport, faded through the `visible` class so the
    // last content stays painted while it goes. It measures the anchor on every
    // open and window resize; the composer grows as the draft does.
    let { cls, open, anchor, maxWidth = 340, viewportInset = false, view, message = false, children } = $props();

    let el;

    function position() {
        const rect = anchor.getBoundingClientRect();
        const margin = 10;
        const width = Math.min(rect.width, maxWidth, viewportInset ? window.innerWidth - margin * 2 : Infinity);
        const left = Math.max(margin, Math.min(rect.left, window.innerWidth - width - margin));
        el.style.left = left + 'px';
        el.style.bottom = (window.innerHeight - rect.top + 6) + 'px';
        el.style.width = width + 'px';
    }

    $effect(() => {
        if (!open) return;
        view;
        position();
        window.addEventListener('resize', position);
        return () => window.removeEventListener('resize', position);
    });
</script>

<div bind:this={el} class="{cls}{message ? ' command-selector--message' : ''}" class:visible={open}>
    {@render children()}
</div>
