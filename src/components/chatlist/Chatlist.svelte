<script>
    // The chat list. A keyed {#each} reuses row nodes across updates, so a change inside
    // ONE chat patches that row instead of rebuilding the list.
    // Page globals (arrChats, helpers) arrive as props: this bundle is an IIFE and does
    // not share the classic scripts' global lexical scope.
    let { h, snapshot } = $props();   // h: ChatlistHelpers (js/render/chatlist/list.js)
    let channelsShown = $state(false);   // the pane's channel list has something to show

    import ChannelList from './ChannelList.svelte';
    import InviteRow from './InviteRow.svelte';
    import EmptyState from './EmptyState.svelte';
    import { listVersion, invitesVersion, paneState, openChatId, communityVersion, setListHasRows, clockTick } from '../lib/signals.svelte.js';
    import ChatlistRow from './ChatlistRow.svelte';

    // Snapshot re-pulls the raw page state when the list's shape, the invites, the
    // pane or the open chat change. Rows keep their own derivations through all of it.
    const snap = $derived.by(() => {
        listVersion();
        invitesVersion();
        const pane = paneState();
        const raw = snapshot();
        return { ...raw, paneCommunityId: pane.communityId, dmsOnly: pane.dmsOnly, openChat: openChatId() };
    });

    // Visible chats in list order — the wrapper's subscriber runs sortChats() before
    // this derives (it subscribed first), so snapshot().chats is already sorted.
    const chats = $derived.by(() => {
        const { chats: all, dmsOnly } = snap;
        const out = [];
        for (const chat of all) {
            if (!h.chatIsVisibleInList(chat)) continue;
            if (dmsOnly && h.chatIsGroup(chat)) continue;
            out.push(chat);
        }
        return out;
    });

    // Invites are spliced in place rather than reassigned, so the snapshot copies
    // them — a same-reference array would never invalidate this derived.
    const invites = $derived(snap.invites);
    const paneCommunityId = $derived(snap.paneCommunityId);
    const openChat = $derived(snap.openChat);
    const empty = $derived(chats.length === 0 && invites.length === 0);

    // Clock tick for relative timestamps and presence-dot recency, into the rows as a prop.
    const tick = $derived(clockTick());

    // ── actions: leaf widgets stay the vanilla builders / DOM disciplines ──

    // Message-less communities lazy-load their latest membership event so the preview
    // can say "X has joined" — a side effect, so it lives in an effect (the helper's
    // per-chat guard makes re-runs no-ops). Its landing bumps the store itself.
    $effect(() => {
        for (const c of chats) if (h.chatIsGroup(c)) h.ensureCommunityPreviewActivity(c);
    });

    // Widescreen selection stamp — the vanilla render re-stamped after every
    // rebuild; rows persist here, so re-stamp on list shape / selection changes.
    $effect(() => {
        chats;
        invites;
        paneCommunityId;
        openChat;
        h.wsMarkActiveRow();
    });

    // The pane's bottom fadeout hides over an empty list: it exists to soften a
    // scroller, and over the empty state it just washes out the intro.
    $effect(() => { setListHasRows(paneCommunityId ? channelsShown : (chats.length + invites.length) > 0); });
</script>

{#if paneCommunityId}
    <ChannelList communityId={paneCommunityId} pane {h} onShown={(on) => (channelsShown = on)} />
{:else}
    {#each invites as invite (invite.community_id)}
        <InviteRow {invite} {h} />
    {/each}
    {#each chats as chat (chat.id)}
        <ChatlistRow
            {h}
            {chat}
            pinned={snap.pinned.includes(h.chatPinKey(chat))}
            {tick}
        />
    {/each}
    {#if empty}
        <EmptyState {h} />
    {/if}
{/if}

<!-- No <style>: global styles.css cascades. -->
