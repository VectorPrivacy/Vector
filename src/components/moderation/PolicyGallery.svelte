<script>
    // The templates. "Start from scratch" answers a different question, so it gets its own
    // row; the defaults are already the row above, whose "See the rules" opens the same editor.
    import { polPresets } from '../lib/policy.svelte.js';
    let { h } = $props();
    const shown = $derived(polPresets().filter(x => x.id !== 'blank' && x.id !== 'vector_defaults'));
    const blank = $derived(polPresets().find(x => x.id === 'blank'));
</script>

<div class="pol-gallery">
    {#each shown as p (p.id)}
        <button class="pol-card" onclick={() => h.open(p)}>
            <div class="pol-card-name">{p.name}</div>
            <div class="pol-card-desc">{p.description}</div>
            <div class="pol-card-eg">Catches: {p.example}</div>
        </button>
    {/each}
</div>

<div class="pol-scratch">
    {#if blank}
        <button class="pol-scratch-btn" onclick={() => h.open(blank)}>
            <span class="pol-scratch-plus">+</span>
            <span class="pol-scratch-text">
                <span class="pol-scratch-name">{blank.name}</span>
                <span class="pol-scratch-desc">{blank.description}</span>
            </span>
        </button>
    {/if}
</div>
