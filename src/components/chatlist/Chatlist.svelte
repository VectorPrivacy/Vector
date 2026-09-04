<script>
    // The chat-list island (migration Phase 1, SVELTE_MIGRATION_PLAN.md §3.2).
    // Byte-identical DOM to the vanilla renderChatlistNow path
    // (js/render/chatlist/{list,row,channels}.js): keyed {#each} reuses row nodes
    // across updates, so a change inside ONE chat patches that row instead of
    // rebuilding the list — the granularity the state-hash-gated full render
    // could not offer.
    // Page globals (arrChats, helpers) arrive as props: this bundle is an IIFE and
    // does not share the classic scripts' global lexical scope.
    let { h, snapshot } = $props();

    import { chatlistVersion, timeTickVersion } from '../lib/stores.js';
    import ChatlistRow from './ChatlistRow.svelte';

    // Snapshot re-pulls the raw page state on every invalidation.
    const snap = $derived.by(() => {
        $chatlistVersion;
        return snapshot();
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

    // Clock tick for relative timestamps and presence-dot recency, and the list's
    // own invalidation version — both flow into the rows as props.
    const tick = $derived($timeTickVersion);
    const version = $derived($chatlistVersion);

    // Rebuild key for the community pane (widescreen: inside a community the list IS
    // that community's channel list). Same input set the legacy state-hash gate used,
    // scoped to the pane's community — read fresh on every invalidation.
    const paneKey = $derived.by(() => {
        const id = paneCommunityId;
        if (!id) return '';
        let primary = null;
        for (const c of snap.chats) {
            if (c.metadata?.custom_fields?.community_id !== id) continue;
            if (!primary) primary = c;
            if (h.isPrimaryChannelChat(c)) { primary = c; break; }
        }
        if (!primary) return id;
        const parts = [];
        h.channelStateHashParts(primary, parts);
        return parts.join('\x00');
    });

    // Rebuild key for the pane header: identity-relevant fields only, so member
    // counts landing (or a rename) rebuild it but unrelated bumps don't — the
    // builder's avatar would otherwise reload on every message anywhere.
    const headKey = $derived.by(() => {
        const id = paneCommunityId;
        if (!id) return '';
        let primary = null;
        for (const c of snap.chats) {
            if (c.metadata?.custom_fields?.community_id !== id) continue;
            if (!primary) primary = c;
            if (h.isPrimaryChannelChat(c)) { primary = c; break; }
        }
        if (!primary) return id;
        const cf = primary.metadata.custom_fields;
        return [id, cf.name || '', primary.metadata.avatar_cached || '', cf.proto_version || '', h.communityMemberSubtext(id)].join('\x00');
    });

    // ── actions: leaf widgets stay the vanilla builders / DOM disciplines ──

    function inviteKey(inv) {
        return `${inv.community_id}\x00${inv.name || ''}`;
    }

    // Keyed on identity fields, not the invite object: the row's icon fetch is
    // async (and swaps its own placeholder when it lands), so re-running on every
    // bump would re-show the placeholder forever.
    function inviteInto(node, invite) {
        let cur = inviteKey(invite);
        node.replaceChildren(h.renderCommunityInviteItem(invite));
        return {
            update: (inv) => {
                if (!inv || inviteKey(inv) === cur) return;
                cur = inviteKey(inv);
                node.replaceChildren(h.renderCommunityInviteItem(inv));
            },
        };
    }

    // The pane re-renders through `update` — an action without one ignores param
    // changes, which left the first-opened community's channels stuck in the pane.
    function paneInto(node, key) {
        let cur = null;
        const render = (k) => {
            if (k === cur) return;
            cur = k;
            node.replaceChildren(h.renderCommunityChannels(k.split('\x00')[0], { pane: true }) || []);
        };
        render(key);
        return { update: render };
    }

    function builtInto(node, builder) {
        node.replaceChildren(builder());
    }

    // The community pane's header lives OUTSIDE the mount target (#ws-community-head
    // is a sibling of #chat-list, above the scrollport) — stamped from here, rebuilt
    // only when its key changes.
    let prevHeadKey = '\x01';
    $effect(() => {
        const key = headKey;
        const head = document.getElementById('ws-community-head');
        if (!head) return;
        if (key === prevHeadKey) return;
        prevHeadKey = key;
        if (!key) head.replaceChildren();
        // Bare id (community with no primary chat row yet): the builder handles
        // the null-primary fallback itself.
        else head.replaceChildren(h.renderCommunityListHeader(key.split('\x00')[0]));
    });

    // Message-less communities lazy-load their latest membership event so the preview
    // can say "X has joined" — a side effect, so it lives in an effect (the helper's
    // per-chat guard makes re-runs no-ops). Its landing bumps the store itself.
    $effect(() => {
        for (const c of chats) if (h.chatIsGroup(c)) h.ensureCommunityPreviewActivity(c);
    });

    // The last row gets the scroll-past-fadeout margin boost. Rows persist, so the
    // previous holder must be cleared (the vanilla path rebuilt everything). Hosts
    // are display:contents (no box), so walk back to the last real element.
    let prevLast = null;
    $effect(() => {
        chats;
        invites;
        paneCommunityId;
        const list = document.getElementById('chat-list');
        let last = list?.lastElementChild || null;
        while (last && last.style.display === 'contents') last = last.previousElementSibling;
        if (prevLast && prevLast !== last) prevLast.style.marginBottom = '';
        if (last) last.style.marginBottom = '50px';
        prevLast = last;
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

    // The bottom fadeout exists to soften a scrolling list; over the empty
    // state it just washes out the intro. Keyed on real rendered content so
    // pane mode (channels, not chats) reads correctly too.
    $effect(() => {
        chats;
        invites;
        paneCommunityId;
        const has = document.querySelector('#chat-list .chatlist-contact, #chat-list .chatlist-channels');
        const el = document.querySelector('#chats .fadeout-bottom');
        if (el) el.style.display = has ? '' : 'none';
    });
</script>

{#if paneCommunityId}
    <div use:paneInto={paneKey}></div>
{:else}
    {#each invites as invite (invite.community_id)}
        <div
            class="chatlist-invite-host"
            style="display:contents;"
            use:inviteInto={invite}
        ></div>
    {/each}
    {#each chats as chat (chat.id)}
        <ChatlistRow
            {h}
            {chat}
            pinned={snap.pinned.includes(h.chatPinKey(chat))}
            {version}
            {tick}
        />
    {/each}
    {#if empty}
        <div style="display:contents;" use:builtInto={h.buildChatlistEmptyState}></div>
        <div style="display:contents;" use:builtInto={h.buildChatlistIntro}></div>
    {/if}
{/if}

<!-- No <style>: global styles.css cascades; the DOM is byte-identical to the vanilla
     renderChatlistNow output (display:contents hosts keep #chat-list's child structure flat). -->
