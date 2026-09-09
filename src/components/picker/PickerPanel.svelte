<script>
    // The emoji / GIF picker inside its root: search and the mode toggle, the rail, the
    // sections (search results, recents, the equipped packs, all emoji), the creator that
    // swaps in over them, the GIF panel, and the creator's overlays. The grids arrive on
    // first open (`ready`); scroll spy, rail follow and jump-scroll stay the app's, working
    // on the hosts handed over at mount.
    import { onMount } from 'svelte';
    import { pickerState, panelState } from '../lib/picker.svelte.js';
    import PackSidebar from './PackSidebar.svelte';
    import RecentsGrid from './RecentsGrid.svelte';
    import AllGrid from './AllGrid.svelte';
    import SearchGrid from './SearchGrid.svelte';
    import PackSections from './PackSections.svelte';
    import PackCreator from './PackCreator.svelte';
    import GifGrid from './GifGrid.svelte';
    import CreatorOverlays from './CreatorOverlays.svelte';

    let { h } = $props();
    // h: mounted(els), searchInput(e), searchKeydown(e), setMode('emoji'|'gif'), railClick(e), railStop(),
    //    mainClick(e), mainScroll(), gifClick(e), gifScroll(), islands (the grids' bag), creator (the creator's bag),
    //    overlays (the overlays' bag)

    const st = pickerState();
    const p = panelState();

    let search = $state(null), sidebar = $state(null), main = $state(null);
    let recents = $state(null), all = $state(null), results = $state(null), gif = $state(null);
    onMount(() => h.mounted({ search, sidebar, main, recents, all, results, gif }));

    // A search or the creator takes the sections' place; the results section shows for a query.
    const sectionsHidden = $derived(p.creatorOpen || !!st.query);
</script>

<div class="emoji-search-container">
    <div class="emoji-search-wrapper">
        <input id="emoji-search-input" bind:this={search} placeholder={p.mode === 'gif' ? 'Search GIFs...' : 'Search Emojis...'}
               autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
               oninput={(e) => h.searchInput(e)} onkeydown={(e) => h.searchKeydown(e)}>
        <span class="emoji-search-icon icon icon-search"></span>
    </div>
    <div class="picker-mode-toggle">
        <button class="picker-mode-btn" class:active={p.mode === 'emoji'} data-mode="emoji" onclick={(e) => { e.stopPropagation(); h.setMode('emoji'); }}>Emoji</button>
        <button class="picker-mode-btn" class:active={p.mode === 'gif'} data-mode="gif" onclick={(e) => { e.stopPropagation(); h.setMode('gif'); }}>GIF</button>
    </div>
</div>
<div class="emoji-picker-content" style:display={p.mode === 'gif' ? 'none' : ''}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="emoji-sidebar" bind:this={sidebar} onclick={(e) => h.railClick(e)} onpointerdown={() => h.railStop()} onwheel={() => h.railStop()}>
        {#if p.ready}<PackSidebar h={h.islands} />{/if}
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="emoji-main" bind:this={main} onclick={(e) => h.mainClick(e)} onscroll={() => h.mainScroll()}>
        <div class="emoji-section" id="emoji-search-results-container" hidden={!st.query}>
            <div class="emoji-section-header">
                <span class="header-text">Search Results</span>
            </div>
            <div class="emoji-grid" id="emoji-search-results" bind:this={results}>
                {#if p.ready}<SearchGrid h={h.islands} />{/if}
            </div>
        </div>
        <div class="emoji-section" id="emoji-recents" style:display={sectionsHidden ? 'none' : ''}>
            <div class="emoji-section-header">
                <span class="section-icon icon icon-clock"></span>
                <span class="header-text">Recently Used</span>
                <span class="icon icon-chevron-down"></span>
            </div>
            <div class="emoji-grid" id="emoji-recents-grid" bind:this={recents}>
                {#if p.ready}<RecentsGrid h={h.islands} />{/if}
            </div>
        </div>
        <div id="emoji-pack-sections" style:display={sectionsHidden ? 'none' : 'contents'}>
            {#if p.ready}<PackSections h={h.islands} />{/if}
        </div>
        <div class="emoji-section" id="emoji-all" style:display={sectionsHidden ? 'none' : ''}>
            <div class="emoji-section-header">
                <span class="section-icon icon icon-smile-face"></span>
                <span class="header-text">Emojis</span>
                <span class="icon icon-chevron-down"></span>
            </div>
            <div class="emoji-grid" id="emoji-all-grid" bind:this={all}>
                {#if p.ready && all}<AllGrid grid={all} h={h.islands} />{/if}
            </div>
        </div>
        <div class="emoji-creator" id="emoji-creator" hidden={!p.creatorOpen}>
            <PackCreator h={h.creator} />
        </div>
    </div>
</div>
<div class="gif-picker-content" style:display={p.mode === 'gif' ? 'flex' : 'none'}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="gif-grid" id="gif-grid" bind:this={gif} onclick={(e) => h.gifClick(e)} onscroll={() => h.gifScroll()}>
        {#if gif}<GifGrid grid={gif} h={h.islands} />{/if}
    </div>
</div>
<CreatorOverlays h={h.overlays} />
