<script>
    import { bindRailEl, railEls, railLayout, railFolderOpen } from '../lib/rail.svelte.js';
    import { dragGhost, nearestByCentre } from '../lib/reorder.js';
    import RailFolder from './RailFolder.svelte';
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
            if (!h.computeRowUnreadCount(chat)) continue;
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

    // ── arrangement ──
    // The stored layout painted over the live communities. A key this device cannot
    // resolve is skipped, never dropped; a community the layout has never seen joins
    // at the end, in the order the list found it.
    const cid = (chat) => chat.metadata?.custom_fields?.community_id;
    const rows = $derived.by(() => {
        const byKey = new Map();
        for (const chat of groups.spaces) byKey.set(cid(chat), chat);
        const out = [];
        const seen = new Set();
        for (const node of railLayout().nodes) {
            if (node.type === 'item') {
                const chat = byKey.get(node.key);
                if (chat && !seen.has(node.key)) { seen.add(node.key); out.push({ type: 'item', key: node.key, chat }); }
                continue;
            }
            const members = [];
            for (const key of node.keys || []) {
                if (seen.has(key)) continue;
                const chat = byKey.get(key);
                if (!chat) continue;
                seen.add(key);
                members.push(chat);
            }
            if (members.length) out.push({ type: 'folder', id: node.id, name: node.name || '', hue: node.hue ?? null, members });
        }
        for (const chat of groups.spaces) {
            const key = cid(chat);
            if (!seen.has(key)) { seen.add(key); out.push({ type: 'item', key, chat }); }
        }
        return out;
    });

    // ── drag to rearrange ──
    // The strip owns what a drag paints (armed row, dragged row, drop mark); the ghost is
    // the gesture's own. Rows are named 'i:<community>' and folder heads 'f:<folder>'.
    const els = new Map();   // row name → { el, key?, folder? }
    let armedId = $state(null);
    let draggingId = $state(null);
    let drop = $state(null);   // { id, mode: 'above' | 'below' | 'combine', target }
    let ghost = null;

    const rowName = (src) => (src.kind === 'folder' ? `f:${src.id}` : `i:${src.key}`);
    const anchorOf = (row) => (row.type === 'folder' ? { kind: 'folder', id: row.id } : { kind: 'item', key: row.key });

    /** Every row a drop can land on, top to bottom, with where it sits in the layout. */
    function targets() {
        const out = [];
        rows.forEach((row, top) => {
            if (row.type === 'item') {
                const t = els.get(`i:${row.key}`);
                if (t?.el.isConnected) out.push({ id: `i:${row.key}`, el: t.el, kind: 'item', key: row.key, top });
                return;
            }
            const head = els.get(`f:${row.id}`);
            if (head?.el.isConnected) out.push({ id: `f:${row.id}`, el: head.el, kind: 'folder', folder: row.id, top });
            if (!railFolderOpen(row.id)) return;
            row.members.forEach((chat, i) => {
                const t = els.get(`i:${cid(chat)}`);
                const next = row.members[i + 1];
                if (t?.el.isConnected) out.push({ id: `i:${cid(chat)}`, el: t.el, kind: 'child', key: cid(chat), folder: row.id, next: next ? cid(next) : null, top });
            });
        });
        return out;
    }

    /** Where a drag held at `y` would land, or null where letting go would change nothing. */
    function resolve(src, y) {
        const list = targets();
        const hit = nearestByCentre(list.map((t) => ({ key: t.id, el: t.el })), 0, y, 'y');
        if (!hit) return null;
        const t = list.find((x) => x.id === hit.key);
        if (t.id === rowName(src)) return null;
        const r = t.el.getBoundingClientRect();
        const f = Math.min(1, Math.max(0, (y - r.top) / r.height));
        const nextTop = rows[t.top + 1];
        const below = () => (nextTop ? { at: 'before', anchor: anchorOf(nextTop) } : { at: 'end' });
        const above = { at: 'before', anchor: anchorOf(rows[t.top]) };

        if (src.kind === 'folder') {
            if (t.kind === 'child') return null;   // a folder never nests
            return f < 0.5 ? { id: t.id, mode: 'above', target: above } : { id: t.id, mode: 'below', target: below() };
        }
        if (t.kind === 'item') {
            if (f < 0.3) return { id: t.id, mode: 'above', target: above };
            if (f > 0.7) return { id: t.id, mode: 'below', target: below() };
            return { id: t.id, mode: 'combine', target: { at: 'combine', with_key: t.key } };
        }
        if (t.kind === 'folder') {
            if (f < 0.3) return { id: t.id, mode: 'above', target: above };
            const row = rows[t.top];
            const first = railFolderOpen(t.folder) ? cid(row.members[0]) : null;
            return { id: t.id, mode: 'combine', target: { at: 'into-folder', folder_id: t.folder, ...(first ? { before_key: first } : {}) } };
        }
        // Inside an open folder. The bottom of its last row is the way back out.
        if (!t.next && f > 0.8) return { id: t.id, mode: 'below', target: below() };
        if (f < 0.5) return { id: t.id, mode: 'above', target: { at: 'into-folder', folder_id: t.folder, before_key: t.key } };
        return { id: t.id, mode: 'below', target: { at: 'into-folder', folder_id: t.folder, ...(t.next ? { before_key: t.next } : {}) } };
    }

    // Auto-scroll while the drag sits at (or past) the strip's edges, as the pack rail does.
    const ZONE_PX = 18;
    const MAX_PX = 9;
    let autoRaf = null;
    let dragY = 0;
    let dragSrc = null;
    function autoTick() {
        autoRaf = null;
        const rail = railEls().spacesRows;
        if (!rail || !dragSrc) return;
        const r = rail.getBoundingClientRect();
        let v = 0;
        if (dragY < r.top + ZONE_PX) v = -Math.min(MAX_PX, (r.top + ZONE_PX - dragY) * 0.25);
        else if (dragY > r.bottom - ZONE_PX) v = Math.min(MAX_PX, (dragY - (r.bottom - ZONE_PX)) * 0.25);
        if (!v) return;
        const before = rail.scrollTop;
        rail.scrollTop = before + v;
        if (rail.scrollTop === before) return;
        drop = resolve(dragSrc, dragY);
        autoRaf = requestAnimationFrame(autoTick);
    }
    function autoScroll(y) { dragY = y; if (!autoRaf) autoRaf = requestAnimationFrame(autoTick); }
    function autoStop() { if (autoRaf) { cancelAnimationFrame(autoRaf); autoRaf = null; } }

    /** The communities as painted, which is what a first drag freezes into the layout. */
    function live() {
        const out = [];
        for (const row of rows) {
            if (row.type === 'item') out.push(row.key);
            else for (const chat of row.members) out.push(cid(chat));
        }
        return out;
    }

    const drag = {
        el(node, info) { els.set(info.key ? `i:${info.key}` : `f:${info.folder}`, { el: node, ...info }); },
        mark: (id) => (drop?.id === id ? drop.mode : null),
        armed: (id) => armedId === id,
        dragging: (id) => draggingId === id,
        press(src, onMenu) {
            const id = rowName(src);
            return {
                onMenu: onMenu || ((x, y) => {
                    const chat = groups.spaces.find((c) => cid(c) === src.key);
                    if (chat) h.openCommunityMenu(chat, x, y);
                }),
                closeMenu: () => h.closeMenu(),
                onArm: (on) => { armedId = on ? id : (armedId === id ? null : armedId); },
                onDragStart: (ev, node) => {
                    draggingId = id;
                    dragSrc = src;
                    ghost = dragGhost(node, 'ws-rail-ghost', (g) => g.querySelector('.ws-rail-folder-body')?.remove());
                },
                onDragMove: (mv) => { ghost?.move(mv.clientX, mv.clientY); drop = resolve(src, mv.clientY); autoScroll(mv.clientY); },
                onDragEnd: (up) => {
                    autoStop();
                    const landed = resolve(src, up.clientY);
                    draggingId = null;
                    dragSrc = null;
                    ghost?.remove();
                    ghost = null;
                    drop = null;
                    if (landed) h.railDrop(src, landed.target, live());
                },
            };
        },
    };

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
        {#each rows as row (row.type === 'folder' ? `f:${row.id}` : `i:${row.key}`)}
            {#if row.type === 'folder'}
                <RailFolder {h} folder={row} {activeId} {drag} />
            {:else}
                <RailItem
                    {h}
                    chat={row.chat}
                    isCommunity={true}
                    active={activeId === row.chat.id}
                    press={drag.press({ kind: 'item', key: row.key }, null)}
                    mark={drag.mark(`i:${row.key}`)}
                    armed={drag.armed(`i:${row.key}`)}
                    dragging={drag.dragging(`i:${row.key}`)}
                    el={(node) => drag.el(node, { key: row.key })}
                />
            {/if}
        {/each}
    </div>
</div>

<!-- No <style>: widescreen.css's .ws-rail-* rules cascade; the DOM matches the vanilla strip. -->
