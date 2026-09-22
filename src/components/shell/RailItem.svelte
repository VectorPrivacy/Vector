<script>
    // One rail shortcut. Reads its own chat/profile/community signals, so a change in
    // any other chat leaves this node untouched.
    import { chatVersion, profileVersion, communityVersion } from '../lib/signals.svelte.js';
    import Avatar from '../ui/Avatar.svelte';

    let { h, chat, isCommunity, active = false } = $props();   // h: RailHelpers (js/render/rail.js)

    const vm = $derived.by(() => {
        chatVersion(chat.id);
        const communityId = isCommunity ? chat.metadata?.custom_fields?.community_id : null;
        if (communityId) communityVersion(communityId);
        else profileVersion(chat.id);
        const profile = isCommunity ? null : h.getProfile(chat.id);
        const name = isCommunity
            ? (chat.metadata?.custom_fields?.name || 'Community')
            : h.getName(profile || chat.id);
        // Community shortcuts total their channels, so unread in a collapsed channel still shows.
        const unread = isCommunity ? h.computeListRowUnreadCount(chat) : h.computeRowUnreadCount(chat);
        // A NUMBER is for someone calling your name, or a room still set to tell you everything;
        // ordinary unread is a dot: the room is awake without asking you to act.
        const pings = isCommunity ? h.computeCommunityPingCount(chat) : h.computeRowBadgeCount(chat);
        return {
            name,
            twemoji: isCommunity || !!(profile?.nickname || profile?.name),
            src: isCommunity
                ? (chat.metadata?.avatar_cached ? h.convertFileSrc(chat.metadata.avatar_cached) : null)
                : h.getProfileAvatarSrc(profile) || null,
            unread,
            pings,
            muted: isCommunity ? !!chat.community_muted : !!chat.muted,
            communityId,
        };
    });

    const isDot = $derived(!vm.pings);

    // Keyed on the string so a re-derive that yields the same name never restarts twemoji.
    function name(node, v) {
        let cur;
        const render = (vm) => {
            if (vm.name === cur) return;
            cur = vm.name;
            node.textContent = vm.name;
            if (vm.twemoji) h.twemojify(node);
        };
        render(v);
        return { update: render };
    }

    // A community's shortcut returns you to the channel you left it in, not its primary.
    function open() {
        h.openChat(isCommunity ? (h.wsChannelForCommunity(vm.communityId) || chat.id) : chat.id);
    }

    // Right-click or long-press: the community's own menu, or the DM's row menu.
    function menu(node) {
        h.attachLongPressContextMenu(node, (x, y) => {
            if (isCommunity) h.openCommunityMenu(chat, x, y);
            else h.showChatRowContextMenu(chat, false, vm.unread, x, y);
        });
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div
    class="ws-rail-item btn"
    class:is-community={isCommunity}
    class:is-quiet={isCommunity && !vm.unread}
    class:is-muted={vm.muted}
    class:active
    id="ws-rail-item-{chat.id}"
    title={vm.name}
    use:menu
    onclick={open}
>
    <Avatar src={vm.src} size={26} group={isCommunity} class="ws-rail-item-avatar" />
    <span class="ws-rail-item-name cutoff" use:name={vm}></span>
    {#if vm.pings || vm.unread}
        <span class="ws-rail-item-badge" class:is-dot={isDot} class:muted={vm.muted && !isDot}>
            {isDot ? '' : (vm.pings > 99 ? '99+' : String(vm.pings))}
        </span>
    {/if}
</div>
