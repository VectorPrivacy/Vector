<script>
    // A downloaded video. Width is the CSS's (max-width + auto, so portrait clips shrink
    // rather than squash). An upload in flight dims it under a progress ring.
    import UploadOverlay from './UploadOverlay.svelte';
    import { messageVersion } from '../../lib/chatview.svelte.js';
    import { claimPlayback, releasePlayback } from '../../lib/audio.svelte.js';
    import { popOut, takeBack, yieldPopout, popoutPlayingAudio } from '../../lib/popout.svelte.js';
    import { createVideo, adoptVideo, ownsVideo, parkVideo, dropVideo, videoLane } from '../../lib/videohost.js';
    let { att, msg, h } = $props();   // h: MediaHelpers (js/render/chat/message-row.js)
    const uploading = $derived.by(() => { messageVersion(msg.id); return !!(msg.mine && msg.pending); });
    let container = $state(null);

    // Mount-time: a row's chat never changes under it.
    // svelte-ignore state_referenced_locally
    const chatId = h.openChat();
    // svelte-ignore state_referenced_locally
    const back = takeBack(att.id, 'video', chatId);
    const owner = {};
    // The floating player hands back the element it was playing, so nothing reloads.
    // svelte-ignore state_referenced_locally
    const adopted = adoptVideo(back?.hostKey, owner);
    // svelte-ignore state_referenced_locally
    const { key, el } = adopted ? { key: back.hostKey, el: adopted } : createVideo(h.mediaUrl(att.path), owner);
    el.className = '';
    el.style.cssText = 'height: auto; border-radius: 8px; cursor: pointer;';
    $effect(() => {
        el.controls = !uploading;
        el.style.opacity = uploading ? '0.25' : '';
    });

    // One sound at a time, and a playing video follows you out of the chat.
    function host(node) {
        node.prepend(el);
        // The ring covers the media, not the wrapper: the overlay is sized to the media's
        // rendered box for as long as it is on screen.
        const pin = () => {
            container.style.setProperty('--media-w', el.offsetWidth + 'px');
            container.style.setProperty('--media-h', el.offsetHeight + 'px');
        };
        const ro = new ResizeObserver(pin);
        ro.observe(el);
        const lane = videoLane(key);
        const onPlay = () => { yieldPopout('video'); claimPlayback(lane, () => el.pause(), 'video'); };
        const onPause = () => releasePlayback(lane, 'video');
        const onMeta = () => {
            h.onVideoMeta(el);
            // The player's element was gone: pick up where it was from its saved place.
            if (!back) return;
            el.muted = !!back.muted;
            el.volume = back.volume ?? 1;
            el.currentTime = back.time;
            if (back.playing) el.play().catch(() => {});
        };
        el.addEventListener('play', onPlay);
        el.addEventListener('pause', onPause);
        if (adopted) {
            if (!el.paused) claimPlayback(lane, () => el.pause(), 'video');
        } else {
            el.addEventListener('loadedmetadata', onMeta, { once: true });
        }
        return {
            destroy() {
                ro.disconnect();
                el.removeEventListener('play', onPlay);
                el.removeEventListener('pause', onPause);
                el.removeEventListener('loadedmetadata', onMeta);
                releasePlayback(lane, 'video');
                if (!ownsVideo(key, owner)) return;
                if (!el.paused && !el.ended && !popoutPlayingAudio()) {
                    parkVideo(key);
                    const handed = popOut({
                        kind: 'video', id: att.id, hostKey: key, att, msg, chatId, src: el.currentSrc || el.src,
                        time: el.currentTime, playing: true, muted: el.muted, volume: el.volume,
                        aspect: el.videoWidth && el.videoHeight ? el.videoWidth / el.videoHeight : 16 / 9,
                    });
                    if (handed) return;
                }
                dropVideo(key);
            },
        };
    }
</script>

<div style="position: relative; display: block; line-height: 0; max-width: 100%;" bind:this={container} use:host>
    {#if uploading}
        <UploadOverlay pendingId={msg.id} {h} />
    {/if}
</div>
