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

    /** How many unread DMs the rail surfaces. The Chat tab's badge carries the total. */
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

    // Everything waiting behind the Chat tab is the vanilla side's to paint.
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

<!-- With nothing unread, one door to the DMs stands where the unread rows would. -->
<div id="ws-rail-messages" class="ws-rail-group" hidden={groups.dms.length > 0}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="ws-rail-messages btn" title="Messages" onclick={() => h.openDmHome()}>
        <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
            <path d="M21.5 18L14.8571 12M9.14286 12L2.50003 18M2 7L10.1649 12.7154C10.8261 13.1783 11.1567 13.4097 11.5163 13.4993C11.8339 13.5785 12.1661 13.5785 12.4837 13.4993C12.8433 13.4097 13.1739 13.1783 13.8351 12.7154L22 7M6.8 20H17.2C18.8802 20 19.7202 20 20.362 19.673C20.9265 19.3854 21.3854 18.9265 21.673 18.362C22 17.7202 22 16.8802 22 15.2V8.8C22 7.11984 22 6.27976 21.673 5.63803C21.3854 5.07354 20.9265 4.6146 20.362 4.32698C19.7202 4 18.8802 4 17.2 4H6.8C5.11984 4 4.27976 4 3.63803 4.32698C3.07354 4.6146 2.6146 5.07354 2.32698 5.63803C2 6.27976 2 7.11984 2 8.8V15.2C2 16.8802 2 17.7202 2.32698 18.362C2.6146 18.9265 3.07354 19.3854 3.63803 19.673C4.27976 20 5.11984 20 6.8 20Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
        <span class="ws-rail-messages-label">Messages</span>
    </div>
</div>
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
