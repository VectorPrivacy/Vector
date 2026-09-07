<script>
    // The picker's rail: Recents and All, one tab per equipped pack (in the store's order),
    // and the "+" creator slot last. Tabs are keyed by pack id so a reorder moves elements
    // rather than rebuilding them, and each keeps the gesture arbiter the app installs.
    import { pickerState, pickerPacks } from '../lib/picker.svelte.js';

    let { h } = $props();   // h: bindCachedImg(img, url, kind), installTabGestures(tab, pack), packIsDead(pack), packInitial(pack), openCreator()

    const st = pickerState();
    const packs = $derived(pickerPacks());

    function icon(img, url) { h.bindCachedImg(img, url, 'emoji_pack_icon'); }
    function gestures(tab, pack) { h.installTabGestures(tab, pack); }
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
            data-pack-id={pack.id} data-theme-slot={pack._isThemeSlot ? '1' : undefined}
            title={pack.title || pack.identifier} use:gestures={pack}>
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
