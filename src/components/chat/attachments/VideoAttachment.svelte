<script>
    // A downloaded video. Width is the CSS's (max-width + auto, so portrait clips shrink
    // rather than squash). An upload in flight dims it under a progress ring.
    import UploadOverlay from './UploadOverlay.svelte';
    import { messageVersion } from '../../lib/chatview.svelte.js';
    import { claimPlayback, releasePlayback } from '../../lib/audio.svelte.js';
    import { popOut, takeBack, yieldPopout, popoutPlayingAudio } from '../../lib/popout.svelte.js';
    let { att, msg, h } = $props();   // h: mediaUrl(path), onVideoMeta(video), cancelUpload, openChat()
    const uploading = $derived.by(() => { messageVersion(msg.id); return !!(msg.mine && msg.pending); });
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

    // Mount-time: a row's chat never changes under it.
    // svelte-ignore state_referenced_locally
    const chatId = h.openChat();
    // svelte-ignore state_referenced_locally
    const back = takeBack(att.id, 'video', chatId);
    function onMeta(video) {
        h.onVideoMeta(video);
        if (!back) return;
        video.muted = !!back.muted;
        video.volume = back.volume ?? 1;
        video.currentTime = back.time;
        if (back.playing) video.play().catch(() => {});
    }
    // One sound at a time, and a playing video follows you out of the chat.
    function playback(video) {
        const key = `video:${att.id}`;
        const onPlay = () => { yieldPopout('video'); claimPlayback(key, () => video.pause()); };
        const onPause = () => releasePlayback(key);
        video.addEventListener('play', onPlay);
        video.addEventListener('pause', onPause);
        return {
            destroy() {
                video.removeEventListener('play', onPlay);
                video.removeEventListener('pause', onPause);
                if (!video.paused && !video.ended && !popoutPlayingAudio()) {
                    popOut({
                        kind: 'video', id: att.id, att, msg, chatId, src: video.currentSrc || video.src,
                        time: video.currentTime, playing: true, muted: video.muted, volume: video.volume,
                        aspect: video.videoWidth && video.videoHeight ? video.videoWidth / video.videoHeight : 16 / 9,
                    });
                }
                releasePlayback(key);
                video.pause();
                video.removeAttribute('src');
                video.load();
            },
        };
    }
</script>

<div style="position: relative; display: block; line-height: 0; max-width: 100%;" bind:this={container}>
    <!-- svelte-ignore a11y_media_has_caption -->
    <video controlsList="nodownload" controls={!uploading} preload="metadata" playsinline src={h.mediaUrl(att.path)}
           style="height: auto; border-radius: 8px; cursor: pointer;" style:opacity={uploading ? '0.25' : null}
           onloadedmetadata={(e) => onMeta(e.currentTarget)} use:pin use:playback></video>
    {#if uploading}
        <UploadOverlay pendingId={msg.id} {h} />
    {/if}
</div>
