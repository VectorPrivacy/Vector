<script>
    // An app's icon from the resolved-icon map, with the play glyph until it lands or when it can't.
    import { mktIcons, mktSetIcon } from '../lib/marketplace.svelte.js';
    let { app, cls = '', ph = '', h } = $props();
    const key = $derived(h.iconKey(app));
    const src = $derived(key ? mktIcons().get(key) : false);
</script>

{#if src}
    <img {src} alt={app.name} class={cls} onerror={() => mktSetIcon(key, false)}>
{:else}
    <span class="icon icon-play {ph}"></span>
{/if}
