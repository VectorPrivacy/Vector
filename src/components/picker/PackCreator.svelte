<script>
    // The pack creator's editable surface: the logo button's face, the count, the emoji
    // cells (by position, so a reorder moves cells under the same gesture arbiter) and the
    // dropzone label. Uploads, naming, cropping and publishing stay the app's.
    import { creatorState, creatorEmojis, markCreatorBroken } from '../lib/packcreator.svelte.js';

    let { els, h } = $props();   // els: logo, count, dropzone, dropzoneLabel; h: bindCachedImg(img, url, kind, onUnavailable), unavailableMessage(reason), goneMessage(), isMobile(), dragActive(), remove(idx), cellClick(idx, broken), installReorder(cell, idx)

    const c = creatorState();
    const emojis = $derived(creatorEmojis());

    // ── the adopted chrome ──
    $effect(() => {
        els.count.textContent = `(${emojis.length}/${c.max})`;
        const atCap = emojis.length >= c.max;
        els.dropzone.classList.toggle('is-disabled', atCap);
        els.dropzoneLabel.textContent = atCap ? `Maximum ${c.max} reached` : 'Upload Emoji';
    });
    $effect(() => {
        const btn = els.logo;
        const { blobUrl, url, dead } = c.logo;
        btn.replaceChildren();
        btn.classList.toggle('has-image', !!(blobUrl || url));
        btn.classList.toggle('emoji-creator-cell-broken', !!(url && !blobUrl && dead));
        btn.title = url && !blobUrl && dead ? 'This pack icon is no longer available (it may have been deleted).' : '';
        if (blobUrl) {
            // A local preview of an in-progress upload: a blob: URL is safe to assign.
            const img = document.createElement('img'); img.alt = ''; img.src = blobUrl; btn.appendChild(img);
        } else if (url) {
            const img = document.createElement('img'); img.alt = ''; h.bindCachedImg(img, url, 'emoji_pack_icon'); btn.appendChild(img);
        } else {
            const span = document.createElement('span'); span.className = 'icon icon-image'; btn.appendChild(span);
        }
    });

    // ── cells ──
    let hovered = $state(-1);
    function image(img, [e, idx]) {
        if (e.blobUrl) { img.src = e.blobUrl; return; }
        // The editor must never hide a failed emoji: the creator has to see it's broken.
        h.bindCachedImg(img, e.url, 'emoji', (el, reason) => markCreatorBroken(idx, h.unavailableMessage(reason)));
        if (e.dead) markCreatorBroken(idx, h.goneMessage());
    }
    function reorder(cell, idx) { h.installReorder(cell, idx); }
    function brokenMessage(idx, e) { return c.broken[idx] || (e.dead ? h.goneMessage() : ''); }
</script>

{#each emojis as e, idx (idx)}
    {@const broken = brokenMessage(idx, e)}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions, a11y_mouse_events_have_key_events -->
    <div class="emoji-creator-cell" data-idx={idx} data-emoji-tooltip=":{e.shortcode}:"
         class:emoji-creator-cell-broken={!!broken} class:is-hovered={hovered === idx} title={broken || undefined}
         onmouseenter={() => { if (!h.dragActive()) hovered = idx; }} onmouseleave={() => { if (hovered === idx) hovered = -1; }}
         onclick={(ev) => { if (ev.target.closest('.emoji-creator-cell-remove')) return; if (ev.currentTarget.dataset.suppressClick === '1') { delete ev.currentTarget.dataset.suppressClick; return; } h.cellClick(idx, !!broken); }}
         use:reorder={idx}>
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
