<script>
    // The settings nav, shared by Settings and Community Settings: a search over every
    // anchor, then each section with its timeline of anchors into the content, the open
    // one expanded. A search opens every section and keeps only the anchors it matches.
    // Nothing unmounts as the query changes: rows fold shut and open again, so typing
    // reads as motion rather than rows blinking in and out.
    // sections: [{ id, label, icon?, danger?, anchors: [{ id, label, icon, keys? }] }]
    let { sections, section, anchor, query, onquery, ongo, onanchor, head, footer } = $props();

    const q = $derived(query.trim().toLowerCase());
    const nav = $derived(sections.map((sec) => {
        const hits = sec.anchors.map((a) => !q || `${sec.label} ${a.label} ${a.keys || ''}`.toLowerCase().includes(q));
        // The timeline draws from the first shown dot to the last, whatever is folded between.
        const first = hits.indexOf(true);
        const last = hits.lastIndexOf(true);
        return {
            ...sec,
            shown: first !== -1,
            open: q ? first !== -1 : sec.id === section,
            anchors: sec.anchors.map((a, i) => ({ ...a, shown: hits[i], first: i === first, last: i === last })),
        };
    }));
    const empty = $derived(!nav.some((sec) => sec.shown));
</script>

<aside class="cs-nav">
    <div class="cs-search">
        <span class="icon icon-search"></span>
        <input placeholder="Search" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
               value={query} oninput={(e) => onquery(e.currentTarget.value)}>
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
                    </button>
                    <div class="cs-anchors-wrap" class:open={sec.open}>
                        <div class="cs-anchors">
                            {#each sec.anchors as a (a.id)}
                                <button class="cs-anchor" class:active={sec.id === section && anchor === a.id}
                                        class:shut={!a.shown} class:first={a.first} class:last={a.last}
                                        tabindex={sec.open && a.shown ? 0 : -1} onclick={() => onanchor(sec.id, a.id)}>
                                    <span class="cs-anchor-dot"></span>
                                    <span class="cs-anchor-icon"><span class="icon icon-{a.icon}"></span></span>
                                    <span class="cs-anchor-label cutoff">{a.label}</span>
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
