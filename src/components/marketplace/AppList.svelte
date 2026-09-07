<script>
    // The catalogue: loading, error, or the apps matching the search and every active filter,
    // newest first, under a title naming the view.
    import { mktState, mktApps } from '../lib/marketplace.svelte.js';
    import AppCard from './AppCard.svelte';
    let { h } = $props();
    const st = mktState();
    const q = $derived(st.query.toLowerCase().trim());
    const filtering = $derived(!!q || st.filters.length > 0);
    const shown = $derived.by(() => {
        let list = mktApps();
        if (q) list = list.filter(a => a.name.toLowerCase().includes(q) || a.description.toLowerCase().includes(q)
            || a.categories.some(c => c.toLowerCase().includes(q)));
        if (st.filters.length) list = list.filter(a => st.filters.every(f => a.categories.some(c => c.toLowerCase() === f)));
        return [...list].sort((a, b) => b.published_at - a.published_at);
    });
    const title = $derived(q ? { icon: 'icon-search', text: 'Search Results' }
        : st.filters.length ? { icon: 'icon-bookmark', text: st.filters.map(f => f.charAt(0).toUpperCase() + f.slice(1)).join(', ') }
        : { icon: 'icon-clock', text: 'New Arrivals' });
</script>

<div class="marketplace-content">
    {#if st.loading}
        <div class="marketplace-loading">
            <span class="icon icon-loading marketplace-loading-icon"></span>
            <p>Loading The Nexus...</p>
        </div>
    {:else if st.error}
        <div class="marketplace-error">
            <span class="icon icon-warning marketplace-error-icon"></span>
            <p>Failed to load Nexus</p>
            <p class="marketplace-error-hint">{st.error}</p>
            <button class="marketplace-retry-btn" onclick={h.retry}>Retry</button>
        </div>
    {:else if !shown.length}
        <div class="marketplace-empty">
            {#if filtering}
                <span class="icon icon-search marketplace-empty-icon"></span>
                <p>No apps found</p>
                <p class="marketplace-empty-hint">Try adjusting your search or filters</p>
            {:else}
                <span class="icon icon-gift marketplace-empty-icon"></span>
                <p>No apps available yet</p>
                <p class="marketplace-empty-hint">Check back later for new Mini Apps!</p>
            {/if}
        </div>
    {:else}
        <div class="marketplace-section-title" class:marketplace-animate-in={st.animate}><span class="icon {title.icon}"></span> {title.text}</div>
        {#each shown as app, i (app.id + "\n" + app.published_at)}
            <AppCard {app} {h} index={i} />
        {/each}
    {/if}
</div>
