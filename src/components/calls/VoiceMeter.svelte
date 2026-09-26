<script>
    // A loudness bar, 0 to 1. The gradient spans the whole track, green through the
    // speaking range to red where a voice clips; the level uncovers as much of it as it
    // reaches, so a colour always means the same loudness.
    let { level = 0, active = false } = $props();
    const hidden = $derived(100 - Math.round(Math.max(0, Math.min(1, level)) * 100));
</script>

<div class="voice-meter" class:voice-meter-active={active}>
    <div class="voice-meter-fill" style:clip-path="inset(0 {hidden}% 0 0 round 4px)"></div>
</div>
