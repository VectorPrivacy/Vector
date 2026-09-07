<script>
    // A community's channels: nested under its chat-list row as a flat disclosure, or as
    // the widescreen pane in Public / Private sections. Each row derives its own weight
    // (muted, unread, pings) from its chat's signal, so a message repaints one row.
    import { communityVersion, chatVersion, openChatId } from '../lib/signals.svelte.js';

    let { communityId, pane = false, h } = $props();
    // h: getChannels, canAddChannels, channelsShown, sectionClosed, toggleSection, chatById,
    //    computeRowBadgeCount, countPingMessages, isPrimaryChannelId, openChannel, createChannel, deleteChannel

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

    // A section is { id, label, channels, canAdd }: the day the backend grows user-defined
    // sections only this grouping changes. Today the protocol knows public vs private.
    const sections = $derived.by(() => {
        if (!state) return [];
        const { channels, canManage } = state;
        const out = [];
        const open = channels.filter(c => !c.private);
        const shut = channels.filter(c => c.private);
        if (open.length) out.push({ id: 'public', label: 'Public', channels: open, canAdd: canManage });
        if (shut.length) out.push({ id: 'private', label: 'Private', channels: shut, canAdd: canManage });
        if (!out.length && canManage) out.push({ id: 'public', label: 'Public', channels: [], canAdd: true });
        return out;
    });

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
</script>

{#snippet row(channel, canManage)}
    {@const chat = (chatVersion(channel.id), h.chatById(channel.id))}
    {@const pings = chat ? h.countPingMessages(chat) : 0}
    <!-- Three tiers, loudest first: something to read, nothing to read, and a room you asked
         to be quiet. Muted wins outright: a standing instruction, not a state unread overrides. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="chatlist-channel" id="chatlist-channel-{channel.id}"
         class:active={openChatId() === channel.id}
         class:is-muted={!!chat?.muted}
         class:has-unread={!chat?.muted && !!chat && h.computeRowBadgeCount(chat) > 0}
         class:is-read={!chat?.muted && !(chat && h.computeRowBadgeCount(chat) > 0)}
         onclick={() => h.openChannel(communityId, channel)}>
        <span class="chatlist-channel-hash"><span class="icon icon-channel-hash"></span></span>
        <span class="chatlist-channel-name cutoff">{channel.name}</span>
        <!-- A number only for someone calling your name; ordinary unread is the row's own weight. -->
        {#if pings}
            <span class="chatlist-channel-badge">{pings > 99 ? '99+' : pings}</span>
        {/if}
        <!-- The primary channel anchors the community's row and history; the backend refuses to tombstone it. -->
        {#if canManage && !h.isPrimaryChannelId(channel.id)}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="chatlist-channel-delete btn" title="Delete channel" onclick={(e) => { e.stopPropagation(); h.deleteChannel(communityId, channel); }}>
                <span class="icon icon-x"></span>
            </div>
        {/if}
    </div>
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
        <div class="chatlist-channels chatlist-channels-pane">
            {#each sections as section (section.id)}
                <div class="chatlist-channel-section" class:is-closed={isClosed(section.id)}>
                    <div class="chatlist-channel-section-head">
                        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                        <div class="chatlist-channel-section-toggle btn" onclick={() => toggle(section.id)}>
                            <span class="chatlist-channel-section-label">{section.label}</span>
                            <span class="chatlist-channel-section-caret"><span class="icon icon-chevron-down"></span></span>
                        </div>
                        {#if section.canAdd}
                            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                            <div class="chatlist-channel-section-add btn" title="Add a {section.label.toLowerCase()} channel"
                                 onclick={(e) => { e.stopPropagation(); h.createChannel(communityId, section.id === 'private'); }}>
                                <span class="icon icon-plus"></span>
                            </div>
                        {/if}
                    </div>
                    <div class="chatlist-channel-section-body">
                        {#each section.channels as channel (channel.id)}
                            {@render row(channel, section.canAdd)}
                        {/each}
                    </div>
                </div>
            {/each}
        </div>
    {/if}
{/if}
