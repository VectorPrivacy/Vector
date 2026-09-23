// Community Settings: the modal's loading and publishing. The drafts live in
// lib/community-settings.svelte.js and nothing in them publishes until Save; bans and
// role membership are the exception, acting at once as every membership verb does.

VectorSvelte.setScreen('communitySettings', {
    h: {
        close: () => closeCommunitySettings(),
        pickIcon: () => csPickIcon(),
        save: () => csSave(),
        reset: () => VectorSvelte.csReset(),
        unban: () => csUnbanSelected(),
        setRoleHolders: (roleId, add, remove) => csSetRoleHolders(roleId, add, remove),
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
    return !!(caps?.manage_metadata || caps?.ban || caps?.manage_roles);
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
            canRoles: !!caps.manage_roles,
        });
        if (caps.ban) csLoadBans(communityId);
        if (caps.manage_roles) csLoadRoles(communityId);
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
 * Publish every unsaved draft: the Overview's, then the Roles'. Each half commits on
 * its own success, so a failure leaves only what failed waiting on the bar.
 */
async function csSave() {
    const st = VectorSvelte.csState();
    const communityId = st.communityId;
    if (!communityId || st.saving) return;
    VectorSvelte.csSetSaving(true);
    let unlisten = null;
    try {
        if (VectorSvelte.csOverviewDirty() && st.canEdit) {
            unlisten = await csSaveOverview(communityId);
        }
        if (VectorSvelte.csRoleOrderDirty() || VectorSvelte.csRoleEditDirty()) {
            await csSaveRoles(communityId);
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

/** The text in one metadata edit, then the icon. Returns the progress unlistener. */
async function csSaveOverview(communityId) {
    const st = VectorSvelte.csState();
    const name = st.draft.name.trim();
    const description = st.draft.description.trim();
    if (!name) throw new Error('A community needs a name');
    const renamed = name !== st.saved.name;
    const described = description !== st.saved.description;
    const iconPath = st.draft.iconPath;
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
    if (!iconPath) return null;
    const unlisten = await window.__TAURI__.event.listen('community_image_upload_progress', (e) => {
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
    return unlisten;
}

/**
 * The dragged order, then the role in the editor. A role created here reopens as
 * itself once it exists, so its Members section becomes reachable.
 */
async function csSaveRoles(communityId) {
    const roles = VectorSvelte.csState().roles;
    if (VectorSvelte.csRoleOrderDirty()) {
        await invoke('reorder_community_roles', { communityId, ordered: roles.order });
    }
    let reopen = roles.edit?.id ?? null;
    if (VectorSvelte.csRoleEditDirty()) {
        const e = roles.edit;
        const name = e.name.trim();
        if (!name) throw new Error('A role needs a name');
        const args = { communityId, name, color: e.color, permissions: e.permissions, channelId: e.channel_id };
        if (e.id === null) reopen = await invoke('create_community_role', args);
        else await invoke('edit_community_role', { ...args, roleId: e.id });
    }
    await csLoadRoles(communityId);
    VectorSvelte.csSetRoleOrder(null);
    const fresh = VectorSvelte.csState().roles.view?.roles.find(r => r.role_id === reopen);
    if (VectorSvelte.csState().roles.edit) VectorSvelte.csEditRole(fresh || null);
}

/** The roles view, and the members the editor can hand a role to. */
async function csLoadRoles(communityId) {
    let view = null;
    try { view = await invoke('get_community_roles_view', { communityId }); } catch (e) { console.warn('[Roles] view failed:', e); return; }
    if (VectorSvelte.csState().communityId !== communityId) return;
    // The proven owner, for the list's fixed top row (never a Role: position 0 is theirs alone).
    view.owner = csCommunityChats(communityId).find(c => c.metadata?.custom_fields?.owner_npub)?.metadata.custom_fields.owner_npub || null;
    VectorSvelte.csSetRolesView(view);
    const members = await fetchCommunityMembers(communityId).catch(() => []);
    if (VectorSvelte.csState().communityId !== communityId) return;
    const npubs = members.map(m => m.npub);
    VectorSvelte.csSetRoleMembers(npubs);
    for (const np of npubs.filter(np => !getProfile(np) && !strangerProfileRequested.has(np))) {
        strangerProfileRequested.add(np);
        invoke('load_profile', { npub: np }).catch(() => {});
    }
}

/**
 * Give a role to `add` and take it from `remove`, each member's whole set rewritten
 * (a Grant replaces). Immediate, like every membership verb; keys for a
 * channel-scoped role move with it backend-side.
 */
async function csSetRoleHolders(roleId, add, remove) {
    const st = VectorSvelte.csState();
    const communityId = st.communityId;
    const view = st.roles.view;
    if (!communityId || !view || st.roles.busy) return;
    const heldBy = (npub) => view.roles.filter(r => r.holders.includes(npub)).map(r => r.role_id);
    VectorSvelte.csSetRolesBusy(true);
    let failed = 0;
    try {
        for (const npub of [...add, ...remove]) {
            const current = heldBy(npub);
            const next = add.includes(npub)
                ? [...new Set([...current, roleId])]
                : current.filter(id => id !== roleId);
            try {
                await invoke('set_community_member_roles', { communityId, npub, roleIds: next });
            } catch (e) {
                failed++;
                console.warn('[Roles] set_member_roles failed:', e);
                if (failed === 1) showToast(String(e));
            }
        }
        await csLoadRoles(communityId);
        csRepaint(communityId);
    } finally {
        VectorSvelte.csSetRolesBusy(false);
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
