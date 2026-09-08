<script>
    // The community overview's member roster: rank sections, search, the admin crown,
    // kick/ban through the row menu, and the owner-facing banlist with unban.
    //
    // The roster owns its live lists (members, admins, banned, role graph, filter) as
    // $state; the vanilla side feeds it through setRoster / setProfiles / setFilter and
    // learns of member-driven changes (a kick, a promotion) through the onChange callback
    // so its caches stay in step. Every mutation flows: confirm → act (row pinned busy) →
    // re-read the settled truth → patch state; nothing is re-rendered by hand.
    import MemberRow from './MemberRow.svelte';
    import { profileVersion } from '../lib/signals.svelte.js';
    import MemberSection from './MemberSection.svelte';

    let {
        communityId,
        myNpub = '',
        ownerNpub = null,          // PROVEN owner (verified attestation), or null
        caps = {},                 // community capabilities: manage_admin_role, kick, ban
        profiles = [],             // initial snapshot; later loads ride setProfiles()
        members = [],              // [{ npub }]
        admins = [],
        banned = [],
        roleGraph = null,          // { roles: [{ role_id, name, position }], grants: [{ npub, role_ids }] }
        loading = false,           // no cached roster yet: show the loading line until setRoster
        h,                         // vanilla helpers (see mountMemberRoster)
        onChange = () => {},       // ({ members, admins, banned }) after a member-driven change
    } = $props();

    // svelte-ignore state_referenced_locally
    let memberList = $state.raw(members);
    // svelte-ignore state_referenced_locally
    let adminList = $state.raw(admins);
    // svelte-ignore state_referenced_locally
    let bannedList = $state.raw(banned);
    // svelte-ignore state_referenced_locally
    let graph = $state.raw(roleGraph);
    // svelte-ignore state_referenced_locally
    let profilesList = $state.raw(profiles);
    // svelte-ignore state_referenced_locally
    let isLoading = $state(loading);
    let filter = $state('');
    let acting = $state.raw(new Set());   // npubs with an action in flight

    // Props are mount-time constants here; the picker is remounted, never re-propped.
    // svelte-ignore state_referenced_locally
    const ui = {
        placeholder: () => h.createPlaceholderAvatar(false, 25),
        twemojify: h.twemojify,
        renderCustomEmojiShortcodes: h.renderCustomEmojiShortcodes,
        showTooltip: h.showGlobalTooltip,
        hideTooltip: h.hideGlobalTooltip,
    };

    // The overview's hover sweep in the accent colour (a cheap cached layer).
    const accent = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
    const hoverBg = `linear-gradient(to right, ${accent}40, transparent)`;

    // ── instance exports: the vanilla<->island bridge ──

    /** Authoritative lists landed (or a cached paint is being replaced). */
    export function setRoster(next) {
        if (next.members) memberList = next.members;
        if (next.admins) adminList = next.admins;
        if (next.banned) bannedList = next.banned;
        if ('roleGraph' in next) graph = next.roleGraph;
        isLoading = false;
    }

    export function setProfiles(value) {
        profilesList = value || [];
    }

    export function setFilter(value) {
        filter = String(value || '');
    }

    export function getRoster() {
        return { members: memberList, admins: adminList, banned: bannedList };
    }

    // ── derivations ──

    const profileById = $derived(new Map(profilesList.map((p) => [p.id, p])));
    // A row reads the LIVE profile and tracks its signal, so a status or name change
    // repaints the row without a re-mount; the snapshot only covers a missing lookup.
    function profileFor(npub) {
        profileVersion(npub);
        return (h.getProfile ? h.getProfile(npub) : null) || profileById.get(npub) || null;
    }
    const adminSet = $derived(new Set(adminList));
    const iAmOwner = $derived(!!(myNpub && ownerNpub && myNpub === ownerNpub));

    function nameOf(profile) {
        return profile ? profile.nickname || profile.name || profile.display_name || '' : '';
    }
    function displayOf(npub, profile) {
        return nameOf(profile) || npub.slice(0, 10) + '...' + npub.slice(-6);
    }

    // Outrank, in role-engine positions: the owner outranks everyone; an admin outranks
    // only non-admins; never the owner, never yourself. Best-effort — the backend
    // re-verifies, this only decides which controls to show.
    function iOutrank(npub) {
        if (npub === ownerNpub || npub === myNpub) return false;
        if (iAmOwner) return true;
        return !adminSet.has(npub);
    }

    // A member's section is their HIGHEST role (lowest position number).
    const heldBy = $derived.by(() => {
        const roleById = new Map((graph?.roles || []).map((r) => [r.role_id, r]));
        const out = new Map();
        for (const g of graph?.grants || []) {
            let best = null;
            for (const id of g.role_ids || []) {
                const role = roleById.get(id);
                if (role && (!best || role.position < best.position)) best = role;
            }
            if (best) out.set(g.npub, best);
        }
        return out;
    });

    // Sections are ROLES in hierarchy order; Admin/Members is the fallback pair when
    // the graph defines nothing.
    const sectionOrder = $derived.by(() => {
        const order = [];
        for (const role of [...(graph?.roles || [])].sort((a, b) => a.position - b.position)) {
            order.push({ id: role.role_id, label: role.name });
        }
        if (!order.length) order.push({ id: 'admin', label: 'Admin' });
        order.push({ id: 'members', label: 'Members' });
        return order;
    });

    const f = $derived(filter.trim().toLowerCase());

    // Row view-models bucketed by section: owner → admins → members, A→Z within a tier.
    const sections = $derived.by(() => {
        const tierOf = (npub) => (npub === ownerNpub ? 0 : adminSet.has(npub) ? 1 : 2);
        const vms = [];
        for (const m of memberList) {
            const profile = profileFor(m.npub);
            const display = displayOf(m.npub, profile);
            if (f && !(display + ' ' + m.npub).toLowerCase().includes(f)) continue;
            const isOwner = m.npub === ownerNpub;
            const isAdmin = adminSet.has(m.npub);
            vms.push({
                npub: m.npub,
                profile,
                display,
                hasName: !!nameOf(profile),
                src: profile ? h.getProfileAvatarSrc(profile) || null : null,
                isOwner,
                isAdmin,
                rank: isOwner ? 'owner' : isAdmin ? 'admin' : null,
                rankLabel: heldBy.get(m.npub)?.name || null,
                // The crown gutter exists only when there is a control to hold.
                crown: !isOwner && caps.manage_admin_role
                    ? { active: isAdmin, promote: !isAdmin, title: isAdmin ? 'Remove admin' : 'Make admin' }
                    : null,
                canModerate: iOutrank(m.npub) && !!(caps.kick || caps.ban),
                tier: tierOf(m.npub),
            });
        }
        vms.sort((a, b) => a.tier - b.tier || a.display.toLowerCase().localeCompare(b.display.toLowerCase()));

        const buckets = new Map(sectionOrder.map((s) => [s.id, []]));
        for (const vm of vms) {
            // A role the graph no longer defines must not drop its holder off the roster.
            const id = heldBy.get(vm.npub)?.role_id || (vm.isOwner || vm.isAdmin ? 'admin' : 'members');
            (buckets.get(id) || buckets.get('members')).push(vm);
        }
        return sectionOrder.map((s) => ({ ...s, rows: buckets.get(s.id) })).filter((s) => s.rows.length);
    });

    const shown = $derived(sections.reduce((n, s) => n + s.rows.length, 0));

    // Owner-only banlist: banned members are excluded above, so this is the only place
    // they surface, with the unban affordance.
    const bannedRows = $derived.by(() => {
        if (!caps.ban || f) return [];
        return bannedList.map((npub) => {
            const profile = profileFor(npub);
            return { npub, profile, display: displayOf(npub, profile), hasName: !!nameOf(profile), src: profile ? h.getProfileAvatarSrc(profile) || null : null };
        });
    });

    // ── actions ──

    function setActing(npub, busy) {
        const next = new Set(acting);
        if (busy) next.add(npub);
        else next.delete(npub);
        acting = next;
    }

    async function toggleAdmin(vm, e) {
        e.stopPropagation();
        if (acting.has(vm.npub)) return;
        const makeAdmin = !vm.isAdmin;
        const confirmed = await h.popupConfirm(
            makeAdmin ? 'Make Admin' : 'Remove Admin',
            makeAdmin
                ? `Make <b>${h.escapeHtml(vm.display)}</b> an admin? They'll be able to moderate this community (ban, hide messages, manage settings).`
                : `Remove <b>${h.escapeHtml(vm.display)}</b> as an admin? They'll lose all moderation powers.`,
            false, '', 'vector_warning.svg');
        if (!confirmed) return;
        setActing(vm.npub, true);
        try {
            await h.invoke(makeAdmin ? 'grant_community_admin' : 'revoke_community_admin', { communityId, npub: vm.npub });
            // Re-read the roster the backend settled on rather than assuming the flip: a
            // published edition the fold hasn't adopted yet would otherwise show as done
            // and silently revert on the next open.
            const settled = await h.invoke('get_community_admins', { communityId }).catch(() => null);
            if (settled) {
                adminList = settled;
                if (settled.includes(vm.npub) !== makeAdmin) h.showToast('Published, but not confirmed yet. It should apply shortly.');
            } else {
                adminList = makeAdmin ? [...new Set([...adminList, vm.npub])] : adminList.filter((n) => n !== vm.npub);
            }
            // The in-chat tags read the shared roster cache, not this panel's copy.
            h.applyCommunityAdmins(communityId, adminList);
            h.dmsgClearDeleteMetaCache();
            onChange(getRoster());
        } catch (err) {
            h.showToast(String(err));
        } finally {
            setActing(vm.npub, false);
        }
    }

    async function remove(vm, ban) {
        const confirmed = await h.popupConfirm(
            ban ? 'Ban member' : 'Kick member',
            ban
                ? `Ban <b>${h.escapeHtml(vm.display)}</b>? They'll be removed from the community and can't rejoin unless you unban them.`
                : `Kick <b>${h.escapeHtml(vm.display)}</b>? They'll be removed from the community but can rejoin with a new invite.`,
            false, '', 'vector_warning.svg');
        if (!confirmed) return;
        setActing(vm.npub, true);
        try {
            await h.invoke(ban ? 'ban_community_member' : 'kick_community_member', { communityId, npub: vm.npub });
            memberList = memberList.filter((x) => x.npub !== vm.npub);
            h.dmsgClearDeleteMetaCache();
            h.refreshCommunityMemberCount(communityId, true);
            onChange(getRoster());
        } catch (err) {
            // A private-community ban can fail with the (long, important) bunker read-cut
            // explanation — a persistent notice, not a toast.
            if (ban) await h.popupConfirm("Couldn't ban", h.escapeHtml(String(err)), true, '', 'vector_warning.svg');
            else h.showToast(String(err));
        } finally {
            setActing(vm.npub, false);
        }
    }

    async function unban(row, e) {
        e.stopPropagation();
        if (acting.has(row.npub)) return;
        setActing(row.npub, true);
        try {
            await h.invoke('unban_community_member', { communityId, npub: row.npub });
            bannedList = bannedList.filter((x) => x !== row.npub);
            h.dmsgClearDeleteMetaCache();
            onChange(getRoster());
        } catch (err) {
            h.showToast(String(err));
        } finally {
            setActing(row.npub, false);
        }
    }

    // Moderation lives on the row menu (right-click / long-press), with a "⋯" handle
    // on hover so it is discoverable without welding destructive buttons onto every row.
    function menuItems(vm) {
        const items = [];
        if (caps.kick) items.push({ label: 'Kick', hint: 'can rejoin with an invite', icon: 'x', onClick: () => remove(vm, false) });
        if (caps.ban) items.push({ label: 'Ban', hint: 'cannot rejoin', icon: 'x-user', danger: true, onClick: () => remove(vm, true) });
        return items;
    }

    function rowMenu(node, vm) {
        let cur = vm;
        if (!cur.canModerate) return;
        h.attachLongPressContextMenu(node, (x, y) => h.showContextMenu({ x, y, items: menuItems(cur) }));
        return { update: (v) => { cur = v; } };
    }

    function openMenu(vm, e) {
        e.stopPropagation();
        const r = e.currentTarget.getBoundingClientRect();
        h.showContextMenu({ x: r.left, y: r.bottom + 4, items: menuItems(vm) });
    }

    // Row → mini-profile. stopPropagation so the opening click doesn't reach the
    // document-level outside-click handler that would dismiss the just-opened popup.
    function openProfile(npub, e) {
        e.stopPropagation();
        h.showMiniProfile(npub, e.currentTarget.querySelector('.member-pick-avatar'));
    }
</script>

{#if isLoading && !memberList.length}
    <p class="cmt-empty" style="text-align:center;">Loading members…</p>
{:else if !shown}
    <p class="group-placeholder" style="text-align:center;padding:14px;">
        {f ? 'No matches.' : 'No one has spoken yet. Members appear here once they post.'}
    </p>
{:else}
    {#each sections as section (section.id)}
        <MemberSection
            label={section.label}
            count={section.rows.length}
            closed={h.memberSectionClosed(communityId, section.id)}
            forceOpen={!!f}
            ontoggle={(closing) => h.setMemberSectionClosed(communityId, section.id, closing)}
        >
            {#each section.rows as vm (vm.npub)}
                <div style="display:contents;" use:rowMenu={vm}>
                    <MemberRow
                        npub={vm.npub}
                        profile={vm.profile}
                        src={vm.src}
                        display={vm.display}
                        hasName={vm.hasName}
                        rank={vm.rank}
                        rankLabel={vm.rankLabel}
                        {hoverBg}
                        acting={acting.has(vm.npub)}
                        onactivate={(e) => openProfile(vm.npub, e)}
                        {ui}
                    >
                        {#snippet gutter()}
                            {#if vm.crown}
                                <div class="member-crown-slot">
                                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                                    <div
                                        class="member-pick-admin"
                                        class:active={vm.crown.active || acting.has(vm.npub)}
                                        class:promote={vm.crown.promote}
                                        title={vm.crown.title}
                                        style:cursor="pointer"
                                        style:pointer-events={acting.has(vm.npub) ? 'none' : null}
                                        onclick={(e) => toggleAdmin(vm, e)}
                                    >
                                        <span class="icon {acting.has(vm.npub) ? 'icon-loading spin' : 'icon-crown'}"></span>
                                    </div>
                                </div>
                            {/if}
                        {/snippet}
                        {#snippet trailing()}
                            {#if vm.canModerate}
                                <div class="member-pick-actions">
                                    <div class="member-pick-more" title="Moderate" role="button" tabindex="0"
                                        onclick={(e) => openMenu(vm, e)}
                                        onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), openMenu(vm, e))}
                                    >
                                        <span class="icon icon-dots-horizontal"></span>
                                    </div>
                                </div>
                            {/if}
                        {/snippet}
                    </MemberRow>
                </div>
            {/each}
        </MemberSection>
    {/each}
{/if}

{#if bannedRows.length}
    <div style="font-size:12px;text-transform:uppercase;letter-spacing:0.06em;opacity:0.5;margin:16px 0 6px;padding-left:2px;">
        Banned ({bannedRows.length})
    </div>
    {#each bannedRows as row (row.npub)}
        <MemberRow
            npub={row.npub}
            profile={row.profile}
            src={row.src}
            display={row.display}
            hasName={row.hasName}
            dim
            onactivate={(e) => openProfile(row.npub, e)}
            {ui}
        >
            {#snippet trailing()}
                <button
                    class="cmt-btn cmt-btn-sm cmt-btn-secondary"
                    title="Unban"
                    style="margin-left:auto;"
                    disabled={acting.has(row.npub)}
                    onclick={(e) => unban(row, e)}
                >
                    {#if acting.has(row.npub)}
                        <span class="icon icon-loading spin"></span>Unbanning
                    {:else}
                        <span class="icon icon-add-user"></span>Unban
                    {/if}
                </button>
            {/snippet}
        </MemberRow>
    {/each}
{/if}

<!-- No <style>: the global .member-* rules cascade in; the DOM matches the vanilla roster. -->
