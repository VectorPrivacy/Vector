<script>
    // The @time picker: each style, previewed for the time typed so far.
    import AnchoredPanel from './AnchoredPanel.svelte';
    import { composerPopup } from '../lib/composer.svelte.js';

    let { anchor } = $props();

    const open = $derived(composerPopup().kind === 'time');
    let view = $state.raw({ query: '', full: '', rows: [], active: 0, pick: () => {} });
    $effect(() => { if (open) view = composerPopup(); });
</script>

<AnchoredPanel cls="mention-selector time-selector" {open} {anchor} {view}>
    <div class="mention-selector-header">Time{#if view.query}<span class="time-selector-query"> · {view.query}</span>{/if}</div>
    {#if view.rows.length}
        <div class="time-selector-full">{view.full}</div>
        {#each view.rows as row, i (row.style)}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
                class="mention-item time-item"
                class:active={i === view.active}
                onmousedown={(e) => { e.preventDefault(); view.pick(i); }}
            >
                <span class="time-item-preview">{row.preview}</span>
                <span class="time-item-name">{row.name}</span>
            </div>
        {/each}
    {:else}
        <div class="time-selector-hint">Try “tomorrow 5pm”, “friday 9:30”, “dec 25” or “in 2 hours”</div>
    {/if}
</AnchoredPanel>
