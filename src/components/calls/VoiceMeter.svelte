<script>
    // A loudness bar, 0 to 1: green through the speaking range, amber near the top.
    // The peak holds for a moment so a word leaves a mark.
    let { level = 0, active = false } = $props();
    let peak = $state(0);
    let peakTimer = null;
    $effect(() => {
        if (level >= peak) {
            peak = level;
            clearTimeout(peakTimer);
            peakTimer = setTimeout(() => { peak = 0; }, 900);
        }
        return () => clearTimeout(peakTimer);
    });
</script>

<div class="voice-meter" class:voice-meter-active={active} class:voice-meter-hot={level > 0.88}>
    <div class="voice-meter-fill" style="width: {Math.round(level * 100)}%"></div>
    {#if active && peak > 0.02}
        <div class="voice-meter-peak" style="left: {Math.round(peak * 100)}%"></div>
    {/if}
</div>
