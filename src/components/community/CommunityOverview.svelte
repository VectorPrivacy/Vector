<script>
    import { shellState, shellScreens } from '../lib/shell.svelte.js';
    import { setOverviewRoster } from '../lib/overview.svelte.js';
    import MemberRoster from './MemberRoster.svelte';
    import MemberSearch from './MemberSearch.svelte';
    // The Community overview's scroll body, from overview state: mute, icon (with the
    // manage-metadata pencil and upload ring), name and description (inline-editable
    // for managers), the action row and the v2 upgrade row. The chat header above it
    // is the shell's (GroupOverviewPane). The member search and roster host are static: the
    // roster island mounts into #group-overview-members from the app.
    import { overviewState } from '../lib/overview.svelte.js';

    let { h } = $props();   // h: memberSubtext, toggleMute, pickIcon, rename, setDescription, invite, moderate, leaveOrDelete, migrate

    const ov = overviewState();
    const manage = $derived(!!ov.caps.manage_metadata);

    let iconBroken = $state(false);
    $effect(() => { ov.avatarSrc; iconBroken = false; });

    // ── inline edits ──
    let editing = $state(null);     // 'name' | 'description' | null
    let draft = $state('');
    let leaving = $state(false);
    function startEdit(kind) {
        if (!manage) return;
        draft = kind === 'name' ? ov.name : ov.description;
        editing = kind;
    }
    function commit() {
        if (!editing) return;
        const kind = editing;
        editing = null;
        const value = draft.trim();
        if (kind === 'name') { if (value && value !== ov.name) h.rename(value); }
        else if (value !== ov.description) h.setDescription(value);
    }
    function cancel() { editing = null; }
    function focus(el) { el.focus(); if (el.select) el.select(); }

    const migrationLabel = $derived.by(() => {
        const s = ov.migration;
        if (!s) return '';
        if (s.state === 'locked') {
            const days = Math.max(0, Math.ceil((s.unlock_at - Date.now() / 1000) / 86400));
            return days > 1 ? `Upgrade unlocks in ${days} days` : 'Upgrade unlocks soon';
        }
        return s.state === 'in_progress' ? 'Resume upgrade' : 'Upgrade to Concord v2';
    });
    let upgrading = $state(false);
    async function migrate() {
        if (upgrading || ov.migration?.state === 'locked') return;
        upgrading = true;
        try { await h.migrate(); } finally { upgrading = false; }
    }
    const shell = shellState();
    const screens = shellScreens();
    let rosterInst = $state(null);
    $effect(() => { setOverviewRoster(rosterInst); });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="group-mute-btn" class="profile-option" onclick={h.toggleMute}>
    <span class="icon icon-volume-{ov.muted ? 'mute' : 'max'} navbar-icon"></span>
    <p class="navbar-text" style="font-size: 10px;">{ov.muted ? 'Unmute' : 'Mute'}</p>
</div>
<!-- The whole icon is the pick target for managers; the pencil is the cue. -->
<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="group-overview-avatar-frame" style:cursor={manage ? 'pointer' : null} onclick={manage ? h.pickIcon : null}>
    <span id="group-overview-avatar" class="icon icon-chat-circle" style:display={ov.avatarSrc && !iconBroken ? 'none' : 'inline-block'}></span>
    {#if ov.avatarSrc && !iconBroken}
        <img src={ov.avatarSrc} class="group-overview-avatar-img" alt="" onerror={() => { iconBroken = true; }}>
    {/if}
    {#if manage}
        <div class="group-avatar-edit-overlay" style:opacity={ov.upload ? '1' : null}>
            {#if ov.upload}
                <div class="profile-upload-spinner community-upload-ring" style="--progress: {Math.max(5, ov.upload.progress || 0)}%;"></div>
            {:else}
                <span class="icon icon-edit" style="width:16px;height:16px;background-color:#fff;"></span>
            {/if}
        </div>
    {/if}
</div>
<div class="group-overview-body">
    {#if editing === 'name'}
        <input type="text" class="group-name-input" maxlength="32" bind:value={draft} use:focus
               onblur={commit} onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); e.target.blur(); } if (e.key === 'Escape') cancel(); }}>
    {:else}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
        <h3 id="group-overview-secondary-name" class="chat-contact-with-status btn cutoff" class:group-editable={manage}
            style="max-width: 90%; margin-left: auto; margin-right: auto;" onclick={() => startEdit('name')}>{ov.name}</h3>
    {/if}
    {#if editing === 'description'}
        <textarea class="group-name-input" maxlength="500" rows="2" bind:value={draft} use:focus
                  onblur={commit} onkeydown={(e) => { if (e.key === 'Escape') cancel(); }}></textarea>
    {:else if ov.description || manage}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span id="group-overview-description" class="chat-contact-status" class:group-editable={manage} class:group-placeholder={!ov.description && manage}
              style="width: 90%; white-space: pre-line; overflow-y: auto; max-height: 225px; font-style: normal; margin-top: 10px;"
              onclick={() => startEdit('description')}>{ov.description || 'Add a description...'}</span>
    {/if}
    <div style="margin-top: 40px; width: 90%; margin-left: auto; margin-right: auto;">
        <div class="group-overview-actions">
            {#if ov.caps.create_invite}
                <button id="group-invite-member-btn" class="btn accept-btn btn-bounce" style="background-color: transparent; display: flex; align-items: center; gap: 6px;" onclick={h.invite}>
                    <span class="icon icon-add-user" style="width: 16px; height: 16px; flex-shrink: 0; position: relative; background-color: var(--icon-color-primary);"></span>
                    <span style="color: white;">Invite</span>
                </button>
            {/if}
            <!-- Batch containment needs BAN, and the rotation it drives is a v2 verb. -->
            {#if ov.caps.ban && ov.isV2}
                <button id="group-moderate-btn" class="btn accept-btn btn-bounce" class:raid-alert={!!ov.raid} title={ov.raid ? `${ov.raid.suspects} accounts flagged as a raid` : null}
                        style="background-color: transparent; display: flex; align-items: center; gap: 6px;" onclick={h.moderate}>
                    <span class="icon icon-warning" style="width: 16px; height: 16px; flex-shrink: 0; position: relative; background-color: var(--danger-color);"></span>
                    <span style="color: white;">Moderate</span>
                </button>
            {/if}
            <!-- The owner cannot leave their own root: their button dissolves it for everyone. -->
            <button id="group-leave-btn" class="btn cancel-btn btn-bounce" style="display: flex; align-items: center; gap: 6px;"
                    style:opacity={leaving ? '0.5' : null} style:pointer-events={leaving ? 'none' : null}
                    onclick={async () => { leaving = true; try { await h.leaveOrDelete(); } finally { leaving = false; } }}>
                <span class="icon icon-x-user" style="width: 16px; height: 16px; flex-shrink: 0; position: relative; background-color: var(--danger-color);"></span>
                <span style="color: white;">{ov.isOwner ? 'Delete Community' : 'Leave'}</span>
            </button>
        </div>
        {#if ov.migration}
            {@const locked = ov.migration.state === 'locked' || upgrading}
            <div id="group-migrate-row" style="display: flex; justify-content: center; margin-bottom: 10px;">
                <button class="btn accept-btn btn-bounce" style="background-color: transparent; display: flex; align-items: center; gap: 6px; margin: 0 auto;"
                        style:opacity={locked ? '0.5' : null} style:pointer-events={locked ? 'none' : null} onclick={migrate}>
                    <svg viewBox="0 0 124.86 124.86" style="width: 16px; height: 16px; flex-shrink: 0;" aria-hidden="true">
                        <path fill="#fff" d="M97.91,28.84c8.64,9.11,13.38,20.99,13.38,33.59s-4.74,24.48-13.38,33.6c.3.32.61.64.93.96,2.94,2.95,6.2,5.47,9.69,7.54,4.8-5.25,8.64-11.22,11.41-17.79,3.26-7.7,4.91-15.88,4.91-24.3s-1.65-16.6-4.91-24.3c-2.78-6.56-6.61-12.54-11.41-17.79-3.49,2.07-6.75,4.59-9.69,7.54-.32.32-.63.64-.93.96Z"/>
                        <path fill="#59fcb3" d="M99.58,61.81c0-10.02-3.9-19.44-10.99-26.53-7.09-7.09-16.51-10.99-26.53-10.99-10.02,0-19.44,3.9-26.53,10.99-7.09,7.09-10.99,16.51-10.99,26.53s3.9,19.44,10.99,26.53c7.09,7.09,16.51,10.99,26.53,10.99s19.45-3.9,26.53-10.99c7.09-7.09,10.99-16.51,10.99-26.53ZM38.1,61.81c0-13.21,10.75-23.96,23.96-23.96s23.96,10.75,23.96,23.96-10.75,23.95-23.96,23.95-23.96-10.75-23.96-23.95Z"/>
                        <path fill="#1ea680" d="M89.25,106.58c-.67-.67-1.33-1.36-1.96-2.05-7.45,4.42-15.97,6.77-24.86,6.77-13.05,0-25.32-5.08-34.55-14.31-9.23-9.23-14.31-21.5-14.31-34.55s5.08-25.32,14.31-34.55c9.23-9.23,21.5-14.31,34.55-14.31,8.89,0,17.41,2.36,24.86,6.77.64-.7,1.29-1.38,1.96-2.05,2.69-2.69,5.58-5.1,8.66-7.23-3.49-2.42-7.23-4.47-11.18-6.14-7.7-3.26-15.88-4.91-24.3-4.91s-16.6,1.65-24.3,4.91c-7.44,3.14-14.11,7.65-19.84,13.38-5.73,5.73-10.23,12.41-13.38,19.84C1.65,45.83,0,54,0,62.43s1.65,16.6,4.91,24.3c3.14,7.44,7.64,14.11,13.38,19.84,5.73,5.73,12.41,10.23,19.84,13.38,7.7,3.26,15.88,4.91,24.3,4.91,8.43,0,16.6-1.65,24.3-4.91,3.95-1.67,7.69-3.72,11.18-6.14-3.08-2.13-5.98-4.55-8.66-7.23Z"/>
                    </svg>
                    <span style="color: white;">{upgrading ? 'Upgrading...' : migrationLabel}</span>
                </button>
            </div>
        {/if}
        {#if !shell.ws}<MemberSearch />{/if}
        <div id="group-overview-members" style="padding: 6px; border-radius: 8px; border: 1px solid rgba(57, 57, 57, 0.5);">
            {#if screens.roster}
                {#key screens.roster.key}
                    <MemberRoster {...screens.roster.props} bind:this={rosterInst} />
                {/key}
            {/if}
        </div>
    </div>
</div>
