<script>
    // Above the target, centred, clamped inside the viewport: a wide tooltip (a long URL)
    // over an edge-hugging target would otherwise bleed off-screen.
    import { tooltipState } from '../lib/tooltip.svelte.js';
    const tip = tooltipState();
    let el = $state(null);
    let pos = $state({ left: 0, top: 0 });
    $effect(() => {
        const rect = tip.rect;
        tip.text;
        if (!rect || !el) return;
        const pad = 8;
        const half = el.offsetWidth / 2;
        const centerX = rect.left + rect.width / 2;
        pos = { left: Math.max(pad + half, Math.min(window.innerWidth - pad - half, centerX)), top: rect.top - 8 };
    });
</script>

<div class="global-tooltip" class:visible={tip.visible} bind:this={el}
     style:left="{pos.left}px" style:top="{pos.top}px" style:transform="translate(-50%, -100%)">{tip.text}</div>
