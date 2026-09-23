<script>
    // Community Settings → Roles. The list in CORD-04 display order (position, then the
    // lower role_id), dragged to reorder; and the editor for one role: its look, its
    // permissions and its members. Order and edits are drafts under the save bar;
    // membership acts at once. A role is never deleted: CORD-04 has no role tombstone,
    // so there is nothing honest to offer until it does.
    import { csState, csSetRoleOrder, csEditRole, csPatchRoleEdit } from '../lib/community-settings.svelte.js';
    import { reorderable, dragGhost, nearestByCentre } from '../lib/reorder.js';
    import { profileVersion } from '../lib/signals.svelte.js';
    import MemberRow from '../people/MemberRow.svelte';

    // h: setRoleHolders(roleId, add, remove), name(npub), profile(npub), avatarSrc(npub), ui
    let { h } = $props();

    const st = csState();
    const view = $derived(st.roles.view);
    const edit = $derived(st.roles.edit);

    // ── the permission catalogue ──
    // Every assignable CORD-04 §3 bit, grouped as a person thinks of them. Retired and
    // reserved bits are absent; any a role already carries ride through its edits.
    const BIT = {
        MANAGE_ROLES: 1n << 0n, MANAGE_CHANNELS: 1n << 1n, MANAGE_METADATA: 1n << 2n, KICK: 1n << 3n,
        BAN: 1n << 4n, MANAGE_MESSAGES: 1n << 5n, CREATE_INVITE: 1n << 6n, VIEW_AUDIT_LOG: 1n << 8n,
        MENTION_EVERYONE: 1n << 9n, PIN_MESSAGES: 1n << 11n,
    };
    const GROUPS = [
        {
            title: 'Community',
            perms: [
                { bit: BIT.MANAGE_METADATA, name: 'Manage Community', desc: 'Change the community\'s name, icon and description.' },
                { bit: BIT.MANAGE_CHANNELS, name: 'Manage Channels', desc: 'Create, rename and delete channels.' },
                { bit: BIT.MANAGE_ROLES, name: 'Manage Roles', desc: 'Create and edit roles beneath their own, and give them to members.' },
                { bit: BIT.CREATE_INVITE, name: 'Create Invites', desc: 'Make invite links and invite people directly.' },
                { bit: BIT.VIEW_AUDIT_LOG, name: 'View Audit Log', desc: 'See the record of moderation and management actions.' },
            ],
        },
        {
            title: 'Moderation',
            perms: [
                { bit: BIT.KICK, name: 'Kick Members', desc: 'Remove members beneath them. A kicked member can rejoin.' },
                { bit: BIT.BAN, name: 'Ban Members', desc: 'Ban members beneath them, silencing them and cutting them off for good.' },
                { bit: BIT.MANAGE_MESSAGES, name: 'Manage Messages', desc: 'Hide messages from members beneath them.' },
                { bit: BIT.PIN_MESSAGES, name: 'Pin Messages', desc: 'Pin messages to a channel for everyone to see.' },
            ],
        },
        {
            title: 'Chat',
            perms: [
                { bit: BIT.MENTION_EVERYONE, name: 'Mention @everyone', desc: 'Notify everyone in the community at once.' },
            ],
        },
    ];

    // Discord's palette: 0 is "no colour", the theme's own.
    const COLORS = [0x1abc9c, 0x2ecc71, 0x3498db, 0x9b59b6, 0xe91e63, 0xf1c40f, 0xe67e22, 0xe74c3c, 0x95a5a6, 0x607d8b,
        0x11806a, 0x1f8b4c, 0x206694, 0x71368a, 0xad1457, 0xc27c0e, 0xa84300, 0x992d22, 0x979c9f, 0x546e7a];
    const hex = (c) => `#${c.toString(16).padStart(6, '0')}`;
    const tint = (c) => (c ? hex(c) : '#99aab5');

    // ── the list ──
    const channelName = (id) => view?.channels.find((c) => c.channel_id === id)?.name || 'a private channel';
    const byId = $derived(new Map((view?.roles || []).map((r) => [r.role_id, r])));
    // The list as the draft orders it, or as published.
    const rows = $derived.by(() => {
        if (!view) return [];
        if (!st.roles.order) return view.roles;
        return st.roles.order.map((id) => byId.get(id)).filter(Boolean);
    });

    function openRole(role) {
        csEditRole(role);
    }
    function newRole() {
        csEditRole({ role_id: null, name: 'new role', color: 0, permissions: '0', channel_id: null });
    }

    // ── drag to reorder ── (the emoji pack rail's gesture and look)
    // Only a role beneath you moves, and only among the others beneath you.
    const rowEls = new Map();
    function rowEl(node, id) { rowEls.set(id, node); return { destroy() { rowEls.delete(id); } }; }
    let dragging = $state(null);
    let drop = $state(null);   // { key, before }
    let ghost = null;
    const targets = () => rows.filter((r) => r.manageable).map((r) => ({ key: r.role_id, el: rowEls.get(r.role_id) })).filter((t) => t.el);
    const resolve = (y) => nearestByCentre(targets(), 0, y, 'y');

    function gestures(role) {
        return {
            onMenu: () => {},
            onDragStart: (ev, node) => { dragging = role.role_id; ghost = dragGhost(node, 'cs-role-ghost'); },
            onDragMove: (mv) => { ghost?.move(mv.clientX, mv.clientY); drop = resolve(mv.clientY); },
            onDragEnd: (up) => {
                const t = resolve(up.clientY);
                rowEls.get(role.role_id)?.setAttribute('data-suppress-click', '1');
                ghost?.remove(); ghost = null;
                dragging = null; drop = null;
                if (!t || t.key === role.role_id) return;
                const ids = rows.map((r) => r.role_id).filter((id) => id !== role.role_id);
                let at = ids.indexOf(t.key);
                if (!t.before) at += 1;
                ids.splice(at, 0, role.role_id);
                const published = view.roles.map((r) => r.role_id);
                csSetRoleOrder(ids.join() === published.join() ? null : ids);
            },
            ignore: () => !role.manageable,
        };
    }
    function rowClick(e, role) {
        if (e.currentTarget.dataset.suppressClick === '1') { delete e.currentTarget.dataset.suppressClick; return; }
        openRole(role);
    }

    // ── the editor ──
    const current = $derived(edit?.id ? byId.get(edit.id) : null);
    // A role you can't manage opens read-only: you may look, not touch.
    const editable = $derived(!edit ? false : edit.id === null ? !!view?.can_manage : !!current?.manageable);
    const perms = $derived(edit ? BigInt(edit.permissions) : 0n);
    const basePerms = $derived(edit ? BigInt(edit.base.permissions) : 0n);
    const grantable = $derived(view ? BigInt(view.grantable) : 0n);
    function has(bit) { return (perms & bit) === bit; }
    // A bit you lack can't be added, but one the role already had may be kept or removed.
    function canToggle(bit) { return editable && (has(bit) || (grantable & bit) === bit || (basePerms & bit) === bit); }
    function toggle(bit) {
        if (!canToggle(bit)) return;
        csPatchRoleEdit({ permissions: String(has(bit) ? perms & ~bit : perms | bit) });
    }
    const nameBytes = $derived(new TextEncoder().encode(edit?.name || '').length);

    // ── members ──
    let memberQuery = $state('');
    let adding = $state(false);
    let pick = $state(new Set());
    const memberRow = (npub) => {
        profileVersion(npub);
        const name = h.name(npub);
        return { npub, name, profile: h.profile(npub), src: h.avatarSrc(npub) };
    };
    const shortNpub = (npub) => `${npub.slice(0, 12)}…${npub.slice(-6)}`;
    const ownerName = $derived.by(() => {
        const o = view?.owner;
        if (!o) return '';
        profileVersion(o);
        return h.name(o) || shortNpub(o);
    });
    const matches = (r, q) => !q || `${r.name} ${r.npub}`.toLowerCase().includes(q);
    const holders = $derived.by(() => {
        const q = memberQuery.trim().toLowerCase();
        return (current?.holders || []).map(memberRow).filter((r) => matches(r, q));
    });
    const candidates = $derived.by(() => {
        const q = memberQuery.trim().toLowerCase();
        const held = new Set(current?.holders || []);
        return st.roles.members.filter((n) => !held.has(n)).map(memberRow).filter((r) => matches(r, q));
    });
    function togglePick(npub) {
        const next = new Set(pick);
        if (next.has(npub)) next.delete(npub); else next.add(npub);
        pick = next;
    }
    async function addPicked() {
        const add = [...pick];
        await h.setRoleHolders(edit.id, add, []);
        pick = new Set();
        adding = false;
    }
    $effect(() => { edit?.id; adding = false; pick = new Set(); memberQuery = ''; });
</script>

{#if !view}
    <section class="cs-block" data-anchor="role-list">
        <h3 class="cs-heading">Roles</h3>
        <p class="cs-hint">Loading...</p>
    </section>
{:else if !edit}
    <section class="cs-block" data-anchor="role-list">
        <div class="cs-heading-row">
            <h3 class="cs-heading">Roles</h3>
            {#if view.can_manage && view.roles.length < view.max_roles}
                <button class="cs-btn-save cs-role-create" onclick={newRole}>Create Role</button>
            {/if}
        </div>
        <p class="cs-lede">Members get the permissions of every role they hold. A role can manage only the roles beneath it, so the order matters: drag to arrange.</p>
        <div class="cs-roles">
            <!-- The owner as the list's fixed top: not a Role (none may claim position 0),
                 so it is drawn here, costs no slot, and nothing can pick, move or grant it. -->
            {#if view.owner}
                <div class="cs-role cs-role-owner">
                    <span class="cs-role-grip"><span class="icon icon-locked"></span></span>
                    <span class="cs-role-crown"><span class="icon icon-crown"></span></span>
                    <span class="cs-role-name cutoff">Owner</span>
                    <span class="cs-role-count cutoff">{ownerName}</span>
                    <span class="cs-role-open"></span>
                </div>
            {/if}
            {#each rows as role (role.role_id)}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="cs-role" class:locked={!role.manageable} class:is-dragging={dragging === role.role_id}
                     class:drop-above={drop?.key === role.role_id && drop.before} class:drop-below={drop?.key === role.role_id && !drop.before}
                     use:rowEl={role.role_id} use:reorderable={gestures(role)} onclick={(e) => rowClick(e, role)}>
                    <span class="cs-role-grip">{#if role.manageable}<span class="icon icon-grip"></span>{:else}<span class="icon icon-locked"></span>{/if}</span>
                    <span class="cs-role-dot" style:background-color={tint(role.color)}></span>
                    <span class="cs-role-name cutoff">{role.name || 'Unnamed role'}</span>
                    {#if role.channel_id}<span class="cs-role-scope cutoff">#{channelName(role.channel_id)}</span>{/if}
                    <span class="cs-role-count">{role.holders.length} {role.holders.length === 1 ? 'member' : 'members'}</span>
                    <span class="cs-role-open"><span class="icon icon-chevron-right"></span></span>
                </div>
            {:else}
                <p class="cmt-empty" style="text-align:center;">No roles yet. Create one to start giving members permissions.</p>
            {/each}
        </div>
        <p class="cs-hint">{view.roles.length} of {view.max_roles} roles</p>
    </section>
{:else}
    <button class="cs-back" onclick={() => csEditRole(null)}><span class="icon icon-chevron-left"></span>All Roles</button>

    <section class="cs-block" data-anchor="role-display">
        <h3 class="cs-heading">{edit.id === null ? 'New Role' : 'Display'}</h3>
        {#if !editable}<p class="cs-lede">This role sits at or above your own, so you can view it but not change it.</p>{/if}
        <div class="cs-field">
            <div class="cs-field-head">
                <label class="cs-label" for="cs-role-name">Role Name</label>
                <span class="cs-count" class:warn={nameBytes > 64 || !edit.name.trim()}>{nameBytes}/64</span>
            </div>
            <input id="cs-role-name" class="cs-input" type="text" disabled={!editable}
                   value={edit.name} oninput={(e) => csPatchRoleEdit({ name: e.currentTarget.value })}>
        </div>
        <div class="cs-field cs-field-gap">
            <span class="cs-label">Role Colour</span>
            <div class="cs-swatches">
                <button class="cs-swatch cs-swatch-default" class:on={edit.color === 0} disabled={!editable}
                        title="Default" aria-label="Default colour" onclick={() => csPatchRoleEdit({ color: 0 })}></button>
                <label class="cs-swatch cs-swatch-custom" class:on={edit.color !== 0 && !COLORS.includes(edit.color)}
                       style:background-color={edit.color !== 0 && !COLORS.includes(edit.color) ? hex(edit.color) : null} title="Custom colour">
                    <span class="icon icon-palette"></span>
                    <input type="color" disabled={!editable} value={hex(edit.color || 0x99aab5)}
                           oninput={(e) => csPatchRoleEdit({ color: parseInt(e.currentTarget.value.slice(1), 16) || 0 })}>
                </label>
                {#each COLORS as c (c)}
                    <button class="cs-swatch" class:on={edit.color === c} style:background-color={hex(c)} disabled={!editable}
                            aria-label={hex(c)} onclick={() => csPatchRoleEdit({ color: c })}></button>
                {/each}
            </div>
            <div class="cs-role-preview">
                <span class="cs-role-dot" style:background-color={tint(edit.color)}></span>
                <span style:color={edit.color ? hex(edit.color) : null}>{edit.name.trim() || 'new role'}</span>
            </div>
        </div>
        <div class="cs-field cs-field-gap">
            <span class="cs-label">Access</span>
            {#if edit.id === null}
                <select class="cs-input cs-select" value={edit.channel_id || ''} onchange={(e) => csPatchRoleEdit({ channel_id: e.currentTarget.value || null })}>
                    <option value="">Every channel</option>
                    {#each view.channels.filter((c) => c.private) as c (c.channel_id)}
                        <option value={c.channel_id}>#{c.name} only</option>
                    {/each}
                </select>
                <p class="cs-hint">A role scoped to a private channel is that channel's access list: holding it is what lets someone read there. This can't be changed once the role exists.</p>
            {:else}
                <p class="cs-hint cs-hint-tight">{edit.channel_id ? `#${channelName(edit.channel_id)} only. Holding this role is what lets someone read that channel.` : 'Every channel.'}</p>
            {/if}
        </div>
    </section>

    <section class="cs-block" data-anchor="role-permissions">
        <h3 class="cs-heading">Permissions</h3>
        <p class="cs-lede">You can only turn on permissions you hold yourself.</p>
        {#each GROUPS as group (group.title)}
            <p class="cs-perm-group">{group.title}</p>
            {#each group.perms as p (p.name)}
                <label class="toggle-container cs-perm" class:disabled={!canToggle(p.bit)}>
                    <span class="cs-perm-text">
                        <span class="cs-perm-name">{p.name}</span>
                        <span class="cs-perm-desc">{p.desc}</span>
                    </span>
                    <input type="checkbox" checked={has(p.bit)} disabled={!canToggle(p.bit)} onchange={() => toggle(p.bit)}>
                    <span class="neon-toggle"></span>
                </label>
            {/each}
        {/each}
    </section>

    {#if edit.id !== null}
        <section class="cs-block" data-anchor="role-members">
            <div class="cs-heading-row">
                <h3 class="cs-heading">Members</h3>
                {#if editable && !adding}
                    <button class="cs-btn-save cs-role-create" onclick={() => { adding = true; memberQuery = ''; }}>Add Members</button>
                {/if}
            </div>
            <div class="emoji-search-container cs-bans-search">
                <span class="emoji-search-icon icon icon-search"></span>
                <input type="text" placeholder="Search" autocomplete="off" spellcheck="false"
                       value={memberQuery} oninput={(e) => { memberQuery = e.currentTarget.value; }}>
            </div>
            <div class="cs-bans" class:busy={st.roles.busy}>
                {#if adding}
                    {#each candidates as row (row.npub)}
                        <MemberRow npub={row.npub} profile={row.profile} src={row.src}
                                   display={row.name || shortNpub(row.npub)} hasName={!!row.name}
                                   keyHint={row.name ? shortNpub(row.npub) : ''}
                                   onactivate={() => togglePick(row.npub)} ui={h.ui}>
                            {#snippet trailing()}<div class="member-pick-indicator" class:selected={pick.has(row.npub)}></div>{/snippet}
                        </MemberRow>
                    {:else}
                        <p class="cmt-empty" style="text-align:center;">{memberQuery.trim() ? 'No matches.' : 'Everyone already holds this role.'}</p>
                    {/each}
                    <div class="cs-role-add-bar">
                        <button class="cs-btn-text" onclick={() => { adding = false; pick = new Set(); }}>Cancel</button>
                        <button class="cs-btn-save" disabled={!pick.size || st.roles.busy} onclick={addPicked}>
                            {st.roles.busy ? 'Adding...' : `Add ${pick.size || ''}`}
                        </button>
                    </div>
                {:else}
                    {#each holders as row (row.npub)}
                        <MemberRow npub={row.npub} profile={row.profile} src={row.src}
                                   display={row.name || shortNpub(row.npub)} hasName={!!row.name}
                                   keyHint={row.name ? shortNpub(row.npub) : ''} ui={h.ui}>
                            {#snippet trailing()}
                                {#if editable}
                                    <button class="cs-role-remove" title="Remove from role" disabled={st.roles.busy}
                                            onclick={(e) => { e.stopPropagation(); h.setRoleHolders(edit.id, [], [row.npub]); }}>
                                        <span class="icon icon-x"></span>
                                    </button>
                                {/if}
                            {/snippet}
                        </MemberRow>
                    {:else}
                        <p class="cmt-empty" style="text-align:center;">{memberQuery.trim() ? 'No matches.' : 'Nobody holds this role yet.'}</p>
                    {/each}
                {/if}
            </div>
        </section>
    {/if}
{/if}
