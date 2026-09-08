<script>
    // An emoji pack shared in a message: skeleton while the relay fetch runs, then thumbs
    // (the app's canvas grid), logo, title, count, description and the copy / add-remove
    // actions. The Add / Remove label follows the equipped packs, so a subscription made
    // anywhere else repaints it.
    import { pickerPacks } from '../lib/picker.svelte.js';

    let { naddr, h } = $props();
    // h: resolve(naddr) → Promise<{state, pack?, error?}>, bindCachedImg, mountPreviewGrid(left, pack) → destroy,
    //    copyShareLink(naddr), toggle(naddr, pack, subscribed) → Promise<boolean>, onResized(card)

    let result = $state(null);
    let card = $state(null);
    // Mount-time: a card is born for one naddr.
    // svelte-ignore state_referenced_locally
    h.resolve(naddr).then((r) => { result = r; });
    $effect(() => { if (result && card) h.onResized(card); });

    const pack = $derived(result?.state === 'ok' ? result.pack : null);
    // Theme packs are pinned, not subscriptions.
    const subscribed = $derived(!!pack && pickerPacks().some(p => p.id === pack.id && !p.is_theme));
    const count = $derived(pack ? pack.emojis.length : 0);

    let copied = $state(false);
    async function copy(ev) {
        ev.stopPropagation();
        if (await h.copyShareLink(naddr)) { copied = true; setTimeout(() => { copied = false; }, 1500); }
    }
    let busy = $state(false);
    async function toggle(ev) {
        ev.stopPropagation();
        if (busy || !pack) return;
        busy = true;
        try { await h.toggle(naddr, pack, subscribed); } finally { busy = false; }
    }
    function thumbs(left, p) { return { destroy: h.mountPreviewGrid(left, p) }; }
    function logo(img, url) { h.bindCachedImg(img, url, 'emoji_pack_icon'); }
</script>

<div class="emoji-pack-preview" data-naddr={naddr} data-pack-id={pack?.id} class:is-loading={!result} class:is-error={result?.state === 'err'} bind:this={card}>
    <div class="emoji-pack-preview-grid" class:is-canvas={!!pack && count > 0}>
        {#if !result}
            {#each Array(6) as _, i (i)}<div class="pack-skel pack-skel-thumb"></div>{/each}
        {:else if pack}
            {#if count === 0}
                <div class="emoji-pack-preview-empty">Empty pack</div>
            {:else}
                <div use:thumbs={pack}></div>
            {/if}
        {/if}
    </div>
    <div class="emoji-pack-preview-meta">
        {#if !result}
            <div class="emoji-pack-preview-title-row"><div class="pack-skel pack-skel-logo"></div><div class="pack-skel pack-skel-title"></div></div>
            <div class="pack-skel pack-skel-sub"></div>
            <div class="pack-skel pack-skel-actions"></div>
        {:else if result.state === 'err'}
            <div class="emoji-pack-preview-title-row"><span class="emoji-pack-preview-title">Pack unavailable</span></div>
            <div class="emoji-pack-preview-desc">{result.error || 'Failed to fetch'}</div>
        {:else}
            <div class="emoji-pack-preview-title-row">
                {#if pack.image_url}<img class="emoji-pack-preview-logo" alt="" use:logo={pack.image_url}>{/if}
                <span class="emoji-pack-preview-title">{pack.title || pack.identifier}</span>
            </div>
            <div class="emoji-pack-preview-sub">{count} emoji{count === 1 ? '' : 's'}</div>
            {#if pack.description}<div class="emoji-pack-preview-desc">{pack.description}</div>{/if}
            <div class="emoji-pack-preview-actions">
                <button class="btn emoji-pack-preview-copy" title="Copy share link" onclick={copy}><span class="icon" class:icon-copy={!copied} class:icon-check={copied}></span></button>
                <button class="btn emoji-pack-preview-add" class:is-subscribed={subscribed} data-pack-id={pack.id} disabled={busy} onclick={toggle}>
                    {busy ? (subscribed ? 'Removing…' : 'Adding…') : (subscribed ? 'Remove' : 'Add Pack')}
                </button>
            </div>
        {/if}
    </div>
</div>
