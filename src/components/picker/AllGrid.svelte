<script>
    // Every stock emoji, twemojified lazily as it scrolls into view.
    import EmojiGrid from './EmojiGrid.svelte';
    let { grid, h } = $props();
    // svelte-ignore state_referenced_locally
    const items = h.all();
</script>
<EmojiGrid {items} {grid} lazy {h} />
