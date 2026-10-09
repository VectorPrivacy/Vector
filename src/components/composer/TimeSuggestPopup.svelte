<script>
    // The time suggestion: the phrase the draft ends with, as everyone would read it.
    import AnchoredPanel from './AnchoredPanel.svelte';
    import { composerPopup } from '../lib/composer.svelte.js';

    let { anchor } = $props();

    const open = $derived(composerPopup().kind === 'timesuggest');
    let view = $state.raw({ phrase: '', preview: '', full: '', countdown: false, key: false, x: null, accept: () => {} });
    $effect(() => { if (open) view = composerPopup(); });
</script>

<AnchoredPanel cls="time-suggest" {open} {anchor} {view} maxWidth={360} shrink atX={view.x}>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="time-suggest-row" title="{view.countdown ? 'Make it a countdown' : 'Show in everyone’s time'}: {view.full}"
         onmousedown={(e) => { e.preventDefault(); view.accept(); }}>
        <span class="icon icon-clock time-suggest-icon"></span>
        <span class="time-suggest-preview">{view.preview}</span>
        {#if view.key}<kbd class="time-suggest-key">Tab</kbd>{/if}
    </div>
</AnchoredPanel>
