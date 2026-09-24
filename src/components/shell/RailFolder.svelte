<script>
    // A folder on the rail: a tile of its communities' avatars while shut, and those
    // communities stacked beneath it, on a band of the folder's colour, while open.
    // Shut, it carries its members' unread so nothing inside goes quiet unnoticed.
    import { chatVersion, communityVersion } from '../lib/signals.svelte.js';
    import { railFolderOpen, toggleRailFolder } from '../lib/rail.svelte.js';
    import { reorderable } from '../lib/reorder.js';
    import Avatar from '../ui/Avatar.svelte';
    import RailItem from './RailItem.svelte';

    // folder: { id, name, hue, members: chat[] }. drag: the strip's per-row drag props.
    let { h, folder, activeId, drag } = $props();   // h: RailHelpers (js/render/rail.js)

    const open = $derived(railFolderOpen(folder.id));

    const vm = $derived.by(() => {
        let unread = 0;
        let pings = 0;
        let allMuted = true;
        const tiles = [];
        for (const chat of folder.members) {
            chatVersion(chat.id);
            const communityId = chat.metadata?.custom_fields?.community_id;
            if (communityId) communityVersion(communityId);
            unread += h.computeListRowUnreadCount(chat);
            pings += h.computeCommunityPingCount(chat);
            if (!chat.community_muted) allMuted = false;
            if (tiles.length < 4) {
                tiles.push({
                    id: chat.id,
                    src: chat.metadata?.avatar_cached ? h.convertFileSrc(chat.metadata.avatar_cached) : null,
                });
            }
        }
        const names = folder.members.map((c) => c.metadata?.custom_fields?.name || 'Community');
        return { unread, pings, allMuted, tiles, label: folder.name || names.join(', ') };
    });

    const holdsActive = $derived(folder.members.some((c) => c.id === activeId));
    const color = $derived(folder.hue == null ? null : `hsl(${folder.hue} 65% 62%)`);

    function toggle(e) {
        toggleRailFolder(folder.id);
    }

    function head(node, g) {
        drag.el(node, { folder: folder.id });
        return reorderable(node, g);
    }
</script>

<div class="ws-rail-folder" class:is-open={open} style:--folder-color={color}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div
        class="ws-rail-item ws-rail-folder-head btn"
        class:is-quiet={!vm.unread}
        class:is-muted={vm.allMuted}
        class:active={!open && holdsActive}
        class:is-drag-armed={drag.armed(`f:${folder.id}`)}
        class:is-dragging={drag.dragging(`f:${folder.id}`)}
        class:drop-above={drag.mark(`f:${folder.id}`) === 'above'}
        class:drop-below={drag.mark(`f:${folder.id}`) === 'below'}
        class:drop-combine={drag.mark(`f:${folder.id}`) === 'combine'}
        title={vm.label}
        use:head={drag.press({ kind: 'folder', id: folder.id }, (x, y) => h.openFolderMenu(folder, x, y))}
        onclick={toggle}
    >
        <span class="ws-rail-folder-tile" class:is-open={open}>
            <span class="ws-rail-folder-glyph"><span class="icon icon-folder"></span></span>
            <span class="ws-rail-folder-grid">
                {#each vm.tiles as t (t.id)}
                    <Avatar src={t.src} size={11} group={true} class="ws-rail-folder-mini" />
                {/each}
            </span>
        </span>
        <span class="ws-rail-item-name cutoff">{vm.label}</span>
        {#if !open && (vm.pings || vm.unread)}
            <span class="ws-rail-item-badge" class:is-dot={!vm.pings} class:muted={vm.allMuted && !!vm.pings}>
                {vm.pings ? (vm.pings > 99 ? '99+' : String(vm.pings)) : ''}
            </span>
        {/if}
    </div>
    <div class="ws-rail-folder-body">
        <div class="ws-rail-folder-inner">
            {#each folder.members as chat (chat.id)}
                <RailItem
                    {h}
                    {chat}
                    isCommunity={true}
                    active={activeId === chat.id}
                    press={drag.press({ kind: 'item', key: chat.metadata?.custom_fields?.community_id }, null)}
                    mark={drag.mark(`i:${chat.metadata?.custom_fields?.community_id}`)}
                    armed={drag.armed(`i:${chat.metadata?.custom_fields?.community_id}`)}
                    dragging={drag.dragging(`i:${chat.metadata?.custom_fields?.community_id}`)}
                    el={(node) => drag.el(node, { folder: folder.id, key: chat.metadata?.custom_fields?.community_id })}
                />
            {/each}
        </div>
    </div>
</div>
