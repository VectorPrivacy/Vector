<script>
    // One chat row (migration Phase 1 — SVELTE_MIGRATION_PLAN.md §3.2).
    //
    // Chat objects are RAW (shared by reference with eventCache and mutated in
    // place), so the row cannot rely on reference identity for change detection:
    // its own signals (chat, DM profile, community) and the clock `tick` are read
    // inside the vm derivation, while the keyed parent each keeps the DOM node
    // itself alive across updates.
    let { h, chat, pinned, tick } = $props();

    import { chatVersion, profileVersion, communityVersion } from '../lib/signals.svelte.js';

    // COMMUNITY_UNREAD_PLUS_THRESHOLD (row.js): past one synced page the count is a
    // lower bound, so render "N+" rather than a false exact figure.
    const UNREAD_PLUS_THRESHOLD = 20;

    // Row view-model. The dependency pins: the clock tick plus this row's OWN signals —
    // its chat, its DM profile, its community (a community row aggregates every
    // channel's unread). A touch on another chat leaves this row alone.
    const vm = $derived.by(() => {
        tick;
        chatVersion(chat.id);
        const isGroup = h.chatIsGroup(chat);
        const communityId = chat.metadata?.custom_fields?.community_id;
        if (isGroup && communityId) communityVersion(communityId);
        else profileVersion(chat.id);
        const profile = !isGroup ? h.getProfile(chat.id) : null;
        return {
            chat,
            isGroup,
            profile,
            communityId: chat.metadata?.custom_fields?.community_id,
            nUnread: h.computeListRowBadgeCount(chat),
            last: chat.messages[chat.messages.length - 1] || null,
            name: isGroup
                ? (chat.metadata?.custom_fields?.name || `Group ${chat.id.substring(0, 8)}...`)
                : h.getName(profile || chat.id),
            preview: h.generateChatPreviewText(chat),
            presence: isGroup ? null : presenceOf(chat),
            pinned,
            joining: !!chat._joining,
        };
    });

    // Emulated presence: last incoming message <5m online, <30m away (row.js).
    function presenceOf(chat) {
        let last = null;
        for (let i = chat.messages.length - 1; i >= 0; i--) {
            if (!chat.messages[i].mine) { last = chat.messages[i]; break; }
        }
        if (!last) return null;
        if (last.at > Date.now() - 300000) return 'online';
        if (last.at > Date.now() - 1800000) return 'away';
        return null;
    }

    function countText(vm) {
        if (vm.nUnread > 99) return '99+';
        if (vm.chat.chat_type === 'Community' && vm.nUnread >= UNREAD_PLUS_THRESHOLD) return `${vm.nUnread}+`;
        return String(vm.nUnread);
    }

    // Rebuild key for the nested channel list (still the vanilla builder, channels.js).
    // `channelStateHashParts` is the same input set the legacy state-hash gate used —
    // channel set, expanded state, caps flag, per-channel name/unread — so the nested
    // list rebuilds exactly when its own inputs changed, never on unrelated bumps.
    const channelsKey = $derived.by(() => {
        const cid = chat.metadata?.custom_fields?.community_id;
        if (cid) communityVersion(cid);
        const parts = [];
        h.channelStateHashParts(chat, parts);
        return parts.join('\x00');
    });

    // ── actions: leaf widgets stay the vanilla builders / DOM disciplines ──

    function placeholderInto(node, isGroup) {
        node.replaceChildren(h.createPlaceholderAvatar(isGroup, 50));
    }

    function imgFallback(isGroup) {
        return (e) => e.currentTarget.replaceWith(h.createPlaceholderAvatar(isGroup, 50));
    }

    // Name text + twemojify. Keyed on the string so a re-derive that yields the
    // same name never touches the node (twemojify would restart image loads).
    function h4Into(node, v) {
        let cur;
        const render = (vm) => {
            if (vm.name === cur) return;
            cur = vm.name;
            node.textContent = vm.name;
            if (!vm.isGroup && (vm.profile?.nickname || vm.profile?.name)) h.twemojify(node);
        };
        render(v);
        return { update: render };
    }

    // Preview line: innerHTML/textContent + twemoji + custom-emoji shortcodes,
    // matching row.js's full-render path. Keyed on rendered content for the same
    // reason as h4Into.
    function previewInto(node, p) {
        let cur;
        const render = (pv) => {
            const key = (pv.isHtml ? 'H\x00' : 'T\x00') + pv.text;
            if (key === cur) return;
            cur = key;
            if (pv.isHtml) node.innerHTML = pv.text;
            else node.textContent = pv.text;
            if (pv.needsTwemoji) h.twemojify(node);
            if (pv.emojiTags) h.renderCustomEmojiShortcodes(node, pv.emojiTags);
        };
        render(p);
        return { update: render };
    }

    // Context menu: attach listeners once, always fire with the fresh view-model.
    function rowMenu(node, v) {
        let cur = v;
        h.attachLongPressContextMenu(node, (x, y) => h.showChatRowContextMenu(cur.chat, cur.isGroup, cur.nUnread, x, y));
        return { update: (v) => { cur = v; } };
    }

    // Nested channel list host. Re-renders through `update` on key change — the
    // key carries the channel set, expanded state and per-channel unreads (the
    // same inputs the legacy state-hash gate used).
    function channelsInto(node, key) {
        let cur = null;
        const render = (k) => {
            if (k === cur) return;
            cur = k;
            const sep = k.indexOf('\x00');
            const communityId = sep === -1 ? k : k.slice(0, sep);
            node.replaceChildren(h.renderCommunityChannels(communityId) || []);
        };
        render(key);
        return { update: render };
    }
</script>

<div
    class="chatlist-contact"
    class:has-unread={vm.nUnread}
    class:chatlist-joining={vm.joining}
    id="chatlist-{vm.chat.id}"
    use:rowMenu={vm}
>
    <div style:position="relative">
        {#if vm.isGroup}
            {#if vm.chat.metadata?.avatar_cached}
                <!-- svelte-ignore a11y_missing_attribute (byte-identical to the vanilla avatar img) -->
                <img
                    src={h.convertFileSrc(vm.chat.metadata.avatar_cached)}
                    style:width="50px"
                    style:height="50px"
                    style:object-fit="cover"
                    style:border-radius="50%"
                    onerror={imgFallback(true)}
                />
            {:else}
                <div use:placeholderInto={true}></div>
            {/if}
        {:else}
            {#if h.getProfileAvatarSrc(vm.profile)}
                <!-- svelte-ignore a11y_missing_attribute (byte-identical to the vanilla avatar img) -->
                <img
                    src={h.getProfileAvatarSrc(vm.profile)}
                    style:width="50px"
                    style:height="50px"
                    style:object-fit="cover"
                    style:border-radius="50%"
                    onerror={imgFallback(false)}
                />
            {:else}
                <div use:placeholderInto={false}></div>
            {/if}
        {/if}
        {#if vm.presence}
            <div
                class="avatar-status-icon"
                style:background-color={vm.presence === 'online' ? '#59fcb3' : '#fce459'}
            ></div>
        {/if}
    </div>
    <div class="chatlist-contact-preview">
        <div class="chatlist-contact-header">
            <!-- svelte-ignore a11y_missing_content (text is written by the h4Into action) -->
            <h4 class="cutoff" use:h4Into={vm}></h4>
            {#if vm.isGroup}
                <!-- svelte-ignore a11y_no_static_element_interactions (byte-identical to the vanilla tooltip span) -->
                <span
                    class="icon icon-users-multi chatlist-type-icon"
                    onmouseenter={(e) => h.showGlobalTooltip('Group Chat', e.currentTarget)}
                    onmouseleave={() => h.hideGlobalTooltip()}
                ></span>
            {:else if vm.profile?.bot}
                <!-- svelte-ignore a11y_no_static_element_interactions (byte-identical to the vanilla tooltip span) -->
                <span
                    class="icon icon-bot chatlist-type-icon"
                    onmouseenter={(e) => h.showGlobalTooltip('Bot', e.currentTarget)}
                    onmouseleave={() => h.hideGlobalTooltip()}
                ></span>
            {/if}
            {#if vm.pinned}
                <!-- svelte-ignore a11y_no_static_element_interactions (byte-identical to the vanilla tooltip span) -->
                <span
                    class="icon icon-pin chatlist-type-icon"
                    onmouseenter={(e) => h.showGlobalTooltip('Pinned', e.currentTarget)}
                    onmouseleave={() => h.hideGlobalTooltip()}
                ></span>
            {/if}
            {#if vm.last}
                <span class="chatlist-contact-inline-time">{h.timeAgo(vm.last.at)}</span>
            {/if}
        </div>
        <p class="cutoff" class:typing-indicator-text={vm.preview.isTyping} use:previewInto={vm.preview}></p>
    </div>
    {#if vm.nUnread}
        <span class="chatlist-contact-count">{countText(vm)}</span>
    {/if}
    {#if vm.isGroup && vm.communityId && h.communityHasChannelList(vm.communityId)}
        <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events (byte-identical to the vanilla expander div) -->
        <div
            class="chatlist-expander btn"
            class:expanded={h.communityChannelsShown(vm.communityId)}
            title="Channels"
            onclick={(e) => { e.stopPropagation(); h.toggleCommunityExpanded(vm.communityId); }}
        >
            <span class="icon icon-chevron-down"></span>
        </div>
    {/if}
</div>
{#if vm.isGroup && vm.communityId}
    <div style="display:contents;" use:channelsInto={channelsKey}></div>
{/if}

<!-- No <style>: global styles.css cascades; the DOM is byte-identical to the vanilla
     renderChat output (display:contents hosts keep #chat-list's child structure flat). -->
