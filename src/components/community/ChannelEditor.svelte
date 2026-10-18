<script>
    // Channel Settings, in Community Settings' layout: sections on the left with their anchors,
    // headed blocks in one scroll, edits behind one save bar. Overview is the name, topic and
    // view (the view only tells a client how to open it); Permissions, on the new roles, is one
    // overwrite per role or member, each bit allowed, denied, or left to what they hold
    // server-wide. A Category has Permissions alone.
    import { tick } from 'svelte';
    import {
        ceOverlay, channelEditorState, setChannelEditorSection, setChannelEditorQuery, selectOverwrite, setOverwriteBit,
        overwriteKey, storedOverwrite, channelEditorDirty,
    } from '../lib/channel-editor.svelte.js';
    import { popIn } from '../lib/popin.js';
    import { anchorScroll } from '../lib/anchorscroll.svelte.js';
    import SectionNav from '../settings/SectionNav.svelte';

    // h: close(), save(), reset(), setSync({ communityId, channelId, categoryId })
    let { h } = $props();
    const ov = ceOverlay.state();
    const st = channelEditorState();
    const dirty = $derived(channelEditorDirty());

    const BIT = (n) => 1n << BigInt(n);
    const PERM_GROUPS = [
        { id: 'perm-channel', title: 'Channel', keys: 'view send threads attach files reactions history manage messages hide', perms: [
            { bit: BIT(13), name: 'View Channel', desc: 'See the channel and read it. A private channel\'s key follows this.' },
            { bit: BIT(14), name: 'Send Messages', desc: 'Post here. Without it for @everyone, only those you allow can post.' },
            { bit: BIT(15), name: 'Send in Threads', desc: 'Reply inside a thread.' },
            { bit: BIT(16), name: 'Attach Files', desc: 'Share images, videos and files.' },
            { bit: BIT(17), name: 'Add Reactions', desc: 'React to messages.' },
            { bit: BIT(18), name: 'Read Message History', desc: 'Read what was said before they could see the channel.' },
            { bit: BIT(5), name: 'Manage Messages', desc: 'Hide messages from members beneath them here.' },
        ] },
        { id: 'perm-voice', title: 'Voice', keys: 'connect speak video priority mute deafen move timeout', perms: [
            { bit: BIT(19), name: 'Connect', desc: 'Join the channel\'s call.' },
            { bit: BIT(20), name: 'Speak', desc: 'Be heard in the call.' },
            { bit: BIT(21), name: 'Video', desc: 'Share a camera or a screen.' },
            { bit: BIT(22), name: 'Priority Speaker', desc: 'Lower others while speaking.' },
            { bit: BIT(23), name: 'Mute Members', desc: 'Mute others in the call.' },
            { bit: BIT(24), name: 'Deafen Members', desc: 'Stop others hearing the call.' },
            { bit: BIT(25), name: 'Move Members', desc: 'Move others between calls.' },
            { bit: BIT(26), name: 'Timeout Members', desc: 'Time out members beneath them here.' },
        ] },
    ];
    const VIEWS = [
        { id: 'chat', name: 'Text', desc: 'Messages first.' },
        { id: 'forum', name: 'Forum', desc: 'Opens on its threads.' },
        { id: 'voice', name: 'Voice', desc: 'Opens its call, with the chat beside it.' },
    ];

    const hex = (c) => `#${(c || 0x99aab5).toString(16).padStart(6, '0')}`;
    const permsOpen = $derived(!!st.perms?.activated && !!st.perms?.can_manage);
    const syncable = $derived(permsOpen && !st.isCategory && (!!st.perms?.synced || !!st.perms?.category));

    const SECTIONS = $derived([
        ...(st.isCategory ? [] : [{
            id: 'overview',
            label: 'Overview',
            icon: 'info',
            anchors: [
                { id: 'ch-name', label: 'Name', icon: 'edit', keys: 'name rename title' },
                { id: 'ch-topic', label: 'Topic', icon: 'align-left', keys: 'topic description about' },
                { id: 'ch-view', label: 'Opens As', icon: 'channel-hash', keys: 'view text forum voice opens' },
            ],
        }]),
        {
            id: 'permissions',
            label: 'Permissions',
            icon: 'shield-filled',
            anchors: [
                ...(syncable ? [{ id: 'ch-sync', label: 'Category Sync', icon: 'folder', keys: 'sync category inherit' }] : []),
                { id: 'ch-targets', label: 'Roles & Members', icon: 'add-user', keys: 'roles members everyone overwrite who' },
                ...(permsOpen && !st.perms?.synced ? PERM_GROUPS.map((g) => ({ id: g.id, label: g.title, icon: 'shield-filled', keys: g.keys })) : []),
            ],
        },
    ]);
    const section = $derived(SECTIONS.find((x) => x.id === st.section) || SECTIONS[0]);
    const anchors = anchorScroll(st.isCategory ? 'ch-targets' : 'ch-name');

    async function goSection(id) {
        if (st.section === id) return;
        setChannelEditorSection(id);
        await tick();
        anchors.top(SECTIONS.find((x) => x.id === id)?.anchors[0]?.id);
    }
    async function goAnchor(sectionId, anchorId) {
        if (st.section !== sectionId) {
            setChannelEditorSection(sectionId);
            await tick();
        }
        anchors.jump(anchorId);
    }
    $effect(() => {
        ov.tick;
        anchors.top(st.isCategory ? 'ch-targets' : 'ch-name');
    });

    // A refused close, or a switch of target with bits unsaved, shakes the bar and says why.
    let warn = $state(false);
    let warnTimer = null;
    $effect(() => {
        if (!st.nudge) return;
        warn = false;
        requestAnimationFrame(() => { warn = true; });
        clearTimeout(warnTimer);
        warnTimer = setTimeout(() => { warn = false; }, 1600);
    });

    // The targets with an overwrite here, @everyone first, plus whichever is open.
    const targets = $derived.by(() => {
        if (!st.perms) return [];
        const roleOf = (id) => st.perms.roles.find((r) => r.role_id === id);
        const keys = new Set(['everyone', ...st.perms.overwrites.map(overwriteKey), st.target]);
        return [...keys].map((key) => {
            if (key === 'everyone') return { key, label: '@everyone', color: null, rank: Infinity, set: true };
            const [kind, id] = key.split(':');
            const stored = storedOverwrite(key);
            const set = stored.allow !== '0' || stored.deny !== '0';
            if (kind === 'role') { const r = roleOf(id); return { key, label: r?.name || 'Deleted role', color: r?.color, rank: r?.position ?? 1e9, set }; }
            return { key, label: st.perms.member_names?.[id] || `${id.slice(0, 10)}…`, color: null, rank: 2e9, member: true, set };
        }).sort((a, b) => (a.key === 'everyone') - (b.key === 'everyone') || a.rank - b.rank);
    });
    const addable = $derived((st.perms?.roles || []).filter((r) => !targets.some((t) => t.key === `role:${r.role_id}`)));
    const addableMembers = $derived((st.perms?.members || [])
        .filter((m) => !targets.some((t) => t.key === `member:${m.id}`))
        .map((m) => ({ id: m.id, name: st.perms.member_names?.[m.id] || `${m.id.slice(0, 10)}…` }))
        .sort((a, b) => a.name.localeCompare(b.name)));
    const open = $derived(targets.find((t) => t.key === st.target));
    // A private channel opens only to a role's or a member's allow, whatever @everyone says.
    const closedToEveryone = (bit) => st.private && st.target === 'everyone' && bit === BIT(13);
    const bitState = (bit) => {
        if (!st.draft) return 'inherit';
        if (BigInt(st.draft.allow) & bit) return 'allow';
        if (BigInt(st.draft.deny) & bit) return 'deny';
        return 'inherit';
    };
    const grantable = $derived(st.perms ? BigInt(st.perms.grantable) : 0n);
    // A bit you lack can't be set, but one already set may be cleared.
    function canTouch(bit) {
        const stored = storedOverwrite(st.target);
        return (grantable & bit) === bit || ((BigInt(stored.allow) | BigInt(stored.deny)) & bit) === bit;
    }

    const enc = new TextEncoder();
    const nameBytes = $derived(enc.encode(st.name).length);
    const topicBytes = $derived(enc.encode(st.topic).length);
    const valid = $derived(st.isCategory || (!!st.name.trim() && nameBytes <= 64 && topicBytes <= 1024));
    const bar = $derived(dirty || st.busy || !!st.error);
</script>

<svelte:window onkeydown={(e) => { if (ov.active && e.key === 'Escape') { e.preventDefault(); h.close(); } }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="mod-overlay cs-overlay" class:active={ov.active} class:closing={ov.closing}
     onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
    <div class="cs-card" use:popIn={ov.tick}>
        <SectionNav sections={SECTIONS} section={section.id} anchor={anchors.active} query={st.query}
                    onquery={setChannelEditorQuery} ongo={goSection} onanchor={goAnchor}>
            {#snippet head()}
                <span class="cs-nav-glyph"><span class="icon {st.isCategory ? 'icon-folder' : st.private ? 'icon-locked' : 'icon-channel-hash'}"></span></span>
                <span class="cs-nav-title cutoff">{st.saved.name || (st.isCategory ? 'Category' : 'Channel')}</span>
            {/snippet}
        </SectionNav>

        <main class="cs-main">
            <header class="cs-top">
                <h2 class="cs-top-title">{section.label}</h2>
                <button class="cs-close" aria-label="Close" onclick={() => h.close()}>
                    <span class="cs-close-x">&#x2715;</span>
                    <span class="cs-close-key">ESC</span>
                </button>
            </header>

            <div class="cs-scroll" bind:this={anchors.el} onscroll={() => anchors.spy()}>
                {#key section.id}
                <div class="cs-content cs-content-enter" class:loading={st.busy}>
                    {#if section.id === 'overview'}
                        <section class="cs-block" data-anchor="ch-name">
                            <h3 class="cs-heading">Name</h3>
                            <div class="cs-field">
                                <div class="cs-field-head">
                                    <label class="cs-label" for="ce-name">Channel Name</label>
                                    <span class="cs-count" class:warn={nameBytes > 64 || !st.name.trim()}>{nameBytes}/64</span>
                                </div>
                                <input id="ce-name" class="cs-input" type="text" disabled={st.busy} bind:value={st.name}
                                       onkeydown={(e) => { if (e.key === 'Enter' && valid) h.save(); }}>
                            </div>
                        </section>
                        <section class="cs-block" data-anchor="ch-topic">
                            <h3 class="cs-heading">Topic</h3>
                            <div class="cs-field">
                                <div class="cs-field-head">
                                    <label class="cs-label" for="ce-topic">What this channel is for</label>
                                    <span class="cs-count" class:warn={topicBytes > 1024}>{topicBytes}/1024</span>
                                </div>
                                <textarea id="ce-topic" class="cs-input cs-textarea" rows="4" disabled={st.busy} bind:value={st.topic}
                                          placeholder="Shown at the top of the channel"></textarea>
                            </div>
                        </section>
                        <section class="cs-block" data-anchor="ch-view">
                            <h3 class="cs-heading">Opens As</h3>
                            <p class="cs-lede">How a member's client opens the channel. It changes nothing about who can do what.</p>
                            <div class="ce-views">
                                {#each VIEWS as v (v.id)}
                                    <button class="ce-view" class:on={st.view === v.id} disabled={st.busy} onclick={() => { st.view = v.id; }}>
                                        <span class="cs-perm-name">{v.name}</span>
                                        <span class="cs-perm-desc">{v.desc}</span>
                                    </button>
                                {/each}
                            </div>
                        </section>
                    {:else if !st.perms}
                        <section class="cs-block" data-anchor="ch-targets">
                            <h3 class="cs-heading">Roles & Members</h3>
                            <p class="cs-hint">Loading...</p>
                        </section>
                    {:else if !permsOpen}
                        <section class="cs-block" data-anchor="ch-targets">
                            <h3 class="cs-heading">Roles & Members</h3>
                            <p class="cs-lede">{st.perms.activated ? 'Changing who can do what here needs Manage Roles.' : 'Channel permissions come with the new roles. Switch to them in Community Settings, under Roles.'}</p>
                        </section>
                    {:else}
                        {#if syncable}
                            <section class="cs-block" data-anchor="ch-sync">
                                <h3 class="cs-heading">Category Sync</h3>
                                <div class="ce-sync">
                                    {#if st.perms.synced}
                                        <p class="cs-lede">This channel follows its category's permissions. Edit them from the category, or stop syncing to set its own.</p>
                                        <button class="cs-btn-text" disabled={st.busy} onclick={() => h.setSync({ communityId: st.communityId, channelId: st.channelId, categoryId: null })}>Stop syncing</button>
                                    {:else}
                                        <p class="cs-lede">This channel sets its own permissions.</p>
                                        <button class="cs-btn-text" disabled={st.busy} onclick={() => h.setSync({ communityId: st.communityId, channelId: st.channelId, categoryId: st.perms.category })}>Sync with its category</button>
                                    {/if}
                                </div>
                            </section>
                        {/if}
                        {#if !st.perms.synced}
                            <section class="cs-block" data-anchor="ch-targets">
                                <h3 class="cs-heading">Roles & Members</h3>
                                <p class="cs-lede">{st.isCategory ? 'Channels synced to this category follow these rules.' : 'Pick who to set rules for. Anything left alone follows what they hold across the community.'}</p>
                                <div class="cs-roles">
                                    {#each targets as t (t.key)}
                                        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                                        <div class="cs-role ce-target-row" class:on={st.target === t.key} onclick={() => !st.busy && selectOverwrite(t.key)}>
                                            {#if t.member}<span class="cs-role-grip"><span class="icon icon-user"></span></span>
                                            {:else}<span class="cs-role-dot" style:background-color={hex(t.color)}></span>{/if}
                                            <span class="cs-role-name cutoff">{t.label}</span>
                                            {#if !t.set}<span class="cs-role-count">no rules yet</span>{/if}
                                            <span class="cs-role-open">{#if st.target === t.key}<span class="icon icon-check"></span>{/if}</span>
                                        </div>
                                    {/each}
                                </div>
                                {#if addable.length || addableMembers.length}
                                    <div class="ce-adds">
                                        {#if addable.length}
                                            <select class="cs-input cs-select" value="" disabled={st.busy}
                                                    onchange={(e) => { if (e.currentTarget.value) selectOverwrite(`role:${e.currentTarget.value}`); e.currentTarget.value = ''; }}>
                                                <option value="">Add a role…</option>
                                                {#each addable as r (r.role_id)}<option value={r.role_id}>{r.name}</option>{/each}
                                            </select>
                                        {/if}
                                        {#if addableMembers.length}
                                            <select class="cs-input cs-select" value="" disabled={st.busy}
                                                    onchange={(e) => { if (e.currentTarget.value) selectOverwrite(`member:${e.currentTarget.value}`); e.currentTarget.value = ''; }}>
                                                <option value="">Add a member…</option>
                                                {#each addableMembers as m (m.id)}<option value={m.id}>{m.name}</option>{/each}
                                            </select>
                                        {/if}
                                    </div>
                                {/if}
                            </section>
                            {#each PERM_GROUPS as g (g.id)}
                                <section class="cs-block" data-anchor={g.id}>
                                    <h3 class="cs-heading">{g.title}{#if open}<span class="ce-for">for {open.label}</span>{/if}</h3>
                                    {#each g.perms as p (p.name)}
                                        {@const state = bitState(p.bit)}
                                        {@const closed = closedToEveryone(p.bit)}
                                        <div class="ce-bit" class:disabled={!canTouch(p.bit) || closed}>
                                            <span class="cs-perm-text">
                                                <span class="cs-perm-name">{p.name}</span>
                                                <span class="cs-perm-desc">{closed ? 'A private channel opens only to the roles and members you allow.' : p.desc}</span>
                                            </span>
                                            <div class="ce-tri">
                                                <button class="ce-tri-deny" class:on={state === 'deny'} title="Deny" aria-label="Deny" disabled={!canTouch(p.bit) || closed || st.busy} onclick={() => setOverwriteBit(p.bit, 'deny')}><span class="icon icon-x"></span></button>
                                                <button class="ce-tri-inherit" class:on={state === 'inherit'} title="Inherit" aria-label="Inherit" disabled={!canTouch(p.bit) || closed || st.busy} onclick={() => setOverwriteBit(p.bit, 'inherit')}>/</button>
                                                <button class="ce-tri-allow" class:on={state === 'allow'} title="Allow" aria-label="Allow" disabled={!canTouch(p.bit) || closed || st.busy} onclick={() => setOverwriteBit(p.bit, 'allow')}><span class="icon icon-check"></span></button>
                                            </div>
                                        </div>
                                    {/each}
                                </section>
                            {/each}
                        {/if}
                    {/if}
                </div>
                {/key}
            </div>

            <div class="cs-savebar" class:visible={bar} class:warn={warn && dirty}>
                <span class="cs-savebar-text" class:cs-err={!!st.error && !warn}>
                    {#if st.busy}
                        Saving...
                    {:else if warn}
                        {st.nudged === 'switch' ? `Save or reset the rules for ${open?.label || 'this target'} first` : 'Save or reset your changes before closing'}
                    {:else if st.error}
                        {st.error}
                    {:else}
                        You have unsaved changes
                    {/if}
                </span>
                <div class="cs-savebar-actions">
                    <button class="cs-btn-text" disabled={st.busy || !dirty} onclick={() => h.reset()}>Reset</button>
                    <button class="cs-btn-save" disabled={st.busy || !dirty || !valid} onclick={() => h.save()}>Save Changes</button>
                </div>
            </div>
        </main>
    </div>
</div>
