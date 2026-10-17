<script>
    // A channel's name, topic and view (the view only tells a client how to open it), and on
    // the new roles its permissions: one overwrite per role or member, each bit allowed,
    // denied, or left to what they hold server-wide.
    import {
        channelEditorState, closeChannelEditor, setChannelEditorTab, selectOverwrite, setOverwriteBit,
        overwriteDirty, overwriteKey, storedOverwrite,
    } from '../lib/channel-editor.svelte.js';

    // h: save({ communityId, channelId, name, topic, view }), saveOverwrite({ communityId, channelId, target, allow, deny }),
    //    setSync({ communityId, channelId, categoryId })
    let { h } = $props();
    const st = channelEditorState();
    const BIT = (n) => 1n << BigInt(n);
    const PERM_GROUPS = [
        { title: 'Channel', perms: [
            { bit: BIT(13), name: 'View Channel', desc: 'See the channel and read it. A private channel\'s key follows this.' },
            { bit: BIT(14), name: 'Send Messages' },
            { bit: BIT(15), name: 'Send in Threads' },
            { bit: BIT(16), name: 'Attach Files' },
            { bit: BIT(17), name: 'Add Reactions' },
            { bit: BIT(18), name: 'Read Message History' },
            { bit: BIT(5), name: 'Manage Messages', desc: 'Hide messages from members beneath them here.' },
        ] },
        { title: 'Voice', perms: [
            { bit: BIT(19), name: 'Connect' },
            { bit: BIT(20), name: 'Speak' },
            { bit: BIT(21), name: 'Video' },
            { bit: BIT(22), name: 'Priority Speaker' },
            { bit: BIT(23), name: 'Mute Members' },
            { bit: BIT(24), name: 'Deafen Members' },
            { bit: BIT(25), name: 'Move Members' },
            { bit: BIT(26), name: 'Timeout Members' },
        ] },
    ];
    const hex = (c) => `#${(c || 0x99aab5).toString(16).padStart(6, '0')}`;
    const permsTab = $derived(!!st.perms?.activated && !!st.perms?.can_manage);
    // The targets with an overwrite here, @everyone first, plus whichever is open.
    const targets = $derived.by(() => {
        if (!st.perms) return [];
        const roleOf = (id) => st.perms.roles.find((r) => r.role_id === id);
        const keys = new Set(['everyone', ...st.perms.overwrites.map(overwriteKey), st.target]);
        return [...keys].map((key) => {
            if (key === 'everyone') return { key, label: '@everyone', color: null, rank: Infinity };
            const [kind, id] = key.split(':');
            if (kind === 'role') { const r = roleOf(id); return { key, label: r?.name || 'Deleted role', color: r?.color, rank: r?.position ?? 1e9 }; }
            return { key, label: st.perms.member_names?.[id] || `${id.slice(0, 10)}…`, color: null, rank: 2e9, member: true };
        }).sort((a, b) => (a.key === 'everyone') - (b.key === 'everyone') || a.rank - b.rank);
    });
    const addable = $derived((st.perms?.roles || []).filter((r) => !targets.some((t) => t.key === `role:${r.role_id}`)));
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
    function saveOverwrite() {
        if (!overwriteDirty() || st.busy) return;
        const [kind, id] = st.target === 'everyone' ? ['everyone', null] : st.target.split(':');
        h.saveOverwrite({ communityId: st.communityId, channelId: st.channelId, kind, id, allow: st.draft.allow, deny: st.draft.deny });
    }
    const VIEWS = [
        { id: 'chat', name: 'Text', desc: 'Messages first.' },
        { id: 'forum', name: 'Forum', desc: 'Opens on its threads.' },
        { id: 'voice', name: 'Voice', desc: 'Opens its call, with the chat beside it.' },
    ];
    const enc = new TextEncoder();
    const nameBytes = $derived(enc.encode(st.name).length);
    const topicBytes = $derived(enc.encode(st.topic).length);
    const valid = $derived(!!st.name.trim() && nameBytes <= 64 && topicBytes <= 1024);
    function save() {
        if (!valid || st.busy) return;
        h.save({ communityId: st.communityId, channelId: st.channelId, name: st.name.trim(), topic: st.topic.trim() || null, view: st.view });
    }
</script>

{#if st.open}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="modal-overlay" onclick={(e) => { if (e.target === e.currentTarget && !st.busy) closeChannelEditor(); }}>
        <div class="modal-box cmt-modal ce-modal" class:cmt-busy={st.busy}>
            <div class="cmt-header">
                <div class="cmt-header-icon"><span class="icon {st.isCategory ? 'icon-folder' : 'icon-channel-hash'}"></span></div>
                <div class="cmt-header-text">
                    <h3 class="cmt-title">{st.isCategory ? `${st.name} Permissions` : 'Edit Channel'}</h3>
                    <p class="cmt-subtitle">{st.isCategory ? 'Channels synced to this category follow these rules.' : 'How it shows and how it opens.'}</p>
                </div>
                <button class="relay-dialog-close cmt-close-x" disabled={st.busy} onclick={closeChannelEditor}>&times;</button>
            </div>
            <div class="cmt-body">
                {#if permsTab && !st.isCategory}
                    <div class="ce-tabs">
                        <button class="ce-tab" class:on={st.tab === 'overview'} onclick={() => setChannelEditorTab('overview')}>Overview</button>
                        <button class="ce-tab" class:on={st.tab === 'permissions'} onclick={() => setChannelEditorTab('permissions')}>Permissions</button>
                    </div>
                {/if}
                {#if st.isCategory && st.perms && !permsTab}
                    <p class="cs-lede">Category permissions are part of the new roles, and need Manage Roles.</p>
                {:else if st.tab === 'permissions' && permsTab && st.perms.synced}
                    <div class="ce-sync">
                        <p class="cs-lede">This channel follows its category's permissions. Edit them from the category, or stop syncing to set its own.</p>
                        <button class="cs-btn-text" disabled={st.busy} onclick={() => h.setSync({ communityId: st.communityId, channelId: st.channelId, categoryId: null })}>Stop syncing</button>
                    </div>
                {:else if st.tab === 'permissions' && permsTab}
                    {#if !st.isCategory && st.perms.category}
                        <div class="ce-sync">
                            <p class="cs-hint">This channel sets its own permissions.</p>
                            <button class="cs-btn-text" disabled={st.busy} onclick={() => h.setSync({ communityId: st.communityId, channelId: st.channelId, categoryId: st.perms.category })}>Sync with its category</button>
                        </div>
                    {/if}
                    <div class="ce-perms">
                        <div class="ce-targets">
                            {#each targets as t (t.key)}
                                <button class="ce-target" class:on={st.target === t.key} disabled={st.busy} onclick={() => selectOverwrite(t.key)}>
                                    {#if !t.member}<span class="cs-role-dot" style:background-color={hex(t.color)}></span>{/if}
                                    <span class="cutoff">{t.label}</span>
                                </button>
                            {/each}
                            {#if addable.length}
                                <select class="cs-input cs-select ce-add" value="" disabled={st.busy}
                                        onchange={(e) => { if (e.currentTarget.value) selectOverwrite(`role:${e.currentTarget.value}`); e.currentTarget.value = ''; }}>
                                    <option value="">Add a role…</option>
                                    {#each addable as r (r.role_id)}<option value={r.role_id}>{r.name}</option>{/each}
                                </select>
                            {/if}
                        </div>
                        <div class="ce-bits">
                            {#each PERM_GROUPS as g (g.title)}
                                <p class="cs-perm-group">{g.title}</p>
                                {#each g.perms as p (p.name)}
                                    {@const state = bitState(p.bit)}
                                    <div class="ce-bit" class:disabled={!canTouch(p.bit)}>
                                        <span class="cs-perm-text">
                                            <span class="cs-perm-name">{p.name}</span>
                                            {#if p.desc}<span class="cs-perm-desc">{p.desc}</span>{/if}
                                        </span>
                                        <div class="ce-tri">
                                            <button class="ce-tri-deny" class:on={state === 'deny'} title="Deny" disabled={!canTouch(p.bit) || st.busy} onclick={() => setOverwriteBit(p.bit, 'deny')}><span class="icon icon-x"></span></button>
                                            <button class="ce-tri-inherit" class:on={state === 'inherit'} title="Inherit" disabled={!canTouch(p.bit) || st.busy} onclick={() => setOverwriteBit(p.bit, 'inherit')}>/</button>
                                            <button class="ce-tri-allow" class:on={state === 'allow'} title="Allow" disabled={!canTouch(p.bit) || st.busy} onclick={() => setOverwriteBit(p.bit, 'allow')}><span class="icon icon-check"></span></button>
                                        </div>
                                    </div>
                                {/each}
                            {/each}
                        </div>
                    </div>
                    <div class="cmt-footer">
                        <div class="cmt-status" class:cmt-err={!!st.error}>{st.error}</div>
                        <button class="cs-btn-text" disabled={st.busy || !overwriteDirty()} onclick={() => selectOverwrite(st.target)}>Reset</button>
                        <button class="cs-btn-save" disabled={!overwriteDirty() || st.busy} onclick={saveOverwrite}>{st.busy ? 'Saving...' : 'Save'}</button>
                    </div>
                {:else}
                <div class="cs-field">
                    <div class="cs-field-head">
                        <label class="cs-label" for="ce-name">Channel Name</label>
                        <span class="cs-count" class:warn={nameBytes > 64 || !st.name.trim()}>{nameBytes}/64</span>
                    </div>
                    <input id="ce-name" class="cs-input" type="text" disabled={st.busy} bind:value={st.name}
                           onkeydown={(e) => { if (e.key === 'Enter') save(); }}>
                </div>
                <div class="cs-field cs-field-gap">
                    <div class="cs-field-head">
                        <label class="cs-label" for="ce-topic">Topic</label>
                        <span class="cs-count" class:warn={topicBytes > 1024}>{topicBytes}/1024</span>
                    </div>
                    <textarea id="ce-topic" class="cs-input cs-textarea" rows="3" disabled={st.busy} bind:value={st.topic}
                              placeholder="What this channel is for"></textarea>
                </div>
                <div class="cs-field cs-field-gap">
                    <span class="cs-label">Opens As</span>
                    <div class="ce-views">
                        {#each VIEWS as v (v.id)}
                            <button class="ce-view" class:on={st.view === v.id} disabled={st.busy} onclick={() => { st.view = v.id; }}>
                                <span class="cs-perm-name">{v.name}</span>
                                <span class="cs-perm-desc">{v.desc}</span>
                            </button>
                        {/each}
                    </div>
                </div>
                <div class="cmt-footer">
                    <div class="cmt-status" class:cmt-err={!!st.error}>{st.error}</div>
                    <button class="cs-btn-text" disabled={st.busy} onclick={closeChannelEditor}>Cancel</button>
                    <button class="cs-btn-save" disabled={!valid || st.busy} onclick={save}>{st.busy ? 'Saving...' : 'Save'}</button>
                </div>
                {/if}
            </div>
        </div>
    </div>
{/if}
