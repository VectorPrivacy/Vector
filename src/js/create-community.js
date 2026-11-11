// New Chat and Create Community: one panel, a Svelte island (components/community/CreateCommunity);
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
        title: 'Choose Community Icon', multiple: false, directory: false,
        filters: [{ name: 'Image', extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp'] }],
    });
    if (!file) return;
    // On Android a content:// URI needs cache_android_file's preview file, not convertFileSrc.
    const cropped = await cropAvatarPick(file, await pickedImagePreviewSrc(file), 'Crop Community Icon', 'community');
    if (cropped) VectorSvelte.ccSetAvatar(file, cropped.preview, cropped.crop);
}

/**
 * CreateGroupHelpers: the Create Community screen.
 * @typedef {Object} CreateGroupHelpers
 * @property {(text: string) => string|null} extractNpub
 * @property {() => object[]} profiles
 * @property {(npub: string, picker: object) => void} pasteNpub
 * @property {() => void} pickAvatar
 * @property {() => void} close
 * @property {() => Promise<void>} create   Create Community from the panel's name and icon
 * @property {() => void} finish   leave Create Community for the new community
 * @property {(npubs: string[]) => Promise<void>} sendInvites   a Private community's direct invites
 * @property {() => void} copyLink   a Public community's first link
 * @property {() => void} showLinkQr
 * @property {(el: Element, text: string) => void} renderQr   the QR beside a Public community's link
 * @property {() => void} retryLink
 * @property {() => Promise<object>} pickerProps     ContactPicker's props for this open
 * @property {(text: string) => ('invite'|'go'|null)} linkKind   a pasted link the footer can open
 * @property {(text: string) => void} openLink
 * @property {(npub: string) => void} startDm
 * @property {() => void} scan
 * @property {(text: string) => string|null} inviteKey   the invite a pasted link names
 * @property {(key: string) => Promise<object>} resolveInvite   { state: 'ok'|'err', info, iconSrc }
 * @property {(communityId: string) => boolean} isMember
 * @property {(key: string, communityId: string) => Promise<void>} joinInvite
 * @property {() => void} openHub
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
            finish: ccFinish,
            sendInvites: ccSendInvites,
            copyLink: ccCopyLink,
            showLinkQr: () => { const url = VectorSvelte.ccState().link; if (url) openQrOverlay(url); },
            renderQr: (el, text) => renderQrInto(el, text),
            retryLink: ccMintLink,
            linkKind: (text) => {
                const kind = parseContactInput(text)?.kind;
                return kind === 'invite' || kind === 'go' ? kind : null;
            },
            openLink: (text) => {
                const parsed = parseContactInput(text);
                leaveCreateGroup();
                if (parsed?.kind === 'invite') previewAndJoinCommunityLink(parsed.url);
                else if (parsed?.kind === 'go') executeDeepLinkAction({ action_type: 'go', target: parsed.payload });
            },
            startDm: (npub) => { leaveCreateGroup(); openChat(npub); },
            scan: () => openQrScanner(),
            inviteKey: (text) => {
                const m = (text || '').match(/(naddr1[a-z0-9]{20,})[^#\s]*#([A-Za-z0-9_-]{20,})/i);
                return m ? `${m[1].toLowerCase()}#${m[2]}` : null;
            },
            resolveInvite: (key) => _resolveCommunityInvitePreview(key),
            isMember: (communityId) => !!findCommunityChat(communityId),
            joinInvite: ccJoinInvite,
            openHub: () => openUrl('https://vectorapp.io/hub'),
            pickerProps: async () => ({
                profiles: [...arrProfiles],
                getProfile: (npub) => getProfile(npub),
                myNpub: arrProfiles.find(p => p.mine)?.id,
                banned: [], members: [],
                dmNpubs: await fetchDmContacts(),
                chatTsById: new Map(arrChats.map(c => [c.id, getChatSortTimestamp(c)])),
                avatarSrc: (p) => (p ? getProfileAvatarSrc(p) : null) || null,
                getName: (x) => getName(x),
                streamHidden: (x) => streamHidden(x),
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

/**
 * Open the panel: New Chat (one person to DM) or Create Community.
 * @param {'chat'|'community'} [mode]
 */
function openCreateGroup(mode = 'chat') {
    pushBack('create-group', closeCreateGroup);
    VectorSvelte.ccOpen(mode);
    VectorSvelte.showPane('createGroup', true);
    VectorSvelte.showPane('chats', false);
    VectorSvelte.showPane('chat', false);
    VectorSvelte.showPane('navbar', false);
}

function openCreateCommunity() {
    openCreateGroup('community');
}

/** Join (or, as a member, open) the community a pasted invite names. The panel stays up,
 *  busy, through the join's few seconds, and steps aside once the community opens. */
async function ccJoinInvite(key, communityId) {
    const member = findCommunityChat(communityId);
    if (member) { leaveCreateGroup(); openChat(member.id); return; }
    VectorSvelte.ccSetBusy(true, 'Joining...');
    await _joinCommunityFromCard(key, communityId);
    if (!findCommunityChat(communityId)) { VectorSvelte.ccSetBusy(false); return; }
    // The join opened the community, which already took the screen.
    popBack('create-group');
    VectorSvelte.showPane('createGroup', false);
}

/** Put the panel away for whatever it opens next: the destination takes the screen. */
function leaveCreateGroup() {
    popBack('create-group');
    VectorSvelte.showPane('createGroup', false);
    VectorSvelte.showPane('navbar', true);
}

/** Close the panel and return to the chat list; once a community exists, go to it instead. */
async function closeCreateGroup() {
    if (VectorSvelte.ccState().created) return ccFinish();
    popBack('create-group');
    VectorSvelte.showPane('createGroup', false);
    VectorSvelte.showPane('navbar', true);
    await openChatlist();
    adjustSize();
}

/**
 * Progress for a call that reports none. `stages` are the steps it runs, in order, each given an
 * even share of `ms`, the usual time it takes: the ring eases toward 90% on that clock and the
 * label follows it, and the call's end fills the ring.
 */
async function ccStaged(stages, ms, work) {
    const t0 = performance.now();
    const seq = VectorSvelte.ccState().seq;   // a later open of the panel is not this one's to paint
    const tick = () => {
        if (VectorSvelte.ccState().seq !== seq) return;
        const p = 0.9 * (1 - Math.exp(-2.3 * (performance.now() - t0) / ms));
        VectorSvelte.ccPatch({ prog: { label: stages[Math.min(stages.length - 1, Math.floor(p / 0.9 * stages.length))], pct: p, stages } });
    };
    tick();
    const timer = setInterval(tick, 120);
    try {
        return await work();
    } finally {
        clearInterval(timer);
    }
}

/** Creating a community, by stage; the last is its finish. */
const CC_CREATE_STAGES = ['Creating your community', 'Putting it online', 'Making you the owner', 'Almost done', 'Community created'];

/** Settles once the new community's icon is up (or there is none): an invite carries the icon
 *  the community had when it was minted, so step 3 waits on this before handing any out. */
let ccIconUpload = Promise.resolve();

/**
 * Create a single-channel Community from steps 1 and 2, then move to step 3 while its icon
 * uploads. A Public community gets its first invite link at once.
 */
async function createCommunityFromPanel() {
    const st = VectorSvelte.ccState();
    const name = st.name.trim();
    if (!name || st.busy || st.created) return;
    const avatarPath = st.avatarPath;
    const avatarCrop = st.avatarCrop;
    VectorSvelte.ccSetBusy(true, 'Creating...');
    try {
        const created = await ccStaged(
            CC_CREATE_STAGES.slice(0, -1), 3500,
            () => invoke('create_community', { name, channelName: null, relays: null }),
        );
        VectorSvelte.ccPatch({ prog: { label: CC_CREATE_STAGES.at(-1), pct: 1, stages: CC_CREATE_STAGES } });
        const communityId = created.community_id;
        const channelId = created.channel_id;

        const chat = getOrCreateChat(channelId, 'Community');
        chat.metadata = chat.metadata || {};
        chat.metadata.custom_fields = chat.metadata.custom_fields || {};
        chat.metadata.custom_fields.name = name;
        chat.metadata.custom_fields.description = '';
        chat.metadata.custom_fields.community_id = communityId;
        chat.metadata.custom_fields.is_owner = 'true';
        // The stamps the backend persists: without channel_name the header names the
        // channel after the community, and without primary_channel it isn't the row.
        chat.metadata.custom_fields.channel_name = created.channel_name || 'general';
        chat.metadata.custom_fields.primary_channel = channelId;
        // Stamp the proven owner npub so the crown/Owner tag shows now, not after reload.
        if (created.owner_npub) chat.metadata.custom_fields.owner_npub = created.owner_npub;
        // Creation time sorts the empty community to the top right away; reloads re-source it.
        chat.metadata.custom_fields.created_at = String(VectorSvelte.now());
        if (avatarPath) {
            chat.metadata.custom_fields.icon = '1';
            // avatar_cached is a RAW path. Desktop: the picked file previews at once. Android: a
            // content:// URI isn't renderable, and a crop isn't the file, so either shows once
            // the upload below lands.
            if (platformFeatures.os !== 'android' && !avatarCrop) chat.metadata.avatar_cached = avatarPath;
        }
        listChanged();

        ccIconUpload = (async () => {
            if (!avatarPath) return;
            try {
                await invoke('set_community_image', { communityId, filepath: avatarPath, isBanner: false, crop: avatarCrop });
                const path = await invoke('cache_community_image', { communityId, isBanner: false });
                if (path) {
                    chat.metadata.avatar_cached = path;
                    communityChanged(communityId);
                    if (strOpenChat === channelId) setChatHeader(chat);
                }
            } catch (err) { console.error('Set community avatar failed:', err); showToast('Community created, but the avatar upload failed'); }
        })();

        // A beat on the full ring, so the finish registers before the step changes.
        await new Promise(r => setTimeout(r, 350));
        VectorSvelte.ccSetBusy(false);
        VectorSvelte.ccPatch({ created: { communityId, channelId }, step: 3, filter: '', selected: 0, prog: null });
        if (st.access === 'public') ccMintLink();
    } catch (e) {
        const friendly = typeof e === 'string' ? e : (e?.message || e || '').toString();
        VectorSvelte.ccPatch({ prog: null });
        popupConfirm('Community creation failed', friendly, true, '', 'vector_warning.svg');
        VectorSvelte.ccSetError(friendly);
    }
}

/** A Public community's first invite link: the link IS what makes it public. It keeps going if
 *  the panel is left, and lands in Invite People either way. */
async function ccMintLink() {
    const created = VectorSvelte.ccState().created;
    if (!created) return;
    const { communityId } = created;
    const stillHere = () => VectorSvelte.ccState().created?.communityId === communityId;
    const iconPending = !!VectorSvelte.ccState().avatarPath;
    VectorSvelte.ccPatch({ link: '', linkError: '' });
    try {
        const url = await ccStaged(
            [...(iconPending ? ['Uploading your icon'] : []), 'Making your link', 'Publishing your link'],
            iconPending ? 5000 : 2800,
            async () => {
                await ccIconUpload;
                return invoke('create_public_invite', { communityId, expiresInSecs: null, label: null });
            },
        );
        if (stillHere()) VectorSvelte.ccPatch({ link: url, prog: null });
        else showToast('Invite link ready in Invite People');
    } catch (e) {
        if (stillHere()) VectorSvelte.ccPatch({ linkError: String(e), prog: null });
    }
}

async function ccCopyLink() {
    const url = VectorSvelte.ccState().link;
    if (!url) return;
    try {
        await navigator.clipboard.writeText(url);
        VectorSvelte.ccPatch({ copied: true });
        setTimeout(() => VectorSvelte.ccPatch({ copied: false }), 1500);
    } catch (_) { showToast('Failed to Copy'); }
}

/** A Private community's direct invites, then into the community. Failures are counted, not fatal. */
async function ccSendInvites(npubs) {
    const created = VectorSvelte.ccState().created;
    if (!created || !npubs.length) return;
    VectorSvelte.ccSetBusy(true, 'Inviting...');
    const n = npubs.length;
    const stages = ['Getting ready', `Inviting ${n} of ${n}`, 'Invites sent', 'Invites failed'];
    VectorSvelte.ccPatch({ prog: { label: stages[0], pct: 0.05, stages } });
    await ccIconUpload;
    let failed = 0;
    for (let i = 0; i < npubs.length; i++) {
        VectorSvelte.ccPatch({ prog: { label: `Inviting ${i + 1} of ${n}`, pct: Math.max(0.05, i / n), stages } });
        try { await invoke('invite_to_community', { communityId: created.communityId, inviteeNpub: npubs[i] }); }
        catch (err) { failed++; console.error('Invite failed for', npubs[i], err); }
    }
    VectorSvelte.ccPatch({ prog: { label: failed === n ? 'Invites failed' : 'Invites sent', pct: 1, stages } });
    await new Promise(r => setTimeout(r, 350));
    VectorSvelte.ccSetBusy(false);
    if (failed) showToast(`${failed} invite${failed === 1 ? '' : 's'} failed to send`);
    ccFinish();
}

/** Into the new community: the panel steps aside for its channel. */
function ccFinish() {
    const created = VectorSvelte.ccState().created;
    leaveCreateGroup();
    VectorSvelte.ccPatch({ created: null });
    if (created) openChat(created.channelId);
}
