<script>
    // Create Community in three short steps: what it's called (with a live preview), who can
    // join, then the way in, a link to share (Public) or friends to invite (Private). The
    // community exists from the end of step 2; step 3 only hands out access to it.
    import { ccState, ccSetSelected } from '../lib/createcommunity.svelte.js';
    import ContactPicker from '../people/ContactPicker.svelte';
    import Avatar from '../ui/Avatar.svelte';
    import StageProgress from '../ui/StageProgress.svelte';
    import { fade } from 'svelte/transition';
    let { h } = $props();   // h: CreateGroupHelpers (js/create-community.js)
    const st = ccState();
    let nameInput = $state(null);
    let filterInput = $state(null);
    let picker = $state(null);
    const nameOk = $derived(!!st.name.trim());
    const pub = $derived(st.access === 'public');

    const STEPS = ['Name', 'Access', 'Invite'];
    const title = $derived(st.step === 1 ? 'Name your community' : st.step === 2 ? 'Who can join?' : pub ? 'Share your invite link' : 'Invite your friends');
    const lead = $derived(
        st.step === 1 ? 'Pick a name and, if you like, an icon. You can change both later.'
        : st.step === 2 ? 'You can change this later from Invite People.'
        : pub ? 'Your community is live. Send this link wherever your people are.'
        : 'Only the people you invite can join. Pick some now, or invite them later.',
    );

    $effect(() => { if (st.step === 1 && nameInput) nameInput.focus(); });
    $effect(() => { if (st.step === 3 && filterInput) filterInput.focus(); });

    // The friend list loads when step 3 asks for it.
    let pickerProps = $state.raw(null);
    $effect(() => {
        pickerProps = null;
        if (st.step !== 3 || pub) return;
        let live = true;
        h.pickerProps().then((props) => { if (live) pickerProps = props; });
        return () => { live = false; };
    });
    $effect(() => { const rev = st.profilesSeq; if (rev && picker) picker.setProfiles(h.profiles()); });

    function onFilter() {
        picker?.setFilter(st.filter);
        const np = h.extractNpub(st.filter);
        if (np && picker) h.pasteNpub(np, picker);
    }
    function next() {
        if (nameOk) st.step = 2;
    }
    // The link as people read it: no scheme, and the long key cut down the middle.
    const prettyLink = $derived.by(() => {
        const u = (st.link || '').replace(/^https?:\/\//, '');
        return u.length > 46 ? u.slice(0, 32) + '…' + u.slice(-8) : u;
    });
    function qr(node, text) {
        const paint = (t) => { if (t) h.renderQr(node, t); };
        paint(text);
        return { update: paint };
    }

    function invite() {
        const picked = [...(picker?.getSelection() || [])];
        if (picked.length) h.sendInvites(picked);
    }
</script>

<div class="cg-setup">
    <ol class="cg-steps" aria-label="Step {st.step} of {STEPS.length}">
        {#each STEPS as label, i (label)}
            <li class:is-done={st.step > i + 1} class:is-current={st.step === i + 1}>
                <span class="cg-step-dot">{#if st.step > i + 1}<span class="icon icon-check"></span>{:else}{i + 1}{/if}</span>
                <span class="cg-step-label">{label}</span>
            </li>
        {/each}
    </ol>
    <h3 class="cg-step-title">{title}</h3>
    <p class="cg-step-lead">{lead}</p>

    {#if st.step === 1}
        <div class="cg-identity">
            <div class="cg-identity-form">
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="create-group-avatar-picker" class:has-image={!!st.avatarPreview} title="Choose community icon" onclick={h.pickAvatar}>
                    {#if st.avatarPreview}
                        <img class="create-group-avatar-preview" src={st.avatarPreview} alt="">
                    {:else}
                        <img class="create-group-avatar-placeholder" src="icons/group-placeholder.svg" alt="">
                    {/if}
                    <div class="create-group-avatar-edit"><span class="icon icon-plus-circle"></span></div>
                </div>
                <div class="cg-name-field">
                    <input id="create-group-name" placeholder="Community Name" maxlength="32" class="chat-input-container chat-new-input" bind:value={st.name} bind:this={nameInput}
                           onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); next(); } }}>
                    <span class="cg-name-count">{st.name.length}/32</span>
                </div>
            </div>
            <!-- The channel pane's own classes, so the preview is how it will look. -->
            <aside class="cg-preview" aria-hidden="true">
                <span class="crop-preview-label">Preview</span>
                <div class="crop-pane cg-preview-pane">
                    <div class="crop-pane-surface">
                        <div class="chatlist-community-head">
                            <Avatar src={st.avatarPreview || null} size={36} group={true} class="chatlist-community-head-avatar" />
                            <div class="chatlist-community-head-meta">
                                <span class="chatlist-community-head-name cutoff">{st.name.trim() || 'Your Community'}</span>
                                <span class="chatlist-community-head-members">
                                    <span class="icon icon-users-multi chatlist-community-head-members-icon"></span>
                                    <span>1 Member</span>
                                </span>
                            </div>
                            <div class="chatlist-community-head-caret"><span class="icon icon-chevron-down"></span></div>
                        </div>
                        <div class="chatlist-channels chatlist-channels-pane cg-preview-channels">
                            <div class="chatlist-channel active">
                                <span class="chatlist-channel-hash"><span class="icon icon-channel-hash"></span></span>
                                <span class="chatlist-channel-name cutoff">general</span>
                            </div>
                        </div>
                    </div>
                </div>
            </aside>
        </div>
    {:else if st.step === 2}
        <div class="cg-choices" role="radiogroup" aria-label="Who can join">
            <button type="button" class="cg-choice" class:is-selected={!pub} role="radio" aria-checked={!pub} disabled={st.busy} onclick={() => { st.access = 'private'; }}>
                <span class="cg-choice-icon"><span class="icon icon-locked"></span></span>
                <span class="cg-choice-text">
                    <span class="cg-choice-title">Private</span>
                    <span class="cg-choice-desc">Only members can invite other people in.</span>
                </span>
                <span class="cg-choice-radio"></span>
            </button>
            <button type="button" class="cg-choice" class:is-selected={pub} role="radio" aria-checked={pub} disabled={st.busy} onclick={() => { st.access = 'public'; }}>
                <span class="cg-choice-icon"><span class="icon icon-globe"></span></span>
                <span class="cg-choice-text">
                    <span class="cg-choice-title">Public</span>
                    <span class="cg-choice-desc">Anyone with an invite link can join. You'll get one to share right away.</span>
                </span>
                <span class="cg-choice-radio"></span>
            </button>
        </div>
    {:else if pub}
        <div class="cg-share" class:is-ready={!!st.link}>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="cg-share-qr" title={st.link ? 'Enlarge QR code' : null} onclick={st.link ? h.showLinkQr : null}>
                {#if st.link}
                    <div class="cg-share-qr-code" use:qr={st.link}></div>
                {:else}
                    <span class="pack-skel cg-share-qr-skel"></span>
                {/if}
            </div>
            <div class="cg-share-body">
                <span class="section-label">Invite link</span>
                {#if st.link}
                    <span class="cg-share-url" title={st.link}>{prettyLink}</span>
                {:else if st.linkError}
                    <span class="cg-share-error">Couldn't create the link. {st.linkError}</span>
                {:else}
                    <span class="cg-share-progress"><StageProgress label={st.prog?.label || 'Getting ready'} pct={st.prog?.pct || 0} /></span>
                {/if}
                <div class="cg-share-actions">
                    {#if st.linkError}
                        <button type="button" class="cmt-btn cmt-btn-primary" onclick={h.retryLink}>Try Again</button>
                    {:else}
                        <button type="button" class="cmt-btn cmt-btn-primary" disabled={!st.link} onclick={h.copyLink}>
                            <span class="icon {st.copied ? 'icon-check' : 'icon-copy'}"></span>{st.copied ? 'Copied' : 'Copy Link'}
                        </button>
                        <button type="button" class="cmt-btn cmt-btn-ghost" disabled={!st.link} onclick={h.showLinkQr}>Show QR</button>
                    {/if}
                </div>
                <span class="cg-share-note">You can make more links, or turn this one off, anytime in Invite People.</span>
            </div>
        </div>
    {:else}
        <div class="cg-invite">
            <div class="emoji-search-container" style="padding: 0; background: transparent; isolation: isolate;">
                <span class="emoji-search-icon icon icon-search"></span>
                <input placeholder="Search friends, or paste an npub" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                       style="padding: 10px 12px 10px 44px; background-color: transparent; border: 1px solid rgba(57, 57, 57, 0.5); border-radius: 8px; color: #fff; font-size: 16px;"
                       bind:value={st.filter} bind:this={filterInput} oninput={onFilter}>
            </div>
            <div class="create-group-list cg-invite-list">
                {#if pickerProps}
                    <ContactPicker bind:this={picker} {...pickerProps} onSelectionChange={(sel) => ccSetSelected(sel.size)} />
                {/if}
            </div>
        </div>
    {/if}
</div>

<div class="create-group-footer">
    <!-- Above the buttons, so they hold still while the stages pass. Its height is kept even
         when empty, for the same reason. -->
    <div class="cg-status">
        {#if st.busy && st.prog}
            <span class="cg-status-line" transition:fade={{ duration: 180 }}><StageProgress label={st.prog.label} pct={st.prog.pct} stages={st.prog.stages} /></span>
        {:else if st.error}
            <span class="cg-status-line is-error" transition:fade={{ duration: 180 }}>{st.status}</span>
        {/if}
    </div>
    {#if st.step === 1}
        <button class="btn accept-btn btn-bounce cg-primary" disabled={!nameOk} onclick={next}>Next</button>
        <button class="btn cancel-btn" onclick={h.close}>Cancel</button>
    {:else if st.step === 2}
        <button class="btn accept-btn btn-bounce cg-primary" disabled={st.busy} onclick={h.create}>Create Community</button>
        <button class="btn cancel-btn" disabled={st.busy} onclick={() => { st.step = 1; }}>Back</button>
    {:else if pub}
        <button class="btn accept-btn btn-bounce cg-primary" onclick={h.finish}>Open Community</button>
    {:else}
        <button class="btn accept-btn btn-bounce cg-primary" disabled={st.busy || !st.selected} onclick={invite}>{st.selected ? `Invite ${st.selected}` : 'Invite'}</button>
        <button class="btn cancel-btn" disabled={st.busy} onclick={h.finish}>Skip for now</button>
    {/if}
</div>
