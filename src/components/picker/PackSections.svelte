<script>
    // One section per equipped pack, in the rail's order: header (logo, title, count,
    // chevron, the pencil on your own packs), the app's canvas grid, and the dead-pack
    // notice. Keyed by pack id, so a reorder moves sections and their decoded frames.
    import { tick } from 'svelte';
    import { pickerState, pickerPacks } from '../lib/picker.svelte.js';

    let { h } = $props();   // h: PickerIslandHelpers (js/picker.js)

    const st = pickerState();
    const packs = $derived(pickerPacks());

    function logo(img, url) { h.bindCachedImg(img, url, 'emoji_pack_icon'); }
    function menu(el, pack) { h.packMenu(el, pack); }
    // The grid follows its pack's emoji: a pack that arrives fuller later (the same id, a
    // new object) rebuilds the canvas, so the drawn cells and the section's size agree. A
    // reload that changes nothing keeps the decoded frames.
    function sameEmojis(a, b) {
        const x = a.emojis || [], y = b.emojis || [];
        return x.length === y.length && x.every((e, i) => e.url === y[i].url && e.shortcode === y[i].shortcode);
    }
    function grid(section, pack) {
        let current = pack;
        let teardown = h.mountGrid(section, pack);
        return {
            update(next) {
                if (next === current) return;
                const rebuild = !sameEmojis(next, current);
                current = next;
                if (!rebuild) return;
                teardown();
                teardown = h.mountGrid(section, next);
            },
            destroy() { teardown(); },
        };
    }
    // The canvases arm and the header chrome calibrates once the sections are in the DOM.
    $effect(() => { packs; tick().then(() => h.afterRender()); });
</script>

{#each packs as pack (pack.id)}
    {@const dead = h.packIsDead(pack)}
    <div class="emoji-section emoji-pack-section" data-pack-id={pack.id}
         class:emoji-pack-dead={dead} class:emoji-pack-dead-noblur={dead && h.isLinux()}
         style:contain-intrinsic-size="0 {(st.chromeSeq, h.sectionHeight(pack))}px" use:grid={pack}>
        <div class="emoji-section-header">
            {#if pack.image_url}
                <img class="emoji-pack-logo" alt="" use:logo={pack.image_url} use:menu={pack}>
            {/if}
            <span class="header-text" use:menu={pack}>{pack.title || pack.identifier}</span>
            <span class="emoji-pack-count">({Array.isArray(pack.emojis) ? pack.emojis.length : 0})</span>
            <span class="icon icon-chevron-down"></span>
            {#if pack.is_own}
                <button type="button" class="emoji-pack-edit-pencil" data-pack-id={pack.id} aria-label="Edit pack" title="Edit Pack"
                        onclick={(e) => { e.stopPropagation(); h.openCreator(pack.id); }}><span class="icon icon-edit"></span></button>
            {/if}
        </div>
        {#if dead}
            <!-- The emojis stay, blurred behind the reason; nothing is ripped away silently. -->
            <div class="emoji-pack-dead-notice">
                <span class="emoji-pack-dead-text">{h.deadMessage(pack)}</span>
                <span class="emoji-pack-dead-subtext">Old messages will still show its emojis.</span>
                {#if !pack.is_theme}
                    <button type="button" class="btn emoji-pack-dead-remove" onclick={(e) => { e.stopPropagation(); h.unsubscribe(pack); }}>Remove Pack</button>
                {/if}
            </div>
        {/if}
    </div>
{/each}
