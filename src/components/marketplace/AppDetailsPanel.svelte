<script>
    // The App Details panel: back and the app in full. Owns its panel element: shown
    // while open, the closing animation settles the store.
    import { mktState, mktClosingEnded } from '../lib/marketplace.svelte.js';
    import AppDetails from './AppDetails.svelte';
    let { h } = $props();   // h: closeDetails() plus the details helpers
    const st = mktState();
</script>

<div class="app-details-panel" id="app-details-panel" data-app-id={st.detailsId || undefined} style:display={st.detailsOpen ? 'flex' : 'none'} class:closing={st.detailsClosing}
     onanimationend={(e) => { if (e.target === e.currentTarget && st.detailsClosing) mktClosingEnded('details'); }}>

<div class="app-details-header">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="btn nav-back-btn app-details-back-btn" onclick={() => h.closeDetails()}>
        <span class="icon icon-chevron-double-left nav-icon"></span>
    </div>
    <h2 class="app-details-title">App Details</h2>
    <div class="app-details-header-spacer"></div>
</div>
<div class="app-details-content"><AppDetails {h} /></div>
</div>
