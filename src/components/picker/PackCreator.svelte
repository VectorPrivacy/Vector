<script>
    // The pack creator: the head (logo, name, count, save-and-exit, delete), the emoji
    // cells (by position, so a reorder moves cells under the same gesture arbiter) and
    // the dropzone. Uploads, naming, cropping and publishing stay the app's.
    import { untrack } from 'svelte';
    import { creatorState, creatorEmojis, markCreatorBroken } from '../lib/packcreator.svelte.js';
    import { reorderable, dragGhost, nearestByCentre } from '../lib/reorder.js';

    let { h } = $props();
    // h: bindCachedImg(img, url, kind, onUnavailable), unavailableMessage(reason), goneMessage(), isMobile(),
    //    remove(idx), cellClick(idx, broken), cellMenu(idx, x, y), reorderEmoji(from, targetIdx, isBefore),
    //    gridMounted(el), pickLogo(), nameInput(value), done(), deletePack(), addFiles(files)

    const c = creatorState();
    const emojis = $derived(creatorEmojis());
    const atCap = $derived(emojis.length >= c.max);
    const logoBroken = $derived(!!(c.logo.url && !c.logo.blobUrl && c.logo.dead));

    let nameEl = $state(null);
    $effect(() => { if (c.focusSeq) untrack(() => setTimeout(() => nameEl?.focus(), 50)); });

    function logoImg(img, url) { h.bindCachedImg(img, url, 'emoji_pack_icon'); }
    let gridEl = $state(null);
    function grid(el) { h.gridMounted(el); }

    // ── dropzone ──
    let dragover = $state(false);
    const dzDisabled = $derived(c.saving || atCap);
    async function pick() { if (!dzDisabled) h.addFiles(await h.pickImages()); }
    function drop(e) {
        e.preventDefault();
        dragover = false;
        if (!dzDisabled) h.addFiles(e.dataTransfer && e.dataTransfer.files);
    }

    // ── cells ──
    let hovered = $state(-1);
    function image(img, [e, idx]) {
        if (e.blobUrl) { img.src = e.blobUrl; return; }
        // The editor must never hide a failed emoji: the creator has to see it's broken.
        h.bindCachedImg(img, e.url, 'emoji', (el, reason) => markCreatorBroken(idx, h.unavailableMessage(reason)));
        if (e.dead) markCreatorBroken(idx, h.goneMessage());
    }

    // ── drag to reorder ──
    // The grid owns what the gesture paints: the armed cell, the dragged cell and the drop
    // marker. The ghost under the pointer is the gesture's own.
    const cellEls = new Map();   // idx → element
    function cellEl(node, idx) { cellEls.set(idx, node); return { update(n) { cellEls.delete(idx); idx = n; cellEls.set(idx, node); }, destroy() { if (cellEls.get(idx) === node) cellEls.delete(idx); } }; }
    let armed = $state(-1);
    let dragging = $state(-1);
    let dropAt = $state(null);   // { key: idx, before }
    let ghost = null;
    // Confined to the grid so a drop on the dropzone or footer never snaps to a phantom slot;
    // then the cell whose centre is nearest, before/after by the pointer's x against its midpoint.
    function resolve(x, y) {
        if (!gridEl) return null;
        const r = gridEl.getBoundingClientRect();
        if (x < r.left || x > r.right || y < r.top || y > r.bottom) return null;
        return nearestByCentre([...cellEls].map(([key, el]) => ({ key, el })), x, y);
    }
    function gestures(idx) {
        return {
            ignore: (ev) => !!ev.target.closest('.emoji-creator-cell-remove'),   // the remove-× is its own target
            onMenu: (x, y) => h.cellMenu(idx, x, y),
            onArm: (on) => { armed = on ? idx : (armed === idx ? -1 : armed); },
            onDragStart: (ev, node) => {
                dragging = idx;
                hovered = -1;   // the gesture owns the cell's look now
                ghost = dragGhost(node, 'emoji-creator-cell-ghost', (g) => g.querySelector('.emoji-creator-cell-remove')?.remove());
            },
            onDragMove: (mv) => { ghost?.move(mv.clientX, mv.clientY); dropAt = resolve(mv.clientX, mv.clientY); },
            onDragEnd: (up) => {
                cellEls.get(idx)?.setAttribute('data-suppress-click', '1');
                dragging = -1;
                ghost?.remove(); ghost = null;
                const t = resolve(up.clientX, up.clientY);
                dropAt = null;
                if (t) h.reorderEmoji(idx, t.key, t.before);
            },
        };
    }
    function brokenMessage(idx, e) { return c.broken[idx] || (e.dead ? h.goneMessage() : ''); }
</script>

<div class="emoji-creator-head">
    <button type="button" class="emoji-creator-logo" id="emoji-creator-logo" aria-label="Pack logo"
            class:has-image={!!(c.logo.blobUrl || c.logo.url)} class:emoji-creator-cell-broken={logoBroken}
            title={logoBroken ? 'This pack icon is no longer available (it may have been deleted).' : ''}
            onclick={() => h.pickLogo()}>
        {#if c.logo.blobUrl}
            <!-- A local preview of an in-progress upload: a blob: URL is safe to assign. -->
            <img alt="" src={c.logo.blobUrl}>
        {:else if c.logo.url}
            {#key c.logo.url}<img alt="" use:logoImg={c.logo.url}>{/key}
        {:else}
            <span class="icon icon-image"></span>
        {/if}
    </button>
    <input type="text" class="emoji-creator-name" id="emoji-creator-name" bind:this={nameEl}
           placeholder="Pack name" maxlength="26" autocomplete="off"
           autocorrect="off" autocapitalize="off" spellcheck="false"
           bind:value={c.name} oninput={() => h.nameInput(c.name)} disabled={c.saving}>
    <span class="emoji-creator-count" id="emoji-creator-count">({emojis.length}/{c.max})</span>
    <button type="button" class="emoji-creator-done" id="emoji-creator-done" aria-label="Save and exit" title="Save Pack" disabled={c.saving} onclick={() => h.done()}>
        <span class="icon" class:icon-edit={!c.saving} class:emoji-creator-done-spinner={c.saving}></span>
    </button>
    <button type="button" class="emoji-creator-delete btn" id="emoji-creator-delete" aria-label="Delete or discard pack" title="Remove Pack" disabled={c.saving} onclick={() => h.deletePack()}>
        <span class="icon icon-trash"></span>
    </button>
</div>
<!-- `is-saving`: CSS freezes reorder, hover and rename while the publish is in flight. -->
<div class="emoji-creator-grid" id="emoji-creator-grid" class:is-saving={c.saving} bind:this={gridEl} use:grid>
    {#each emojis as e, idx (idx)}
        {@const broken = brokenMessage(idx, e)}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions, a11y_mouse_events_have_key_events -->
        <div class="emoji-creator-cell" data-idx={idx} data-emoji-tooltip=":{e.shortcode}:"
             class:emoji-creator-cell-broken={!!broken} class:is-hovered={hovered === idx} title={broken || undefined}
             class:is-drag-armed={armed === idx} class:is-dragging={dragging === idx}
             class:drop-before={dropAt?.key === idx && dropAt.before} class:drop-after={dropAt?.key === idx && !dropAt.before}
             onmouseenter={() => { if (dragging === -1) hovered = idx; }} onmouseleave={() => { if (hovered === idx) hovered = -1; }}
             onclick={(ev) => { if (ev.target.closest('.emoji-creator-cell-remove')) return; if (ev.currentTarget.dataset.suppressClick === '1') { delete ev.currentTarget.dataset.suppressClick; return; } h.cellClick(idx, !!broken); }}
             use:cellEl={idx} use:reorderable={gestures(idx)}>
            <img alt=":{e.shortcode}:" draggable="false" use:image={[e, idx]}>
            {#if !h.isMobile()}
                <!-- Hover-revealed on desktop; touch reaches delete through the long-press menu. -->
                <button type="button" class="emoji-creator-cell-remove" aria-label="Remove emoji" onclick={(ev) => { ev.stopPropagation(); h.remove(idx); }}><span class="icon icon-x"></span></button>
            {/if}
            {#if c.busy[idx]}
                <div class="emoji-creator-cell-busy is-{c.busy[idx]}"><div class="emoji-creator-cell-busy-ring"></div></div>
            {/if}
        </div>
    {/each}
</div>
<button type="button" class="emoji-creator-dropzone" id="emoji-creator-dropzone" class:is-disabled={dzDisabled} class:is-dragover={dragover}
        onclick={pick} ondragover={(e) => { e.preventDefault(); dragover = true; }} ondragleave={() => { dragover = false; }} ondrop={drop}>
    <span class="icon icon-plus-circle"></span>
    <span class="emoji-creator-dropzone-label">{atCap ? `Maximum ${c.max} reached` : 'Upload Emoji'}</span>
</button>
