<script>
    import { shellState, shellScreens } from '../lib/shell.svelte.js';
    import { setOverviewRoster } from '../lib/overview.svelte.js';
    import MemberRoster from './MemberRoster.svelte';
    import MemberSearch from './MemberSearch.svelte';
    // The Community overview's scroll body, from overview state: mute, icon (with the
    // manage-metadata pencil and upload ring), name and description (inline-editable
    // for managers) and the action row. The chat header above it
    // is the shell's (GroupOverviewPane). The member search and roster host are static: the
    // roster island mounts into #group-overview-members from the app.
    import { overviewState } from '../lib/overview.svelte.js';

    let { h } = $props();   // h: memberSubtext, openNotifyMenu, pickIcon, rename, setDescription, setTopic, invite, moderate, channelOf, editChannel, leaveOrDelete

    const ov = overviewState();
    const manage = $derived(!!ov.caps.manage_metadata);
    // In a community of many channels this is the channel's view: its name and topic.
    const manageTopic = $derived(!!ov.caps.manage_channels);

    let iconBroken = $state(false);
    $effect(() => { ov.avatarSrc; iconBroken = false; });

    // ── inline edits ──
    let editing = $state(null);     // 'name' | 'description' | null
    let draft = $state('');
    let leaving = $state(false);
    function startEdit(kind) {
        if (!(kind === 'topic' ? manageTopic : manage)) return;
        draft = kind === 'name' ? ov.name : kind === 'topic' ? ov.channel?.topic || '' : ov.description;
        editing = kind;
    }
    function commit() {
        if (!editing) return;
        const kind = editing;
        editing = null;
        const value = draft.trim();
        if (kind === 'name') { if (value && value !== ov.name) h.rename(value); }
        else if (kind === 'topic') { if (value !== (ov.channel?.topic || '')) h.setTopic(value); }
        else if (value !== ov.description) h.setDescription(value);
    }
    function cancel() { editing = null; }
    function focus(el) { el.focus(); if (el.select) el.select(); }

    const shell = shellState();
    const screens = shellScreens();
    let rosterInst = $state(null);
    $effect(() => { setOverviewRoster(rosterInst); });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="group-mute-btn" class="profile-option"
     onclick={(e) => { const r = e.currentTarget.getBoundingClientRect(); h.openNotifyMenu(r.left, r.bottom + 4); }}>
    <span class="icon icon-{ov.muted ? 'volume-mute' : 'bell'} navbar-icon"></span>
    <p class="navbar-text" style="font-size: 10px;">{ov.muted ? 'Muted' : 'Notify'}</p>
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
    {#if ov.channel}
        <!-- Renamed in its editor, with the rest of the channel. -->
        <h3 id="group-overview-secondary-name" class="chat-contact-with-status cutoff"
            style="max-width: 90%; margin-left: auto; margin-right: auto;">{ov.channel.voice ? '' : '#'}{ov.channel.name}</h3>
        {#if editing === 'topic'}
            <textarea class="group-name-input" rows="2" bind:value={draft} use:focus
                      onblur={commit} onkeydown={(e) => { if (e.key === 'Escape') cancel(); }}></textarea>
        {:else if ov.channel.topic || manageTopic}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <span id="group-overview-description" class="chat-contact-status" class:group-editable={manageTopic} class:group-placeholder={!ov.channel.topic && manageTopic}
                  style="width: 90%; white-space: pre-line; overflow-y: auto; max-height: 225px; font-style: normal; margin-top: 10px;"
                  onclick={() => startEdit('topic')}>{ov.channel.topic || 'Add a topic...'}</span>
        {/if}
    {:else if editing === 'name'}
        <input type="text" class="group-name-input" maxlength="32" bind:value={draft} use:focus
               onblur={commit} onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); e.target.blur(); } if (e.key === 'Escape') cancel(); }}>
    {:else}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
        <h3 id="group-overview-secondary-name" class="chat-contact-with-status btn cutoff" class:group-editable={manage}
            style="max-width: 90%; margin-left: auto; margin-right: auto;" onclick={() => startEdit('name')}>{ov.name}</h3>
    {/if}
    {#if ov.channel}
        <!-- The channel's topic stands in for the community's description. -->
    {:else if editing === 'description'}
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
            <!-- Moderation lives in the community's menu; a flagged raid brings it here, where it
                 can't be missed. Batch containment needs BAN. -->
            {#if ov.caps.ban && ov.raid}
                <button id="group-moderate-btn" class="btn accept-btn btn-bounce raid-alert" title={`${ov.raid.suspects} accounts flagged as a raid`}
                        style="background-color: transparent; display: flex; align-items: center; gap: 6px;" onclick={h.moderate}>
                    <span class="icon icon-warning" style="width: 16px; height: 16px; flex-shrink: 0; position: relative; background-color: var(--danger-color);"></span>
                    <span style="color: white;">Moderate</span>
                </button>
            {:else if ov.caps.manage_channels && h.channelOf()}
                <button id="group-edit-channel-btn" class="btn accept-btn btn-bounce" style="background-color: transparent; display: flex; align-items: center; gap: 6px;" onclick={h.editChannel}>
                    <span class="icon icon-edit" style="width: 16px; height: 16px; flex-shrink: 0; position: relative; background-color: var(--icon-color-primary);"></span>
                    <span style="color: white;">Edit Channel</span>
                </button>
            {/if}
            <!-- The owner dissolves the community from its menu, behind its confirmation. -->
            {#if !ov.isOwner}
                <button id="group-leave-btn" class="btn cancel-btn btn-bounce" style="display: flex; align-items: center; gap: 6px;"
                        style:opacity={leaving ? '0.5' : null} style:pointer-events={leaving ? 'none' : null}
                        onclick={async () => { leaving = true; try { await h.leaveOrDelete(); } finally { leaving = false; } }}>
                    <span class="icon icon-x-user" style="width: 16px; height: 16px; flex-shrink: 0; position: relative; background-color: var(--danger-color);"></span>
                    <span style="color: white;">Leave</span>
                </button>
            {/if}
        </div>
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
