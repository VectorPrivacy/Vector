<script>
    // The community overview's member roster: rank sections, search, and roles and
    // kick/ban through the row menu. The banlist itself lives in Community Settings.
    //
    // The roster owns its live lists (members, admins, banned, role graph, filter) as
    // $state; the vanilla side feeds it through setRoster / setProfiles / setFilter and
    // learns of member-driven changes (a kick) through the onChange callback
    // so its caches stay in step. Every mutation flows: confirm → act (row pinned busy) →
    // re-read the settled truth → patch state; nothing is re-rendered by hand.
    import MemberRow from '../people/MemberRow.svelte';
    import { untrack } from 'svelte';
    import { profileVersion, openChatId, chatVersion } from '../lib/signals.svelte.js';
    import { advancedState } from '../lib/advanced.svelte.js';
    import { communityNickname } from '../lib/nicknames.svelte.js';
    import { timedOutUntil } from '../lib/timeouts.svelte.js';
    import { shellState } from '../lib/shell.svelte.js';

    let {
        communityId,
        myNpub = '',
        ownerNpub = null,          // PROVEN owner (verified attestation), or null
        caps = {},                 // community capabilities: manage_roles, kick, ban
        profiles = [],             // initial snapshot; later loads ride setProfiles()
        members = [],              // [{ npub, unlisted? }]: an `unlisted` member is shown, never counted
        admins = [],
        banned = [],
        roleGraph = null,          // { roles: [{ role_id, name, position, channel_id }], grants: [{ npub, role_ids }] }
        channel = null,            // a Private Channel's id: list only who may read it
        loading = false,           // no cached roster yet: show the loading line until setRoster
        h,                         // the app's bag, registered with the roster screen (js/community.js)
        onChange = () => {},       // ({ members, admins, banned }) after a member-driven change
    } = $props();

    const adv = advancedState();

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
    // What they go by here: your own name for them, then their nickname in this community.
    function displayOf(npub, profile) {
        if (h.streamHidden(npub)) return h.getName(profile || npub);
        return profile?.nickname || communityNickname(communityId, npub) || nameOf(profile) || npub.slice(0, 10) + '...' + npub.slice(-6);
    }

    // Outrank, in role-engine positions: the owner outranks everyone; an admin outranks
    // only non-admins; never the owner, never yourself. Best-effort — the backend
    // re-verifies, this only decides which controls to show.
    function iOutrank(npub) {
        if (npub === ownerNpub || npub === myNpub) return false;
        if (iAmOwner) return true;
        return !adminSet.has(npub);
    }

    // Whether the row menu offers their roles: Manage Roles and a strict outrank, the
    // backend's own gate, read off the graph so the handle shows without a fetch. The
    // owner sets their own, for display, on the new roles.
    function mayRole(npub) {
        if (!caps.manage_roles) return false;
        if (npub === ownerNpub) return iAmOwner && !!graph?.activated;
        if (npub === myNpub) return false;
        if (iAmOwner) return true;
        const mine = topBy.get(myNpub)?.position ?? Infinity;
        return mine < (topBy.get(npub)?.position ?? Infinity);
    }

    // Their own with Change Nickname; a member's beneath you with Manage Nicknames.
    function mayNick(npub) {
        if (npub === myNpub) return !!caps.change_nickname;
        if (!caps.manage_nicknames || npub === ownerNpub) return false;
        if (iAmOwner) return true;
        const mine = topBy.get(myNpub)?.position ?? Infinity;
        return mine < (topBy.get(npub)?.position ?? Infinity);
    }

    // Each member's highest role (lowest position number) among those `pick` admits.
    function highest(pick) {
        const roleById = new Map((graph?.roles || []).filter(pick).map((r) => [r.role_id, r]));
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
    }
    // On the new roles only a role shown separately heads a section; rank still counts them all.
    const sectionRole = (r) => !graph?.activated || r.display;
    const topBy = $derived(highest(() => true));
    // A member's section is their highest role that heads one.
    const heldBy = $derived(highest(sectionRole));
    // On the new roles a name takes the colour of its holder's highest role that has one.
    const colorBy = $derived(graph?.activated ? highest((r) => !!r.color) : new Map());
    const hex = (c) => (c ? '#' + c.toString(16).padStart(6, '0') : null);

    // Sections are ROLES in hierarchy order. The legacy roles fall back to an Admin/Members
    // pair; on the new ones a member with no role shown separately is a Member, the owner too.
    const sectionOrder = $derived.by(() => {
        const order = [];
        for (const role of [...(graph?.roles || [])].filter(sectionRole).sort((a, b) => a.position - b.position)) {
            order.push({ id: role.role_id, label: role.name, role: true });
        }
        if (!order.length && !graph?.activated) order.push({ id: 'admin', label: 'Admin' });
        order.push({ id: 'members', label: 'Members' });
        return order;
    });

    const f = $derived(filter.trim().toLowerCase());

    // Who has posted in the open channel's loaded history: they lead the Members group.
    const posters = $derived.by(() => {
        const id = openChatId();
        if (id) chatVersion(id);
        return h.channelPosters?.(communityId) || null;
    });

    // Who may read the scoped Private Channel: the owner, and holders of a role scoped
    // to it (CORD-04 §2, the roles scoped to a channel ARE its access list). Unscoped
    // until the graph lands, so a slow read shows everyone rather than a false empty.
    const scope = $derived.by(() => {
        if (!scopeChannel || !graph) return null;
        if (graph.activated) return new Set([...(ownerNpub ? [ownerNpub] : []), ...(graph.readers?.[scopeChannel] || [])]);
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
            // The search matches nicknames and real names alike.
            if (f && !(display + ' ' + nameOf(profile) + ' ' + m.npub).toLowerCase().includes(f)) continue;
            const isOwner = m.npub === ownerNpub;
            const isAdmin = adminSet.has(m.npub);
            const veiled = h.streamHidden(m.npub);
            const named = !!(nameOf(profile) || communityNickname(communityId, m.npub));
            vms.push({
                npub: m.npub,
                profile,
                display,
                veiled,
                hasName: !veiled && named,
                // Members order: posted here, then named, then a bare key.
                relevance: posters?.has(m.npub) ? 0 : named ? 1 : 2,
                src: profile ? h.getProfileAvatarSrc(profile) || null : null,
                isOwner,
                isAdmin,
                unnamed: !named && !veiled,
                rankLabel: heldBy.get(m.npub)?.name || null,
                nameColor: hex(colorBy.get(m.npub)?.color),
                timeoutUntil: timedOutUntil(communityId, m.npub),
                unlisted: m.unlisted || null,
                canModerate: iOutrank(m.npub) && !!(caps.kick || caps.ban || caps.moderate_members),
                canRole: mayRole(m.npub),
                canNick: mayNick(m.npub),
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
        // Stable sort: A→Z within each relevance tier is kept from the sort above.
        buckets.get('members').sort((a, b) => a.tier - b.tier || a.relevance - b.relevance);
        return sectionOrder
            .map((s) => ({ ...s, rows: buckets.get(s.id), counted: buckets.get(s.id).filter((vm) => !vm.unlisted).length }))
            .filter((s) => s.rows.length);
    });

    const shown = $derived(sections.reduce((n, s) => n + s.rows.length, 0));

    // ── the window: only the rows near the viewport exist in the DOM ──
    //
    // Every item has a fixed height, so the list is a spacer of the total height with
    // the visible items placed absolutely inside it. Scrolling swaps which items are
    // mounted; nothing else moves. Section collapse lives here (it is a property of
    // the layout now, not of a section element) and is remembered per community.
    const HEAD_H = 36, SECTION_GAP = 12, OVERSCAN_PX = 300;
    // Matches the row heights in styles.css / widescreen.css.
    const shell = shellState();
    const ROW_H = $derived(shell.ws ? 40 : 46);
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
    // The overflow read at mount is only a first guess: tiling.js hides a scroller's overflow
    // for a frame as its content crosses 2048px. Whatever scrolls with the list in it is the one.
    function windowOn(node) {
        let sc = node.parentElement;
        while (sc && !/auto|scroll/.test(getComputedStyle(sc).overflowY)) sc = sc.parentElement;
        const read = () => {
            if (!sc) { viewH = window.innerHeight; return; }
            scrollTop = sc.scrollTop;
            viewH = sc.clientHeight;
            listTop = node.getBoundingClientRect().top - sc.getBoundingClientRect().top + sc.scrollTop;
        };
        const ro = new ResizeObserver(read);
        if (sc) ro.observe(sc);
        const onScroll = (e) => {
            const t = e.target;
            if (t !== sc) {
                if (!(t instanceof Element) || !t.contains(node)) return;
                if (sc) ro.unobserve(sc);
                sc = t;
                ro.observe(sc);
            }
            read();
        };
        read();
        document.addEventListener('scroll', onScroll, { capture: true, passive: true });
        return { destroy() { document.removeEventListener('scroll', onScroll, { capture: true }); ro.disconnect(); } };
    }

    // ── actions ──

    function setActing(npub, busy) {
        const next = new Set(acting);
        if (busy) next.add(npub);
        else next.delete(npub);
        acting = next;
    }

    // Roles and moderation live on the row menu (right-click / long-press). The same menu
    // opens on a member's name in chat (js/member-roles.js).
    function rowItems(vm) {
        return h.memberMenuItems(communityId, vm.npub, {
            onBusy: (npub, on) => setActing(npub, on),
            onRemoved: (npub) => {
                memberList = memberList.filter((x) => x.npub !== npub);
                onChange(getRoster());
            },
        });
    }

    const hasMenu = (vm) => vm.canModerate || vm.canRole || vm.canNick || adv.on;

    async function showRowMenu(vm, x, y) {
        const items = await rowItems(vm);
        if (items.length) h.showContextMenu({ x, y, items });
    }

    // Gated at press time: a row's standing changes with the role graph while it stays mounted.
    function rowMenu(node, vm) {
        let cur = vm;
        h.attachLongPressContextMenu(node, (x, y) => { if (hasMenu(cur)) showRowMenu(cur, x, y); });
        return { update: (v) => { cur = v; } };
    }

    // Advanced Mode: a role's heading copies its id. The Admin / Members fallbacks are not roles.
    function headMenu(section, e) {
        const items = section.role ? h.copyIdItems('Role', section.id) : [];
        if (!items.length) return;
        e.preventDefault();
        h.showContextMenu({ x: e.clientX, y: e.clientY, items });
    }

    // Row → mini-profile. stopPropagation so the opening click doesn't reach the
    // document-level outside-click handler that would dismiss the just-opened popup.
    function openProfile(npub, e) {
        e.stopPropagation();
        h.showMiniProfile(npub, e.currentTarget.querySelector('.member-pick-avatar'), communityId);
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
                     oncontextmenu={(e) => headMenu(it.section, e)}
                     onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), toggleSection(it.section.id))}>
                    <span class="member-section-label section-label">{it.section.label}</span>
                    <span class="member-section-count">{it.section.counted}</span>
                    <span class="member-section-caret"><span class="icon icon-chevron-down"></span></span>
                </div>
            {:else}
                {@const vm = it.vm}
                <div class="member-roster-slot" class:is-unnamed={vm.unnamed && !vm.nameColor} style:top="{it.top}px" use:rowMenu={vm}>
                    <MemberRow
                        npub={vm.npub}
                        profile={vm.profile}
                        src={vm.src}
                        display={vm.display}
                        hasName={vm.hasName}
                        withStatus
                        veiled={vm.veiled}
                        rank={vm.isOwner ? 'owner' : null /* the group heading and name colour mark the rest */}
                        rankLabel={vm.rankLabel}
                        nameColor={vm.nameColor}
                        timeoutUntil={vm.timeoutUntil}
                        unlisted={vm.unlisted}
                        onunlisted={() => h.explainUnlisted(vm.unlisted)}
                        {hoverBg}
                        acting={acting.has(vm.npub)}
                        onactivate={(e) => openProfile(vm.npub, e)}
                        {ui}
                    />
                </div>
            {/if}
        {/each}
    </div>
{/if}


<!-- No <style>: the global .member-* rules cascade in; the DOM matches the vanilla roster. -->
