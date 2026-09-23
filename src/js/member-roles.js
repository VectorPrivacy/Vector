// A member's roles within one community: reading them for the mini profile, and the
// role menu that grants and removes them in place (the roster's row menu and the mini
// profile's add button both open it). The backend re-checks every rule; this side
// only decides which controls to offer.

/** The colour a role paints with: its own, or the neutral grey an uncoloured role wears. */
function roleTint(color) {
    return color ? `#${color.toString(16).padStart(6, '0')}` : '#99aab5';
}

/** The roles view with its proven owner, or null where roles don't apply (a v1 community). */
async function memberRolesView(communityId) {
    try {
        const view = await invoke('get_community_roles_view', { communityId });
        view.owner = csCommunityChats(communityId).find(c => c.metadata?.custom_fields?.owner_npub)?.metadata.custom_fields.owner_npub || null;
        return view;
    } catch (_) {
        return null;
    }
}

/** A member's standing: the highest (lowest-numbered) position among the roles they hold. */
function memberRank(view, npub) {
    let best = Infinity;
    for (const r of view.roles) if (r.position < best && r.holders.includes(npub)) best = r.position;
    return best;
}

/** Whether the caller may change `npub`'s roles: Manage Roles and a strict outrank, never the owner. */
function canEditMemberRoles(view, npub) {
    if (!view?.can_manage || !npub || npub === view.owner) return false;
    return view.is_owner || (view.rank != null && view.rank < memberRank(view, npub));
}

/** Whether the role menu has anything to offer for `npub`. */
function memberRolesOffered(view, npub) {
    return canEditMemberRoles(view, npub) && view.roles.some(r => r.manageable);
}

/**
 * The role menu's items for `npub`: every role the caller may hand out, ticked where
 * held, plus the held ones above the caller shown fixed. Channel-scoped roles follow
 * under their own header, since holding one is what opens a private channel.
 * `ctx` is { communityId, npub, view, busy } and is refreshed in place by each toggle.
 */
function memberRoleItems(ctx) {
    const { view, npub, busy } = ctx;
    const held = new Set(view.roles.filter(r => r.holders.includes(npub)).map(r => r.role_id));
    const channelName = (id) => view.channels.find(c => c.channel_id === id)?.name || '';
    const item = (r) => {
        const ch = r.channel_id ? channelName(r.channel_id) : '';
        return {
            label: r.name,
            hint: ch && ch !== r.name ? `#${ch}` : '',
            swatch: roleTint(r.color),
            checked: held.has(r.role_id),
            busy: busy === r.role_id,
            disabled: !r.manageable || !!busy,
            keepOpen: true,
            onClick: (repaint) => setMemberRole(ctx, r.role_id, !held.has(r.role_id), repaint),
        };
    };
    const shown = view.roles.filter(r => r.manageable || held.has(r.role_id));
    const server = shown.filter(r => !r.channel_id).map(item);
    const channel = shown.filter(r => r.channel_id).map(item);
    return [
        ...server,
        ...(channel.length ? [...(server.length ? [{ divider: true }] : []), { header: 'Private channels' }, ...channel] : []),
    ];
}

/**
 * Open the role menu on its own at (x, y), for the mini profile's add button.
 * Returns false when there is nothing to offer.
 */
async function openMemberRoleMenu(communityId, npub, x, y) {
    const view = await memberRolesView(communityId);
    if (!view || !memberRolesOffered(view, npub)) return false;
    const ctx = { communityId, npub, view, busy: null, header: true };
    showContextMenu({ x, y, items: [{ header: 'Roles' }, ...memberRoleItems(ctx)] });
    return true;
}

/**
 * Give (`on`) or take one role from a member. A Grant replaces their whole set, so the
 * set is re-read right before the write, and the menu takes one change at a time.
 */
async function setMemberRole(ctx, roleId, on, repaint) {
    if (ctx.busy) return;
    const lead = () => (ctx.header ? [{ header: 'Roles' }] : []);
    ctx.busy = roleId;
    repaint([...lead(), ...memberRoleItems(ctx)]);
    try {
        const fresh = await memberRolesView(ctx.communityId);
        if (!fresh) throw new Error('Roles are unavailable here');
        const current = fresh.roles.filter(r => r.holders.includes(ctx.npub)).map(r => r.role_id);
        const next = on ? [...new Set([...current, roleId])] : current.filter(id => id !== roleId);
        await invoke('set_community_member_roles', { communityId: ctx.communityId, npub: ctx.npub, roleIds: next });
    } catch (e) {
        showToast(String(e?.message || e));
    }
    ctx.view = (await memberRolesView(ctx.communityId)) || ctx.view;
    ctx.busy = null;
    repaint([...lead(), ...memberRoleItems(ctx)]);
    await memberRolesChanged(ctx.communityId);
}

/** Repaint every surface that shows who holds what in `communityId`. */
async function memberRolesChanged(communityId) {
    dmsgClearDeleteMetaCache();
    let admins = null;
    let graph = null;
    try { admins = await invoke('get_community_admins', { communityId }); } catch (_) {}
    try { graph = await invoke('get_community_role_graph', { communityId }); } catch (_) {}
    if (admins) applyCommunityAdmins(communityId, admins);
    if (graph) communityRoleGraphCache.set(communityId, graph);
    if (groupRoster && groupRosterCommunityId === communityId) {
        groupRoster.setRoster({ ...(admins ? { admins } : {}), ...(graph ? { roleGraph: graph } : {}) });
    }
    const cs = VectorSvelte.csState();
    if (cs.communityId === communityId && cs.canRoles && !cs.roles.busy) csLoadRoles(communityId);
    const m = VectorSvelte.miniProfile();
    if (m.npub && m.communityId === communityId) await loadMiniProfileRoles(m.npub, communityId);
}

/** Paint `npub`'s roles in `communityId` onto the open mini profile. */
async function loadMiniProfileRoles(npub, communityId) {
    const view = await memberRolesView(communityId);
    const m = VectorSvelte.miniProfile();
    if (m.npub !== npub || m.communityId !== communityId) return;
    if (!view) { VectorSvelte.setMiniProfileRoles(npub, null); return; }
    const editable = canEditMemberRoles(view, npub);
    const channelName = (id) => view.channels.find(c => c.channel_id === id)?.name || '';
    const roles = view.roles.filter(r => r.holders.includes(npub)).map(r => ({
        id: r.role_id,
        name: r.name,
        tint: roleTint(r.color),
        channel: r.channel_id ? channelName(r.channel_id) : '',
        removable: editable && r.manageable,
    }));
    VectorSvelte.setMiniProfileRoles(npub, {
        owner: npub === view.owner,
        roles,
        addable: editable && view.roles.some(r => r.manageable && !r.holders.includes(npub)),
    });
}

/** Take one role off the member the mini profile shows. */
async function removeMiniProfileRole(roleId) {
    const { npub, communityId } = VectorSvelte.miniProfile();
    if (!npub || !communityId) return;
    const view = await memberRolesView(communityId);
    if (!view) return;
    const ctx = { communityId, npub, view, busy: null };
    VectorSvelte.setMiniProfileRoleBusy(roleId);
    await setMemberRole(ctx, roleId, false, () => {});
    VectorSvelte.setMiniProfileRoleBusy(null);
}
