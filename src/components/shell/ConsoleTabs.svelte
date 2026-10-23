<script>
    // The list's filters in the slim layout. A dot marks a tab with something unread
    // behind it; the tab you're on needs none, and All holds every other one's.
    import { listVersion, invitesVersion, chatVersion, communityVersion } from '../lib/signals.svelte.js';
    import { consoleFilters, consoleFilter, setConsoleFilter } from '../lib/console.svelte.js';

    let { h, snapshot, onPick = () => {} } = $props();   // h: ChatlistHelpers (js/render/chatlist/list.js)

    const LABELS = { all: 'All', messages: 'Messages', communities: 'Communities', requests: 'Requests' };

    const filters = $derived(consoleFilters());
    const active = $derived(consoleFilter());

    const dots = $derived.by(() => {
        listVersion();
        invitesVersion();
        const { chats, invites } = snapshot();
        const out = { all: false, messages: false, communities: false, requests: invites.length > 0 };
        for (const chat of chats) {
            if (!h.chatIsVisibleInList(chat)) continue;
            chatVersion(chat.id);
            const group = h.chatIsGroup(chat);
            if (group) communityVersion(chat.metadata?.custom_fields?.community_id);
            if (!h.computeListRowUnreadCount(chat)) continue;
            if (group) out.communities = true;
            else if (h.chatIsRequest(chat)) out.requests = true;
            else out.messages = true;
        }
        return out;
    });

    function pick(filter) {
        if (filter !== active) setConsoleFilter(filter);
        onPick(filter);
    }

    // Four tabs size to their words, so the underline is measured off the one it marks,
    // and again whenever the row's width moves them.
    const tabEls = {};
    let rowEl = $state(null);
    let bar = $state({ left: 0, width: 0, ready: false });
    let placed = false;   // plain: the effect below writes the bar, so it mustn't read it
    function measure() {
        const el = tabEls[active];
        if (!el?.isConnected) return;
        bar = { left: el.offsetLeft, width: el.offsetWidth, ready: placed };
        placed = el.offsetWidth > 0;
    }
    $effect(() => {
        active;
        filters;
        measure();
    });
    $effect(() => {
        if (!rowEl) return;
        const ro = new ResizeObserver(measure);
        ro.observe(rowEl);
        return () => ro.disconnect();
    });
    function tab(node, filter) {
        tabEls[filter] = node;
        return { destroy() { if (tabEls[filter] === node) delete tabEls[filter]; } };
    }
</script>

<div id="cc-tabs" class="cc-tabs" class:is-many={filters.length > 2} role="tablist" bind:this={rowEl}>
    {#each filters as filter (filter)}
        <button class="cc-tab" class:active={filter === active} role="tab" aria-selected={filter === active} use:tab={filter} onclick={() => pick(filter)}>
            <span class="cc-tab-label">{LABELS[filter]}{#if dots[filter] && filter !== active}<span class="cc-tab-dot"></span>{/if}</span>
        </button>
    {/each}
    <span class="cc-tab-bar" class:ready={bar.ready} aria-hidden="true" style:left="{bar.left}px" style:width="{bar.width}px"></span>
</div>
