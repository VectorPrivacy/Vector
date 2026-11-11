<script>
    // A long action's progress: a ring that fills as it goes, and the current stage's name,
    // which rises out as the next rises in. `stages`, every name it will show, sizes the label to
    // the longest, so nothing beside it moves as the names change.
    import { fly } from 'svelte/transition';
    import { cubicOut } from 'svelte/easing';
    let { label = '', pct = 0, size = 16, stages = [] } = $props();
    const R = 9;
    const C = 2 * Math.PI * R;
</script>

<span class="stage-progress">
    <svg class="stage-progress-ring" style:width="{size}px" style:height="{size}px" viewBox="0 0 24 24" aria-hidden="true">
        <circle cx="12" cy="12" r={R} class="stage-progress-track" />
        <circle cx="12" cy="12" r={R} class="stage-progress-fill" stroke-dasharray={C} stroke-dashoffset={C * (1 - Math.max(0, Math.min(1, pct)))} />
    </svg>
    <span class="stage-progress-label" role="status" aria-live="polite">
        {#each stages as stage (stage)}<span class="stage-progress-sizer">{stage}</span>{/each}
        {#key label}
            <span in:fly={{ y: 8, duration: 340, easing: cubicOut }} out:fly={{ y: -8, duration: 260, easing: cubicOut }}>{label}</span>
        {/key}
    </span>
</span>
