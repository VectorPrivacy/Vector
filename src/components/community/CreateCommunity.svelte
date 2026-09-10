<script>
    // The Create Community panel: avatar and name, an optional contact picker for direct
    // invites, and the footer whose status doubles as the selection counter. A name is all
    // that is required; the picker remounts fresh per open.
    import { ccState, ccSetSelected } from '../lib/createcommunity.svelte.js';
    import ContactPicker from '../people/ContactPicker.svelte';
    let { h } = $props();   // h: CreateGroupHelpers (js/create-community.js)
    const st = ccState();
    let picker = $state(null);
    let nameInput = $state(null);
    const nameOk = $derived(!!st.name.trim());

    // Fresh picker props per open: a snapshot of profiles and chat times, like the invite panel.
    let pickerProps = $state.raw(null);
    $effect(() => {
        const session = st.session;
        pickerProps = null;
        if (!session) return;
        let live = true;
        h.pickerProps().then((props) => { if (live) pickerProps = props; });
        return () => { live = false; };
    });
    $effect(() => { if (st.session && nameInput) nameInput.focus(); });
    $effect(() => { const rev = st.profilesRev; if (rev && picker) picker.setProfiles(h.profiles()); });

    // Typing filters; a pasted valid npub is picked, or enters the list as a stranger.
    function onFilter() {
        picker?.setFilter(st.filter);
        const np = h.extractNpub(st.filter);
        if (!np || !picker) return;
        h.pasteNpub(np, picker);
    }
</script>

<div class="create-group-content">
    <center class="chat-new-content">
        <h2 class="chat-new-title"><span class="icon icon-chats chat-new-icon" style="width: 50px; height: 30px; opacity: 0.7; display: inline-block; vertical-align: bottom;"></span>Create New Group</h2>
        <hr class="divider chat-new-divider">
        <div class="row create-group-name-row" style="gap: 12px; align-items: center; padding: 0 15px;">
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="create-group-avatar-picker" class:has-image={!!st.avatarPreview} title="Choose group avatar" onclick={h.pickAvatar}>
                {#if st.avatarPreview}
                    <img class="create-group-avatar-preview" src={st.avatarPreview} alt="">
                {:else}
                    <img class="create-group-avatar-placeholder" src="icons/group-placeholder.svg" alt="">
                {/if}
                <div class="create-group-avatar-edit"><span class="icon icon-plus-circle"></span></div>
            </div>
            <div class="create-group-name-wrap" class:needs-name={st.selected > 0}>
                <input id="create-group-name" placeholder="Group Name..." maxlength="32" class="chat-input-container chat-new-input" bind:value={st.name} bind:this={nameInput}>
                <span class="create-group-name-required">Required*</span>
            </div>
        </div>
    </center>

    <center class="chat-new-center-content" style="padding: 0 15px; box-sizing: border-box; width: 100%;">
        <div class="chat-new-content" style="width: 100%; box-sizing: border-box; margin-left: 0; margin-top: 12px;">
            <div class="emoji-search-container" style="padding: 0; background: transparent; isolation: isolate;">
                <span class="emoji-search-icon icon icon-search"></span>
                <input id="create-group-filter" placeholder="Search" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                       style="padding: 10px 12px 10px 44px; background-color: transparent; border: 1px solid rgba(57, 57, 57, 0.5); border-radius: 8px; color: #fff; font-size: 16px;"
                       bind:value={st.filter} oninput={onFilter}>
            </div>
            <div id="create-group-list" class="create-group-list">
                {#key st.session}
                    {#if pickerProps}
                        <ContactPicker bind:this={picker} {...pickerProps} onSelectionChange={(sel) => ccSetSelected(sel.size)} />
                    {/if}
                {/key}
            </div>
        </div>
    </center>
</div>

<div class="create-group-footer">
    <p class="chat-contact-status" class:cmt-err={st.error}>
        {#if st.status}{st.status}{:else if st.selected}<span class="create-group-selection-pill"><span class="cg-pill-glyph"><span class="icon icon-users-multi"></span></span>{st.selected} User{st.selected === 1 ? '' : 's'} Selected</span>{/if}
    </p>
    <button id="create-group-create-btn" class="btn accept-btn btn-bounce" disabled={!nameOk || st.busy} onclick={() => h.create([...(picker?.getSelection() || [])])}>{st.busy ? 'Creating...' : 'Create Group'}</button>
    <button class="btn cancel-btn" onclick={h.close}>Cancel</button>
</div>
