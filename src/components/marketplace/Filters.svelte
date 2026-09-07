<script>
    // The active category filters as removable tags; nothing rendered when none are set.
    import { mktState, mktRemoveFilter } from '../lib/marketplace.svelte.js';
    const st = mktState();
</script>

{#if st.filters.length}
    <div class="marketplace-filters">
        <div class="marketplace-active-filters">
            {#each st.filters as f (f)}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="marketplace-filter-tag" onclick={() => mktRemoveFilter(f)}>
                    <span>{f}</span>
                    <span class="icon icon-x"></span>
                </div>
            {/each}
        </div>
    </div>
{/if}
