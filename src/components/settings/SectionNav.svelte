<script>
    // The settings nav, shared by Settings and Community Settings: a search over every
    // anchor, then each section with its timeline of anchors into the content, the open
    // one expanded. A search opens every section and keeps only the anchors it matches.
    // Nothing unmounts as the query changes: rows fold shut and open again, so typing
    // reads as motion rather than rows blinking in and out.
    // sections: [{ id, label, icon?, danger?, anchors: [{ id, label, keys?, fresh? }] }]
    // An anchor's `fresh` puts a NEW pill on it, and on its section while that is shut.
    let { sections, section, anchor, query, onquery, ongo, onanchor, head, footer } = $props();

    const q = $derived(query.trim().toLowerCase());
    const nav = $derived(sections.map((sec) => {
        const hits = sec.anchors.map((a) => !q || `${sec.label} ${a.label} ${a.keys || ''}`.toLowerCase().includes(q));
        const open = q ? hits.includes(true) : sec.id === section;
        return {
            ...sec,
            shown: hits.includes(true),
            open,
            fresh: !open && sec.anchors.some((a) => a.fresh),
            anchors: sec.anchors.map((a, i) => ({ ...a, shown: hits[i] })),
        };
    }));
    const empty = $derived(!nav.some((sec) => sec.shown));
    let searchEl = null;

    // The open section's marker is one element that slides to the anchor being read, so
    // it trails the scroll rather than jumping row to row.
    const rows = {};
    function row(node, key) {
        rows[key] = node;
        return { destroy() { if (rows[key] === node) delete rows[key]; } };
    }
    let markY = $state(null);
    $effect(() => {
        const key = `${section}:${anchor}`;
        const measure = () => {
            const el = !q && rows[key];
            markY = el ? el.offsetTop + el.offsetHeight / 2 : null;
        };
        measure();
        // Rows a search folded shut take their fold's time to open again.
        const t = setTimeout(measure, 300);
        return () => clearTimeout(t);
    });
    // Placed where it belongs when its section opens; only later moves are animated.
    function settle(node) {
        node.style.transition = 'none';
        requestAnimationFrame(() => requestAnimationFrame(() => { node.style.transition = ''; }));
    }
</script>

<aside class="cs-nav">
    <div class="cs-search" class:has-query={!!query}>
        <span class="icon icon-search"></span>
        <input placeholder="Search" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
               value={query} oninput={(e) => onquery(e.currentTarget.value)} bind:this={searchEl}>
        {#if query}
            <button class="cs-search-clear" type="button" aria-label="Clear search" onclick={() => { onquery(''); searchEl?.focus(); }}>
                <svg viewBox="0 0 18 18" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
                    <path d="M11.1002 6.29922L6.3002 11.0992M6.3002 6.29922L11.1002 11.0992M16.7002 8.69922C16.7002 13.1175 13.1184 16.6992 8.7002 16.6992C4.28192 16.6992 0.700195 13.1175 0.700195 8.69922C0.700195 4.28094 4.28192 0.699219 8.7002 0.699219C13.1184 0.699219 16.7002 4.28094 16.7002 8.69922Z" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
            </button>
        {/if}
    </div>
    <div class="cs-nav-scroll">
        <div class="cs-nav-head">{@render head()}</div>
        {#each nav as sec (sec.id)}
            <div class="cs-fold" class:shut={!sec.shown}>
                <div class="cs-fold-inner">
                    <button class="cs-section" class:active={!q && sec.id === section} class:danger={sec.danger}
                            tabindex={sec.shown ? 0 : -1} onclick={() => ongo(sec.id)}>
                        {#if sec.icon}<span class="cs-section-icon"><span class="icon icon-{sec.icon}"></span></span>{/if}
                        <span class="cutoff">{sec.label}</span>
                        {#if sec.fresh}<span class="badge-new">NEW</span>{/if}
                    </button>
                    <div class="cs-anchors-wrap" class:open={sec.open}>
                        <div class="cs-anchors">
                            {#if sec.id === section && markY !== null}
                                <span class="cs-anchor-marker" style:transform="translateY({markY}px)" use:settle></span>
                            {/if}
                            {#each sec.anchors as a (a.id)}
                                <button class="cs-anchor" class:active={sec.id === section && anchor === a.id}
                                        class:shut={!a.shown} use:row={`${sec.id}:${a.id}`}
                                        tabindex={sec.open && a.shown ? 0 : -1} onclick={() => onanchor(sec.id, a.id)}>
                                    <span class="cs-anchor-label cutoff">{a.label}</span>
                                    {#if a.fresh}<span class="badge-new">NEW</span>{/if}
                                </button>
                            {/each}
                        </div>
                    </div>
                </div>
            </div>
        {/each}
        <div class="cs-fold cs-fold-empty" class:shut={!empty}>
            <div class="cs-fold-inner">
                <p class="cs-nav-empty">No matches</p>
            </div>
        </div>
        {#if footer}<div class="cs-nav-foot">{@render footer()}</div>{/if}
    </div>
</aside>
