<script>
    // The Community Moderation console: raid triage and the two containment verbs
    // (banlist edition, key rotation). Head, raid banner (or the busy notice that borrows
    // its slot), the Members and Policies tabs, the tallies and the three actions.
    // Ticked = kept; the unticked set is what a rotation cuts.
    import { modState, modIntel, modKeep, modSetQuery } from '../lib/moderation.svelte.js';
    import { modOverlay } from '../lib/moderation.svelte.js';
    import { popIn } from '../lib/popin.js';
    import ModList from './ModList.svelte';
    import ModFilters from './ModFilters.svelte';
    import ModStats from './ModStats.svelte';
    import PolicyDesigner from './PolicyDesigner.svelte';
    import { shellScreens } from '../lib/shell.svelte.js';
    const screens = shellScreens();
    // h: ago(secs), displayName(npub), avatarSrc(npub),
    //    showTab(which), close(), revoke(), rotate(), banRotate()
    let { h } = $props();

    const ov = modOverlay.state();
    const st = modState();
    const intel = $derived(modIntel());
    const keep = $derived(modKeep());
    const members = $derived(st.tab === 'members');
    const cut = $derived(intel ? intel.report.members.filter(x => !keep.has(x.npub)).length : 0);
    const total = $derived(intel ? intel.report.members.length : 0);
    const invites = $derived(intel ? intel.invites.length : 0);
    const room = $derived(intel ? intel.banlist_max - intel.banlist_count : 0);
    const overCap = $derived(cut > room);

    // The raid banner, or nothing; a publish borrows the slot for its progress.
    const alert = $derived.by(() => {
        if (st.busy) return { title: st.busyTitle, body: st.busyBody };
        const r = intel?.report;
        if (!r || !r.raid_detected) return null;
        // `size` is the true cluster; `members` is only a display sample the backend caps.
        const biggest = r.cohorts[0];
        const burst = r.burst_size >= 2 && r.burst_to_ms > r.burst_from_ms
            ? ` ${r.burst_size} joined within ${h.ago(Math.round((r.burst_to_ms - r.burst_from_ms) / 1000))} of each other.` : '';
        return {
            title: `Raid: ${r.suspects} accounts flagged.`,
            body: (biggest ? ` ${biggest.size} posted “${biggest.sample.slice(0, 40)}”.` : '') + burst
                + ' They start unticked, so they are the ones being removed.',
        };
    });
</script>

<svelte:window onkeydown={(e) => { if (ov.active && e.key === 'Escape') h.close(); }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="mod-overlay" class="mod-overlay" class:active={ov.active} class:closing={ov.closing}
     onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
    <div class="mod-card" class:busy={st.busy} use:popIn={ov.tick}>
        <div class="mod-head">
            <div class="mod-head-titles">
                <span class="mod-label">Moderation</span>
                <h3 class="mod-title cutoff">{intel ? (intel.name || 'Community') : ''}</h3>
            </div>
            <span class="mod-epoch">{intel ? `Epoch ${intel.epoch}` : ''}</span>
            <button class="mod-x" aria-label="Close" disabled={st.busy} onclick={() => h.close()}>&#x2715;</button>
        </div>

        {#if alert}
            <div class="mod-alert" class:working={st.busy}>
                <span class="icon icon-warning mod-alert-icon"></span>
                <div class="mod-alert-text">
                    <strong>{alert.title}</strong>
                    <span>{alert.body}</span>
                </div>
            </div>
        {/if}

        <div class="mod-tabs" role="tablist">
            <button class="mod-tab" class:active={members} role="tab" onclick={() => h.showTab('members')}>Members</button>
            <button class="mod-tab" class:active={!members} role="tab" onclick={() => h.showTab('policies')}>Policies</button>
        </div>

        <!-- Both panes stay mounted: the designer is mounted once, the list keeps its scroll. -->
        <div class="mod-policies" style:display={members ? 'none' : ''}>
            {#if screens.policyDesigner}<PolicyDesigner h={screens.policyDesigner.h} />{/if}
        </div>

        <div id="mod-members-pane" style:display={members ? '' : 'none'}>
            <div class="mod-stats"><ModStats /></div>
            <p class="mod-explain">Everyone is kept by default. Untick someone to cut them off at the next rotation.</p>
            <div class="mod-toolbar">
                <div class="mod-search">
                    <span class="icon icon-search mod-search-icon"></span>
                    <input placeholder="Search members..." autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                           value={st.query} oninput={(e) => modSetQuery(e.currentTarget.value)}>
                </div>
                <div class="mod-filters"><ModFilters /></div>
            </div>
            <div class="mod-list"><ModList {h} /></div>
        </div>

        <!-- The removal actions belong to the member list; the Policies tab keeps
             "edit a rule" and "remove people" out of one footer. -->
        <div class="mod-foot" style:display={members ? '' : 'none'}>
            <div class="mod-tally">
                <span class="mod-tally-keep"><b>{total - cut}</b> staying</span>
                <!-- A red "0 removing" reads as a standing alarm; it belongs there once someone is unticked. -->
                <span class="mod-tally-cut" hidden={cut === 0}><b>{cut}</b> removing</span>
                <span class="mod-tally-ban">{intel ? `banlist ${intel.banlist_count}/${intel.banlist_max}` : ''}</span>
            </div>
            <div class="mod-actions">
                <button class="mod-btn" disabled={st.busy || !intel || invites === 0} onclick={() => h.revoke()}>
                    <span class="icon icon-locked"></span><span class="mod-btn-label">{invites ? `Revoke ${invites} invite${invites === 1 ? '' : 's'}` : 'No invite links'}</span>
                </button>
                <!-- A bare rotation with nobody cut is legitimate: it answers a leaked link. -->
                <button class="mod-btn" disabled={st.busy || !intel} onclick={() => h.rotate()}>
                    <span class="icon icon-refresh"></span><span class="mod-btn-label">{cut ? `Remove ${cut} & rotate` : 'Rotate keys'}</span>
                </button>
                <button class="mod-btn mod-btn-danger" disabled={st.busy || !intel || cut === 0 || overCap}
                        title={overCap && intel ? `The banlist holds ${intel.banlist_max}; only ${room} slots are free. Rotate instead: it has no ceiling.` : ''}
                        onclick={() => h.banRotate()}>
                    <span class="icon icon-x-user"></span><span class="mod-btn-label">{cut ? `Ban ${cut} & rotate` : 'Ban & rotate'}</span>
                </button>
            </div>
        </div>
    </div>
</div>
