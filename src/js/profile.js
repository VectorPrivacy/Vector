// Profiles: the own-profile tab, viewing a profile, and the edit flow.
// One global scope: loads after main.js and shares its globals.

/**
 * Renders the user's own profile UI in the chat list
 * @param {Profile} cProfile 
 */
function renderCurrentProfile(cProfile) {
    /* Chatlist Tab */

    // Clear and render avatar
    domAccountAvatarContainer.innerHTML = '';
    const accountAvatarSrc = getProfileAvatarSrc(cProfile);
    const domAvatar = createAvatarImg(accountAvatarSrc, 22, false);
    domAvatar.classList.add('btn');
    domAvatar.onclick = () => openProfile();
    domAccountAvatarContainer.appendChild(domAvatar);

    // Render our Display Name
    domAccountName.textContent = getName(cProfile);
    domAccountName.onclick = () => openProfile();
    if (cProfile?.nickname || cProfile?.name) twemojify(domAccountName);

    // Render our status
    domAccountStatus.textContent = cProfile?.status?.title || 'Set a Status';
    domAccountStatus.onclick = askForStatus;
    twemojify(domAccountStatus);
    renderCustomEmojiShortcodes(domAccountStatus, cProfile?.status?.emoji_tags || []);

}

/**
 * Render the Profile tab based on a given profile
 * @param {Profile} cProfile 
 */
let fProfileViewMounted = false;

/** Show `cProfile` in the expanded profile view; the reconciler derives every field. */
function renderProfileTab(cProfile) {
    if (!cProfile?.id) return;
    if (!fProfileViewMounted) {
        fProfileViewMounted = true;
        mountProfileView();
    }
    VectorSvelte.setOpenProfile(cProfile.id);
    VectorSvelte.touchProfile(cProfile.id);
}

function mountProfileView() {
    const cur = () => getProfile(VectorSvelte.profileViewState().id);
    const copyProfileLink = (iconHost) => {
        const npub = VectorSvelte.profileViewState().id;
        if (!npub) return;
        navigator.clipboard.writeText(`https://vectorapp.io/profile/${npub}`).then(() => {
            showToast('Profile Link Copied');
            const icon = iconHost.querySelector('span');
            icon.classList.replace('icon-share', 'icon-check');
            setTimeout(() => icon.classList.replace('icon-check', 'icon-share'), 2000);
        }).catch(() => showToast('Failed to Copy Profile Link'));
    };
    VectorSvelte.mountProfileView({
        els: {
            root: domProfile,
            navbar: domNavbar,
            headerAvatar: domProfileHeaderAvatarContainer,
            switcher: document.getElementById('my-profile-switcher'),
            name: domProfileName,
            status: domProfileStatus,
            banner: domProfileBanner,
            avatar: domProfileAvatar,
            secondaryName: domProfileNameSecondary,
            secondaryStatus: domProfileStatusSecondary,
            description: domProfileDescription,
            npub: document.getElementById('profile-npub'),
            npubLabel: document.getElementById('profile-npub-label'),
            id: domProfileId,
            options: domProfileOptions,
            optionMute: domProfileOptionMute,
            optionBlock: domProfileOptionBlock,
            moreDropdown: domProfileMoreDropdown,
            editBtn: domProfileEditBtn,
            shareBtn: document.getElementById('profile-share-btn'),
            qrBtn: document.getElementById('profile-qr-btn'),
            backBtn: domProfileBackBtn,
            badgeInvite: domProfileBadgeInvite,
            badgeFawkes: domProfileBadgeFawkes,
            badgeBugHunter: domProfileBadgeBugHunter,
            editBar: domProfileEditBar,
            editLabel: document.getElementById('profile-edit-mode-label'),
            editFields: document.getElementById('profile-edit-fields'),
            headerInfo: document.querySelector('.profile-header-info'),
            npubContainer: document.getElementById('profile-npub-container'),
            badges: document.getElementById('profile-badges'),
            bannerContainer: document.getElementById('profile-banner-container'),
            avatarContainer: document.querySelector('.profile-avatar-container'),
        },
        h: {
            getProfile,
            getName,
            getProfileAvatarSrc,
            getProfileBannerSrc,
            createAvatarImg,
            twemojify,
            renderCustomEmojiShortcodes,
            renderMentions: (el) => renderMentions(el, false, { allowBare: true, queueSync: true }),
            isMuted: (id) => !!arrChats.find(c => c.id === id)?.muted,
            botIcon: () => {
                const botIcon = document.createElement('span');
                botIcon.className = 'icon icon-bot profile-name-bot-icon';
                botIcon.addEventListener('mouseenter', () => showGlobalTooltip('Bot', botIcon));
                botIcon.addEventListener('mouseleave', hideGlobalTooltip);
                return botIcon;
            },
            invitedCount: (npub) => invoke('get_invited_users', { npub }),
            fawkesBadge: resolveFawkesBadge,
            bugHunterTier: resolveBugHunterTier,
            showInviteBadge: (count) => showBadgeCard({ title: 'Vector Beta Inviter', html: `Acquired by inviting <b>${count} ${count === 1 ? 'user' : 'users'}</b> to the Vector Beta!`, svg: 'vector_badge_placeholder.svg' }),
            showFawkesCard,
            showBugHunterCard,
            pickPicture: pickProfilePicture,
        },
    });

    // Controls bind once and read the open profile at click time.
    document.getElementById('profile-qr-btn').onclick = () => {
        const npub = VectorSvelte.profileViewState().id;
        if (npub) openQrOverlay(`https://vectorapp.io/profile/${npub}`);
    };
    document.getElementById('profile-npub-copy').onclick = (e) => {
        const npub = VectorSvelte.profileViewState().id;
        if (!npub) return;
        navigator.clipboard.writeText(npub).then(() => {
            showToast('Copied Profile Link');
        }).catch(() => {
            showToast('Failed to Copy');
            const copyBtn = e.target.closest('#profile-npub-copy');
            if (copyBtn) {
                copyBtn.innerHTML = '<span class="icon icon-check"></span>';
                setTimeout(() => { copyBtn.innerHTML = '<span class="icon icon-copy"></span>'; }, 2000);
            }
        });
    };
    // Own profile
    domProfileEditBtn.onclick = enterProfileEditMode;
    domProfileEditCancelBtn.onclick = () => exitProfileEditMode(true);
    domProfileEditSaveBtn.onclick = () => exitProfileEditMode(false);
    const ownShareBtn = document.getElementById('profile-share-btn');
    ownShareBtn.onclick = () => copyProfileLink(ownShareBtn);
    domProfileStatus.onclick = () => { if (cur()?.mine) askForStatus(); };
    domProfileStatusSecondary.onclick = () => { if (cur()?.mine) askForStatus(); };
    // A contact
    domProfileBackBtn.onclick = () => {
        if (previousChatBeforeProfile) {
            const chatToOpen = previousChatBeforeProfile;
            previousChatBeforeProfile = '';
            openChat(chatToOpen);
        } else {
            openChat(VectorSvelte.profileViewState().id);
        }
    };
    domProfileOptionMessage.onclick = () => openChat(VectorSvelte.profileViewState().id);
    domProfileOptionMute.onclick = () => invoke('toggle_chat_mute', { chatId: VectorSvelte.profileViewState().id });
    domProfileOptionShare.onclick = () => copyProfileLink(domProfileOptionShare);
    domProfileOptionBlock.onclick = async () => {
        domProfileMoreDropdown.style.display = 'none';
        const p = cur();
        if (!p) return;
        if (p.is_blocked) {
            await invoke('unblock_user', { npub: p.id });
            VectorSvelte.reloadBlockedUsers();
            showToast('User Unblocked');
            profileChanged(p.id);
        } else {
            const confirmed = await popupConfirm('Block User', 'Are you sure you want to block this user? You will no longer receive DMs from them.', false, '', 'vector_warning.svg');
            if (!confirmed) return;
            await invoke('block_user', { npub: p.id });
            VectorSvelte.reloadBlockedUsers();
            showToast('User Blocked');
            profileChanged(p.id);
        }
    };
    domProfileOptionNickname.onclick = async () => {
        domProfileMoreDropdown.style.display = 'none';
        const npub = VectorSvelte.profileViewState().id;
        const nick = await popupConfirm('Choose a Nickname', '', false, 'Nickname');
        if (nick === false) return;
        if (nick.length >= 30) return popupConfirm('Woah woah!', 'A ' + nick.length + '-character nickname seems excessive!', true, '', 'vector_warning.svg');
        if (blockedBySync()) return;
        await invoke('set_nickname', { npub, nickname: nick });
    };
    domProfileOptionMore.onclick = (e) => {
        e.stopPropagation();
        const isOpen = domProfileMoreDropdown.style.display !== 'none';
        domProfileMoreDropdown.style.display = isOpen ? 'none' : 'block';
        domProfileOptionMore.classList.toggle('active', !isOpen);
    };
}

/**
 * Open the Expanded Profile view, optionally with a non-default profile
 * @param {Profile} cProfile - An optional profile to render
 */
async function openProfile(cProfile) {
    pushBack('profile', () => {
        domProfile.style.display = 'none';
        if (previousChatBeforeProfile) openChat(previousChatBeforeProfile);
        else openChatlist();
    });
    navbarSelect('profile-btn');
    domNavbar.style.display = '';
    domChats.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';
    domGroupOverview.style.display = 'none';
    // "View Profile" from the member-list mini-profile closes Group Details for good — drop its
    // back entry so back-nav doesn't land on a dead re-hide step.
    popBack('group-overview');
    domChat.style.display = 'none'; // Hide the chat view when opening profile
    domSettingsBtn.style.display = ''; // Ensure settings button is visible (may have been hidden by openChat)

    // Scroll profile back to top
    setTimeout(() => {
        document.getElementById('profile')?.scrollTo(0, 0);
        document.querySelector('.profile-content')?.scrollTo(0, 0);
    }, 50);

    // Render our own profile by default, but otherwise; the given one
    if (!cProfile) {
        cProfile = arrProfiles.find(a => a.mine);
        // Clear previous chat when opening our own profile from navbar
        previousChatBeforeProfile = '';
    }

    // Force immediate refresh when user views profile
    if (cProfile && cProfile.id) {
        invoke("refresh_profile_now", { npub: cProfile.id });

        // Start periodic refresh while viewing this profile (every 30 seconds)
        clearInterval(profileRefreshInterval);
        profileRefreshInterval = setInterval(() => {
            // Only refresh if profile tab is still open
            if (domProfile.style.display === '') {
                invoke("refresh_profile_now", { npub: cProfile.id });
            } else {
                // Profile tab closed, stop refreshing
                clearInterval(profileRefreshInterval);
                profileRefreshInterval = null;
            }
        }, 30000);
    }

    renderProfileTab(cProfile);

    if (domProfile.style.display !== '') {
        // Run a subtle fade-in animation
        domProfile.classList.add('fadein-subtle-anim');
        domProfile.addEventListener('animationend', () => domProfile.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domProfile.style.display = '';
    }
}

/**
 * Edit the profile description inline
 */
function enterProfileEditMode() {
    const cProfile = arrProfiles.find(a => a.mine);
    if (!cProfile) return;
    fProfileEditMode = true;
    VectorSvelte.setProfileEditing(true);
    VectorSvelte.startProfileEdit({
        name: cProfile.name || '',
        about: typeof cProfile.about === 'string' ? cProfile.about : '',
        avatar: getProfileAvatarSrc(cProfile) || null,
        banner: getProfileBannerSrc(cProfile) || null,
    });
}

/** Pick a new avatar or banner while editing; it previews in place until save. */
async function pickProfilePicture(kind) {
    if (!fProfileEditMode) return;
    const { open } = window.__TAURI__.dialog;
    const file = await open({
        title: kind === 'avatar' ? 'Choose Profile Picture' : 'Choose Banner Image',
        multiple: false,
        directory: false,
        filters: [{ name: 'Image', extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp'] }]
    });
    if (!file || !fProfileEditMode) return;
    VectorSvelte.setProfileEditPicture(kind, file, await pickedImagePreviewSrc(file) || '');
}

/** Upload a picked picture and publish it; the profile keeps showing the pick meanwhile. */
function saveProfilePicture(cProfile, kind, path) {
    const cachedKey = kind === 'avatar' ? 'avatar_cached' : 'banner_cached';
    const label = kind === 'avatar' ? 'Avatar' : 'Banner';
    const prev = cProfile[cachedKey];
    // The backend's upload updates the cached path authoritatively once it lands.
    cProfile[cachedKey] = path;
    const revert = () => {
        cProfile[cachedKey] = prev;
        if (domProfile.style.display !== 'none') renderProfileTab(cProfile);
    };
    invoke('upload_avatar', { filepath: path, uploadType: kind })
        .then(url => {
            if (!url) return revert();
            invoke('update_profile', { name: '', avatar: kind === 'avatar' ? url : '', banner: kind === 'banner' ? url : '', about: '' })
                .then(ok => {
                    if (!ok) popupConfirm(`${label} Update Failed!`, `Failed to broadcast ${kind} update to the network.`, true, '', 'vector_warning.svg');
                })
                .catch(e => popupConfirm(`${label} Update Failed!`, escapeHtml(String(e)), true, '', 'vector_warning.svg'));
        })
        .catch(e => {
            revert();
            popupConfirm(`${label} Upload Failed!`, escapeHtml(String(e)), true, '', 'vector_warning.svg');
        });
}

function exitProfileEditMode(fCancel = false) {
    const edit = VectorSvelte.profileEdit();
    const draft = { name: edit.draft.name.trim(), about: edit.draft.about.trim() };
    const snapshot = edit.snapshot;
    const pending = { ...edit.pending };
    fProfileEditMode = false;
    VectorSvelte.endProfileEdit();
    VectorSvelte.setProfileEditing(false);

    const cProfile = arrProfiles.find(a => a.mine);
    if (cProfile) {
        if (!fCancel) {
            cProfile.name = draft.name;
            cProfile.about = draft.about;

            const nameChanged = draft.name !== (snapshot.name || '');
            const aboutChanged = draft.about !== (snapshot.about || '');
            if (nameChanged || aboutChanged) {
                invoke('update_profile', {
                    name: nameChanged ? draft.name : '',
                    avatar: '',
                    banner: '',
                    about: aboutChanged ? (draft.about.length > 0 ? draft.about : ' ') : '',
                }).then(ok => {
                    if (!ok) popupConfirm('Profile Update Failed!', 'Failed to broadcast profile update to the network.', true, '', 'vector_warning.svg');
                }).catch(e => popupConfirm('Profile Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg'));
            }
            if (pending.avatar) saveProfilePicture(cProfile, 'avatar', pending.avatar);
            if (pending.banner) saveProfilePicture(cProfile, 'banner', pending.banner);
            showToast('Profile Saved');
        }
        renderProfileTab(cProfile);
    }
}

function editProfileDescription() {
    // Get the current profile
    const cProfile = arrProfiles.find(a => a.mine);
    if (!cProfile) return;

    // Set the textarea content to current description
    domProfileDescriptionEditor.value = cProfile.about || '';

    // Hide the span and show the textarea
    domProfileDescription.style.display = 'none';
    domProfileDescriptionEditor.style.display = '';

    // Focus the text
    domProfileDescriptionEditor.focus();

    // Handle blur event to save and return to view mode
    domProfileDescriptionEditor.onblur = () => {
        // Hide textarea and show span
        domProfileDescriptionEditor.style.display = 'none';
        domProfileDescription.style.display = '';

        // Remove the blur event listener
        domProfileDescriptionEditor.onblur = null;

        // If nothing was edited, don't change anything
        if (domProfileDescriptionEditor.value === cProfile.about) return;

        // Update the profile's about property
        cProfile.about = domProfileDescriptionEditor.value;

        // Update the span content
        domProfileDescription.textContent = cProfile.about;
        twemojify(domProfileDescription);

        // Upload new About Me to Nostr
        invoke('update_profile', {
            name: '',
            avatar: '',
            banner: '',
            about: cProfile.about,
        }).then(ok => {
            if (!ok) popupConfirm('Bio Update Failed!', 'Failed to broadcast bio update to the network.', true, '', 'vector_warning.svg');
        }).catch(e => popupConfirm('Bio Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg'));
    };

    // Resize it to match the content size (CSS cannot scale textareas based on content)
    domProfileDescriptionEditor.style.height = Math.min(domProfileDescriptionEditor.scrollHeight, 100) + 'px';

    // Handle input events to resize the textarea dynamically
    domProfileDescriptionEditor.oninput = () => {
        domProfileDescriptionEditor.style.height = Math.min(domProfileDescriptionEditor.scrollHeight, 100) + 'px';
    };

    // Handle Enter key to submit (excluding Shift+Enter for line breaks)
    domProfileDescriptionEditor.onkeydown = (evt) => {
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            domProfileDescriptionEditor.blur(); // Trigger the blur event to save
        }
    };
}
