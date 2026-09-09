<script>
    // The pack details modal and its body-level overlay (the deep link and the share flow
    // open it). A click on the backdrop itself dismisses.
    import { packDetails, closePackDetails } from '../lib/packdetails.svelte.js';
    import PackDetailsModal from './PackDetailsModal.svelte';

    let { h } = $props();   // h: the modal body's bag
    const d = $derived(packDetails());
</script>

<svelte:document onkeydown={(e) => { if (e.key === 'Escape' && d) closePackDetails(); }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="pack-details-overlay" class="pack-details-overlay" hidden={!d} onclick={(e) => { if (e.target === e.currentTarget) closePackDetails(); }}>
<div class="pack-details-card" id="pack-details-card">
    <button type="button" class="pack-details-close" id="pack-details-close" aria-label="Close" onclick={closePackDetails}>
        <span class="icon icon-x"></span>
    </button>
    <div class="pack-details-body" id="pack-details-body">
        <PackDetailsModal {h} />
    </div>
</div>
</div>
