<script>
    // A message's attachments. Pictures and videos are components over transfer state;
    // audio players and file boxes stay the app's leaves. Auto-download fires once per
    // attachment from here, deduplicated by the app.
    import ImageAttachment from './attachments/ImageAttachment.svelte';
    import VideoAttachment from './attachments/VideoAttachment.svelte';
    import ThumbhashAttachment from './attachments/ThumbhashAttachment.svelte';
    import FileBox from './attachments/FileBox.svelte';

    let { msg, sender, ctx, h } = $props();
    // h (beyond the leaves'): isImage(ext), isAudio(ext), isVideo(ext), willAutoDownload(att, ctx), isDownloading(att),
    //    renderAudio(node, att, msg), attachUploadProgress(node, msg), autoDownload(att, msg, sender), and FileBox's

    const IMAGE_BLUR = ['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'];

    // The audio player is the app's leaf, its upload ring included.
    function audio(node, att) { h.renderAudio(node, att, msg); if (msg.mine && msg.pending) h.attachUploadProgress(node, msg); }

    // Auto-download: a side effect, once per attachment id (the app dedupes across renders).
    $effect(() => {
        for (const att of msg.attachments || []) {
            if (!att.downloaded && !h.isDownloading(att) && h.willAutoDownload(att, ctx)) h.autoDownload(att, msg, sender);
        }
    });
</script>

{#each msg.attachments || [] as att (att.id)}
    {#if att.downloaded}
        {#if h.isImage(att.extension)}
            <ImageAttachment {att} {msg} {ctx} {sender} {h} />
        {:else if h.isAudio(att.extension)}
            <span style="display:contents" use:audio={att}></span>
        {:else if h.isVideo(att.extension)}
            <VideoAttachment {att} {msg} {h} />
        {:else}
            <FileBox {att} {msg} {sender} phase="downloaded" {h} />
        {/if}
    {:else if h.isDownloading(att)}
        {#if IMAGE_BLUR.includes(att.extension)}
            <ThumbhashAttachment {att} {msg} {ctx} {sender} auto={false} {h} />
        {:else}
            <FileBox {att} {msg} {sender} phase="downloading" {h} />
        {/if}
    {:else}
        {@const auto = h.willAutoDownload(att, ctx)}
        {#if IMAGE_BLUR.includes(att.extension)}
            <ThumbhashAttachment {att} {msg} {ctx} {sender} {auto} {h} />
        {:else}
            <FileBox {att} {msg} {sender} phase={auto ? 'downloading' : 'download'} {h} />
        {/if}
    {/if}
{/each}
