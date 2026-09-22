<script>
    // Community Settings, a fullscreen modal in the settings layout the app is moving to:
    // isolated sections on the left, each with a timeline of anchors into its content, and
    // the content itself as headed blocks in one scroll. Edits collect in a draft behind a
    // save bar; closing with unsaved changes is refused, and the bar says why.
    import { tick } from 'svelte';
    import { csOverlay, csState, csDirty, csSetDraft, csSetSection, csSetQuery } from '../lib/community-settings.svelte.js';
    import { popIn } from '../lib/popin.js';
    import Avatar from '../ui/Avatar.svelte';

    // h: close(), pickIcon(), save(), reset()
    let { h } = $props();

    const ov = csOverlay.state();
    const st = csState();
    const dirty = $derived(csDirty());

    const NAME_MAX = 32;
    const DESCRIPTION_MAX = 500;

    const SECTIONS = [
        {
            id: 'overview',
            label: 'Overview',
            anchors: [
                { id: 'identity', label: 'Icon & Name', icon: 'image', keys: 'icon avatar logo picture image name title rename' },
                { id: 'description', label: 'Description', icon: 'align-left', keys: 'description about bio summary' },
            ],
        },
        {
            id: 'relays',
            label: 'Relays',
            anchors: [
                { id: 'relay-list', label: 'Hosting Relays', icon: 'globe', keys: 'relays servers hosting network nostr' },
            ],
        },
    ];

    const section = $derived(SECTIONS.find((x) => x.id === st.section) || SECTIONS[0]);

    // A search lists every matching anchor, whichever section holds it.
    const nav = $derived.by(() => {
        const q = st.query.trim().toLowerCase();
        if (!q) return SECTIONS.map((sec) => ({ ...sec, open: sec.id === section.id }));
        return SECTIONS.map((sec) => ({
            ...sec,
            open: true,
            anchors: sec.anchors.filter((a) => `${sec.label} ${a.label} ${a.keys}`.toLowerCase().includes(q)),
        })).filter((sec) => sec.anchors.length);
    });

    // ── scroll spy ──
    // The anchor whose block crosses the scroller's midline is the one being read; the
    // last one wins once the scroll bottoms out, however short its block.
    let scroller = $state(null);
    let activeAnchor = $state('identity');
    let spyMutedUntil = 0;
    function spy() {
        if (!scroller || performance.now() < spyMutedUntil) return;
        const blocks = [...scroller.querySelectorAll('[data-anchor]')];
        if (!blocks.length) return;
        const top = scroller.getBoundingClientRect().top;
        const mid = top + scroller.clientHeight / 2;
        let current = blocks[0];
        for (const b of blocks) if (b.getBoundingClientRect().top <= mid) current = b;
        if (scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2) current = blocks[blocks.length - 1];
        activeAnchor = current.dataset.anchor;
    }

    async function goSection(id) {
        if (st.section === id) return;
        csSetSection(id);
        await tick();
        if (scroller) scroller.scrollTop = 0;
        activeAnchor = SECTIONS.find((x) => x.id === id)?.anchors[0]?.id;
    }

    async function goAnchor(sectionId, anchorId) {
        if (st.section !== sectionId) {
            csSetSection(sectionId);
            await tick();
        }
        const block = scroller?.querySelector(`[data-anchor="${anchorId}"]`);
        if (!block) return;
        activeAnchor = anchorId;
        // The smooth scroll passes every block between here and there; the spy would
        // strobe the timeline through each of them.
        spyMutedUntil = performance.now() + 700;
        const offset = block.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
        scroller.scrollTo({ top: scroller.scrollTop + offset - 28, behavior: 'smooth' });
    }

    // Opening lands on the first section with the search cleared.
    $effect(() => {
        ov.tick;
        activeAnchor = 'identity';
        if (scroller) scroller.scrollTop = 0;
    });

    // A refused close shakes the bar and turns its message into the reason.
    let warn = $state(false);
    let warnTimer = null;
    $effect(() => {
        if (!st.nudge) return;
        warn = false;
        requestAnimationFrame(() => { warn = true; });
        clearTimeout(warnTimer);
        warnTimer = setTimeout(() => { warn = false; }, 1600);
    });

    const iconSrc = $derived(st.draft.iconPreview || st.saved.iconSrc);
    const nameEmpty = $derived(!st.draft.name.trim());

    // The scheme recedes; the rest, path included, IS the relay's address.
    function schemeOf(url) {
        const i = url.indexOf('://');
        return i === -1 ? '' : url.slice(0, i + 3);
    }
    function addressOf(url) {
        return url.slice(schemeOf(url).length).replace(/\/$/, '');
    }
</script>

<svelte:window onkeydown={(e) => { if (ov.active && e.key === 'Escape') { e.preventDefault(); h.close(); } }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="mod-overlay cs-overlay" class:active={ov.active} class:closing={ov.closing}
     onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
    <div class="cs-card" use:popIn={ov.tick}>
        <aside class="cs-nav">
            <div class="cs-search">
                <span class="icon icon-search"></span>
                <input placeholder="Search" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                       value={st.query} oninput={(e) => csSetQuery(e.currentTarget.value)}>
            </div>
            <div class="cs-nav-scroll">
                <div class="cs-nav-head">
                    <Avatar src={st.saved.iconSrc} size={28} group={true} class="cs-nav-avatar" />
                    <span class="cs-nav-title cutoff">{st.saved.name || 'Community'}</span>
                </div>
                {#each nav as sec (sec.id)}
                    <button class="cs-section" class:active={!st.query && sec.id === section.id} onclick={() => goSection(sec.id)}>{sec.label}</button>
                    <!-- Always mounted, so opening and shutting can animate rather than jump. -->
                    <div class="cs-anchors-wrap" class:open={sec.open}>
                        <div class="cs-anchors">
                            {#each sec.anchors as a (a.id)}
                                <button class="cs-anchor" class:active={sec.id === section.id && activeAnchor === a.id}
                                        tabindex={sec.open ? 0 : -1} onclick={() => goAnchor(sec.id, a.id)}>
                                    <span class="cs-anchor-dot"></span>
                                    <span class="cs-anchor-icon"><span class="icon icon-{a.icon}"></span></span>
                                    <span class="cs-anchor-label cutoff">{a.label}</span>
                                </button>
                            {/each}
                        </div>
                    </div>
                {/each}
                {#if !nav.length}
                    <p class="cs-nav-empty">Nothing matches "{st.query.trim()}"</p>
                {/if}
            </div>
        </aside>

        <main class="cs-main">
            <header class="cs-top">
                <h2 class="cs-top-title">{section.label}</h2>
                <button class="cs-close" aria-label="Close" onclick={() => h.close()}>
                    <span class="cs-close-x">&#x2715;</span>
                    <span class="cs-close-key">ESC</span>
                </button>
            </header>

            <div class="cs-scroll" bind:this={scroller} onscroll={spy}>
                {#key section.id}
                <div class="cs-content cs-content-enter" class:loading={st.loading}>
                    {#if section.id === 'overview'}
                        <section class="cs-block" data-anchor="identity">
                            <h3 class="cs-heading">Icon & Name</h3>
                            <div class="cs-identity">
                                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                                <div class="cs-icon" class:editable={st.canEdit} onclick={st.canEdit ? h.pickIcon : null}
                                     title={st.canEdit ? 'Change icon' : null}>
                                    <Avatar src={iconSrc} size={null} group={true} class="cs-icon-img" />
                                    {#if st.canEdit}
                                        <span class="cs-icon-edit"><span class="icon icon-edit"></span></span>
                                    {/if}
                                    {#if st.draft.iconPath}<span class="cs-icon-new">New</span>{/if}
                                </div>
                                <div class="cs-field cs-field-grow">
                                    <div class="cs-field-head">
                                        <label class="cs-label" for="cs-name">Community Name</label>
                                        <span class="cs-count" class:warn={nameEmpty}>{st.draft.name.length}/{NAME_MAX}</span>
                                    </div>
                                    <input id="cs-name" class="cs-input" type="text" maxlength={NAME_MAX} disabled={!st.canEdit}
                                           value={st.draft.name} oninput={(e) => csSetDraft({ name: e.currentTarget.value })}>
                                    <p class="cs-hint">The icon shows on the rail and beside every invite. Square images crop best.</p>
                                </div>
                            </div>
                        </section>

                        <section class="cs-block" data-anchor="description">
                            <h3 class="cs-heading">Description</h3>
                            <div class="cs-field">
                                <div class="cs-field-head">
                                    <label class="cs-label" for="cs-description">What this community is about</label>
                                    <span class="cs-count">{st.draft.description.length}/{DESCRIPTION_MAX}</span>
                                </div>
                                <textarea id="cs-description" class="cs-input cs-textarea" rows="5" maxlength={DESCRIPTION_MAX}
                                          disabled={!st.canEdit} placeholder="Tell people what brings everyone here"
                                          value={st.draft.description} oninput={(e) => csSetDraft({ description: e.currentTarget.value })}></textarea>
                            </div>
                        </section>
                    {:else}
                        <section class="cs-block" data-anchor="relay-list">
                            <h3 class="cs-heading">Hosting Relays</h3>
                            <p class="cs-lede">Every message, channel and member list in this community lives on these relays.</p>
                            <div class="cs-relays">
                                {#each st.relays as url (url)}
                                    <div class="cs-relay">
                                        <span class="cs-relay-icon"><span class="icon icon-globe"></span></span>
                                        <span class="cs-relay-url cutoff"><span class="cs-relay-scheme">{schemeOf(url)}</span>{addressOf(url)}</span>
                                    </div>
                                {:else}
                                    <p class="cs-hint">{st.loading ? 'Loading...' : 'No relays recorded for this community.'}</p>
                                {/each}
                            </div>
                        </section>
                    {/if}
                </div>
                {/key}
            </div>

            <div class="cs-savebar" class:visible={dirty} class:warn>
                <span class="cs-savebar-text">
                    {#if st.saving}
                        Saving{st.progress ? ` ${Math.round(st.progress)}%` : '...'}
                    {:else if warn}
                        Save or reset your changes before closing
                    {:else}
                        You have unsaved changes
                    {/if}
                </span>
                <div class="cs-savebar-actions">
                    <button class="cs-btn-text" disabled={st.saving} onclick={() => h.reset()}>Reset</button>
                    <button class="cs-btn-save" disabled={st.saving || nameEmpty} onclick={() => h.save()}>Save Changes</button>
                </div>
            </div>
        </main>
    </div>
</div>
