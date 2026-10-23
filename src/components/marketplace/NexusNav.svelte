<script>
    // The Nexus's own column beside the rail: search, the pages, and every category with
    // its count (the rarer ones fold away behind More), then this device: the controls it
    // has, and whether the catalogue lists only what fits them.
    import { mktState, mktApps, mktSetQuery, mktDevice, mktFits } from '../lib/marketplace.svelte.js';
    import { isInstalled, isGame, tagCounts, tagLabel } from '../lib/nexus.js';
    let { h, searchField } = $props();   // searchField: an action on the field, for the `/` shortcut
    const st = mktState();
    const FEW = 8;

    let more = $state(false);
    const dev = mktDevice();
    const fit = $derived(mktApps().filter(mktFits));
    const installed = $derived(mktApps().filter(isInstalled).length);
    const updates = $derived(mktApps().filter(a => a.update_available).length);
    const tools = $derived(fit.filter(a => !isGame(a)).length);   // the 'app' page: inCategory
    const cats = $derived(tagCounts(fit));
    const controls = $derived([dev.touch && 'Touch', dev.keys && 'Keyboard & Mouse', dev.pad && 'Controller'].filter(Boolean).join(' · '));
    const on = (view, cat = '') => !st.query.trim() && !st.detailsId && st.view === view && (view !== 'category' || st.category === cat);
</script>

{#snippet row(view, cat, glyph, label, n, badge)}
    <button type="button" class="cs-section nx-nav-row" class:sub={!glyph} class:active={on(view, cat)} onclick={() => h.show(view, cat)}>
        {#if glyph}<span class="nx-g {glyph} nx-nav-g"></span>{/if}
        <span class="nx-nav-label">{label}</span>
        {#if badge}<span class="nx-badge">{n}</span>{:else if n}<span class="nx-nav-n">{n}</span>{/if}
    </button>
{/snippet}

<aside class="cs-nav nx-nav">
    <div class="cs-search" class:has-query={!!st.query}>
        <span class="nx-g g-search nx-search-g"></span>
        <input type="text" placeholder="Search the Nexus" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
               value={st.query} oninput={(e) => { if (st.detailsId) h.closeDetails(); mktSetQuery(e.currentTarget.value); }} use:searchField
               onkeydown={(e) => { if (e.key === 'Escape' && st.query) { e.stopPropagation(); mktSetQuery(''); } }}>
        {#if st.query}
            <button type="button" class="cs-search-clear" aria-label="Clear search" onclick={() => mktSetQuery('')}>
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
            </button>
        {/if}
    </div>
    <div class="cs-nav-scroll">
        <div class="cs-nav-head">
            <span class="nx-g g-nexus nx-nav-glyph"></span>
            <span class="cs-nav-title">Nexus</span>
        </div>
        {@render row('discover', '', 'g-globe', 'Discover', 0, false)}
        {@render row('library', '', 'g-grid', 'Library', installed, false)}
        {#if updates}{@render row('updates', '', 'g-up', 'Updates', updates, true)}{/if}
        {@render row('all', '', 'g-lines', 'All Apps', fit.length, false)}

        {#if cats.length}
            <div class="st-group nx-nav-group">Categories</div>
            {#if tools}{@render row('category', 'app', '', 'Apps & Tools', tools, false)}{/if}
            {#each cats.slice(0, FEW) as c (c.tag)}{@render row('category', c.tag, '', tagLabel(c.tag), c.n, false)}{/each}
            {#if cats.length > FEW}
                <div class="cs-fold" class:shut={!more}>
                    <div class="cs-fold-inner">
                        {#each cats.slice(FEW) as c (c.tag)}{@render row('category', c.tag, '', tagLabel(c.tag), c.n, false)}{/each}
                    </div>
                </div>
                <button type="button" class="nx-nav-more" onclick={() => { more = !more; }}>
                    {more ? 'Fewer' : `${cats.length - FEW} More`}<span class="nx-g g-down" class:up={more}></span>
                </button>
            {/if}
        {/if}

        <div class="st-group nx-nav-group">This Device</div>
        <div class="nx-device">
            <div class="nx-device-glyphs">
                <span class="nx-g g-touch" class:on={dev.touch} title="Touch"></span>
                <span class="nx-g g-keys" class:on={dev.keys} title="Keyboard & Mouse"></span>
                <span class="nx-g g-pad" class:on={dev.pad} title="Controller"></span>
            </div>
            <span class="nx-device-label">{controls || 'No controls found'}</span>
            {#if !dev.pad}<span class="nx-device-hint">Press a button on a controller to add it</span>{/if}
            <label class="toggle-container nx-device-toggle">
                <span>Only What Fits</span>
                <input type="checkbox" checked={st.fitOnly} onchange={(e) => h.setFitOnly(e.currentTarget.checked)}>
                <span class="neon-toggle"></span>
            </label>
        </div>
    </div>
</aside>
