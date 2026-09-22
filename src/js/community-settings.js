// Community Settings: the modal's loading and publishing. The draft lives in
// lib/community-settings.svelte.js and nothing in it publishes until Save; bans are
// the exception, acting at once as every moderation verb does.

VectorSvelte.setScreen('communitySettings', {
    h: {
        close: () => closeCommunitySettings(),
        pickIcon: () => csPickIcon(),
        save: () => csSave(),
        reset: () => VectorSvelte.csReset(),
        unban: () => csUnbanSelected(),
        // Empty when they have no name of their own, so the row shows the key once.
        name: (npub) => { const p = getProfile(npub); return p && (p.nickname || p.name || p.display_name) ? getName(p) : ''; },
        avatarSrc: (npub) => { const p = getProfile(npub); return p ? getProfileAvatarSrc(p) || null : null; },
        profile: (npub) => getProfile(npub) || null,
        ui: { twemojify, showTooltip: showGlobalTooltip, hideTooltip: hideGlobalTooltip },
    },
});

/**
 * Whether the caller may change anything the modal holds. The entry point is shown
 * on this alone, so a member who can only read relays is not handed a page of
 * disabled fields.
 * @param {object|null} caps get_community_capabilities
 */
function communitySettingsWritable(caps) {
    return !!(caps?.manage_metadata || caps?.ban);
}

/** The chat row that stands for a community: its primary channel, or any of its channels. */
function csCommunityChats(communityId) {
    return arrChats.filter(c => communityIdOfChat(c) === communityId);
}

async function openCommunitySettings(communityId) {
    if (!communityId) return;
    VectorSvelte.csOpen(communityId);
    VectorSvelte.csOverlay.open({});
    pushBack('community-settings', closeCommunitySettings);
    try {
        const [summary, caps] = await Promise.all([
            invoke('get_community', { communityId }),
            invoke('get_community_capabilities', { communityId }),
        ]);
        if (VectorSvelte.csState().communityId !== communityId) return;
        const chat = csCommunityChats(communityId).find(isPrimaryChannelChat) || csCommunityChats(communityId)[0];
        const cached = chat?.metadata?.avatar_cached;
        VectorSvelte.csLoaded({
            name: summary.name || '',
            description: summary.description || '',
            iconSrc: cached ? convertFileSrc(cached) : null,
            relays: summary.relays || [],
            canEdit: !!caps.manage_metadata,
            canBan: !!caps.ban,
        });
        if (caps.ban) csLoadBans(communityId);
    } catch (e) {
        if (VectorSvelte.csState().communityId !== communityId) return;
        console.error('Failed to load community settings:', e);
        showToast('Could not load this community\'s settings');
        closeCommunitySettings(true);
    }
}

/**
 * Close, unless there are unsaved changes: then the save bar says so and the modal
 * stays. `force` is for a load that failed, where there is nothing to lose.
 */
function closeCommunitySettings(force = false) {
    if (VectorSvelte.csOverlay.closing()) return;
    const st = VectorSvelte.csState();
    if (!force && (st.saving || VectorSvelte.csDirty())) {
        VectorSvelte.csNudge();
        // The back stack pops its entry before calling this; a refused close keeps it.
        pushBack('community-settings', closeCommunitySettings);
        return;
    }
    popBack('community-settings');
    VectorSvelte.csOverlay.close();
    VectorSvelte.csOpen(null);
}

/** Stage a new icon. It shows at once and uploads with the rest on Save. */
async function csPickIcon() {
    const st = VectorSvelte.csState();
    if (!st.canEdit || st.saving) return;
    const { open } = window.__TAURI__.dialog;
    const selected = await open({ multiple: false, filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp'] }] });
    const filePath = typeof selected === 'string' ? selected : selected?.path;
    if (!filePath) return;
    // A picked file can sit outside the asset scope; the backend hands back a copy inside it.
    const preview = await invoke('read_image_preview', { path: filePath }).then(convertFileSrc, () => null);
    if (!preview) {
        showToast('That image could not be read');
        return;
    }
    VectorSvelte.csSetDraft({ iconPath: filePath, iconPreview: preview });
}

/**
 * Publish the draft: the text in one metadata edit, then the icon. Each half commits
 * on its own success, so a failed upload leaves only the icon waiting on the bar.
 */
async function csSave() {
    const st = VectorSvelte.csState();
    const communityId = st.communityId;
    if (!communityId || st.saving || !st.canEdit) return;
    const name = st.draft.name.trim();
    const description = st.draft.description.trim();
    if (!name) return;
    const renamed = name !== st.saved.name;
    const described = description !== st.saved.description;
    const iconPath = st.draft.iconPath;

    VectorSvelte.csSetSaving(true);
    let unlisten = null;
    try {
        if (renamed || described) {
            await invoke('update_community_metadata', {
                communityId,
                name: renamed ? name : null,
                description: described ? description : null,
            });
            for (const chat of csCommunityChats(communityId)) {
                chat.metadata.custom_fields.name = name;
                chat.metadata.custom_fields.description = description;
            }
            VectorSvelte.csCommitted({ name, description });
            // A trimmed value committed while the field still shows its spaces would read as unsaved.
            VectorSvelte.csSetDraft({ name, description });
        }
        if (iconPath) {
            unlisten = await window.__TAURI__.event.listen('community_image_upload_progress', (e) => {
                if (e.payload?.community_id === communityId && !e.payload?.is_banner) {
                    VectorSvelte.csSetSaving(true, e.payload.progress || 0);
                }
            });
            await invoke('set_community_image', { communityId, filepath: iconPath, isBanner: false });
            const cached = await invoke('cache_community_image', { communityId, isBanner: false }).catch(() => null);
            for (const chat of csCommunityChats(communityId)) {
                chat.metadata.custom_fields.icon = '1';
                if (cached) chat.metadata.avatar_cached = cached;
            }
            VectorSvelte.csCommitted({ iconSrc: cached ? convertFileSrc(cached) : VectorSvelte.csState().draft.iconPreview });
        }
        csRepaint(communityId);
    } catch (e) {
        console.error('Failed to save community settings:', e);
        showToast(String(e || 'Failed to save changes'));
        csRepaint(communityId);
    } finally {
        if (unlisten) unlisten();
        VectorSvelte.csSetSaving(false);
    }
}

/** The banlist, and the profiles behind it so rows read as people rather than keys. */
async function csLoadBans(communityId) {
    let bans = [];
    try { bans = await invoke('get_community_banlist', { communityId }) || []; } catch (_) { return; }
    if (VectorSvelte.csState().communityId !== communityId) return;
    VectorSvelte.csSetBans(bans);
    const unknown = bans.filter(np => !getProfile(np) && !strangerProfileRequested.has(np));
    for (const np of unknown) {
        strangerProfileRequested.add(np);
        invoke('load_profile', { npub: np }).catch(() => {});
    }
}

/**
 * Lift every selected ban as ONE banlist edition. Immediate once asked, like every
 * moderation verb: the list is the published state, not a draft, and batching is
 * the point, since each separate unban would publish, and race, its own edition.
 */
async function csUnbanSelected() {
    const st = VectorSvelte.csState();
    const communityId = st.communityId;
    const npubs = [...st.banSel];
    if (!communityId || !st.canBan || st.unbanning || !npubs.length) return;
    VectorSvelte.csSetUnbanning(true);
    try {
        await invoke('unban_community_members', { communityId, npubs });
        if (VectorSvelte.csState().communityId === communityId) VectorSvelte.csRemoveBans(npubs);
        dmsgClearDeleteMetaCache();
        refreshCommunityMemberCount(communityId, true);
        showToast(npubs.length === 1 ? 'Member unbanned' : `${npubs.length} members unbanned`);
    } catch (e) {
        showToast(String(e));
    } finally {
        VectorSvelte.csSetUnbanning(false);
    }
}

/** Every surface showing this community's name or icon. */
function csRepaint(communityId) {
    communityChanged(communityId);
    listChanged();
    const open = arrChats.find(c => c.id === strOpenChat);
    if (open && communityIdOfChat(open) === communityId) setChatHeader(open);
    const { groupId } = VectorSvelte.overviewState();
    if (groupId === communityId && open) renderCommunityOverview(open, true);
}
