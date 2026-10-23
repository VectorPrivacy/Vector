<script>
    // An app's icon from the resolved-icon map; its initial on a grey tile until it lands
    // or when it can't.
    import { mktIcons, mktSetIcon } from '../lib/marketplace.svelte.js';
    let { app, h, size = 56 } = $props();
    const key = $derived(h.iconKey(app));
    const src = $derived(key ? mktIcons().get(key) : false);
    const initial = $derived((app.name.match(/[\p{L}\p{N}]/u)?.[0] || '?').toUpperCase());
</script>

<span class="nx-icon" class:bare={!src} style:--nx-icon={`${size}px`}>
    {#if src}
        <img {src} alt="" draggable="false" onerror={() => mktSetIcon(key, false)}>
    {:else}
        <span class="nx-icon-initial">{initial}</span>
    {/if}
</span>
