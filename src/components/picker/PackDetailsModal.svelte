<script>
    // The pack details modal's body: loading, unavailable, or the pack with its logo,
    // count, description, thumbs and the Add / Remove action. The overlay element is
    // adopted (its close button and backdrop dismiss are the app's).
    import { packDetails, closePackDetails } from '../lib/packdetails.svelte.js';
    import { pickerPacks } from '../lib/picker.svelte.js';

    let { overlay, h } = $props();   // h: bindCachedImg, maxDisplay(), toggle(naddr, pack, subscribed) → Promise<'added'|'removed'|false>

    const d = $derived(packDetails());
    $effect(() => { overlay.hidden = !d; });
    const pack = $derived(d?.state === 'ok' ? d.pack : null);
    const emojis = $derived(pack && Array.isArray(pack.emojis) ? pack.emojis : []);
    const shown = $derived(emojis.slice(0, h.maxDisplay()));
    const subscribed = $derived(!!pack && pickerPacks().some(p => p.id === pack.id && !p.is_theme));
    const fallbackChar = $derived(pack ? (pack.title || pack.identifier || '?').trim().charAt(0).toUpperCase() : '');

    let logoLoaded = $state(false);
    $effect(() => { pack; logoLoaded = false; });
    function logo(img, url) {
        h.bindCachedImg(img, url, 'emoji_pack_icon');
        img.addEventListener('load', () => { logoLoaded = true; }, { once: true });
    }
    // Oversized or unavailable emoji are hidden entirely, as if not in the pack.
    let hidden = $state({});
    function thumb(img, e) { h.bindCachedImg(img, e.url, 'emoji', () => { hidden[e.url] = true; }); }

    let busy = $state(false);
    async function act() {
        if (busy || !pack) return;
        busy = true;
        try {
            const r = await h.toggle(d.naddr, pack, subscribed);
            if (r === 'added') closePackDetails();
        } finally { busy = false; }
    }
</script>

{#if d?.state === 'loading'}
    <div class="pack-details-loading">
        <div class="pack-details-spinner"></div>
        <p class="pack-details-loading-text">Loading pack…</p>
    </div>
{:else if d?.state === 'err'}
    <div class="pack-details-error">
        <p class="pack-details-error-title">Pack unavailable</p>
        <p class="pack-details-error-detail">{d.error || 'Failed to fetch'}</p>
    </div>
{:else if pack}
    <div class="pack-details-header">
        <div class="pack-details-logo" id="pack-details-logo" style:background-color={logoLoaded ? 'transparent' : null}>
            {#if !logoLoaded}<span class="pack-details-logo-fallback">{fallbackChar}</span>{/if}
            {#if pack.image_url}<img alt="" use:logo={pack.image_url}>{/if}
        </div>
        <div class="pack-details-title-block">
            <h3 class="pack-details-title">{pack.title || pack.identifier || 'Untitled'}</h3>
            <div class="pack-details-meta"><span class="emoji-count">{emojis.length}</span> Emoji{emojis.length === 1 ? '' : 's'}</div>
        </div>
    </div>
    {#if pack.description}<p class="pack-details-desc">{pack.description}</p>{/if}
    <div class="pack-details-grid" id="pack-details-grid">
        {#if shown.length === 0}
            <div class="pack-details-empty">Empty pack</div>
        {:else}
            {#each shown as e (e.url + e.shortcode)}
                {#if !hidden[e.url]}
                    <div class="pack-details-thumb"><img alt=":{e.shortcode}:" data-emoji-tooltip=":{e.shortcode}:" use:thumb={e}></div>
                {/if}
            {/each}
        {/if}
    </div>
    <button type="button" class="pack-details-action" class:is-subscribed={subscribed} id="pack-details-action" disabled={busy} onclick={act}>
        {busy ? (subscribed ? 'Removing…' : 'Adding…') : (subscribed ? 'Remove Pack' : 'Add Pack')}
    </button>
{/if}
