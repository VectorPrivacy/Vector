<script>
    // The pack canvases' cell tooltip. Centre-anchored and nowrap, so it measures itself
    // and clamps the centre to keep both edges on screen.
    import { pickerTip } from '../lib/picker.svelte.js';
    const MARGIN = 6;
    const t = pickerTip();
    let el = $state(null);
    let left = $state(0);
    $effect(() => {
        t.text; t.x;
        if (!el || !t.visible) return;
        const half = el.offsetWidth / 2;
        const min = MARGIN + half;
        const max = window.innerWidth - MARGIN - half;
        left = max < min ? window.innerWidth / 2 : Math.min(max, Math.max(min, t.x));
    });
</script>

<div class="emoji-pack-canvas-tooltip" class:is-visible={t.visible} bind:this={el} style:left="{left}px" style:top="{t.y}px">{t.text}</div>
