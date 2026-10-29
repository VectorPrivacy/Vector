<script>
    // Community Settings, a fullscreen modal in the settings layout the app is moving to:
    // isolated sections on the left, each with a timeline of anchors into its content, and
    // the content itself as headed blocks in one scroll. Edits collect in a draft behind a
    // save bar; closing with unsaved changes is refused, and the bar says why.
    import { tick } from 'svelte';
    import { csOverlay, csState, csDirty, csSetDraft, csSetSection, csSetQuery, csSelectBans, csClearBanSel } from '../lib/community-settings.svelte.js';
    import { popIn } from '../lib/popin.js';
    import Avatar from '../ui/Avatar.svelte';
    import BannerArt from './BannerArt.svelte';
    import { bannerState } from '../lib/banners.svelte.js';
    import { relayInfoState } from '../lib/relay-info.svelte.js';
    import MemberRow from '../people/MemberRow.svelte';
    import RolesSection from './RolesSection.svelte';
    import AuditLogSection from './AuditLogSection.svelte';
    import { profileVersion } from '../lib/signals.svelte.js';
    import { anchorScroll } from '../lib/anchorscroll.svelte.js';
    import SectionNav from '../settings/SectionNav.svelte';

    // h: close(), pickIcon(), pickBanner(), save(), reset(), unban() (the selection), name(npub), veiled(npub), searchName(npub), profile(npub),
    //    avatarSrc(npub), copyIdItems(what, id), copyBannedId(npub), showContextMenu(menu), loadAudit(communityId), openLink(key),
    //    ui (MemberRow's { twemojify, showTooltip, hideTooltip })
    let { h } = $props();
    const relayInfo = relayInfoState();

    const ov = csOverlay.state();
    const st = csState();
    const dirty = $derived(csDirty());

    const NAME_MAX = 32;
    const DESCRIPTION_MAX = 500;

    // The Roles section's anchors follow what it shows: the list, or one role open.
    const roleAnchors = $derived(!st.roles.edit
        ? [{ id: 'role-list', label: 'Role List', icon: 'shield-filled', keys: 'roles role list order rank create permissions' }]
        : st.roles.edit.everyone
        // @everyone is permissions alone: everyone holds it, and it has no name to show.
        ? [{ id: 'role-permissions', label: 'Permissions', icon: 'shield-filled', keys: 'everyone role permissions ban kick manage' }]
        : [
            { id: 'role-display', label: 'Display', icon: 'palette', keys: 'role name colour color access channel scope' },
            { id: 'role-permissions', label: 'Permissions', icon: 'shield-filled', keys: 'role permissions ban kick manage' },
            ...(st.roles.edit.id !== null ? [{ id: 'role-members', label: 'Members', icon: 'add-user', keys: 'role members holders assign give' }] : []),
        ]);

    const SECTIONS = $derived([
        {
            id: 'overview',
            label: 'Overview',
            icon: 'info',
            anchors: [
                { id: 'identity', label: 'Icon & Name', icon: 'image', keys: 'icon avatar logo picture image name title rename' },
                { id: 'banner', label: 'Banner', icon: 'image', keys: 'banner cover header art background picture image' },
                { id: 'description', label: 'Description', icon: 'align-left', keys: 'description about bio summary' },
            ],
        },
        {
            id: 'relays',
            label: 'Relays',
            icon: 'globe',
            anchors: [
                { id: 'relay-list', label: 'Hosting Relays', icon: 'globe', keys: 'relays servers hosting network nostr' },
                { id: 'voice-relays', label: 'Voice Relays', icon: 'volume-max', keys: 'voice relays iroh calls audio' },
            ],
        },
        {
            id: 'roles',
            label: 'Roles',
            icon: 'shield-filled',
            needs: 'canRoles',
            anchors: roleAnchors,
        },
        {
            id: 'bans',
            label: 'Bans',
            icon: 'x-user',
            needs: 'canBan',
            anchors: [
                { id: 'ban-list', label: 'Banned Members', icon: 'x-user', keys: 'bans banned unban blocked removed' },
            ],
        },
        {
            id: 'audit',
            label: 'Audit Log',
            icon: 'clock',
            needs: 'canAudit',
            anchors: [
                { id: 'audit-list', label: 'Audit Log', icon: 'clock', keys: 'audit log history changes actions who when record' },
            ],
        },
    ]);

    // A section only for those who can act in it: bans are a moderation surface.
    const sections = $derived(SECTIONS.filter((x) => !x.needs || st[x.needs]));
    const section = $derived(sections.find((x) => x.id === st.section) || sections[0]);

    const anchors = anchorScroll('identity');

    async function goSection(id) {
        if (st.section === id) return;
        csSetSection(id);
        await tick();
        anchors.top(sections.find((x) => x.id === id)?.anchors[0]?.id);
    }

    async function goAnchor(sectionId, anchorId) {
        if (st.section !== sectionId) {
            csSetSection(sectionId);
            await tick();
        }
        anchors.jump(anchorId);
    }

    // Opening lands on the first section with the search cleared.
    $effect(() => {
        ov.tick;
        anchors.top('identity');
    });

    // Opening a role, or going back to the list, is a new page: top of it, first anchor.
    let lastRoleView = null;
    $effect(() => {
        const key = st.roles.edit ? `role:${st.roles.edit.id}` : 'list';
        if (key === lastRoleView) return;
        const first = lastRoleView === null;
        lastRoleView = key;
        if (first || section.id !== 'roles') return;
        anchors.top(roleAnchors[0]?.id);
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
            const veiled = h.veiled(npub);
            if (q && !`${veiled ? h.searchName(npub) : name} ${npub}`.toLowerCase().includes(q)) continue;
            rows.push({ npub, name, veiled, profile: h.profile(npub), src: h.avatarSrc(npub) });
        }
        return rows;
    });
    // Head and tail: the ends are what an impersonator's key can't also match.
    const shortNpub = (npub) => `${npub.slice(0, 12)}…${npub.slice(-6)}`;

    // Select-all acts on what the search shows, so "all" never reaches rows out of view.
    const shownSelected = $derived(banRows.filter((r) => st.banSel.has(r.npub)).length);
    function banMenu(npub, e) {
        const items = h.copyBannedId(npub);
        if (!items.length) return;
        e.preventDefault();
        h.showContextMenu({ x: e.clientX, y: e.clientY, items });
    }
    const allShown = $derived(banRows.length > 0 && shownSelected === banRows.length);
    function toggleAll() {
        csSelectBans(banRows.map((r) => r.npub), !allShown);
    }


    // One bar at the bottom, two jobs: an unban selection while in Bans, else unsaved
    // edits. A refused close always takes it: the bar is the only place that says why.
    const bar = $derived(warn && dirty ? 'save' : section.id === 'bans' && st.banSel.size ? 'unban' : dirty ? 'save' : null);

    const iconSrc = $derived(st.draft.iconPreview || st.saved.iconSrc);
    const banners = bannerState();
    // A staged banner previews its crop on the original; the saved one is already cut.
    const bannerSrc = $derived(st.draft.bannerPreview || banners.src[st.communityId] || null);
    const nameEmpty = $derived(!st.draft.name.trim());

    // The scheme recedes; the rest, path included, IS the relay's address.
    function schemeOf(url) {
        const i = url.indexOf('://');
        return i === -1 ? '' : url.slice(0, i + 3);
    }
    function addressOf(url) {
        return url.slice(schemeOf(url).length).replace(/\/$/, '');
    }

    const RELAYS_MAX = 5;
    let relayInput = $state('');
    let relayError = $state('');
    // The relay being edited in place: its index in the draft, and the text in its field.
    let relayEdit = $state(null);
    // Relays whose icon would not load: they keep the globe.
    let relayIconFailed = $state({});

    $effect(() => {
        if (st.section !== 'relays') return;
        for (const url of st.draft.relays) {
            if (!(url in relayInfo)) h.relayInfo(url);
        }
    });

    /** `raw` as a relay address the list can hold, or an error message. */
    function relayAddress(raw, except = -1) {
        let text = raw.trim();
        if (!text) return { error: '' };
        if (!text.includes('://')) text = `wss://${text}`;
        let url;
        try { url = new URL(text); } catch { return { error: 'That isn\'t a relay address.' }; }
        // Plain ws:// only where the network encrypts the link itself.
        const native = /\.(onion|i2p)$/i.test(url.hostname);
        if (!url.hostname || !(url.protocol === 'wss:' || (url.protocol === 'ws:' && native))) return { error: 'Use wss://, or ws:// for an .onion or .i2p address.' };
        const address = text.replace(/\/+$/, '');
        const same = (a) => a.replace(/\/+$/, '').toLowerCase() === address.toLowerCase();
        if (st.draft.relays.some((r, i) => i !== except && same(r))) return { error: 'That relay is already listed.' };
        return { address };
    }
    function addRelay() {
        const { address, error } = relayAddress(relayInput);
        if (!address) { relayError = error; return; }
        if (st.draft.relays.length >= RELAYS_MAX) { relayError = `At most ${RELAYS_MAX} relays.`; return; }
        csSetDraft({ relays: [...st.draft.relays, address] });
        relayInput = '';
        relayError = '';
    }
    function removeRelay(url) {
        csSetDraft({ relays: st.draft.relays.filter((r) => r !== url) });
    }
    function commitRelayEdit() {
        if (!relayEdit) return;
        const { index, text } = relayEdit;
        const { address, error } = relayAddress(text, index);
        if (address) {
            csSetDraft({ relays: st.draft.relays.map((r, i) => (i === index ? address : r)) });
        } else if (error) {
            relayError = error;
        }
        relayEdit = null;
    }

    const VOICE_RELAYS_MAX = 8;
    let voiceRelayInput = $state('');
    let voiceRelayError = $state('');

    function addVoiceRelay() {
        let raw = voiceRelayInput.trim();
        if (!raw) return;
        if (!raw.includes('://')) raw = `https://${raw}`;
        let url;
        try { url = new URL(raw); } catch { voiceRelayError = 'That isn\'t a relay address.'; return; }
        if (url.protocol !== 'https:' || !url.hostname) { voiceRelayError = 'A voice relay is an https:// address.'; return; }
        const normal = `${url.href.replace(/\/+$/, '')}/`;
        const list = st.draft.voiceRelays;
        if (list.includes(normal)) { voiceRelayError = 'That relay is already listed.'; return; }
        if (list.length >= VOICE_RELAYS_MAX) { voiceRelayError = `At most ${VOICE_RELAYS_MAX} voice relays.`; return; }
        csSetDraft({ voiceRelays: [...list, normal] });
        voiceRelayInput = '';
        voiceRelayError = '';
    }
    function removeVoiceRelay(url) {
        csSetDraft({ voiceRelays: st.draft.voiceRelays.filter((r) => r !== url) });
    }
</script>

<svelte:window onkeydown={(e) => { if (ov.active && e.key === 'Escape') { e.preventDefault(); h.close(); } }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="mod-overlay cs-overlay" class:active={ov.active} class:closing={ov.closing}
     onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
    <div class="cs-card" use:popIn={ov.tick}>
        <SectionNav {sections} section={section.id} anchor={anchors.active} query={st.query}
                    onquery={csSetQuery} ongo={goSection} onanchor={goAnchor}>
            {#snippet head()}
                <Avatar src={st.saved.iconSrc} size={28} group={true} class="cs-nav-avatar" />
                <span class="cs-nav-title cutoff">{st.saved.name || 'Community'}</span>
            {/snippet}
        </SectionNav>

        <main class="cs-main">
            <header class="cs-top">
                <h2 class="cs-top-title">{section.label}</h2>
                <button class="cs-close" aria-label="Close" onclick={() => h.close()}>
                    <span class="cs-close-x">&#x2715;</span>
                    <span class="cs-close-key">ESC</span>
                </button>
            </header>

            <div class="cs-scroll" bind:this={anchors.el} onscroll={() => anchors.spy()}>
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

                        <section class="cs-block" data-anchor="banner">
                            <h3 class="cs-heading">Banner</h3>
                            <div class="cs-banner">
                                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                                <div class="cs-banner-frame" class:editable={st.canEdit} onclick={st.canEdit ? h.pickBanner : null}
                                     title={st.canEdit ? (bannerSrc ? 'Change banner' : 'Add a banner') : null}>
                                    {#if bannerSrc}
                                        <BannerArt src={bannerSrc} rect={st.draft.bannerPreview ? st.draft.bannerCrop : null}
                                                   natural={st.draft.bannerNatural} />
                                    {:else}
                                        <div class="cs-banner-empty">
                                            <span class="icon icon-image"></span>
                                            <span class="cs-banner-empty-text">
                                                <span>{st.canEdit ? 'Add a banner' : 'No banner'}</span>
                                                {#if st.canEdit}<span class="cs-banner-size">(952 × 280)</span>{/if}
                                            </span>
                                        </div>
                                    {/if}
                                    {#if st.canEdit && bannerSrc}
                                        <span class="cs-icon-edit"><span class="icon icon-edit"></span></span>
                                    {/if}
                                    {#if st.draft.bannerPath}<span class="cs-icon-new">New</span>{/if}
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
                    {:else if section.id === 'audit'}
                        <AuditLogSection {h} />
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
                                        hasName={!row.veiled && !!row.name}
                                        keyHint={row.name && !row.veiled ? shortNpub(row.npub) : ''}
                                        onactivate={() => csSelectBans([row.npub])}
                                        oncontextmenu={(e) => banMenu(row.npub, e)}
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
                            <p class="cs-lede">Every message, channel and member list in this community lives on these relays, end-to-end encrypted.</p>
                            <div class="cs-relays">
                                {#each st.draft.relays as url, i (url)}
                                    {@const info = relayInfo[url]}
                                    <div class="cs-relay">
                                        <span class="cs-relay-icon">
                                            {#if info?.iconSrc && !relayIconFailed[url]}
                                                <img class="cs-relay-img" src={info.iconSrc} alt="" onerror={() => { relayIconFailed[url] = true; }} />
                                            {:else}
                                                <span class="icon icon-globe"></span>
                                            {/if}
                                        </span>
                                        {#if relayEdit?.index === i}
                                            <!-- svelte-ignore a11y_autofocus -->
                                            <input class="cs-relay-field" type="text" autofocus autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                                                   value={relayEdit.text} oninput={(e) => { relayEdit.text = e.currentTarget.value; relayError = ''; }}
                                                   onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); commitRelayEdit(); } else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); relayEdit = null; } }}
                                                   onblur={commitRelayEdit} />
                                        {:else}
                                            <span class="cs-relay-text">
                                                {#if info?.name}<span class="cs-relay-name cutoff">{info.name}</span>{/if}
                                                <span class="cs-relay-url cutoff" class:cs-relay-sub={info?.name}><span class="cs-relay-scheme">{schemeOf(url)}</span>{addressOf(url)}</span>
                                            </span>
                                            {#if info?.auth}
                                                <button type="button" class="cs-relay-tag" title="What AUTH means" onclick={() => h.explainAuth()}><span class="icon icon-locked"></span>AUTH</button>
                                            {/if}
                                        {/if}
                                        {#if st.canEdit && relayEdit?.index !== i}
                                            <button class="cs-relay-remove cs-relay-edit" title="Edit" onclick={() => { relayEdit = { index: i, text: url }; relayError = ''; }}><span class="icon icon-edit"></span></button>
                                            {#if st.draft.relays.length > 1}
                                                <button class="cs-relay-remove" title="Remove" onclick={() => removeRelay(url)}><span class="icon icon-x"></span></button>
                                            {/if}
                                        {/if}
                                    </div>
                                {:else}
                                    <p class="cs-hint">{st.loading ? 'Loading...' : 'No relays recorded for this community.'}</p>
                                {/each}
                            </div>
                            {#if st.canEdit}
                                <form class="cs-relay-add" onsubmit={(e) => { e.preventDefault(); addRelay(); }}>
                                    <input class="cs-input" type="text" placeholder="wss://relay.example.com" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                                           disabled={st.draft.relays.length >= RELAYS_MAX}
                                           bind:value={relayInput} oninput={() => { relayError = ''; }} />
                                    <button class="cs-btn-save" type="submit" disabled={!relayInput.trim()}>Add</button>
                                </form>
                                {#if relayError}<p class="cs-hint cs-hint-error">{relayError}</p>{/if}
                                <p class="cs-hint">A new relay is checked and given this community's whole history before anyone moves to it, and the old relays tell members where everyone went.</p>
                            {/if}

                        </section>

                        <section class="cs-block" data-anchor="voice-relays">
                            <h3 class="cs-heading">Voice Relays</h3>
                            <p class="cs-lede">Voice channels connect through these relays, end-to-end encrypted. Each relay sees where callers connect from.</p>
                            <div class="cs-relays">
                                {#each st.draft.voiceRelays as url (url)}
                                    <div class="cs-relay">
                                        <span class="cs-relay-icon"><span class="icon icon-volume-max"></span></span>
                                        <span class="cs-relay-url cutoff"><span class="cs-relay-scheme">{schemeOf(url)}</span>{addressOf(url)}</span>
                                        {#if st.canEdit}
                                            <button class="cs-relay-remove" title="Remove" onclick={() => removeVoiceRelay(url)}><span class="icon icon-x"></span></button>
                                        {/if}
                                    </div>
                                {:else}
                                    {#if st.loading}
                                        <p class="cs-hint">Loading...</p>
                                    {:else}
                                        <p class="cs-hint">Using iroh's public relays, run by n0. <button type="button" class="net-link" onclick={() => h.openLink('irohRelays')}>Learn more</button></p>
                                    {/if}
                                {/each}
                            </div>
                            {#if st.canEdit}
                                <form class="cs-relay-add" onsubmit={(e) => { e.preventDefault(); addVoiceRelay(); }}>
                                    <input class="cs-input" type="text" placeholder="https://relay.example.com" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                                           disabled={st.draft.voiceRelays.length >= VOICE_RELAYS_MAX}
                                           bind:value={voiceRelayInput} oninput={() => { voiceRelayError = ''; }} />
                                    <button class="cs-btn-save" type="submit" disabled={!voiceRelayInput.trim()}>Add</button>
                                </form>
                                {#if voiceRelayError}<p class="cs-hint cs-hint-error">{voiceRelayError}</p>{/if}
                            {/if}
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
