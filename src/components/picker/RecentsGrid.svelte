<script>
    // Recently used: stock and custom merged by use, re-derived when usage moves.
    import { pickerState } from '../lib/picker.svelte.js';
    import EmojiGrid from './EmojiGrid.svelte';
    let { h } = $props();
    const st = pickerState();
    const items = $derived.by(() => { st.recentsSeq; return h.recents(); });
</script>
<EmojiGrid {items} {h} />
