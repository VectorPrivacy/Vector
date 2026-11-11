<script>
    // Two panels in one frame. New Message: pick one person (or paste an npub) to DM them; a
    // pasted invite or Vector link opens as it would anywhere. Create Community is its own
    // guided flow (CommunitySetup). The picker remounts fresh per open.
    import { ccState, ccSetSelected } from '../lib/createcommunity.svelte.js';
    import ContactPicker from '../people/ContactPicker.svelte';
    import CommunitySetup from './CommunitySetup.svelte';
    let { h } = $props();   // h: CreateGroupHelpers (js/create-community.js)
    const st = ccState();
    let picker = $state(null);
    let filterInput = $state(null);
    const community = $derived(st.mode === 'community');
    // A pasted invite or Vector link: it previews in the list's place, and the footer opens it.
    const link = $derived(community ? null : h.linkKind(st.filter));
    const inviteKey = $derived(link === 'invite' ? h.inviteKey(st.filter) : null);
    // The pasted invite's preview: null while it loads, then { state, info, iconSrc, member }.
    let invite = $state(null);
    $effect(() => {
        const key = inviteKey;
        invite = null;
        if (!key) return;
        let live = true;
        h.resolveInvite(key).then((r) => {
            if (live) invite = { ...r, member: r.state === 'ok' && h.isMember(r.info.community_id) };
        });
        return () => { live = false; };
    });
    const inviteReady = $derived(invite?.state === 'ok');

    // Fresh picker props per open: a snapshot of profiles and chat times, like the invite panel.
    let pickerProps = $state.raw(null);
    $effect(() => {
        const session = st.seq;
        pickerProps = null;
        if (!session || community) return;
        let live = true;
        h.pickerProps().then((props) => { if (live) pickerProps = props; });
        return () => { live = false; };
    });
    $effect(() => { if (st.seq && filterInput) filterInput.focus(); });

    function primary() {
        if (inviteKey) return inviteReady && h.joinInvite(inviteKey, invite.info.community_id);
        if (link) return h.openLink(st.filter);
        const picked = [...(picker?.getSelection() || [])];
        if (picked.length === 1) h.startDm(picked[0]);
    }
    $effect(() => { const rev = st.profilesSeq; if (rev && picker) picker.setProfiles(h.profiles()); });

    // Typing filters; a pasted valid npub is picked, or enters the list as a stranger.
    function onFilter() {
        picker?.setFilter(st.filter);
        const np = h.extractNpub(st.filter);
        if (!np || !picker) return;
        h.pasteNpub(np, picker);
    }
</script>

<div class="create-group-content" class:is-community={community}>
    <center class="chat-new-content">
        <h2 class="chat-new-title"><span class="icon {community ? 'icon-users-multi' : 'icon-chats'} chat-new-icon" style="width: 50px; height: 30px; opacity: 0.7; display: inline-block; vertical-align: bottom;"></span>{community ? 'Create Community' : 'New Message'}</h2>
        <hr class="divider chat-new-divider">
    </center>

    {#if community}
        <CommunitySetup {h} />
    {:else}
        <center class="chat-new-center-content" style="box-sizing: border-box; width: 100%;">
            <div class="chat-new-content" style="width: 100%; box-sizing: border-box; margin-left: 0; margin-top: 12px;">
                <div class="emoji-search-container" style="padding: 0; background: transparent; isolation: isolate;">
                    <span class="emoji-search-icon icon icon-search"></span>
                    <input id="create-group-filter" placeholder="Search (paste npub, invite link, etc.)" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                           style="padding: 10px 12px 10px 44px; background-color: transparent; border: 1px solid rgba(57, 57, 57, 0.5); border-radius: 8px; color: #fff; font-size: 16px;"
                           bind:value={st.filter} bind:this={filterInput} oninput={onFilter}
                           onkeydown={(e) => { if (e.key === 'Enter' && link) { e.preventDefault(); primary(); } }}>
                    <button type="button" class="create-group-scan" title="Scan QR Code" aria-label="Scan QR code" onclick={h.scan}><span class="create-group-scan-glyph"></span></button>
                </div>
                {#if link}
                    <div class="cg-link" class:is-loading={inviteKey && !invite} class:is-invalid={invite && !inviteReady}>
                        {#if !inviteKey}
                            <div class="cg-link-eyebrow">Vector Link</div>
                            <img class="cg-link-icon is-mark" src="icons/vector-mark.svg" alt="">
                            <div class="cg-link-name">Open in Vector</div>
                            <div class="cg-link-desc">This link opens a place inside Vector.</div>
                        {:else if !invite}
                            <div class="cg-link-eyebrow">Community Invite</div>
                            <span class="pack-skel cg-link-icon"></span>
                            <span class="pack-skel cg-link-skel-name"></span>
                            <span class="pack-skel cg-link-skel-desc"></span>
                            <span class="pack-skel cg-link-skel-desc short"></span>
                            <div class="cg-link-wait" role="status"><span class="cg-link-spin" aria-hidden="true"></span>Loading invite...</div>
                        {:else if !inviteReady}
                            <div class="cg-link-eyebrow">Community Invite</div>
                            <img class="cg-link-icon is-warning" src="icons/vector_warning.svg" alt="">
                            <div class="cg-link-name">Invite Unavailable</div>
                            <div class="cg-link-desc">This invite couldn't be loaded. It may have been revoked or expired, or its relays are unreachable.</div>
                        {:else}
                            <div class="cg-link-eyebrow">{invite.member ? 'Your Community' : "You've been invited to join"}</div>
                            <img class="cg-link-icon" src={invite.iconSrc || 'icons/group-placeholder.svg'} alt="">
                            <div class="cg-link-name">{invite.info.name}</div>
                            {#if invite.info.description}<div class="cg-link-desc">{invite.info.description}</div>{/if}
                            {#if invite.member}<div class="cg-link-member"><span class="icon icon-check"></span>You're already a member</div>{/if}
                        {/if}
                    </div>
                    <div class="create-group-footer cg-link-actions">{@render actions()}</div>
                {/if}
                <div id="create-group-list" class="create-group-list" style:display={link ? 'none' : null}>
                    {#key st.seq}
                        {#if pickerProps}
                            <ContactPicker bind:this={picker} {...pickerProps} single onHub={h.openHub} onSelectionChange={(sel) => ccSetSelected(sel.size)} />
                        {/if}
                    {/key}
                </div>
            </div>
        </center>
    {/if}
</div>

{#snippet actions()}
    <button id="create-group-create-btn" class="btn accept-btn btn-bounce"
            disabled={st.busy || (inviteKey ? !inviteReady : !link && st.selected !== 1)} onclick={primary}>
        {st.busy ? st.status || 'Working...' : inviteKey ? (invite?.member ? 'Open Community' : 'Join Community') : link ? 'Open Link' : 'Start Chat'}
    </button>
    <button class="btn cancel-btn" onclick={h.close}>Cancel</button>
{/snippet}

<!-- A link's preview carries the actions itself; otherwise they sit at the foot. Create
     Community's steps carry their own. -->
{#if !link && !community}
    <div class="create-group-footer">
        <p class="chat-contact-status" class:cmt-err={st.error}>
            {#if st.status}{st.status}{/if}
        </p>
        {@render actions()}
    </div>
{/if}
