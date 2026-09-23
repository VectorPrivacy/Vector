<script>
    // Community Settings, a fullscreen modal in the settings layout the app is moving to:
    // isolated sections on the left, each with a timeline of anchors into its content, and
    // the content itself as headed blocks in one scroll. Edits collect in a draft behind a
    // save bar; closing with unsaved changes is refused, and the bar says why.
    import { tick } from 'svelte';
    import { csOverlay, csState, csDirty, csSetDraft, csSetSection, csSetQuery, csSelectBans, csClearBanSel } from '../lib/community-settings.svelte.js';
    import { popIn } from '../lib/popin.js';
    import Avatar from '../ui/Avatar.svelte';
    import MemberRow from '../people/MemberRow.svelte';
    import RolesSection from './RolesSection.svelte';
    import { profileVersion } from '../lib/signals.svelte.js';

    // h: close(), pickIcon(), save(), reset(), unban() (the selection), name(npub), profile(npub),
    //    avatarSrc(npub), ui (MemberRow's { twemojify, showTooltip, hideTooltip })
    let { h } = $props();

    const ov = csOverlay.state();
    const st = csState();
    const dirty = $derived(csDirty());

    const NAME_MAX = 32;
    const DESCRIPTION_MAX = 500;

    // The Roles section's anchors follow what it shows: the list, or one role open.
    const roleAnchors = $derived(!st.roles.edit
        ? [{ id: 'role-list', label: 'Role List', icon: 'shield-filled', keys: 'roles role list order rank create permissions' }]
        : [
            { id: 'role-display', label: 'Display', icon: 'palette', keys: 'role name colour color access channel scope' },
            { id: 'role-permissions', label: 'Permissions', icon: 'shield-filled', keys: 'role permissions ban kick manage' },
            ...(st.roles.edit.id !== null ? [{ id: 'role-members', label: 'Members', icon: 'add-user', keys: 'role members holders assign give' }] : []),
        ]);

    const SECTIONS = $derived([
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
        {
            id: 'roles',
            label: 'Roles',
            needs: 'canRoles',
            anchors: roleAnchors,
        },
        {
            id: 'bans',
            label: 'Bans',
            needs: 'canBan',
            anchors: [
                { id: 'ban-list', label: 'Banned Members', icon: 'x-user', keys: 'bans banned unban blocked removed' },
            ],
        },
    ]);

    // A section only for those who can act in it: bans are a moderation surface.
    const sections = $derived(SECTIONS.filter((x) => !x.needs || st[x.needs]));
    const section = $derived(sections.find((x) => x.id === st.section) || sections[0]);

    // A search lists every matching anchor, whichever section holds it.
    const nav = $derived.by(() => {
        const q = st.query.trim().toLowerCase();
        if (!q) return sections.map((sec) => ({ ...sec, open: sec.id === section.id }));
        return sections.map((sec) => ({
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
        activeAnchor = sections.find((x) => x.id === id)?.anchors[0]?.id;
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

    // Opening a role, or going back to the list, is a new page: top of it, first anchor.
    let lastRoleView = null;
    $effect(() => {
        const key = st.roles.edit ? `role:${st.roles.edit.id}` : 'list';
        if (key === lastRoleView) return;
        const first = lastRoleView === null;
        lastRoleView = key;
        if (first || section.id !== 'roles') return;
        if (scroller) scroller.scrollTop = 0;
        activeAnchor = roleAnchors[0]?.id;
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

    // ── bans ──
    let banQuery = $state('');
    const banRows = $derived.by(() => {
        const q = banQuery.trim().toLowerCase();
        const rows = [];
        for (const npub of st.bans) {
            profileVersion(npub);
            const name = h.name(npub);
            if (q && !`${name} ${npub}`.toLowerCase().includes(q)) continue;
            rows.push({ npub, name, profile: h.profile(npub), src: h.avatarSrc(npub) });
        }
        return rows;
    });
    // Head and tail: the ends are what an impersonator's key can't also match.
    const shortNpub = (npub) => `${npub.slice(0, 12)}…${npub.slice(-6)}`;

    // Select-all acts on what the search shows, so "all" never reaches rows out of view.
    const shownSelected = $derived(banRows.filter((r) => st.banSel.has(r.npub)).length);
    const allShown = $derived(banRows.length > 0 && shownSelected === banRows.length);
    function toggleAll() {
        csSelectBans(banRows.map((r) => r.npub), !allShown);
    }


    // One bar at the bottom, two jobs: an unban selection while in Bans, else unsaved
    // edits. A refused close always takes it: the bar is the only place that says why.
    const bar = $derived(warn && dirty ? 'save' : section.id === 'bans' && st.banSel.size ? 'unban' : dirty ? 'save' : null);

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
                    {:else if section.id === 'roles'}
                        <RolesSection {h} />
                    {:else if section.id === 'bans'}
                        <section class="cs-block" data-anchor="ban-list">
                            <h3 class="cs-heading">Banned Members</h3>
                            <p class="cs-lede">A banned member is silenced everywhere in this community and can't rejoin until they're unbanned.</p>
                            <div class="cs-bans-bar">
                                <!-- The invite picker's own search field, so the two read as one control. -->
                                <div class="emoji-search-container cs-bans-search">
                                    <span class="emoji-search-icon icon icon-search"></span>
                                    <input type="text" placeholder="Search" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                                           value={banQuery} oninput={(e) => { banQuery = e.currentTarget.value; }}>
                                </div>
                                <span class="cs-count" class:warn={st.bans.length >= st.bansMax}>{st.bans.length} of {st.bansMax}</span>
                            </div>
                            <!-- The contact picker's rows and indicators: one selection model app-wide. -->
                            <div class="cs-bans" class:busy={st.unbanning}>
                                {#if banRows.length}
                                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                                    <div class="member-pick-row cs-bans-all" onclick={toggleAll}>
                                        <div class="member-pick-hover"></div>
                                        <span class="cs-bans-all-label">
                                            {allShown ? 'Deselect' : 'Select'} {banQuery.trim() ? 'matches' : 'all'} ({banRows.length})
                                        </span>
                                        <div class="member-pick-indicator" class:selected={allShown}></div>
                                    </div>
                                {/if}
                                {#each banRows as row (row.npub)}
                                    <MemberRow
                                        npub={row.npub}
                                        profile={row.profile}
                                        src={row.src}
                                        display={row.name || shortNpub(row.npub)}
                                        hasName={!!row.name}
                                        keyHint={row.name ? shortNpub(row.npub) : ''}
                                        onactivate={() => csSelectBans([row.npub])}
                                        ui={h.ui}
                                    >
                                        {#snippet trailing()}
                                            <div class="member-pick-indicator" class:selected={st.banSel.has(row.npub)}></div>
                                        {/snippet}
                                    </MemberRow>
                                {:else}
                                    <p class="cmt-empty" style="text-align:center;">{banQuery.trim() ? 'No matches.' : 'Nobody is banned from this community.'}</p>
                                {/each}
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

            <div class="cs-savebar" class:visible={!!bar} class:warn={warn && bar === 'save'} class:is-unban={bar === 'unban'}>
                {#if bar === 'unban'}
                    <span class="cs-savebar-text">
                        {#if st.unbanning}
                            Unbanning {st.banSel.size}...
                        {:else}
                            {st.banSel.size} {st.banSel.size === 1 ? 'member' : 'members'} selected
                        {/if}
                    </span>
                    <div class="cs-savebar-actions">
                        <button class="cs-btn-text" disabled={st.unbanning} onclick={() => csClearBanSel()}>Clear</button>
                        <button class="cs-btn-save" disabled={st.unbanning} onclick={() => h.unban()}>
                            Unban {st.banSel.size === st.bans.length && st.bans.length > 1 ? 'All' : st.banSel.size}
                        </button>
                    </div>
                {:else}
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
                {/if}
            </div>
        </main>
    </div>
</div>
