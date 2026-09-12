// Create Community: the panel is a Svelte island (components/community/CreateCommunity);
// this side navigates to and from it and runs the create pipeline. One global scope.

/**
 * The contact pickers list the people you've sent at least one DM to, not every profile
 * ever seen (a profile store holds thousands of community passers-by). The frontend only
 * carries each chat's last message, so the DB answers this.
 */
async function fetchDmContacts() {
    try {
        return await invoke('get_dm_contacts');
    } catch (_) {
        return arrChats.filter(c => c.chat_type === 'DirectMessage').map(c => c.id);
    }
}

/** A stranger npub rides the stranger path; a known profile is simply picked. */
function ccPasteNpub(np, picker) {
    const myNpub = arrProfiles.find(p => p.mine)?.id;
    if (np === myNpub) return;
    if (arrProfiles.some(p => p.id === np)) { picker.select(np); return; }
    picker.addStranger(np);
    if (!strangerProfileRequested.has(np)) {
        strangerProfileRequested.add(np);
        invoke('load_profile', { npub: np }).then(() => picker.setProfiles([...arrProfiles])).catch(() => {});
    }
}

async function ccPickAvatar() {
    const { open } = window.__TAURI__.dialog;
    const file = await open({
        title: 'Choose Group Avatar', multiple: false, directory: false,
        filters: [{ name: 'Image', extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp'] }],
    });
    if (!file) return;
    // On Android a content:// URI needs cache_android_file's preview file, not convertFileSrc.
    VectorSvelte.ccSetAvatar(file, await pickedImagePreviewSrc(file));
}

/**
 * CreateGroupHelpers: the Create Community screen.
 * @typedef {Object} CreateGroupHelpers
 * @property {(text: string) => string|null} extractNpub
 * @property {() => object[]} profiles
 * @property {(npub: string, picker: object) => void} pasteNpub
 * @property {() => void} pickAvatar
 * @property {() => void} close
 * @property {(selection: string[]) => Promise<void>} create
 * @property {() => Promise<object>} pickerProps     ContactPicker's props for this open
 */
function registerCreateGroupScreen() {
    VectorSvelte.setScreen('createGroup', {
        h: {
            extractNpub,
            profiles: () => [...arrProfiles],
            pasteNpub: ccPasteNpub,
            pickAvatar: ccPickAvatar,
            close: closeCreateGroup,
            create: createCommunityFromPanel,
            pickerProps: async () => ({
                profiles: [...arrProfiles],
                getProfile: (npub) => getProfile(npub),
                myNpub: arrProfiles.find(p => p.mine)?.id,
                banned: [], members: [],
                dmNpubs: await fetchDmContacts(),
                chatTsById: new Map(arrChats.map(c => [c.id, getChatSortTimestamp(c)])),
                avatarSrc: (p) => (p ? getProfileAvatarSrc(p) : null) || null,
                twemojify: (el) => twemojify(el),
                showTooltip: (text, el) => showGlobalTooltip(text, el),
                hideTooltip: () => hideGlobalTooltip(),
                hoverBg: '',   // #create-group-list .member-pick-hover already paints this
            }),
        },
    });
}
// Registered once every script is in: the bag calls helpers from files that load later.
document.addEventListener('DOMContentLoaded', registerCreateGroupScreen, { once: true });

/** Open the Create Community panel. */
function openCreateGroup() {
    // Mutually exclusive with Start New Chat — see openNewChat for why the pane and
    // its back entry both have to go.
    popBack('new-chat');
    VectorSvelte.showPane('chatNew', false);

    pushBack('create-group', closeCreateGroup);
    VectorSvelte.ccOpen();
    VectorSvelte.showPane('createGroup', true);
    VectorSvelte.showPane('chats', false);
    VectorSvelte.showPane('chat', false);
    VectorSvelte.showPane('navbar', false);
}

/** Close the panel and return to the chat list. */
async function closeCreateGroup() {
    popBack('create-group');
    VectorSvelte.showPane('createGroup', false);
    VectorSvelte.showPane('navbar', true);
    await openChatlist();
    adjustSize();
}

/**
 * Create a single-channel Community, then apply the optional icon and send the picked
 * direct invites, and land in the new channel. The channel appears before the icon
 * upload, which can take seconds.
 */
async function createCommunityFromPanel(inviteeNpubs) {
    const st = VectorSvelte.ccState();
    const name = st.name.trim();
    if (!name || st.busy) return;
    const avatarPath = st.avatarPath;
    VectorSvelte.ccSetBusy(true, 'Creating...');
    try {
        const created = await invoke('create_community', { name, channelName: null, relays: null });
        const communityId = created.community_id;
        const channelId = created.channel_id;

        const chat = getOrCreateChat(channelId, 'Community');
        chat.metadata = chat.metadata || {};
        chat.metadata.custom_fields = chat.metadata.custom_fields || {};
        chat.metadata.custom_fields.name = name;
        chat.metadata.custom_fields.description = '';
        chat.metadata.custom_fields.community_id = communityId;
        chat.metadata.custom_fields.is_owner = 'true';
        // Stamp the proven owner npub so the crown/Owner tag shows now, not after reload.
        if (created.owner_npub) chat.metadata.custom_fields.owner_npub = created.owner_npub;
        // Creation time sorts the empty community to the top right away; reloads re-source it.
        chat.metadata.custom_fields.created_at = String(Date.now());
        if (avatarPath) {
            chat.metadata.custom_fields.icon = '1';
            // avatar_cached is a RAW path. Desktop: the picked file previews at once. Android: a
            // content:// URI isn't renderable, so the header refreshes after the upload below.
            if (platformFeatures.os !== 'android') chat.metadata.avatar_cached = avatarPath;
        }
        listChanged();

        openChat(channelId);
        VectorSvelte.showPane('createGroup', false);
        VectorSvelte.ccSetBusy(false);

        // Detached: the community is already on screen; the icon upload must not delay it.
        (async () => {
            // Icon BEFORE invites: the private invite bundle snapshots community.icon.
            if (avatarPath) {
                try {
                    await invoke('set_community_image', { communityId, filepath: avatarPath, isBanner: false });
                    const path = await invoke('cache_community_image', { communityId, isBanner: false });
                    if (path) {
                        chat.metadata.avatar_cached = path;
                        communityChanged(communityId);
                        if (strOpenChat === channelId) setChatHeader(chat);
                    }
                } catch (err) { console.error('Set community avatar failed:', err); showToast('Community created, but the avatar upload failed'); }
            }
            if (inviteeNpubs.length) {
                let ok = 0;
                for (const np of inviteeNpubs) {
                    try { await invoke('invite_to_community', { communityId, inviteeNpub: np }); ok++; }
                    catch (err) { console.error('Invite failed for', np, err); }
                }
                // Success is silent (the invitees just appear); only failures surface.
                const failed = inviteeNpubs.length - ok;
                if (failed) showToast(`${failed} invite${failed === 1 ? '' : 's'} failed to send`);
            }
        })();
    } catch (e) {
        const friendly = typeof e === 'string' ? e : (e?.message || e || '').toString();
        popupConfirm('Community creation failed', friendly, true, '', 'vector_warning.svg');
        VectorSvelte.ccSetError(friendly);
    }
}
