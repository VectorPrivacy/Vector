<script>
    // A list of apps: everything, one category, what's installed, what has an update, or
    // what a search finds. Catalogue pages list what fits this device unless that filter is
    // off, and say how many they left out; what you have is always listed.
    import { mktState, mktApps, mktPlayed, mktActions, mktDevice, mktFits } from '../lib/marketplace.svelte.js';
    import { fitsDevice, inCategory, isInstalled, matches, newestFirst, searchRank } from '../lib/nexus.js';
    import AppRow from './AppRow.svelte';
    let { h } = $props();   // h: MarketplaceHelpers
    const st = mktState();

    const q = $derived(st.query.toLowerCase().trim());
    const lastPlayed = $derived(new Map(mktPlayed().map(p => [p.id, p.at])));
    const dev = mktDevice();
    const yours = $derived(!q && (st.view === 'library' || st.view === 'updates'));
    const all = $derived.by(() => {
        const apps = mktApps();
        if (q) return apps.filter(a => matches(a, q)).sort((a, b) => searchRank(a, q) - searchRank(b, q) || b.published_at - a.published_at);
        if (st.view === 'library') return apps.filter(isInstalled)
            .sort((a, b) => (lastPlayed.get(b.id) || 0) - (lastPlayed.get(a.id) || 0) || a.name.localeCompare(b.name));
        if (st.view === 'updates') return apps.filter(a => a.update_available);
        if (st.view === 'category') return newestFirst(apps.filter(a => inCategory(a, st.category)));
        return newestFirst(apps);
    });
    const list = $derived(yours ? all : all.filter(mktFits));
    const unfit = $derived(yours ? 0 : all.filter(a => !fitsDevice(a, dev)).length);
    const updates = $derived(mktApps().filter(a => a.update_available));
    const updating = $derived(updates.some(a => mktActions().get(a.id)));
    const showUpdateAll = $derived(!q && (st.view === 'library' || st.view === 'updates') && updates.length > 0);
    const note = (app) => {
        if (st.view !== 'library' || q) return '';
        const at = lastPlayed.get(app.id);
        return at ? `Played ${h.playedAgo(at)}` : 'Not played yet';
    };
</script>

{#if showUpdateAll}
    <div class="nx-banner">
        <span class="nx-g g-up nx-banner-g"></span>
        <span class="nx-banner-text">{updates.length === 1 ? '1 update ready' : `${updates.length} updates ready`}</span>
        <button type="button" class="nx-btn accent nx-sm" disabled={updating} onclick={() => h.updateAll()}>{updating ? 'Updating' : 'Update All'}</button>
    </div>
{/if}

{#if list.length}
    <div class="nx-rows">
        {#each list as app, i (app.id)}<AppRow {app} {h} index={i} note={note(app)} />{/each}
    </div>
{:else if q && !unfit}
    <div class="nx-empty">
        <span class="nx-g g-search nx-empty-g"></span>
        <p class="nx-empty-title">No results for “{st.query.trim()}”</p>
        <p class="nx-empty-hint">Try a name, a maker, or a kind of game</p>
    </div>
{:else if st.view === 'library' && !q}
    <div class="nx-empty">
        <span class="nx-g g-grid nx-empty-g"></span>
        <p class="nx-empty-title">Nothing installed yet</p>
        <p class="nx-empty-hint">Apps you install from the Nexus live here</p>
        <button type="button" class="nx-btn accent" onclick={() => h.show('discover')}>Discover Apps</button>
    </div>
{:else if st.view === 'updates' && !q}
    <div class="nx-empty">
        <span class="nx-g g-check nx-empty-g"></span>
        <p class="nx-empty-title">You're up to date</p>
        <p class="nx-empty-hint">Every app you have is on its newest version</p>
    </div>
{:else}
    <div class="nx-empty">
        <span class="nx-g {dev.touch && !dev.keys ? 'g-touch' : 'g-keys'} nx-empty-g"></span>
        <p class="nx-empty-title">Nothing here fits this device</p>
        <p class="nx-empty-hint">These are made for other controls</p>
    </div>
{/if}
{#if unfit}
    <div class="nx-fit-note">
        <span class="nx-g {dev.touch && !dev.keys ? 'g-touch' : 'g-keys'}"></span>
        {#if st.fitOnly}
            <span>{unfit === 1 ? '1 more is' : `${unfit} more are`} made for other controls</span>
            <button type="button" class="nx-link" onclick={() => h.setFitOnly(false)}>Show</button>
        {:else}
            <span>Showing what may not fit this device</span>
            <button type="button" class="nx-link" onclick={() => h.setFitOnly(true)}>Hide</button>
        {/if}
    </div>
{/if}
