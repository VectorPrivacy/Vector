<script>
    // The Nexus and its Play with Friends picker mount once marketplace.js has registered the bag.
    import { mktHandlers } from '../lib/marketplace.svelte.js';
    import Nexus from './Nexus.svelte';
    import PlayWith from './PlayWith.svelte';
    const h = $derived(mktHandlers());
</script>

{#if h}
    <Nexus {h} />
    <PlayWith />
{/if}
