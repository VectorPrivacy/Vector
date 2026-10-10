<script module>
    // The shape each video turned out to have, for the session: an event that didn't say
    // opens in its real frame the next time it is shown.
    const shapes = new Map();   // video url -> { w, h }
</script>

<script>
    // A Nostr event's video, played from a local copy the backend fetched. Under the
    // auto-download limit it fetches on sight; otherwise the first tap fetches it, then plays.
    // Playing as its chat closes, it moves to the floating player, and comes back here.
    import { embedVideoProgress } from '../../lib/nostrembed.svelte.js';
    import { claimPlayback, releasePlayback } from '../../lib/audio.svelte.js';
    import { popOut, takeBack, yieldPopout, popoutPlayingAudio } from '../../lib/popout.svelte.js';
    import { createVideo, adoptVideo, ownsVideo, parkVideo, dropVideo, videoLane } from '../../lib/videohost.js';
    import EmbedImage from './EmbedImage.svelte';
    // origin: { chatId, msgId } of the message showing it, when there is one; tile: a square
    // cell in a grid beside pictures; h: NostrEmbedHelpers
    let { media, poster = null, short = false, tile = false, title = null, origin = null, h } = $props();

    let phase = $state('idle');   // idle | loading | ready | error
    let src = $state(null);
    let error = $state('');
    let playing = $state(false);
    // svelte-ignore state_referenced_locally
    let natural = $state(shapes.get(media.url) || null);
    const pct = $derived(phase === 'loading' ? embedVideoProgress(media.url) : null);
    const dims = $derived(media.width && media.height ? { w: media.width, h: media.height } : natural);
    const ratio = $derived(tile ? '1 / 1' : dims ? `${dims.w} / ${dims.h}` : (short ? '9 / 16' : '16 / 9'));
    const portrait = $derived(!tile && (short || !!(dims && dims.h > dims.w)));
    // Keyed by file, so the row that shows it again takes it back from the floating player.
    // svelte-ignore state_referenced_locally
    const popId = `embed:${media.url}`;
    // svelte-ignore state_referenced_locally
    const back = origin ? takeBack(popId, 'video', origin.chatId) : null;
    const owner = {};
    // The floating player hands back the element it was playing, so nothing reloads.
    // svelte-ignore state_referenced_locally
    let adopted = adoptVideo(back?.hostKey, owner);
    if (back) {
        src = back.src;
        phase = 'ready';
        playing = true;
    }
    // svelte-ignore state_referenced_locally
    const inline = h.inlineVideo();

    async function load() {
        if (phase === 'loading' || phase === 'ready') return;
        phase = 'loading';
        try {
            const path = await h.fetchVideo(media);
            if (!inline) {
                h.reveal(path);
                phase = 'idle';
                return;
            }
            src = h.mediaUrl(path);
            phase = 'ready';
        } catch (e) {
            error = String(e);
            phase = /cancel/i.test(error) ? 'idle' : 'error';
            playing = false;
        }
    }
    async function tap(e) {
        e.stopPropagation();
        if (phase === 'loading') {
            h.cancelVideo(media.url);
            return;
        }
        if (!inline) {
            await load();
            return;
        }
        playing = true;
        await load();
    }
    // svelte-ignore state_referenced_locally
    if (!back && inline && h.willAutoDownload(media)) load();

    // No poster from the event and no download coming: the video's opening, fetched as a few
    // hundred KB of ranges, shows its first frame.
    let preview = $state(null);
    // svelte-ignore state_referenced_locally
    if (!back && inline && !poster && !h.willAutoDownload(media)) {
        h.videoPreview(media).then((path) => { preview = h.mediaUrl(path); }).catch(() => {});
    }

    function takeShape(video) {
        if (video.videoWidth && video.videoHeight) {
            natural = { w: video.videoWidth, h: video.videoHeight };
            shapes.set(media.url, natural);
        }
        h.onResized();
    }

    // One sound at a time, and a playing video follows you out of the chat (lib/videohost.js
    // moves the element itself, so the floating player picks it up mid-frame).
    function host(node) {
        const live = adopted;
        adopted = null;
        const { key, el } = live ? { key: back.hostKey, el: live } : createVideo(src, owner);
        el.className = '';
        el.style.cssText = '';
        el.controls = true;
        node.append(el);
        // Per element, not per file: the card and the modal each hold their own copy.
        const laneKey = videoLane(key);
        const onPlay = () => { yieldPopout('video'); claimPlayback(laneKey, () => el.pause(), 'video'); };
        const onPause = () => releasePlayback(laneKey, 'video');
        const onMeta = () => {
            takeShape(el);
            // The player's element was gone: pick up where it was from its saved place.
            if (back) {
                el.muted = !!back.muted;
                el.volume = back.volume ?? 1;
                el.currentTime = back.time;
                if (back.playing) el.play().catch(() => {});
            }
        };
        const onError = () => { playing = false; phase = 'idle'; src = null; };
        el.addEventListener('play', onPlay);
        el.addEventListener('pause', onPause);
        el.addEventListener('error', onError);
        if (live) {
            takeShape(el);
            if (!el.paused) claimPlayback(laneKey, () => el.pause(), 'video');
        } else {
            el.addEventListener('loadedmetadata', onMeta, { once: true });
            if (!back) el.play().catch(() => {});
        }
        return {
            destroy() {
                el.removeEventListener('play', onPlay);
                el.removeEventListener('pause', onPause);
                el.removeEventListener('error', onError);
                el.removeEventListener('loadedmetadata', onMeta);
                releasePlayback(laneKey, 'video');
                if (!ownsVideo(key, owner)) return;
                if (origin && !el.paused && !el.ended && !popoutPlayingAudio()) {
                    parkVideo(key);
                    const handed = popOut({
                        kind: 'video', id: popId, hostKey: key, att: { name: title || 'Nostr video' }, msg: { id: origin.msgId },
                        chatId: origin.chatId, src: el.currentSrc || el.src, time: el.currentTime, playing: true,
                        muted: el.muted, volume: el.volume,
                        aspect: el.videoWidth && el.videoHeight ? el.videoWidth / el.videoHeight : 16 / 9,
                    });
                    if (handed) return;
                }
                dropVideo(key);
            },
        };
    }
</script>

<div class="ne-video" class:is-portrait={portrait} style:aspect-ratio={ratio}>
    {#if playing && phase === 'ready'}
        <div style="display: contents" use:host></div>
    {:else}
        <button type="button" class="ne-video-poster" onclick={tap}
                aria-label={phase === 'loading' ? 'Cancel download' : inline ? 'Play video' : 'Open video'}>
            {#if poster}
                <EmbedImage url={poster} cls="ne-poster-img" {h} />
            {:else if src || preview}
                <!-- No poster from the event: the opening frame of the local copy, whole or its first part. -->
                <video class="ne-poster-img" src="{src || preview}#t=0.001" preload="metadata" muted playsinline disablepictureinpicture tabindex="-1" aria-hidden="true"
                       onloadedmetadata={(e) => takeShape(e.currentTarget)}></video>
            {/if}
            <span class="ne-video-play" class:is-loading={phase === 'loading'} class:is-error={phase === 'error'}>
                {#if phase === 'loading'}
                    <svg viewBox="0 0 36 36" class:is-spinning={pct == null || pct < 0}>
                        <circle cx="18" cy="18" r="16" class="ne-ring-track"></circle>
                        <circle cx="18" cy="18" r="16" class="ne-ring" style:stroke-dashoffset={100.5 - (pct > 0 ? pct : 25) * 1.005}></circle>
                    </svg>
                {:else if phase === 'error'}
                    <span class="ne-video-glyph">!</span>
                {:else}
                    <span class="icon icon-play"></span>
                {/if}
            </span>
            {#if media.duration}<span class="ne-video-badge">{h.duration(media.duration)}</span>{/if}
            {#if phase === 'error'}<span class="ne-video-error" title={error}>Couldn't download the video. Tap to try again.</span>{/if}
        </button>
    {/if}
</div>
