<script>
    // Both Nexus panels mount once marketplace.js has registered the bag.
    import { mktHandlers } from '../lib/marketplace.svelte.js';
    import MarketplacePanel from './MarketplacePanel.svelte';
    import AppDetailsPanel from './AppDetailsPanel.svelte';
    const h = $derived(mktHandlers());
</script>

{#if h}
    <MarketplacePanel {h} />
    <AppDetailsPanel {h} />
{/if}
