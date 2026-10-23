<script>
    // A titled row that scrolls sideways, with a See All when there's more than it shows and
    // arrows for a pointer that can't swipe. The edges fade only where more waits.
    let { title, glyph = '', more = null, children } = $props();   // glyph: an nx-g class; more: opens the full list
    let track = $state(null);
    let atStart = $state(true);
    let atEnd = $state(true);

    function measure() {
        if (!track) return;
        atStart = track.scrollLeft <= 2;
        atEnd = track.scrollLeft + track.clientWidth >= track.scrollWidth - 2;
    }
    $effect(() => {
        if (!track) return;
        // Its size, or its tiles: either can move where the ends are.
        const ro = new ResizeObserver(measure);
        const mo = new MutationObserver(measure);
        ro.observe(track);
        mo.observe(track, { childList: true });
        measure();
        return () => { ro.disconnect(); mo.disconnect(); };
    });
    // A page of tiles at a time, landing on a tile's edge.
    function page(dir) {
        track?.scrollBy({ left: dir * Math.max(160, track.clientWidth * 0.8), behavior: 'smooth' });
    }
</script>

<section class="nx-shelf">
    <div class="nx-shelf-head">
        <h3 class="nx-h3">{#if glyph}<span class="nx-g {glyph} nx-shelf-g"></span>{/if}{title}</h3>
        {#if more}<button type="button" class="nx-link" onclick={more}>See All</button>{/if}
        <div class="nx-shelf-arrows">
            <button type="button" class="nx-arrow" aria-label="Scroll back" disabled={atStart} onclick={() => page(-1)}><span class="nx-g g-left"></span></button>
            <button type="button" class="nx-arrow" aria-label="Scroll on" disabled={atEnd} onclick={() => page(1)}><span class="nx-g g-right"></span></button>
        </div>
    </div>
    <div class="nx-shelf-track" class:fade-start={!atStart} class:fade-end={!atEnd} bind:this={track} onscroll={measure}>
        {@render children()}
    </div>
</section>
