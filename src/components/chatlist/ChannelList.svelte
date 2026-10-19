<script>
    // A community's channels: nested under its chat-list row as a flat disclosure, or as
    // the widescreen pane in its Categories (Public / Private sections where it has none).
    // A manager drags channels between and within them, and Categories among themselves.
    // Each row derives its own weight (muted, unread, pings) from its chat's signal, so a
    // message repaints one row.
    import { communityVersion, chatVersion, markedChatId } from '../lib/signals.svelte.js';
    import { reorderable, dragGhost, nearestByCentre } from '../lib/reorder.js';
    import { voicePresent, voiceLevel, voiceCallHandlers, voiceSession, voiceJoining, SPEAKING } from '../lib/voice.svelte.js';
    import Avatar from '../ui/Avatar.svelte';

    let { communityId, pane = false, h, onShown = () => {} } = $props();
    // h: getChannels, getCategories, canAddChannels, channelsShown, sectionClosed, toggleSection, chatById,
    //    computeRowUnreadCount, countPingMessages, isPrimaryChannelId, openChannel, openChannelMenu,
    //    openCategoryMenu, attachLongPressContextMenu, createChannel, deleteChannel, moveChannel, moveCategory

    const state = $derived.by(() => {
        communityVersion(communityId);
        const channels = h.getChannels(communityId);
        if (!channels) return null;
        const canManage = h.canAddChannels(communityId);
        // Nested, the list is optional: hidden when collapsed, and a lone channel is not
        // worth unfolding. As the pane it IS the navigation, so a single channel still shows.
        if (!pane && (!h.channelsShown(communityId) || (channels.length < 2 && !canManage))) return null;
        return { channels, canManage };
    });

    // The pane's owner dims its bottom fadeout when nothing rendered.
    $effect(() => { onShown(!!state); });

    const categories = $derived.by(() => { communityVersion(communityId); return h.getCategories(communityId); });

    // A section is { id, label, channels, canAdd, category }: a Category, the channels filed
    // under none (first, without a heading), or, in a community without Categories, the
    // Public and Private halves of its one list.
    const sections = $derived.by(() => {
        if (!state) return [];
        const { channels, canManage } = state;
        const out = [];
        if (!categories.length) {
            const open = channels.filter(c => !c.private);
            const shut = channels.filter(c => c.private);
            if (open.length) out.push({ id: 'public', label: 'Public', channels: open, canAdd: canManage, category: null });
            if (shut.length) out.push({ id: 'private', label: 'Private', channels: shut, canAdd: canManage, category: null });
            if (!out.length && canManage) out.push({ id: 'public', label: 'Public', channels: [], canAdd: true, category: null });
            return out;
        }
        const loose = channels.filter(c => !c.category);
        if (loose.length || (canManage && dragging?.kind === 'channel')) {
            out.push({ id: 'none', label: '', channels: loose, canAdd: false, category: null, headless: true });
        }
        for (const k of categories) {
            const inside = channels.filter(c => c.category === k.id);
            // A Category shows while it holds a channel the viewer can see, or to a manager.
            if (!inside.length && !canManage) continue;
            out.push({ id: k.id, label: k.name, channels: inside, canAdd: canManage, category: k });
        }
        return out;
    });

    // ── organising ── (the role list's gesture and look)
    const rowEls = new Map();
    const headEls = new Map();
    const sectionEls = new Map();
    function tracked(map) {
        return (node, id) => { map.set(id, node); return { destroy() { if (map.get(id) === node) map.delete(id); } }; };
    }
    const rowEl = tracked(rowEls);
    const headEl = tracked(headEls);
    const sectionEl = tracked(sectionEls);
    let dragging = $state(null);   // { kind: 'channel' | 'category', id }
    let drop = $state(null);       // { key, before }, key 'ch:<id>' or 'cat:<id>'
    let ghost = null;

    function targets() {
        if (!dragging) return [];
        // A Category moves past whole Categories, its channels included.
        if (dragging.kind === 'category') {
            return categories.filter((k) => k.id !== dragging.id).map((k) => ({ key: `cat:${k.id}`, el: sectionEls.get(k.id) })).filter((t) => t.el);
        }
        const heads = categories.map((k) => ({ key: `cat:${k.id}`, el: headEls.get(k.id) }));
        const rows = (state?.channels || []).filter((c) => c.id !== dragging.id).map((c) => ({ key: `ch:${c.id}`, el: rowEls.get(c.id) }));
        return [...rows, ...heads].filter((t) => t.el);
    }
    const resolve = (y) => nearestByCentre(targets(), 0, y, 'y');

    function land(t) {
        if (!t || !dragging) return;
        const [kind, key] = [t.key.slice(0, t.key.indexOf(':')), t.key.slice(t.key.indexOf(':') + 1)];
        if (dragging.kind === 'category') {
            const order = categories.map((k) => k.id).filter((id) => id !== dragging.id);
            const at = order.indexOf(key) + (t.before ? 0 : 1);
            if (categories.map((k) => k.id).join() !== [...order.slice(0, at), dragging.id, ...order.slice(at)].join()) {
                h.moveCategory(communityId, dragging.id, at);
            }
            return;
        }
        const channels = state.channels;
        const moved = channels.find((c) => c.id === dragging.id);
        // On a heading: the top of that Category.
        const category = kind === 'cat' ? key : (channels.find((c) => c.id === key)?.category || null);
        const group = channels.filter((c) => c.id !== dragging.id && (c.category || null) === category);
        const index = kind === 'cat' ? 0 : group.findIndex((c) => c.id === key) + (t.before ? 0 : 1);
        const was = channels.filter((c) => (c.category || null) === category);
        if ((moved.category || null) === category && was.indexOf(moved) === index) return;
        h.moveChannel(communityId, dragging.id, category, index);
    }

    function gestures(kind, id, onMenu) {
        return {
            onMenu,
            onDragStart: (ev, node) => { dragging = { kind, id }; ghost = dragGhost(node, 'chatlist-drag-ghost'); },
            onDragMove: (mv) => { ghost?.move(mv.clientX, mv.clientY); drop = resolve(mv.clientY); },
            onDragEnd: (up) => {
                const t = resolve(up.clientY);
                ghost?.remove(); ghost = null;
                land(t);
                dragging = null; drop = null;
            },
            ignore: () => !state?.canManage,
        };
    }
    // A manager's rows reorder; everyone else's keep the plain long-press menu. Each fold
    // hands the action a fresh channel or Category; the handlers follow it.
    function channelGesture(node, channel) {
        let cur = channel;
        const menu = (x, y) => h.openChannelMenu(communityId, cur, x, y);
        if (pane && state?.canManage) {
            const r = reorderable(node, gestures('channel', cur.id, menu));
            return { update(next) { cur = next; r.update(gestures('channel', next.id, menu)); }, destroy: r.destroy };
        }
        const r = h.attachLongPressContextMenu(node, menu);
        return { update(next) { cur = next; }, destroy: r?.destroy };
    }
    function categoryGesture(node, category) {
        const of = (k) => gestures('category', k.id, (x, y) => h.openCategoryMenu(communityId, k, x, y));
        const r = reorderable(node, of(category));
        return { update(next) { r.update(of(next)); }, destroy: r.destroy };
    }

    // Collapsed sections: seeded from the persisted set, flipped here so the fold is
    // immediate; the app persists the change.
    let closed = $state({});
    function isClosed(sectionId) {
        return closed[sectionId] ?? h.sectionClosed(communityId, sectionId);
    }
    function toggle(sectionId) {
        closed[sectionId] = !isClosed(sectionId);
        h.toggleSection(communityId, sectionId);
    }

    // A fresh object per version bump: the chat is mutated in place, so handing the chat
    // itself on would compare equal and leave the row stale.
    function rowState(id) {
        chatVersion(id);
        const chat = h.chatById(id);
        return {
            muted: !!chat?.muted,
            unread: !!chat && h.computeRowUnreadCount(chat) > 0,
            // A room set to Nothing is silent everywhere, including its own row.
            pings: chat && (chat.notify | 0) < 2 ? h.countPingMessages(chat) : 0,
        };
    }
</script>

{#snippet row(channel, canManage)}
    {@const r = rowState(channel.id)}
    <!-- The menu is the only way in: the row itself stays navigation. -->
    <!-- Three tiers, loudest first: something to read, nothing to read, and a room you asked
         to be quiet. Muted wins outright: a standing instruction, not a state unread overrides. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="chatlist-channel" id="chatlist-channel-{channel.id}"
         class:active={markedChatId() === channel.id}
         class:is-muted={r.muted}
         class:has-unread={r.unread}
         class:is-read={!r.unread}
         class:is-dragging={dragging?.id === channel.id}
         class:drop-above={drop?.key === `ch:${channel.id}` && drop.before}
         class:drop-below={drop?.key === `ch:${channel.id}` && !drop.before}
         title={channel.topic || null}
         use:rowEl={channel.id}
         use:channelGesture={channel}
         onclick={() => h.openChannel(communityId, channel)}>
        <!-- Grouped by Category, a private channel says so with its own glyph. -->
        <span class="chatlist-channel-hash"><span class="icon {channel.view === 'voice' ? 'icon-volume-max' : categories.length && channel.private ? 'icon-locked' : 'icon-channel-hash'}"></span></span>
        <span class="chatlist-channel-name cutoff">{channel.name}</span>
        <!-- A number only for someone calling your name; ordinary unread is the row's own weight. -->
        {#if r.pings}
            <span class="chatlist-channel-badge">{r.pings > 99 ? '99+' : r.pings}</span>
        {/if}
        <!-- The primary channel anchors the community's row and history; the backend refuses to tombstone it. -->
        {#if canManage && !h.isPrimaryChannelId(channel.id)}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="chatlist-channel-delete btn" title="Delete channel" onclick={(e) => { e.stopPropagation(); h.deleteChannel(communityId, channel); }}>
                <span class="icon icon-x"></span>
            </div>
        {/if}
    </div>
    <!-- Who is in a voice channel's call, whether or not we are. -->
    {#if channel.view === 'voice'}
        {@const live = voiceSession()?.channelId === channel.id}
        {#if voiceJoining() === channel.id}
            <div class="chatlist-voice-member is-connecting">Connecting…</div>
        {/if}
        {#each voicePresent(channel.id) as p (p.npub)}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="chatlist-voice-member" class:is-speaking={live && !p.muted && voiceLevel(p.npub, voiceCallHandlers().myNpub?.()) > SPEAKING}
                 onclick={(e) => { e.stopPropagation(); h.showMiniProfile(p.npub, e.currentTarget, communityId); }}
                 oncontextmenu={(e) => { e.preventDefault(); e.stopPropagation(); voiceCallHandlers().memberMenu?.(communityId, channel.id, p.npub, e.clientX, e.clientY); }}>
                <Avatar src={h.getProfileAvatarSrc(h.getProfile(p.npub))} size={18} />
                <span class="cutoff">{h.getName(p.npub)}</span>
                <span class="chatlist-voice-flags">
                    {#if p.screen}<span class="chatlist-voice-live" title="Sharing their screen">LIVE</span>{/if}
                    {#if p.camera}<span class="icon icon-video" title="Camera on"></span>{/if}
                    {#if p.muted}<span class="icon icon-mic-off" class:is-server={p.server_muted} title={p.server_muted ? 'Muted by a moderator' : 'Muted'}></span>{/if}
                    {#if p.deafened}<span class="icon icon-volume-mute" class:is-server={p.server_deafened} title={p.server_deafened ? 'Deafened by a moderator' : 'Deafened'}></span>{/if}
                </span>
            </div>
        {/each}
    {/if}
{/snippet}

{#if state}
    {#if !pane}
        <div class="chatlist-channels">
            {#each state.channels as channel (channel.id)}
                {@render row(channel, state.canManage)}
            {/each}
            {#if state.canManage}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="chatlist-channel chatlist-channel-add" onclick={() => h.createChannel(communityId, false)}>
                    <span class="chatlist-channel-hash">+</span>
                    <span class="chatlist-channel-name">Add channel</span>
                </div>
            {/if}
        </div>
    {:else}
        {#key state.canManage}
        <div class="chatlist-channels chatlist-channels-pane" class:is-organising={!!dragging}>
            {#each sections as section (section.id)}
                {#if section.headless}
                    <div class="chatlist-channel-section chatlist-channel-section-loose">
                        <div class="chatlist-channel-section-body">
                            {#each section.channels as channel (channel.id)}
                                {@render row(channel, state.canManage)}
                            {/each}
                        </div>
                    </div>
                {:else}
                {@const catDrop = dragging?.kind === 'category' && drop?.key === `cat:${section.id}`}
                <div class="chatlist-channel-section" class:is-closed={isClosed(section.id)} class:is-dragging={dragging?.id === section.id}
                     class:drop-above={catDrop && drop.before} class:drop-below={catDrop && !drop.before} use:sectionEl={section.id}>
                    <!-- A Category's heading is its handle: dragged among the others, or the top
                         of the Category a dragged channel lands in. -->
                    {#snippet head()}
                        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                        <div class="chatlist-channel-section-toggle btn" onclick={() => toggle(section.id)}>
                            <span class="chatlist-channel-section-label">{section.label}</span>
                            <span class="chatlist-channel-section-caret"><span class="icon icon-chevron-down"></span></span>
                        </div>
                        {#if section.canAdd}
                            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                            <div class="chatlist-channel-section-add btn" title={section.category ? `Add a channel to ${section.label}` : `Add a ${section.label.toLowerCase()} channel`}
                                 onclick={(e) => { e.stopPropagation(); h.createChannel(communityId, section.id === 'private', section.category?.id || null); }}>
                                <span class="icon icon-plus"></span>
                            </div>
                        {/if}
                    {/snippet}
                    {#if section.category && state.canManage}
                        <div class="chatlist-channel-section-head"
                             class:drop-above={dragging?.kind === 'channel' && drop?.key === `cat:${section.id}` && drop.before}
                             class:drop-below={dragging?.kind === 'channel' && drop?.key === `cat:${section.id}` && !drop.before}
                             use:headEl={section.id} use:categoryGesture={section.category}>
                            {@render head()}
                        </div>
                    {:else}
                        <div class="chatlist-channel-section-head">{@render head()}</div>
                    {/if}
                    <div class="chatlist-channel-section-body">
                        {#each section.channels as channel (channel.id)}
                            {@render row(channel, section.canAdd)}
                        {/each}
                    </div>
                </div>
                {/if}
            {/each}
        </div>
        {/key}
    {/if}
{/if}
