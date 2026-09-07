<script>
    // The front page above the list: the Multiplayer spread and the popular category pills.
    // Hidden while searching or filtering.
    import { mktState, mktApps, mktAddFilter } from '../lib/marketplace.svelte.js';
    import AppIcon from './AppIcon.svelte';
    let { h } = $props();
    const st = mktState();
    const browsing = $derived(!st.query.trim() && st.filters.length === 0);
    const inCategory = (app, c) => app.categories.some(x => x.toLowerCase() === c);
    const multiplayer = $derived(mktApps().filter(a => inCategory(a, 'multiplayer')));
    const spread = $derived([...multiplayer].sort((a, b) => b.published_at - a.published_at).slice(0, 4));
    const popular = $derived.by(() => {
        const count = {};
        for (const app of mktApps()) for (const c of app.categories) { const n = c.toLowerCase(); count[n] = (count[n] || 0) + 1; }
        return Object.entries(count).map(([name, n]) => ({ name, n }))
            .sort((a, b) => b.n - a.n).slice(0, 8)
            .filter(c => !['multiplayer', 'game', 'app'].includes(c.name));
    });
</script>

<div class="marketplace-featured">
    {#if browsing}
        {#if spread.length}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="marketplace-featured-category highlighted" class:marketplace-animate-in={st.animate}
                 style:animation-delay={st.animate ? '0s' : undefined} onclick={() => mktAddFilter('multiplayer')}>
                <div class="marketplace-featured-header">
                    <div class="marketplace-featured-title">
                        Multiplayer
                        <span class="marketplace-featured-count">{multiplayer.length} {multiplayer.length === 1 ? 'app' : 'apps'}</span>
                    </div>
                </div>
                <p class="marketplace-featured-description">Team up with friends and dive into the Vectorverse together</p>
                <div class="marketplace-card-spread">
                    {#each spread as app (app.id)}
                        <div class="marketplace-card-spread-item"><AppIcon {app} {h} /></div>
                    {/each}
                </div>
            </div>
        {/if}
        {#if popular.length}
            <div class="marketplace-popular-categories" class:marketplace-animate-in={st.animate}
                 style:animation-delay={st.animate ? '0.05s' : undefined}>
                {#each popular as c (c.name)}
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                    <div class="marketplace-popular-category" onclick={() => mktAddFilter(c.name)}>{c.name}</div>
                {/each}
            </div>
        {/if}
    {/if}
</div>
