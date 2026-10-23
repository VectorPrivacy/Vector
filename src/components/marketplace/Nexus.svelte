<script>
    // The Nexus: Vector's mini app store. Beside the rail (widescreen) its own column holds
    // the search, pages and categories; on a phone they sit under a back header instead.
    // The page (a view, a search, or one app) rises in when it changes; an app opened on a
    // phone slides in over the list, and going back lands where the list was left.
    import { mktState, mktApps, mktSetQuery, mktClosingEnded, mktPicker, mktFits, mktDevice } from '../lib/marketplace.svelte.js';
    import { shellState } from '../lib/shell.svelte.js';
    import { popupState } from '../lib/popup.svelte.js';
    import { permissionState } from '../lib/overlays.svelte.js';
    import { inCategory, isGame, isInstalled, matches, tagCounts, tagLabel } from '../lib/nexus.js';
    import NexusNav from './NexusNav.svelte';
    import Discover from './Discover.svelte';
    import Browse from './Browse.svelte';
    import Details from './Details.svelte';
    let { h } = $props();   // h: MarketplaceHelpers (js/marketplace.js)
    const st = mktState();
    const dev = mktDevice();
    const ws = $derived(shellState().ws);

    const app = $derived(st.detailsId ? mktApps().find(a => a.id === st.detailsId) || null : null);
    const searching = $derived(!!st.query.trim());
    const count = (list) => `${list.length} ${list.length === 1 ? 'app' : 'apps'}`;
    const title = $derived.by(() => {
        if (app) return app.name;
        if (searching) return 'Search';
        switch (st.view) {
            case 'library': return 'Library';
            case 'updates': return 'Updates';
            case 'all': return 'All Apps';
            case 'category': return st.category === 'app' ? 'Apps & Tools' : tagLabel(st.category);
            default: return 'Discover';
        }
    });
    const heading = $derived.by(() => {
        const apps = mktApps();
        const fit = apps.filter(mktFits);
        if (searching) {
            const q = st.query.toLowerCase().trim();
            return { title: 'Results', sub: count(fit.filter(a => matches(a, q))) };
        }
        switch (st.view) {
            case 'library': return { title: 'Library', sub: count(apps.filter(isInstalled)) };
            case 'updates': return { title: 'Updates', sub: '' };
            case 'all': return { title: 'All Apps', sub: count(fit) };
            case 'category': return { title, sub: count(fit.filter(a => inCategory(a, st.category))) };
            default: return null;
        }
    });
    // The phone's page strip: the pages, then the categories the catalogue uses most.
    const chips = $derived.by(() => {
        const apps = mktApps();
        const out = [{ view: 'discover', cat: '', label: 'Discover' }, { view: 'library', cat: '', label: 'Library' }];
        const updates = apps.filter(a => a.update_available).length;
        if (updates) out.push({ view: 'updates', cat: '', label: 'Updates', n: updates });
        out.push({ view: 'all', cat: '', label: 'All' });
        if (apps.some(a => !isGame(a))) out.push({ view: 'category', cat: 'app', label: 'Apps & Tools' });
        for (const c of tagCounts(apps.filter(mktFits)).filter(c => c.n > 1).slice(0, 10)) out.push({ view: 'category', cat: c.tag, label: tagLabel(c.tag) });
        return out;
    });
    const chipOn = (c) => !searching && st.view === c.view && (c.view !== 'category' || st.category === c.cat);

    // Where the list was when an app opened over it, so Back returns there.
    let scroller = $state(null);
    let listTop = 0;
    let listKey = '';
    let lastApp = null;
    let returning = $state(false);
    $effect.pre(() => {
        st.tick;
        const id = st.detailsId;
        // Only Back lands where the list was: a tag or a search from the app goes somewhere new.
        const key = `${st.view}|${st.category}|${st.query}`;
        if (id && !lastApp && scroller) { listTop = scroller.scrollTop; listKey = key; }
        returning = !id && !!lastApp && key === listKey;
        lastApp = id;
    });
    $effect(() => {
        st.tick;
        if (scroller) scroller.scrollTop = returning ? listTop : 0;
    });

    let searchEl = null;
    function searchField(node) { searchEl = node; return { destroy() { if (searchEl === node) searchEl = null; } }; }

    function back() {
        if (st.detailsId) h.closeDetails();
        else h.back();
    }
    function keys(e) {
        if (!st.panelOpen || st.panelClosing || e.defaultPrevented) return;
        // A dialog over the Nexus owns the keyboard.
        if (mktPicker().open || popupState().open || permissionState().open) return;
        const typing = e.target instanceof HTMLElement && (e.target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName));
        if (e.key === '/' && !typing && searchEl) { e.preventDefault(); searchEl.focus(); return; }
        if (e.key !== 'Escape' || typing) return;
        if (st.detailsId) { e.preventDefault(); h.closeDetails(); }
        else if (!ws) { e.preventDefault(); h.back(); }
    }
</script>

<svelte:window onkeydown={keys} />

<div class="nx" class:closing={st.panelClosing} style:display={st.panelOpen ? null : 'none'}
     onanimationend={(e) => { if (e.target === e.currentTarget && e.animationName === 'nx-close' && st.panelClosing) mktClosingEnded(); }}>
    <!-- Nothing renders while it's shut: the catalogue arrives at login, the Nexus may never open. -->
    {#if st.panelOpen}
    {#if ws}<NexusNav {h} {searchField} />{/if}

    <section class="nx-main">
        <header class="nx-top">
            {#if !ws || app}
                <button type="button" class="nx-top-back" aria-label="Back" onclick={back}><span class="nx-g g-left"></span></button>
            {/if}
            <h2 class="nx-top-title" class:center={!ws}>{ws ? title : (app ? app.name : 'Nexus')}</h2>
            <button type="button" class="nx-top-btn" class:nx-spinning={st.loading} aria-label="Refresh" onclick={() => h.retry()} disabled={st.loading}>
                <span class="nx-g g-refresh"></span>
            </button>
        </header>

        {#if !ws && !app}
            <div class="nx-mhead">
                <div class="nx-msearch" class:has-query={searching}>
                    <span class="nx-g g-search"></span>
                    <input type="text" placeholder="Search the Nexus" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                           value={st.query} oninput={(e) => mktSetQuery(e.currentTarget.value)} use:searchField>
                    {#if searching}
                        <button type="button" class="nx-msearch-clear" aria-label="Clear search" onclick={() => mktSetQuery('')}>
                            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
                        </button>
                    {/if}
                </div>
                <div class="nx-mchips">
                    <button type="button" class="nx-chip nx-chip-fit" class:on={st.fitOnly} aria-pressed={st.fitOnly}
                            onclick={() => h.setFitOnly(!st.fitOnly)}>
                        <span class="nx-g {dev.touch && !dev.keys ? 'g-touch' : 'g-keys'}"></span>Fits This Device
                    </button>
                    <span class="nx-mchips-sep"></span>
                    {#each chips as c (c.view + c.cat)}
                        <button type="button" class="nx-chip" class:on={chipOn(c)} onclick={() => h.show(c.view, c.cat)}>
                            {c.label}{#if c.n}<span class="nx-chip-n">{c.n}</span>{/if}
                        </button>
                    {/each}
                </div>
            </div>
        {/if}

        <div class="nx-scroll" bind:this={scroller}>
            {#key st.tick}
                <div class="nx-page" class:push={!ws && !!app} class:pop={!ws && !app && returning}>
                    {#if st.loading && !mktApps().length}
                        <div class="nx-empty">
                            <span class="nx-g g-nexus nx-empty-g nx-pulse"></span>
                            <p class="nx-empty-title">Opening the Nexus</p>
                        </div>
                    {:else if st.error && !mktApps().length}
                        <div class="nx-empty">
                            <span class="nx-g g-warn nx-empty-g"></span>
                            <p class="nx-empty-title">The Nexus didn't load</p>
                            <p class="nx-empty-hint">{st.error}</p>
                            <button type="button" class="nx-btn accent" onclick={() => h.retry()}>Try Again</button>
                        </div>
                    {:else if app}
                        <Details {h} />
                    {:else}
                        {#if heading}
                            <div class="nx-heading">
                                <h1 class="nx-h1">{heading.title}</h1>
                                {#if heading.sub}<span class="nx-heading-sub">{heading.sub}</span>{/if}
                            </div>
                        {/if}
                        {#if st.view === 'discover' && !searching}<Discover {h} />{:else}<Browse {h} />{/if}
                    {/if}
                </div>
            {/key}
        </div>
    </section>
    {/if}
</div>
