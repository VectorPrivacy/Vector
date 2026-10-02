<script>
    // A post's pictures and clips under its text: pictures open in the viewer, clips play inline.
    import EmbedVideo from './EmbedVideo.svelte';
    import EmbedImage from './EmbedImage.svelte';
    let { media, origin = null, h } = $props();   // origin: { chatId, msgId }; h: NostrEmbedHelpers
    const shown = $derived(media.slice(0, 4));
</script>

<div class="ne-media" class:is-grid={shown.length > 1}>
    {#each shown as m, i (i)}
        {#if m.is_video}
            <EmbedVideo media={m} poster={m.poster} {origin} {h} />
        {:else}
            <EmbedImage url={m.url} cls="ne-media-img" ratio={m.width && m.height ? `${m.width} / ${m.height}` : null} preview {h} onload={() => h.onResized()} />
        {/if}
    {/each}
    {#if media.length > 4}<span class="ne-media-more">+{media.length - 4}</span>{/if}
</div>
