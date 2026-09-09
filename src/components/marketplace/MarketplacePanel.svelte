<script>
    // The Nexus panel: back, search, the active filter tags and the scroll body. Renders
    // inside the panel container, which the opener shows, hides and animates.
    import { mktState, mktSetQuery } from '../lib/marketplace.svelte.js';
    import Filters from './Filters.svelte';
    import Marketplace from './Marketplace.svelte';
    let { h } = $props();   // h: back() plus the catalogue helpers
    const st = mktState();
</script>

<div class="marketplace-header">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="btn nav-back-btn marketplace-back-btn" onclick={h.back}>
        <span class="icon icon-chevron-double-left nav-icon"></span>
    </div>
    <h2 class="marketplace-title">Nexus</h2>
</div>
<div class="marketplace-search-container">
    <span class="marketplace-search-icon icon icon-search"></span>
    <input type="text" id="marketplace-search-input" placeholder="Search apps..." autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
           value={st.query} oninput={(e) => mktSetQuery(e.currentTarget.value)}>
</div>
<Filters />
<div class="marketplace-scroll-container"><Marketplace {h} /></div>
