<script>
    // The community overview's member roster: rank sections, search, the admin crown,
    // and kick/ban through the row menu. The banlist itself lives in Community Settings.
    //
    // The roster owns its live lists (members, admins, banned, role graph, filter) as
    // $state; the vanilla side feeds it through setRoster / setProfiles / setFilter and
    // learns of member-driven changes (a kick, a promotion) through the onChange callback
    // so its caches stay in step. Every mutation flows: confirm → act (row pinned busy) →
    // re-read the settled truth → patch state; nothing is re-rendered by hand.
    import MemberRow from '../people/MemberRow.svelte';
    import { untrack } from 'svelte';
    import { profileVersion } from '../lib/signals.svelte.js';

    let {
        communityId,
        myNpub = '',
        ownerNpub = null,          // PROVEN owner (verified attestation), or null
        caps = {},                 // community capabilities: manage_admin_role, kick, ban
        profiles = [],             // initial snapshot; later loads ride setProfiles()
        members = [],              // [{ npub }]
        admins = [],
        banned = [],
        roleGraph = null,          // { roles: [{ role_id, name, position, channel_id }], grants: [{ npub, role_ids }] }
        channel = null,            // a Private Channel's id: list only who may read it
        loading = false,           // no cached roster yet: show the loading line until setRoster
        h,                         // the app's bag, registered with the roster screen (js/community.js)
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
    // svelte-ignore state_referenced_locally
    let scopeChannel = $state(channel);
    let filter = $state('');
    let acting = $state.raw(new Set());   // npubs with an action in flight

    // Props are mount-time constants here; the picker is remounted, never re-propped.
    // svelte-ignore state_referenced_locally
    const ui = {
        twemojify: h.twemojify,
        renderCustomEmojiShortcodes: h.renderCustomEmojiShortcodes,
        showTooltip: h.showGlobalTooltip,
        hideTooltip: h.hideGlobalTooltip,
    };

    // The overview's hover sweep in the accent colour (a cheap cached layer).
    const accent = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
    const hoverBg = `linear-gradient(to right, ${accent}40, transparent)`;

    // ── instance exports: the vanilla<->island bridge ──

    /** The open channel changed: a Private Channel's id, or null for a public one. */
    export function setChannel(id) {
        scopeChannel = id || null;
    }

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

    // Who may read the scoped Private Channel: the owner, and holders of a role scoped
    // to it (CORD-04 §2, the roles scoped to a channel ARE its access list). Unscoped
    // until the graph lands, so a slow read shows everyone rather than a false empty.
    const scope = $derived.by(() => {
        if (!scopeChannel || !graph) return null;
        const access = new Set((graph.roles || []).filter((r) => r.channel_id === scopeChannel).map((r) => r.role_id));
        const out = new Set();
        if (ownerNpub) out.add(ownerNpub);
        for (const g of graph.grants || []) if (g.role_ids?.some((id) => access.has(id))) out.add(g.npub);
        return out;
    });

    // Row view-models bucketed by section: owner → admins → members, A→Z within a tier.
    const sections = $derived.by(() => {
        const tierOf = (npub) => (npub === ownerNpub ? 0 : adminSet.has(npub) ? 1 : 2);
        const vms = [];
        for (const m of memberList) {
            if (scope && !scope.has(m.npub)) continue;
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

    // ── the window: only the rows near the viewport exist in the DOM ──
    //
    // Every item has a fixed height, so the list is a spacer of the total height with
    // the visible items placed absolutely inside it. Scrolling swaps which items are
    // mounted; nothing else moves. Section collapse lives here (it is a property of
    // the layout now, not of a section element) and is remembered per community.
    const ROW_H = 46, HEAD_H = 36, SECTION_GAP = 12, OVERSCAN_PX = 300;
    // svelte-ignore state_referenced_locally
    let closedIds = $state(new Set());
    $effect.pre(() => {
        // Seed from the remembered state whenever the set of sections changes.
        const ids = sections.map((s) => s.id);
        untrack(() => {
            const next = new Set(ids.filter((id) => h.memberSectionClosed(communityId, id)));
            if (next.size !== closedIds.size || [...next].some((id) => !closedIds.has(id))) closedIds = next;
        });
    });
    function toggleSection(id) {
        const next = new Set(closedIds);
        const closing = !next.has(id);
        if (closing) next.add(id); else next.delete(id);
        closedIds = next;
        h.setMemberSectionClosed(communityId, id, closing);
    }
    const layout = $derived.by(() => {
        const items = [];
        let y = 0;
        sections.forEach((section, i) => {
            if (i) y += SECTION_GAP;
            const closed = closedIds.has(section.id) && !f;
            items.push({ key: 'h:' + section.id, kind: 'head', top: y, h: HEAD_H, section, closed });
            y += HEAD_H;
            if (closed) return;
            for (const vm of section.rows) {
                items.push({ key: vm.npub, kind: 'row', top: y, h: ROW_H, vm });
                y += ROW_H;
            }
        });
        return { items, total: y };
    });
    let scrollTop = $state(0);
    let viewH = $state(0);
    let listTop = $state(0);   // the list's offset inside the scroller
    const visible = $derived.by(() => {
        const from = scrollTop - listTop - OVERSCAN_PX;
        const to = scrollTop - listTop + viewH + OVERSCAN_PX;
        const out = [];
        for (const it of layout.items) {
            if (it.top + it.h < from) continue;
            if (it.top > to) break;
            out.push(it);
        }
        return out;
    });
    // Tracks the nearest scrolling ancestor: its scroll position and its height.
    function windowOn(node) {
        let sc = node.parentElement;
        while (sc && !/auto|scroll/.test(getComputedStyle(sc).overflowY)) sc = sc.parentElement;
        if (!sc) { viewH = window.innerHeight; return; }
        const read = () => {
            scrollTop = sc.scrollTop;
            viewH = sc.clientHeight;
            listTop = node.getBoundingClientRect().top - sc.getBoundingClientRect().top + sc.scrollTop;
        };
        read();
        sc.addEventListener('scroll', read, { passive: true });
        const ro = new ResizeObserver(read);
        ro.observe(sc);
        return { destroy() { sc.removeEventListener('scroll', read); ro.disconnect(); } };
    }

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

    // Moderation lives on the row menu (right-click / long-press), with a "⋯" handle
    // on hover so it is discoverable without welding destructive buttons onto every row.
    function menuItems(vm) {
        const items = [];
        if (caps.kick) items.push({ label: 'Kick', hint: 'can rejoin with an invite', icon: 'x', onClick: () => remove(vm, false) });
        if (caps.ban) items.push({ label: 'Ban', hint: 'cannot rejoin', icon: 'x-user', danger: true, onClick: () => remove(vm, true) });
        return items;
    }

    // Gated at press time: a row's moderability changes with the admin set while it stays mounted.
    function rowMenu(node, vm) {
        let cur = vm;
        h.attachLongPressContextMenu(node, (x, y) => { if (cur.canModerate) h.showContextMenu({ x, y, items: menuItems(cur) }); });
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
    <div class="member-roster-window" style:height="{layout.total}px" use:windowOn>
        {#each visible as it (it.key)}
            {#if it.kind === 'head'}
                <div class="member-section-head btn" class:is-closed={it.closed} role="button" tabindex="0"
                     style:top="{it.top}px"
                     onclick={() => toggleSection(it.section.id)}
                     onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), toggleSection(it.section.id))}>
                    <span class="member-section-label">{it.section.label}</span>
                    <span class="member-section-count">{it.section.rows.length}</span>
                    <span class="member-section-caret"><span class="icon icon-chevron-down"></span></span>
                </div>
            {:else}
                {@const vm = it.vm}
                <div class="member-roster-slot" style:top="{it.top}px" use:rowMenu={vm}>
                    <MemberRow
                        npub={vm.npub}
                        profile={vm.profile}
                        src={vm.src}
                        display={vm.display}
                        hasName={vm.hasName}
                        withStatus
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
            {/if}
        {/each}
    </div>
{/if}


<!-- No <style>: the global .member-* rules cascade in; the DOM matches the vanilla roster. -->
