<script>
    import { markedChatId } from '../lib/signals.svelte.js';
    import { avatarThumb } from '../lib/avatar.js';
    import { shellState } from '../lib/shell.svelte.js';
    // One chat row.
    //
    // Chat objects are RAW (shared by reference with eventCache and mutated in
    // place), so the row cannot rely on reference identity for change detection:
    // its own signals (chat, DM profile, community) and the clock `tick` are read
    // inside the vm derivation, while the keyed parent each keeps the DOM node
    // itself alive across updates.
    let { h, chat, pinned, tick } = $props();   // h: ChatlistHelpers (js/render/chatlist/list.js)

    import Avatar from '../ui/Avatar.svelte';
    import { watchProfile } from '../lib/profilewatch.js';
    import { chatVersion, profileVersion, communityVersion } from '../lib/signals.svelte.js';
    import { transfer } from '../lib/attachments.svelte.js';

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
        // A community's channels all touch its community, so `shown` re-derives with them.
        const shown = isGroup ? h.listRowPreviewChat(chat) : chat;
        return {
            chat,
            isGroup,
            profile,
            communityId: chat.metadata?.custom_fields?.community_id,
            // Two different questions: nMark is whether the row has anything in it at all,
            // nUnread is how much of that the level lets the badge say out loud.
            nMark: h.computeListRowUnreadCount(chat),
            nUnread: h.computeListRowBadgeCount(chat),
            pinged: isGroup && h.listRowPingsMe(chat),
            last: shown.messages[shown.messages.length - 1] || null,
            name: isGroup
                ? (chat.metadata?.custom_fields?.name || `Group ${chat.id.substring(0, 8)}...`)
                : h.getName(profile || chat.id),
            preview: h.generateChatPreviewText(shown),
            pinned,
            joining: !!chat._joining,
        };
    });

    // A row still sending counts its upload up. Kept apart from vm, which the per-frame
    // transfer updates would otherwise rebuild.
    const sendPct = $derived.by(() => {
        const t = vm.preview.pendingId ? transfer(vm.preview.pendingId) : null;
        return t?.kind === 'upload' && t.phase === 'active' && t.pct > 0 ? t.pct : null;
    });
    const preview = $derived(sendPct == null ? vm.preview : { ...vm.preview, text: `Sending (${sendPct}%)` });

    function countText(vm) {
        if (vm.nUnread > 99) return '99+';
        if (vm.chat.chat_type === 'Community' && vm.nUnread >= UNREAD_PLUS_THRESHOLD) return `${vm.nUnread}+`;
        return String(vm.nUnread);
    }

    // ── actions: leaf widgets stay the vanilla builders / DOM disciplines ──

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
        h.attachLongPressContextMenu(node, (x, y) => h.showChatRowContextMenu(cur.chat, cur.isGroup, cur.nMark, x, y));
        return { update: (v) => { cur = v; } };
    }

</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div
    class="chatlist-contact"
    class:is-group={vm.isGroup}
    class:has-unread={vm.nMark}
    class:chatlist-joining={vm.joining}
    class:ws-active={shellState().ws && markedChatId() === vm.chat.id}
    id="chatlist-{vm.chat.id}"
    use:rowMenu={vm}
    use:watchProfile={vm.isGroup ? null : vm.chat.id}
    onclick={() => h.rowClick(vm)}
>
    <div style:position="relative">
        {#if vm.isGroup}
            <Avatar src={vm.chat.metadata?.avatar_cached ? h.convertFileSrc(avatarThumb(vm.chat.metadata.avatar_cached)) : null} size={50} group />
        {:else}
            <Avatar src={h.getProfileAvatarSrc(vm.profile)} size={50} />
        {/if}
    </div>
    <div class="chatlist-contact-preview">
        <div class="chatlist-contact-header">
            <!-- svelte-ignore a11y_missing_content (text is written by the h4Into action) -->
            <h4 class="cutoff" use:h4Into={vm}></h4>
            {#if vm.isGroup}
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <span
                    class="icon icon-users-multi chatlist-type-icon"
                    onmouseenter={(e) => h.showGlobalTooltip('Group Chat', e.currentTarget)}
                    onmouseleave={() => h.hideGlobalTooltip()}
                ></span>
            {:else if vm.profile?.bot}
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <span
                    class="icon icon-bot chatlist-type-icon"
                    onmouseenter={(e) => h.showGlobalTooltip('Bot', e.currentTarget)}
                    onmouseleave={() => h.hideGlobalTooltip()}
                ></span>
            {/if}
            {#if vm.pinned}
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <span
                    class="icon icon-pin chatlist-type-icon"
                    onmouseenter={(e) => h.showGlobalTooltip('Pinned', e.currentTarget)}
                    onmouseleave={() => h.hideGlobalTooltip()}
                ></span>
            {/if}
            {#if vm.pinged || vm.nUnread}
                <span class="chatlist-contact-flags">
                    {#if vm.pinged}<span class="chatlist-contact-at">@</span>{/if}
                    {#if vm.nUnread}<span class="chatlist-contact-count" class:muted={vm.chat.muted}>{countText(vm)}</span>{/if}
                </span>
            {/if}
        </div>
        <div class="chatlist-contact-line">
            <p class="cutoff" class:typing-indicator-text={vm.preview.isTyping} use:previewInto={preview}></p>
            {#if vm.last}
                <span class="chatlist-contact-time">{h.timeAgo(vm.last.at)}</span>
            {/if}
        </div>
    </div>
</div>

<!-- No <style>: global styles.css cascades -->
