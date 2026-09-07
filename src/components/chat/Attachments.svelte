<script>
    // A message's attachments. Pictures and videos are components over transfer state;
    // audio players and file boxes stay the app's leaves. Auto-download fires once per
    // attachment from here, deduplicated by the app.
    import ImageAttachment from './attachments/ImageAttachment.svelte';
    import VideoAttachment from './attachments/VideoAttachment.svelte';
    import ThumbhashAttachment from './attachments/ThumbhashAttachment.svelte';

    let { msg, sender, ctx, h } = $props();
    // h (beyond the leaves'): isImage(ext), isAudio(ext), isVideo(ext), willAutoDownload(att, ctx), isDownloading(att),
    //    renderAudio(node, att, msg), fileBox(node, att, state, opts), attachUploadProgress(node, msg), autoDownload(att, msg, sender)

    const IMAGE_BLUR = ['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'];

    function audio(node, att) { h.renderAudio(node, att, msg); if (msg.mine && msg.pending) h.attachUploadProgress(node, msg); }
    function file(node, att) { h.fileBox(node, att, 'downloaded', { msg }); if (msg.mine && msg.pending) h.attachUploadProgress(node, msg); }
    function fileDownloading(node, att) { h.fileBox(node, att, 'downloading', {}); }
    function fileDownload(node, att) { h.fileBox(node, att, 'download', { failed: att.download_failed, onClick: () => h.startDownload(att, msg, sender) }); }

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
            <span style="display:contents" use:file={att}></span>
        {/if}
    {:else if h.isDownloading(att)}
        {#if IMAGE_BLUR.includes(att.extension)}
            <ThumbhashAttachment {att} {msg} {ctx} {sender} auto={false} {h} />
        {:else}
            <span style="display:contents" use:fileDownloading={att}></span>
        {/if}
    {:else}
        {@const auto = h.willAutoDownload(att, ctx)}
        {#if IMAGE_BLUR.includes(att.extension)}
            <ThumbhashAttachment {att} {msg} {ctx} {sender} {auto} {h} />
        {:else if auto}
            <span style="display:contents" use:fileDownloading={att}></span>
        {:else}
            <span style="display:contents" use:fileDownload={att}></span>
        {/if}
    {/if}
{/each}
