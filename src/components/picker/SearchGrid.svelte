<script>
    // Search results for the query; the section shows only while there is one.
    import { pickerState } from '../lib/picker.svelte.js';
    import EmojiGrid from './EmojiGrid.svelte';
    let { section, h } = $props();
    const st = pickerState();
    const items = $derived(st.query ? h.search(st.query) : []);
    $effect(() => { section.hidden = !st.query; });
</script>
<EmojiGrid {items} {h} />
