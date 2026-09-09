<script>
    // The open chat's header as ONE reconciler: the peer's profile landing, a typer, a
    // member count or a renamed channel repaints it with no retro-resolve code. The
    // helpers arrive from chat.js after the shell mounts, so they are read lazily.
    import { openChatId, chatVersion, profileVersion, communityVersion, listVersion, invitesVersion } from '../lib/signals.svelte.js';
    import { chatHeaderHandlers } from '../lib/chatpane.svelte.js';
    import { pinsState, pinsEls } from '../lib/pins.svelte.js';
    import { wallpaperState } from '../lib/wallpaper.svelte.js';
    import Avatar from '../ui/Avatar.svelte';

    const h = () => chatHeaderHandlers();
    // The pins scrim's hit-test needs the button: a tap on it toggles, never dismisses.
    let pinsButton = $state(null);
    $effect(() => { pinsEls().button = pinsButton; });
    const pins = pinsState();
    const wp = wallpaperState();

    const vm = $derived.by(() => {
        const id = openChatId();
        const H = h();
        if (!id || !H) return null;
        chatVersion(id);
        const chat = H.getChat(id) || null;
        const communityId = chat?.metadata?.custom_fields?.community_id || null;
        if (communityId) communityVersion(communityId);
        else profileVersion(id);
        const notes = id === H.myNpub();
        const group = !notes && !!chat && H.isGroup(chat);
        const profile = notes || group ? null : (H.getProfile(id) || null);
        let name, avatarSrc = null, twemoji = false, click = null;
        if (notes) {
            name = 'Notes';
        } else if (group) {
            name = H.communityChatTitle(chat) || `Group ${id.substring(0, 10)}...`;
            avatarSrc = chat.metadata?.avatar_cached ? H.convertFileSrc(chat.metadata.avatar_cached) : null;
            click = () => H.openCommunity(chat);
        } else {
            name = H.getName(profile || id);
            twemoji = !!(profile?.nickname || profile?.name);
            avatarSrc = H.getProfileAvatarSrc(profile) || null;
            if (profile) click = () => H.openProfile(profile);
        }
        // Subtext: a typer outranks everything but Notes.
        const typing = chat ? H.typingText(chat) : '';
        let subtext = '', tags = [], gradient = false;
        if (notes) subtext = 'Encrypted Notes to Self';
        else if (typing) { subtext = typing; gradient = true; }
        else if (group) subtext = H.memberSubtext(communityId);
        else { subtext = profile?.status?.title || ''; tags = profile?.status?.emoji_tags || []; }
        return { id, notes, group, name, twemoji, avatarSrc, click, subtext, tags, gradient, menu: !!chat && H.menuCount(chat) > 0 };
    });

    function nameInto(node, [text, twemoji]) {
        const render = ([t, tw]) => { node.textContent = t; if (tw) h().twemojify(node); };
        render([text, twemoji]);
        return { update: render };
    }

    // ── subtext: status, typing, member count ──
    // Shown text swaps in place; going empty collapses the line (the 300ms wait
    // matches the CSS transition) and a chat switch resets it without the fade.
    let status = $state({ text: '', tags: [], gradient: false, hidden: true });
    let shownId = null;
    let hideTimer = null;
    $effect(() => {
        const v = vm;
        const id = v ? v.id : null;
        if (id !== shownId) {
            shownId = id;
            if (hideTimer) { clearTimeout(hideTimer); hideTimer = null; }
            status = { text: '', tags: [], gradient: false, hidden: true };
        }
        if (!v) return;
        if (v.subtext) {
            if (hideTimer) { clearTimeout(hideTimer); hideTimer = null; }
            status = { text: v.subtext, tags: v.tags, gradient: v.gradient, hidden: false };
        } else if (status.text && !status.hidden) {
            status.hidden = true;
            hideTimer = setTimeout(() => {
                status = { text: '', tags: [], gradient: false, hidden: true };
                hideTimer = null;
            }, 300);
        }
    });
    function statusInto(node, [text, tags, gradient]) {
        const render = ([t, tg, g]) => {
            node.textContent = t;
            if (t && !g) { h().twemojify(node); h().renderCustomEmojiShortcodes(node, tg); }
        };
        render([text, tags, gradient]);
        return { update: render };
    }

    // ── back dot: another chat has unread, or an invite waits ──
    // Reads every chat's version: any row's unread moving is what changes the answer.
    const backDot = $derived.by(() => {
        const H = h();
        if (!openChatId() || !H) return false;
        listVersion();
        invitesVersion();
        for (const c of H.chats()) chatVersion(c.id);
        return H.backDotWanted();
    });

    function openMenu(e) {
        e.stopPropagation();
        h()?.openMenu(e.currentTarget.getBoundingClientRect());
    }
</script>

<div class="chat-header">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="chat-back-btn" class="btn nav-back-btn" onclick={() => h()?.closeChat()}>
        <span class="icon icon-chevron-double-left nav-icon"></span>
        <span class="update-notification-dot" style:display={backDot ? null : 'none'}></span>
    </div>
    <div class="profile-header-info">
        <div class="profile-header-name-row">
            <div id="chat-header-avatar-container">
                {#if vm && !vm.notes}
                    {#key `${vm.id}|${vm.group}`}
                        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                        <span style="display:contents" onclick={() => vm?.click?.()}>
                            <Avatar src={vm.avatarSrc} size={22} class="btn" placeholder={() => h().createAvatarImg(null, 22, vm.group)} />
                        </span>
                    {/key}
                {/if}
            </div>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions, a11y_missing_content -->
            <h3 id="chat-contact" class="cutoff" class:btn={!!vm?.click} class:chat-contact={status.hidden} class:chat-contact-with-status={!status.hidden}
                use:nameInto={[vm ? vm.name : '', !!vm?.twemoji]} onclick={() => vm?.click?.()}></h3>
        </div>
        <span id="chat-contact-status" class="cutoff chat-contact-status btn" class:status-hidden={status.hidden} class:typing-indicator-text={status.gradient}
              use:statusInto={[status.text, status.tags, status.gradient]}></span>
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="btn pins-header-btn" class:pins-open={pins.open} title="Pinned Messages" style:display={pins.button ? null : 'none'}
         bind:this={pinsButton} onclick={() => h()?.togglePins()}>
        <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
            <path d="M8.3767 15.6163L2.71985 21.2732M11.6944 6.64181L10.1335 8.2027C10.0062 8.33003 9.94252 8.39369 9.86999 8.44427C9.80561 8.48917 9.73616 8.52634 9.66309 8.555C9.58077 8.58729 9.49249 8.60495 9.31592 8.64026L5.65145 9.37315C4.69915 9.56361 4.223 9.65884 4.00024 9.9099C3.80617 10.1286 3.71755 10.4213 3.75771 10.7109C3.8038 11.0434 4.14715 11.3867 4.83387 12.0735L11.9196 19.1592C12.6063 19.8459 12.9497 20.1893 13.2821 20.2354C13.5718 20.2755 13.8645 20.1869 14.0832 19.9928C14.3342 19.7701 14.4294 19.2939 14.6199 18.3416L15.3528 14.6771C15.3881 14.5006 15.4058 14.4123 15.4381 14.33C15.4667 14.2569 15.5039 14.1875 15.5488 14.1231C15.5994 14.0505 15.663 13.9869 15.7904 13.8596L17.3512 12.2987C17.4326 12.2173 17.4734 12.1766 17.5181 12.141C17.5578 12.1095 17.5999 12.081 17.644 12.0558C17.6936 12.0274 17.7465 12.0048 17.8523 11.9594L20.3467 10.8904C21.0744 10.5785 21.4383 10.4226 21.6035 10.1706C21.7481 9.95025 21.7998 9.68175 21.7474 9.42348C21.6875 9.12813 21.4076 8.84822 20.8478 8.28839L15.7047 3.14526C15.1448 2.58543 14.8649 2.30552 14.5696 2.24565C14.3113 2.19329 14.0428 2.245 13.8225 2.38953C13.5705 2.55481 13.4145 2.91866 13.1027 3.64636L12.0337 6.14071C11.9883 6.24653 11.9656 6.29944 11.9373 6.34905C11.9121 6.39313 11.8836 6.43522 11.852 6.47496C11.8165 6.51971 11.7758 6.56041 11.6944 6.64181Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="btn nav-menu-btn" title="Chat Options" style:display={vm?.menu ? null : 'none'} onclick={openMenu}>
        <span class="icon icon-dots-horizontal nav-icon"></span>
    </div>
    <!-- Mirrors the Profile edit bar over the header while a wallpaper preview is staged. -->
    {#if wp.editShown}
        <div id="wallpaper-edit-bar" style="display: flex;" style:opacity={wp.editActive ? 1 : 0}>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div id="wallpaper-edit-cancel-btn" style:pointer-events={wp.busy ? 'none' : null} style:opacity={wp.busy ? 0.5 : null} onclick={() => h()?.wallpaperCancel()}>
                <span class="icon icon-edit-x"></span>
                <span>Cancel</span>
            </div>
            <span id="wallpaper-edit-mode-label">{wp.label}</span>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div id="wallpaper-edit-save-btn" style:pointer-events={wp.busy ? 'none' : null} style:opacity={wp.busy ? 0.5 : null} onclick={() => h()?.wallpaperSave()}>
                <span class="icon icon-save"></span>
                <span>Save</span>
            </div>
        </div>
    {/if}
</div>
