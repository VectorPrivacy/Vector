<script>
    // The recent Mini Apps grid: Nexus first, then apps by last use. Search hides Nexus and
    // reveals hidden apps that match; edit mode adds a delete badge to everything but Nexus.
    import { gridState, gridApps } from '../lib/miniappsgrid.svelte.js';
    let { h } = $props();
    const st = gridState();
    const q = $derived(st.query.toLowerCase().trim());
    const searching = $derived(q.length > 0);
    const rows = $derived(gridApps().map(a => {
        const matches = !searching || a.name.toLowerCase().includes(q);
        const shown = a.hidden ? (searching && matches) : matches;
        return { a, shown };
    }));
    const visible = $derived(rows.filter(r => r.shown).length + (searching ? 0 : 1));

    function tip(a) { return a.pivx ? (a.hidden ? 'Restore PIVX Wallet' : 'PIVX Wallet') : a.name; }
</script>

<button class="attachment-panel-item" id="attachment-panel-marketplace" draggable="false"
        class:hidden-by-search={searching} onclick={h.openNexus}>
    <div class="attachment-panel-btn attachment-panel-marketplace-btn">
        <span class="icon icon-nexus"></span>
    </div>
    <span class="attachment-panel-label">Nexus</span>
</button>
{#each rows as { a, shown } (a.key)}
    <button class="attachment-panel-item" class:attachment-panel-miniapp={!a.pivx} class:miniapp-disabled={a.hidden}
            class:hidden-by-search={!shown} id={a.pivx ? 'attachment-panel-pivx' : undefined} draggable="false"
            style:display={a.hidden && !shown ? 'none' : undefined}
            onclick={() => h.open(a)}
            onmouseenter={(e) => h.showTip(tip(a), e.currentTarget)} onmouseleave={h.hideTip}>
        <div class="attachment-panel-btn" class:attachment-panel-pivx-btn={a.pivx} class:attachment-panel-miniapp-btn={!a.pivx}>
            {#if a.pivx}
                <span class="icon icon-pivx"></span>
            {:else if a.icon}
                <img src={a.icon} alt={a.name} class="attachment-panel-miniapp-icon" onerror={() => h.iconFailed(a)}>
            {:else}
                <span class="icon icon-play"></span>
            {/if}
            {#if a.downloading}
                <div class="miniapp-downloading-overlay">
                    <div class="miniapp-downloading-spinner" data-app-id={a.marketplaceId}></div>
                </div>
            {/if}
        </div>
        <span class="attachment-panel-label" class:cutoff={!a.pivx}>{a.name}</span>
        {#if a.hasUpdate && !a.downloading}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="miniapp-update-badge" role="button" tabindex="-1" onclick={(e) => { e.stopPropagation(); h.update(a); }}>
                <span class="icon icon-arrow-up"></span>
            </div>
        {/if}
        {#if st.editMode && !a.downloading}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="miniapp-delete-badge" role="button" tabindex="-1" onclick={(e) => { e.stopPropagation(); e.preventDefault(); h.remove(a); }}>
                <span class="icon icon-x"></span>
            </div>
        {/if}
    </button>
{/each}
{#if st.empty && !searching}
    <div class="attachment-panel-empty">No recent Mini Apps</div>
{/if}
{#if searching && visible === 0}
    <div class="miniapps-no-results">
        <p>No Mini Apps found</p>
        <p class="miniapps-no-results-hint">Try a different search, or check out the Nexus!</p>
    </div>
{/if}
