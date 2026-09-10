<script>
    import { bindRailEl } from '../lib/rail.svelte.js';
    const bindSpacesRows = bindRailEl('spacesRows');
    // Widescreen rail shortcuts: unread DMs over communities, between the logo and
    // the nav tabs. Owns #ws-rail-shortcuts. The two groups derive from the chat list's
    // order (listVersion) and from each candidate chat's own signal, so a reply in one
    // chat re-derives the DM selection (a cheap scan) and patches one item; a rename
    // or avatar touches one item; nothing rebuilds the strip.
    import { listVersion, chatVersion, openChatId } from '../lib/signals.svelte.js';
    import RailItem from './RailItem.svelte';

    let { h, snapshot } = $props();   // h: RailHelpers (js/render/rail.js)

    /** How many unread DMs the rail surfaces. The mail badge carries the rest. */
    const DM_COUNT = 3;

    const groups = $derived.by(() => {
        listVersion();
        const { chats, myNpub } = snapshot();
        const dms = [];
        const spaces = [];
        let unreadDms = 0;
        for (const chat of chats) {
            if (h.chatIsGroup(chat)) {
                if (!chat.metadata?.custom_fields?.community_id) continue;
                if (!h.isPrimaryChannelChat(chat)) continue;
                spaces.push(chat);
                continue;
            }
            if (!chat.messages.length || chat.id === myNpub) continue;
            if (h.getProfile(chat.id)?.is_blocked) continue;
            // Unread only, so a muted chat (scores 0) never takes one of the slots.
            // Reading the chat's signal is what re-runs this when its unread moves.
            chatVersion(chat.id);
            if (!h.computeRowBadgeCount(chat)) continue;
            unreadDms++;
            if (dms.length < DM_COUNT) dms.push(chat);
        }
        return { dms, spaces, unreadDms };
    });

    // The shortcut that REPRESENTS the open chat: a community has one, built from its
    // primary channel, and any of its channels lights it.
    const activeId = $derived.by(() => {
        const open = openChatId();
        if (!open) return null;
        const { chats } = snapshot();
        const chat = chats.find((c) => c.id === open);
        const communityId = h.communityIdOfChat(chat);
        if (!communityId) return open;
        const primary = chats.find((c) => h.communityIdOfChat(c) === communityId && h.isPrimaryChannelChat(c));
        return (primary || chat)?.id || open;
    });

    // Everything waiting behind the mail button is the vanilla side's to paint.
    $effect(() => h.onUnreadDms(groups.unreadDms));

    // Scroll is the common case for the fade; the strip also changes height when the
    // rail collapses, the window resizes, or rows come and go.
    function fade(node) {
        node.addEventListener('scroll', h.syncRailFade, { passive: true });
        const ro = new ResizeObserver(h.syncRailFade);
        ro.observe(node);
        return { destroy: () => ro.disconnect() };
    }
    $effect(() => {
        groups;
        h.syncRailFade();
    });
</script>

<div id="ws-rail-dms" class="ws-rail-group" hidden={groups.dms.length === 0}>
    <div class="ws-rail-group-label">New Messages</div>
    <div class="ws-rail-rows">
        {#each groups.dms as chat (chat.id)}
            <RailItem {h} {chat} isCommunity={false} active={activeId === chat.id} />
        {/each}
    </div>
</div>
<div id="ws-rail-spaces" class="ws-rail-group" hidden={groups.spaces.length === 0}>
    <div class="ws-rail-group-label">Communities</div>
    <div class="ws-rail-rows" use:bindSpacesRows use:fade>
        {#each groups.spaces as chat (chat.id)}
            <RailItem {h} {chat} isCommunity={true} active={activeId === chat.id} />
        {/each}
    </div>
</div>

<!-- No <style>: widescreen.css's .ws-rail-* rules cascade; the DOM matches the vanilla strip. -->
