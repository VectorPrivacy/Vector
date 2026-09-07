<script>
    // A downloaded video. Width is the CSS's (max-width + auto, so portrait clips shrink
    // rather than squash). An upload in flight dims it under a progress ring.
    import UploadOverlay from './UploadOverlay.svelte';
    let { att, msg, h } = $props();   // h: mediaUrl(path), onVideoMeta(video), cancelUpload
    const uploading = $derived(msg.mine && msg.pending);
    let container = $state(null);
    // The ring centres on the media, not the row: the wrapper follows the media's rendered
    // width for as long as it is on screen (the clamp settles after metadata, and resizes).
    function pin(media) {
        const set = () => { if (media.offsetWidth) container.style.width = media.offsetWidth + 'px'; };
        const ro = new ResizeObserver(set);
        ro.observe(media);
        return { destroy: () => ro.disconnect() };
    }
</script>

{#if uploading}
    <div style="position: relative; display: inline-block; line-height: 0; max-width: 100%;" bind:this={container}>
        <!-- svelte-ignore a11y_media_has_caption -->
        <video controlsList="nodownload" preload="metadata" playsinline src={h.mediaUrl(att.path)}
               style="height: auto; border-radius: 8px; cursor: pointer; opacity: 0.25;"
               onloadedmetadata={(e) => h.onVideoMeta(e.currentTarget)} use:pin></video>
        <UploadOverlay pendingId={msg.id} {h} />
    </div>
{:else}
    <!-- svelte-ignore a11y_media_has_caption -->
    <video controlsList="nodownload" controls preload="metadata" playsinline src={h.mediaUrl(att.path)}
           style="height: auto; border-radius: 8px; cursor: pointer;"
           onloadedmetadata={(e) => h.onVideoMeta(e.currentTarget)}></video>
{/if}
