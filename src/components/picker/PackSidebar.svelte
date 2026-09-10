<script>
    // The picker's rail: Recents and All, one tab per equipped pack (in the store's order),
    // and the "+" creator slot last. Tabs are keyed by pack id so a reorder moves elements
    // rather than rebuilding them, and each keeps the gesture arbiter the app installs.
    import { pickerState, pickerPacks } from '../lib/picker.svelte.js';
    import { reorderable, dragGhost, nearestByCentre } from '../lib/reorder.js';

    let { h } = $props();   // h: bindCachedImg(img, url, kind), packIsDead(pack), packInitial(pack), openCreator(),
                            //    showTabMenu(pack, x, y), closeMenu(), reorderPack(fromId, toId, isBefore)

    const st = pickerState();
    const packs = $derived(pickerPacks());

    function icon(img, url) { h.bindCachedImg(img, url, 'emoji_pack_icon'); }

    // ── drag to reorder ──
    // The rail owns what the gesture paints: which tab is armed, which is being dragged,
    // and where the drop marker sits. The ghost under the pointer is the gesture's own.
    const tabEls = new Map();   // pack id → element, for hit tests
    function tabEl(node, id) { tabEls.set(id, node); return { destroy() { tabEls.delete(id); } }; }
    let armed = $state(null);
    let dragging = $state(null);
    let drop = $state(null);    // { key: pack id, before }
    let ghost = null;
    const tabs = () => packs.map(p => ({ key: p.id, el: tabEls.get(p.id) })).filter(t => t.el);
    // Before/after by the pointer's Y against the nearest tab's midpoint (vertical distance only).
    const resolve = (y) => nearestByCentre(tabs(), 0, y, 'y');

    // Auto-scroll while the drag sits at (or past) the rail's vertical edges, so a long rail
    // is traversed in one drag. Speed follows the overshoot; the loop ends when the pointer
    // is back inside the band, the rail hits its end, or the drag ends.
    const ZONE_PX = 14;
    const MAX_PX = 9;   // per frame
    let autoRaf = null;
    let dragY = 0;
    function autoTick() {
        autoRaf = null;
        const rail = tabs()[0]?.el.parentElement;
        if (!rail || dragging === null) return;
        const r = rail.getBoundingClientRect();
        let v = 0;
        if (dragY < r.top + ZONE_PX) v = -Math.min(MAX_PX, (r.top + ZONE_PX - dragY) * 0.25);
        else if (dragY > r.bottom - ZONE_PX) v = Math.min(MAX_PX, (dragY - (r.bottom - ZONE_PX)) * 0.25);
        if (!v) return;
        const before = rail.scrollTop;
        rail.scrollTop = before + v;
        if (rail.scrollTop === before) return;   // rail end reached
        drop = resolve(dragY);   // the tabs moved under a still pointer
        autoRaf = requestAnimationFrame(autoTick);
    }
    function autoScroll(y) { dragY = y; if (!autoRaf) autoRaf = requestAnimationFrame(autoTick); }
    function autoStop() { if (autoRaf) { cancelAnimationFrame(autoRaf); autoRaf = null; } }

    function gestures(pack) {
        return {
            onMenu: (x, y) => h.showTabMenu(pack, x, y),
            closeMenu: () => h.closeMenu(),
            onArm: (on) => { armed = on ? pack.id : (armed === pack.id ? null : armed); },
            onDragStart: (ev, node) => { dragging = pack.id; ghost = dragGhost(node, 'emoji-pack-tab-ghost'); },
            onDragMove: (mv) => { ghost?.move(mv.clientX, mv.clientY); drop = resolve(mv.clientY); autoScroll(mv.clientY); },
            onDragEnd: (up) => {
                autoStop();
                tabEls.get(pack.id)?.setAttribute('data-suppress-click', '1');
                dragging = null;
                ghost?.remove(); ghost = null;
                const t = resolve(up.clientY);
                drop = null;
                if (t) h.reorderPack(pack.id, t.key, t.before);
            },
        };
    }
</script>

<button class="emoji-category-btn" class:active={st.active === 'recents'} data-category="recents" aria-label="Recently used">
    <span class="icon icon-clock"></span>
</button>
<button class="emoji-category-btn" class:active={st.active === 'all'} data-category="all" aria-label="All emojis">
    <span class="icon icon-smile-face"></span>
</button>
{#each packs as pack (pack.id)}
    <button class="emoji-category-btn emoji-pack-tab" class:active={st.active === pack.id}
            class:emoji-pack-tab-dead={h.packIsDead(pack)} class:emoji-pack-tab-letter={!pack.image_url}
            class:is-drag-armed={armed === pack.id} class:is-dragging={dragging === pack.id}
            class:drop-above={drop?.key === pack.id && drop.before} class:drop-below={drop?.key === pack.id && !drop.before}
            data-pack-id={pack.id} data-theme-slot={pack._isThemeSlot ? '1' : undefined}
            title={pack.title || pack.identifier} use:tabEl={pack.id} use:reorderable={gestures(pack)}>
        {#if pack.image_url}
            <!-- No native image drag: a slow press must start the tab reorder, not grab the icon. -->
            <img alt="" draggable="false" use:icon={pack.image_url}>
        {:else}
            <!-- A <div>, not a <span>: the picker's span rule forces every span to a 30px circle. -->
            <div class="emoji-pack-tab-letter-plate">{h.packInitial(pack)}</div>
        {/if}
    </button>
{/each}
<button class="emoji-category-btn emoji-pack-tab-create" title="Create new pack" onclick={(e) => { e.stopPropagation(); h.openCreator(); }}>
    <span class="icon icon-plus-circle"></span>
</button>
