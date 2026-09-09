<script>
    // Search results for the query; the panel shows the section while there is one.
    import { pickerState } from '../lib/picker.svelte.js';
    import EmojiGrid from './EmojiGrid.svelte';
    let { h } = $props();
    const st = pickerState();
    const items = $derived(st.query ? h.search(st.query) : []);
</script>
<EmojiGrid {items} {h} />
