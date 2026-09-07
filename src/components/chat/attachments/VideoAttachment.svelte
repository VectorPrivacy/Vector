<script>
    // A downloaded video. Width is the CSS's (max-width + auto, so portrait clips shrink
    // rather than squash). An upload in flight dims it under a progress ring.
    import UploadOverlay from './UploadOverlay.svelte';
    let { att, msg, h } = $props();   // h: mediaUrl(path), onVideoMeta(video), cancelUpload
    const uploading = $derived(msg.mine && msg.pending);
    let container = $state(null);
    function pin(video) {
        const set = () => { if (video.offsetWidth) container.style.width = video.offsetWidth + 'px'; };
        set();
        video.addEventListener('loadedmetadata', set, { once: true });
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
