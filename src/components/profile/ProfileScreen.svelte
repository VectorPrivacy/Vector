<script>
    // The expanded profile: header, banner and avatar, names and status, badges, the contact
    // options, description and npub, plus our own profile's Edit Mode. Every field derives
    // from the open profile's signal; the container (#profile) stays with the nav.
    import { profileViewState, profileVersion, chatVersion } from '../lib/signals.svelte.js';
    import { profileEdit, profileEditDirty } from '../lib/profileedit.svelte.js';
    import { profileScreen } from '../lib/profilescreen.svelte.js';
    import ProfileEditFields from './ProfileEditFields.svelte';
    import Avatar from '../ui/Avatar.svelte';

    const BLANK = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';

    let { root, h } = $props();
    // h: getProfile, getName, getProfileAvatarSrc, getProfileBannerSrc, twemojify,
    //    renderCustomEmojiShortcodes, renderMentions, isMuted, botIcon, invitedCount, fawkesBadge,
    //    bugHunterTier, showInviteBadge, showFawkesCard, showBugHunterCard, showNavbar(on),
    //    back(), message(), toggleMute(), copyProfileLink() → Promise<boolean>, copyNpub() → Promise<boolean>,
    //    showQr(), block(), nickname(), enterEdit(), exitEdit(cancel), setStatus(), pickPicture(kind),
    //    toggleSwitcher(), toggleSwitcherEdit()

    const view = profileViewState();
    const edit = profileEdit();
    const sc = profileScreen();

    const live = $derived.by(() => {
        const id = view.id;
        if (!id) return null;
        profileVersion(id);
        chatVersion(id);
        const p = h.getProfile(id) || { id };
        const hasName = !!(p.nickname || p.name);
        const statusTitle = p.status?.title || '';
        return {
            id, p,
            mine: !!p.mine,
            name: h.getName(p),
            hasName,
            statusTitle,
            statusText: statusTitle || (p.mine ? 'Set a Status' : ''),
            emojiTags: p.status?.emoji_tags || [],
            bannerSrc: h.getProfileBannerSrc(p),
            avatarSrc: h.getProfileAvatarSrc(p),
            hasBanner: !!p.banner,
            secondary: p.nickname || p.name || p.display_name || (p.mine ? 'Anonymous' : id.substring(0, 10) + '…'),
            bot: !!p.bot,
            about: typeof p.about === 'string' ? p.about : '',
            muted: !!h.isMuted(id),
            blocked: !!p.is_blocked,
        };
    });
    // Our own profile in Edit Mode holds its last derivation: the backend may emit a stale
    // banner mid-edit, and the user's picked preview must survive it.
    let m = $state.raw(null);
    $effect(() => {
        const t = live;
        if (!t || (view.editing && t.mine)) return;
        m = t;
    });

    // ── pictures: the profile's, or the pick previewed in Edit Mode ──
    const bannerSrc = $derived(edit.active && edit.preview.banner ? edit.preview.banner : (m?.bannerSrc || ''));
    const avatarSrc = $derived(edit.active && edit.preview.avatar ? edit.preview.avatar : (m?.avatarSrc || ''));
    let bannerBroken = $state(false);
    let avatarBroken = $state(false);
    $effect(() => { bannerSrc; bannerBroken = false; });
    $effect(() => { avatarSrc; avatarBroken = false; });
    const bannerEmpty = $derived(!bannerSrc || bannerBroken);
    const avatarEmpty = $derived(!avatarSrc || avatarBroken);
    // A contact without a banner gets the short light strip; our own profile keeps the dark one.
    const bannerDark = $derived(!!m && (m.mine || m.hasBanner));

    // ── the container's mode classes and the navbar ──
    $effect(() => { root.classList.toggle('is-own-profile', !!m?.mine); });
    $effect(() => { root.classList.toggle('profile-edit-active', edit.active); });
    $effect(() => {
        // A contact's profile can land while another screen is open; only a shown screen owns the navbar.
        if (!m || root.style.display === 'none') return;
        h.showNavbar(m.mine);
    });

    // ── text that needs twemoji or shortcodes rendered after it lands ──
    function nameInto(node, [text, hasName]) {
        const render = ([t, hn]) => { node.textContent = t; if (hn) h.twemojify(node); };
        render([text, hasName]);
        return { update: render };
    }
    function statusInto(node, [text, tags]) {
        const render = ([t, tg]) => {
            node.textContent = t;
            if (tg) { h.twemojify(node); h.renderCustomEmojiShortcodes(node, tg); }
        };
        render([text, tags]);
        return { update: render };
    }
    function secondaryInto(node, [text, hasName, bot]) {
        const render = ([t, hn, b]) => {
            node.textContent = t;
            if (hn) h.twemojify(node);
            if (b) node.appendChild(h.botIcon());
        };
        render([text, hasName, bot]);
        return { update: render };
    }
    function aboutInto(node, about) {
        const render = (a) => {
            node.textContent = a || 'No description yet';
            h.twemojify(node);
            if (a) h.renderMentions(node);
        };
        render(about);
        return { update: render };
    }

    // ── badges: async lookups, each guarded against the profile changing under it ──
    let badges = $state.raw({ invites: 0, fawkes: false, bug: 0 });
    $effect(() => {
        const t = m;
        if (!t) return;
        const { id, mine } = t;
        badges = { invites: 0, fawkes: false, bug: 0 };
        const still = () => view.id === id;
        h.invitedCount(id).then((count) => { if (still() && count > 0) badges = { ...badges, invites: count }; }).catch(() => {});
        h.fawkesBadge(id, mine).then((has) => { if (still() && has) badges = { ...badges, fawkes: true }; }).catch(() => {});
        h.bugHunterTier(id, mine).then((tier) => { if (still() && tier > 0) badges = { ...badges, bug: tier }; }).catch(() => {});
    });

    // ── the More dropdown: a click anywhere else closes it, as does a profile change ──
    let moreOpen = $state(false);
    $effect(() => { m; moreOpen = false; });

    // ── copy feedback ──
    let copiedOwn = $state(false);
    let copiedOption = $state(false);
    let copiedNpub = $state(false);
    async function copyLink(which) {
        if (!await h.copyProfileLink()) return;
        if (which === 'own') { copiedOwn = true; setTimeout(() => { copiedOwn = false; }, 2000); }
        else { copiedOption = true; setTimeout(() => { copiedOption = false; }, 2000); }
    }
    async function copyNpub() {
        if (!await h.copyNpub()) return;
        copiedNpub = true;
        setTimeout(() => { copiedNpub = false; }, 2000);
    }

    // ── Edit Mode chrome: the bar fades in over the header and out again ──
    let barShown = $state(false);
    let barVisible = $state(false);
    let barTimer = null;
    $effect(() => {
        const on = edit.active;
        clearTimeout(barTimer);
        if (on) {
            barShown = true; barVisible = false;
            barTimer = setTimeout(() => { barVisible = true; }, 10);
        } else {
            barVisible = false;
            barTimer = setTimeout(() => { barShown = false; }, 250);
        }
    });
    // The banner's overlay yields to the avatar's while the pointer is over the avatar.
    let bannerContainer = $state(null);
    let avatarContainer = $state(null);
    let avatarHovered = $state(false);
    $effect(() => { if (!edit.active) avatarHovered = false; });
    function trackHover(e) {
        if (!edit.active || !bannerContainer || !avatarContainer) return;
        const bannerRect = bannerContainer.getBoundingClientRect();
        const avatarRect = avatarContainer.getBoundingClientRect();
        const inBanner = e.clientY <= bannerRect.top + 200;
        const inAvatar = e.clientX >= avatarRect.left && e.clientX <= avatarRect.right
            && e.clientY >= avatarRect.top && e.clientY <= avatarRect.bottom;
        avatarHovered = inAvatar || !inBanner;
    }
    const hidden = $derived(edit.active ? 'none' : '');
</script>

<svelte:document onclick={() => { moreOpen = false; }} />

{#if m}
<div class="profile-header">
    <button id="profile-switcher-trash-toggle" class="profile-switcher-trash-toggle btn" aria-label="Toggle delete mode"
            style:display={sc.switcherOpen ? '' : 'none'} onclick={(e) => { e.stopPropagation(); h.toggleSwitcherEdit(); }}>
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m3 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6h14ZM10 11v6M14 11v6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
    </button>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="profile-back-btn" class="btn nav-back-btn" style:display={m.mine || edit.active ? 'none' : ''} onclick={() => h.back()}>
        <span class="icon icon-chevron-double-left nav-icon"></span>
    </div>
    <div class="profile-header-info" style:display={hidden}>
        <div class="profile-header-name-row">
            <div id="profile-header-avatar-container" style:display={m.mine ? 'none' : ''}>{#if !m.mine}<Avatar src={m.avatarSrc} size={22} class="btn" />{/if}</div>
            <!-- svelte-ignore a11y_missing_content -->
            <h3 id="profile-name" class="cutoff" class:chat-contact={!m.statusText} class:chat-contact-with-status={!!m.statusText}
                style:display={m.mine ? 'none' : ''} use:nameInto={[m.mine ? '' : m.name, m.hasName]}></h3>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div id="my-profile-switcher" class="my-profile-switcher" class:open={sc.switcherOpen} style:display={m.mine ? '' : 'none'} onclick={() => h.toggleSwitcher()}>
                <h3>My Profile</h3>
                <svg class="my-profile-switcher-chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                    <path d="M6 9l6 6 6-6" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
            </div>
        </div>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span id="profile-status" class="cutoff chat-contact-status" class:btn={m.mine} style:display={m.mine ? 'none' : ''}
              use:statusInto={[m.statusText, m.statusTitle ? m.emojiTags : null]} onclick={() => { if (m.mine) h.setStatus(); }}></span>
    </div>
    <div id="profile-edit-bar" style="position: absolute; top: 0; left: 0; right: 0; bottom: 0; align-items: center; justify-content: space-between; padding: 0 20px; background-color: rgba(22,22,22,0.95);"
         style:display={barShown ? 'flex' : 'none'} style:opacity={barVisible ? '1' : '0'}>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div id="profile-edit-cancel-btn" onclick={() => h.exitEdit(true)}>
            <span class="icon icon-edit-x"></span>
            <span>Exit</span>
        </div>
        <span id="profile-edit-mode-label" style="font-size: 12px; opacity: 0.8; position: absolute; left: 50%; transform: translateX(-50%);">
            {profileEditDirty() ? 'Unsaved Changes Made' : 'Edit Mode is Enabled'}
        </span>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div id="profile-edit-save-btn" onclick={() => h.exitEdit(false)}>
            <span class="icon icon-save"></span>
            <span>Save</span>
        </div>
    </div>
</div>
<div class="profile-content">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div id="profile-banner-container" style="position: relative;" class:avatar-hovered={avatarHovered} bind:this={bannerContainer} onmousemove={trackHover}>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
        <img id="profile-banner" class="profile-banner" alt="" class:is-empty={bannerEmpty} class:btn={edit.active}
             src={bannerEmpty ? BLANK : bannerSrc} onerror={() => { bannerBroken = true; }}
             style:background-color={bannerDark ? 'rgb(27, 27, 27)' : ''} style:height={bannerDark ? '' : '115px'}
             onclick={() => { if (edit.active) h.pickPicture('banner'); }}>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div id="profile-edit-btn" class="profile-banner-edit profile-share-btn profile-option btn" style:display={m.mine && !edit.active ? 'flex' : 'none'} onclick={() => h.enterEdit()}>
            <span class="icon icon-edit navbar-icon"></span>
            <p class="navbar-text">Edit</p>
        </div>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div id="profile-share-btn" class="profile-share-btn profile-option btn" style:display={m.mine && !edit.active ? 'block' : 'none'} onclick={() => copyLink('own')}>
            <span class="icon navbar-icon" class:icon-share={!copiedOwn} class:icon-check={copiedOwn}></span>
            <p class="navbar-text">Share</p>
        </div>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div id="profile-qr-btn" class="profile-share-btn profile-qr-btn profile-option btn" role="button" tabindex="0" aria-label="Show profile QR code" style:display={edit.active ? 'none' : 'block'} onclick={() => h.showQr()}>
            <div id="profile-qr-icon" class="profile-qr-icon">
                <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
                    <path d="M6.5 6.5H6.51M17.5 6.5H17.51M6.5 17.5H6.51M13 13H13.01M17.5 17.5H17.51M17 21H21V17M14 16.5V21M21 14H16.5M15.6 10H19.4C19.9601 10 20.2401 10 20.454 9.89101C20.6422 9.79513 20.7951 9.64215 20.891 9.45399C21 9.24008 21 8.96005 21 8.4V4.6C21 4.03995 21 3.75992 20.891 3.54601C20.7951 3.35785 20.6422 3.20487 20.454 3.10899C20.2401 3 19.9601 3 19.4 3H15.6C15.0399 3 14.7599 3 14.546 3.10899C14.3578 3.20487 14.2049 3.35785 14.109 3.54601C14 3.75992 14 4.03995 14 4.6V8.4C14 8.96005 14 9.24008 14.109 9.45399C14.2049 9.64215 14.3578 9.79513 14.546 9.89101C14.7599 10 15.0399 10 15.6 10ZM4.6 10H8.4C8.96005 10 9.24008 10 9.45399 9.89101C9.64215 9.79513 9.79513 9.64215 9.89101 9.45399C10 9.24008 10 8.96005 10 8.4V4.6C10 4.03995 10 3.75992 9.89101 3.54601C9.79513 3.35785 9.64215 3.20487 9.45399 3.10899C9.24008 3 8.96005 3 8.4 3H4.6C4.03995 3 3.75992 3 3.54601 3.10899C3.35785 3.20487 3.20487 3.35785 3.10899 3.54601C3 3.75992 3 4.03995 3 4.6V8.4C3 8.96005 3 9.24008 3.10899 9.45399C3.20487 9.64215 3.35785 9.79513 3.54601 9.89101C3.75992 10 4.03995 10 4.6 10ZM4.6 21H8.4C8.96005 21 9.24008 21 9.45399 20.891C9.64215 20.7951 9.79513 20.6422 9.89101 20.454C10 20.2401 10 19.9601 10 19.4V15.6C10 15.0399 10 14.7599 9.89101 14.546C9.79513 14.3578 9.64215 14.2049 9.45399 14.109C9.24008 14 8.96005 14 8.4 14H4.6C4.03995 14 3.75992 14 3.54601 14.109C3.35785 14.2049 3.20487 14.3578 3.10899 14.546C3 14.7599 3 15.0399 3 15.6V19.4C3 19.9601 3 20.2401 3.10899 20.454C3.20487 20.6422 3.35785 20.7951 3.54601 20.891C3.75992 21 4.03995 21 4.6 21Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
                </svg>
            </div>
        </div>
        <div class="profile-avatar-container" bind:this={avatarContainer}>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
            <img id="profile-avatar" class="profile-avatar" alt="" class:is-placeholder={avatarEmpty} class:btn={edit.active}
                 src={avatarEmpty ? BLANK : avatarSrc} onerror={() => { avatarBroken = true; }}
                 onclick={() => { if (edit.active) h.pickPicture('avatar'); }}>
        </div>
    </div>
    <div>
        <!-- svelte-ignore a11y_missing_content -->
        <h3 id="profile-secondary-name" class="chat-contact-with-status" style:display={hidden} use:secondaryInto={[m.secondary, m.hasName, m.bot]}></h3>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span id="profile-secondary-status" class="cutoff chat-contact-status" class:btn={m.mine} style="width: 90%;" style:display={hidden}
              use:statusInto={[m.statusText, m.statusTitle ? m.emojiTags : null]} onclick={() => { if (m.mine) h.setStatus(); }}></span>
        <div id="profile-edit-fields" style="flex-direction: column; gap: 12px; width: 90%; margin: 16px auto 0;" style:display={edit.active ? 'flex' : 'none'}>
            <ProfileEditFields />
        </div>
        <div id="profile-badges" style="margin-top: 5px;" style:display={hidden}>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
            <img id="profile-badge-invites" src="./icons/vector_badge_hex_placeholder.svg" alt="Beta Inviter" class="btn" style="height: 30px; width: 30px;"
                 style:display={badges.invites > 0 ? '' : 'none'} onclick={() => h.showInviteBadge(badges.invites)}>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
            <img id="profile-badge-fawkes" src="./icons/fawkes_mask.svg" alt="Vector" class="btn" style="height: 30px; width: 30px; margin-left: 5px;"
                 style:display={badges.fawkes ? '' : 'none'} onclick={() => h.showFawkesCard()}>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
            <img id="profile-badge-bughunter" src={'./icons/bughunter_' + (badges.bug || 1) + '.svg'} alt="Bug Hunter" class="btn" style="height: 30px; width: 30px; margin-left: 5px;"
                 style:display={badges.bug > 0 ? '' : 'none'} onclick={() => h.showBugHunterCard(badges.bug)}>
        </div>
        <div id="profile-option-list" class="profile-options" style:display={m.mine ? 'none' : ''}>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div id="profile-option-message" class="profile-option" onclick={() => h.message()}>
                <span class="icon icon-message navbar-icon"></span>
                <p class="navbar-text">Message</p>
            </div>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div id="profile-option-mute" class="profile-option" onclick={() => h.toggleMute()}>
                <span class="icon navbar-icon" class:icon-volume-mute={m.muted} class:icon-volume-max={!m.muted}></span>
                <p class="navbar-text">{m.muted ? 'Unmute' : 'Mute'}</p>
            </div>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div id="profile-option-share" class="profile-option" onclick={() => copyLink('option')}>
                <span class="icon navbar-icon" class:icon-share={!copiedOption} class:icon-check={copiedOption}></span>
                <p class="navbar-text">Share</p>
            </div>
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div id="profile-option-more" class="profile-option btn" style="position: relative;" class:active={moreOpen} onclick={(e) => { e.stopPropagation(); moreOpen = !moreOpen; }}>
                <span class="icon icon-dots-horizontal navbar-icon"></span>
                <p class="navbar-text">More</p>
                <div id="profile-more-dropdown" class="profile-more-dropdown" style:display={moreOpen ? 'block' : 'none'}>
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                    <div id="profile-option-nickname" class="profile-more-item" onclick={() => { moreOpen = false; h.nickname(); }}>
                        <span>Nickname</span>
                        <span class="icon icon-edit" style="width: 18px; height: 18px; background-color: white;"></span>
                    </div>
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                    <div id="profile-option-block" class="profile-more-item is-danger" onclick={() => { moreOpen = false; h.block(); }}>
                        <span class="is-danger-text">{m.blocked ? 'Unblock' : 'Block'}</span>
                        <span class="icon icon-x-user is-danger-icon" style="width: 18px; height: 18px;"></span>
                    </div>
                </div>
            </div>
        </div>
        <span id="profile-description" class="chat-contact-status" class:group-placeholder={!m.about}
              style="width: 90%; white-space: pre-line; overflow-wrap: break-word; font-style: normal; margin-top: 10px; text-align: center; display: block;"
              style:display={edit.active ? 'none' : 'block'} use:aboutInto={m.about}></span>
        <div style="margin: 40px 0 0 0; width: 90%; padding-left: 5%;">
            <h3 id="profile-npub-label" style="color: #f7f4f4; margin: 0; text-align: center;" style:display={hidden}>{m.mine ? 'My nPub Key' : 'nPub Key'}</h3>
        </div>
        <div id="profile-npub-container" class="profile-npub-container" style="margin-top: 5px; padding-top: 0;" style:display={hidden}>
            <span id="profile-npub" class="profile-npub" data-full-npub={m.id}>{m.id.slice(0, 16) + '...' + m.id.slice(-16)}</span>
            <button id="profile-npub-copy" class="btn profile-npub-copy" title="Copy npub" onclick={copyNpub}>
                <span class="icon" class:icon-copy={!copiedNpub} class:icon-check={copiedNpub}></span>
            </button>
        </div>
    </div>
</div>
{/if}
