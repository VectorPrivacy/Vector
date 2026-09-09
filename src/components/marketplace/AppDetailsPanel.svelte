<script>
    // The App Details panel: back and the app in full. Renders inside the panel
    // container, which the opener shows, hides and animates.
    import AppDetails from './AppDetails.svelte';
    let { h } = $props();   // h: closeDetails() plus the details helpers
</script>

<div class="app-details-header">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="btn nav-back-btn app-details-back-btn" onclick={() => h.closeDetails()}>
        <span class="icon icon-chevron-double-left nav-icon"></span>
    </div>
    <h2 class="app-details-title">App Details</h2>
    <div class="app-details-header-spacer"></div>
</div>
<div class="app-details-content"><AppDetails {h} /></div>
