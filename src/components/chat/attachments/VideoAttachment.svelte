<script>
    // A downloaded video. Width is the CSS's (max-width + auto, so portrait clips shrink
    // rather than squash). An upload in flight dims it under a progress ring.
    import UploadOverlay from './UploadOverlay.svelte';
    let { att, msg, h } = $props();   // h: mediaUrl(path), onVideoMeta(video), cancelUpload
    const uploading = $derived(msg.mine && msg.pending);
    let container = $state(null);
    // The ring covers the media, not the wrapper: the overlay is sized to the media's rendered
    // box for as long as it is on screen. Pinning the wrapper instead would cap the media
    // through its percentage max-width and freeze it at its pre-metadata size.
    function pin(media) {
        const set = () => {
            container.style.setProperty('--media-w', media.offsetWidth + 'px');
            container.style.setProperty('--media-h', media.offsetHeight + 'px');
        };
        const ro = new ResizeObserver(set);
        ro.observe(media);
        return { destroy: () => ro.disconnect() };
    }
</script>

<div style="position: relative; display: block; line-height: 0; max-width: 100%;" bind:this={container}>
    <!-- svelte-ignore a11y_media_has_caption -->
    <video controlsList="nodownload" controls={!uploading} preload="metadata" playsinline src={h.mediaUrl(att.path)}
           style="height: auto; border-radius: 8px; cursor: pointer;" style:opacity={uploading ? '0.25' : null}
           onloadedmetadata={(e) => h.onVideoMeta(e.currentTarget)} use:pin></video>
    {#if uploading}
        <UploadOverlay pendingId={msg.id} {h} />
    {/if}
</div>
