// Profiles: the own-profile tab, viewing a profile, and the edit flow.
// One global scope: loads after main.js and shares its globals.

/**
 * Renders the user's own profile UI in the chat list
 * @param {Profile} cProfile 
 */
function renderCurrentProfile(cProfile) {
    VectorSvelte.setAccount({
        name: getName(cProfile),
        hasName: !!(cProfile?.nickname || cProfile?.name),
        statusText: cProfile?.status?.title || 'Set a Status',
        emojiTags: cProfile?.status?.emoji_tags || [],
        avatarSrc: getProfileAvatarSrc(cProfile, true) || null,
    });
}

/**
 * Render the Profile tab based on a given profile
 * @param {Profile} cProfile 
 */
/** Show `cProfile` in the expanded profile view; the reconciler derives every field. */
function renderProfileTab(cProfile) {
    if (!cProfile?.id) return;
    VectorSvelte.setOpenProfile(cProfile.id);
    VectorSvelte.touchProfile(cProfile.id);
}

function registerProfileScreen() {
    const openId = () => VectorSvelte.profileViewState().id;
    const cur = () => getProfile(openId());
    // The shareable vectorapp.io URL; the caller paints its own copied tick.
    const copyProfileLink = async () => {
        const npub = openId();
        if (!npub) return false;
        try {
            await navigator.clipboard.writeText(`https://vectorapp.io/profile/${npub}`);
            showToast('Profile Link Copied');
            return true;
        } catch {
            showToast('Failed to Copy Profile Link');
            return false;
        }
    };
    VectorSvelte.setScreen('profile', {
        h: {
            getProfile,
            getName,
            getProfileAvatarSrc,
            getProfileBannerSrc,
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
            showNavbar: (on) => { VectorSvelte.showPane('navbar', on); },
            pickPicture: pickProfilePicture,
            // Own profile
            enterEdit: enterProfileEditMode,
            exitEdit: (cancel) => exitProfileEditMode(cancel),
            setStatus: askForStatus,
            toggleSwitcher: () => profileSwitcher.toggle(),
            toggleSwitcherEdit: () => profileSwitcher.toggleEditMode(),
            showQr: () => {
                const npub = openId();
                if (npub) openQrOverlay(`https://vectorapp.io/profile/${npub}`);
            },
            copyProfileLink,
            copyNpub: async () => {
                const npub = openId();
                if (!npub) return false;
                try {
                    await navigator.clipboard.writeText(npub);
                    showToast('Copied Profile Link');
                    return true;
                } catch {
                    showToast('Failed to Copy');
                    return false;
                }
            },
            // A contact
            back: () => {
                if (previousChatBeforeProfile) {
                    const chatToOpen = previousChatBeforeProfile;
                    previousChatBeforeProfile = '';
                    openChat(chatToOpen);
                } else {
                    openChat(openId());
                }
            },
            message: () => openChat(openId()),
            toggleMute: () => invoke('toggle_chat_mute', { chatId: openId() }),
            block: async () => {
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
            },
            nickname: async () => {
                const npub = openId();
                const nick = await popupConfirm('Choose a Nickname', '', false, 'Nickname');
                if (nick === false) return;
                if (nick.length >= 30) return popupConfirm('Woah woah!', 'A ' + nick.length + '-character nickname seems excessive!', true, '', 'vector_warning.svg');
                if (blockedBySync()) return;
                await invoke('set_nickname', { npub, nickname: nick });
            },
        },
    });
}
// Registered once every script is in: the bag calls helpers from files that load later.
document.addEventListener('DOMContentLoaded', registerProfileScreen, { once: true });

/**
 * Open the Expanded Profile view, optionally with a non-default profile
 * @param {Profile} cProfile - An optional profile to render
 */
async function openProfile(cProfile) {
    pushBack('profile', () => {
        VectorSvelte.showPane('profile', false);
        if (previousChatBeforeProfile) openChat(previousChatBeforeProfile);
        else openChatlist();
    });
    navbarSelect('profile-btn');
    VectorSvelte.showPane('navbar', true);
    VectorSvelte.showPane('chats', false);
    VectorSvelte.showPane('settings', false);
    VectorSvelte.showPane('invites', false);
    VectorSvelte.showPane('groupOverview', false);
    // "View Profile" from the member-list mini-profile closes Group Details for good — drop its
    // back entry so back-nav doesn't land on a dead re-hide step.
    popBack('group-overview');
    VectorSvelte.showPane('chat', false); // Hide the chat view when opening profile
    VectorSvelte.setShellFlag('settingsTab', true);

    // Scroll profile back to top
    setTimeout(() => {
        VectorSvelte.shellElements().profile?.scrollTo(0, 0);
        VectorSvelte.profileEls().content?.scrollTo(0, 0);
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
            if (VectorSvelte.paneShown('profile')) {
                invoke("refresh_profile_now", { npub: cProfile.id });
            } else {
                // Profile tab closed, stop refreshing
                clearInterval(profileRefreshInterval);
                profileRefreshInterval = null;
            }
        }, 30000);
    }

    renderProfileTab(cProfile);

    if (!VectorSvelte.paneShown('profile')) {
        VectorSvelte.revealPane('profile', 'fadein-subtle-anim');
        VectorSvelte.showPane('profile', true);
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
        avatar: getProfileAvatarSrc(cProfile, true) || null,
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
        if (VectorSvelte.paneShown('profile')) renderProfileTab(cProfile);
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
