<script>
    // The chat list. A keyed {#each} reuses row nodes across updates, so a change inside
    // ONE chat patches that row instead of rebuilding the list.
    // Page globals (arrChats, helpers) arrive as props: this bundle is an IIFE and does
    // not share the classic scripts' global lexical scope.
    let { h, snapshot } = $props();   // h: ChatlistHelpers (js/render/chatlist/list.js)

    import ChannelList from './ChannelList.svelte';
    import InviteRow from './InviteRow.svelte';
    import EmptyState from './EmptyState.svelte';
    import { listVersion, invitesVersion, paneState, openChatId, clockTick } from '../lib/signals.svelte.js';
    import ChatlistRow from './ChatlistRow.svelte';
    import ArchiveRow from './ArchiveRow.svelte';
    import { shellState } from '../lib/shell.svelte.js';
    import { consoleState, consoleFilter } from '../lib/console.svelte.js';
    const shell = shellState();
    const cc = consoleState();

    // Which rows a filter keeps.
    function inFilter(chat, filter) {
        const group = h.chatIsGroup(chat);
        if (filter === 'any') return true;
        if (filter === 'communities') return group;
        const request = !group && h.chatIsRequest(chat);
        if (filter === 'requests') return request;
        if (filter === 'messages') return !group && !request;
        return !request;
    }

    // Snapshot re-pulls the raw page state when the list's shape, the invites, the
    // pane or the open chat change. Rows keep their own derivations through all of it.
    const snap = $derived.by(() => {
        listVersion();
        invitesVersion();
        const pane = paneState();
        const raw = snapshot();
        return { ...raw, paneCommunityId: pane.communityId, dmsOnly: pane.dmsOnly, openChat: openChatId() };
    });

    // The slim layout's search spans every chat, whatever the tab or the rail shows.
    const query = $derived(shell.ws ? '' : cc.query.trim().toLowerCase());
    const filter = $derived(shell.ws && !snap.dmsOnly ? 'any' : consoleFilter());

    // Visible chats in list order — the wrapper's subscriber runs sortChats() before
    // this derives (it subscribed first), so snapshot().chats is already sorted.
    // Archived DMs leave the list for the archive row, in the same order: each tab's
    // archive holds its own.
    const lists = $derived.by(() => {
        const out = [], archived = [];
        for (const chat of snap.chats) {
            if (!h.chatIsVisibleInList(chat)) continue;
            if (query) {
                if (h.chatMatchesQuery(chat, query)) out.push(chat);
                continue;
            }
            if (!inFilter(chat, filter)) continue;
            (h.chatIsArchived(chat) ? archived : out).push(chat);
        }
        return { chats: out, archived };
    });
    const chats = $derived(lists.chats);
    const archived = $derived(lists.archived);

    // Invites are spliced in place rather than reassigned, so the snapshot copies
    // them — a same-reference array would never invalidate this derived. They wait
    // under Requests.
    const invites = $derived(!query && filter === 'requests' ? snap.invites : []);
    const paneCommunityId = $derived(query ? null : snap.paneCommunityId);
    const openChat = $derived(snap.openChat);
    const empty = $derived(chats.length === 0 && invites.length === 0 && archived.length === 0);

    // Clock tick for relative timestamps, into the rows as a prop.
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
        archived;
        invites;
        paneCommunityId;
        openChat;
        h.wsMarkActiveRow();
    });
</script>

{#if paneCommunityId}
    {#key paneCommunityId}<ChannelList communityId={paneCommunityId} {h} />{/key}
{:else}
    {#if archived.length}
        <!-- With nothing else listed there is nothing to pull past, so it shows outright. -->
        <ArchiveRow {h} chats={archived} forced={chats.length + invites.length === 0}>
            {#snippet rows()}
                {#each archived as chat (chat.id)}
                    <ChatlistRow {h} {chat} pinned={snap.pinned.includes(chat.id)} {tick} />
                {/each}
            {/snippet}
        </ArchiveRow>
    {/if}
    {#each invites as invite (invite.community_id)}
        <InviteRow {invite} {h} />
    {/each}
    {#each chats as chat (chat.id)}
        <ChatlistRow
            {h}
            {chat}
            pinned={!h.chatIsGroup(chat) && snap.pinned.includes(chat.id)}
            {tick}
        />
    {/each}
    {#if empty}
        {#if query}
            <p class="cc-empty">No chats match "{cc.query.trim()}"</p>
        {:else if filter === 'requests'}
            <p class="cc-empty">New people who message you show up here.</p>
        {:else if filter === 'communities'}
            <p class="cc-empty">Communities you join live here.</p>
        {:else}
            <EmptyState {h} />
        {/if}
    {/if}
{/if}

<!-- No <style>: global styles.css cascades. -->
