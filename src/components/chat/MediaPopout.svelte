<script>
    // The pop-out: media that kept playing after its row left the screen. It floats over
    // every pane, moves by dragging and resizes from any corner. Audio shows as a mini
    // player; a video plays in the box, or at a button press keeps playing as sound alone
    // behind a thumbnail of itself.
    //
    // One grid holds every layout, so switching between a video and its sound-only form
    // moves the same <video> element rather than remounting it.
    import { untrack, flushSync } from 'svelte';
    import { popoutState, closePopout } from '../lib/popout.svelte.js';
    import { audioInfo, claimPlayback, releasePlayback, patchTranscription, transcribeAudio, modelDownloadState, setLyricsOpen,
             splitTitle, songLyrics } from '../lib/audio.svelte.js';
    import Transcription from './attachments/Transcription.svelte';
    import Lyrics from './attachments/Lyrics.svelte';
    import { profileVersion, chatVersion } from '../lib/signals.svelte.js';

    let { h } = $props();   // h: MediaPopoutHelpers (js/voice.js)

    const pop = popoutState();
    const item = $derived(pop.item);
    const session = $derived(item?.kind === 'audio' ? item.session : null);
    const isVideo = $derived(item?.kind === 'video');
    // What both kinds carry: the attachment, its message and the chat it came from.
    const ref = $derived(session || item);
    const meta = $derived(session ? audioInfo(session.id)?.meta ?? null : null);
    const art = $derived(meta?.coverArt || '');
    const accent = $derived(art ? (meta.accent || 'rgb(242, 242, 242)') : null);

    // A voice message's transcript is the attachment's, shared with its row in the chat:
    // made once by whichever side asks first, and open or closed in both.
    const transcription = $derived(session?.voice ? audioInfo(session.id)?.transcription ?? null : null);
    const canTranscribe = $derived(!!session?.voice && h.audio.transcriptionSupported(session.att));
    const download = modelDownloadState();
    const transcribing = $derived(transcription?.phase === 'loading');
    function onTranscribe() {
        if (transcribing || download.active) return;
        if (transcription?.phase === 'ready') { patchTranscription(session.id, { open: !transcription.open }); return; }
        transcribeAudio(session.id, session.att.path, h.audio.transcribe);
    }
    // The box is anchored at its bottom and grows upward: nothing in the chat to hold still.
    // svelte-ignore state_referenced_locally
    const transcriptH = { ...h.audio, holdScroll: () => () => {} };
    // A track's lyrics, open here exactly when they are open on its card.
    // An album: the current song leads, its stretch of the file is what the wave and time show.
    const isAlbum = $derived((session?.tracks.length ?? 0) > 1);
    const trackIdx = $derived(isAlbum ? Math.max(0, session.track) : -1);
    const curSpan = $derived.by(() => { session?.track; session?.durationMs; return session ? session.span(isAlbum ? trackIdx : -1) : { start: 0, end: 0 }; });
    let tracksOpen = $state(false);
    // Narrow, the box keeps what it needs to play and close: the song list, the lyrics and an
    // album's times step aside (the lyrics stay open on the card), and come back with room.
    const COMPACT_W = 340;
    const compact = $derived(width < COMPACT_W);
    const lyrics = $derived(isAlbum ? songLyrics(meta?.lyrics ?? null, curSpan) : (meta?.lyrics ?? null));
    const lyricsOpen = $derived(!!(session && audioInfo(session.id)?.lyricsOpen));
    function lyricsFrom(ms) {
        session.seek(ms);
        positionMs = ms;
        kick();
        if (!session.playing) session.play();
    }

    // Once a transcript or the lyrics have slid open or shut, the box may have grown past the top.
    $effect(() => {
        transcription?.open;
        lyricsOpen;
        const t = setTimeout(clampPlace, 400);
        return () => clearTimeout(t);
    });

    const words = $derived.by(() => {
        if (!ref) return null;
        chatVersion(ref.chatId);
        const chat = h.chatName(ref.chatId);
        // A channel says where it is: its community, then the channel.
        const place = h.channelOf(ref.chatId);
        if (session?.voice) {
            const who = h.who(ref.msg, ref.chatId);
            profileVersion(who.npub);
            return { title: who.name, sub: chat && chat !== who.name ? chat : 'Voice Message', place, avatar: who.avatar };
        }
        if (isAlbum) {
            const t = splitTitle(session.tracks[trackIdx]?.title);
            return { title: t.main, sub: [`${trackIdx + 1} of ${session.tracks.length}`, splitTitle(meta?.album || '').main || meta?.artist].filter(Boolean).join(' · ') };
        }
        if (meta?.artist) return { title: meta.track || ref.att.name, sub: meta.artist };
        return { title: meta?.track || ref.att.name || (isVideo ? 'Video' : 'Audio'), sub: chat, place };
    });

    // ── place and size: remembered per device, the width per kind ──
    const KEY = 'media_popout';
    const LIMITS = { audio: [260, 480], video: [220, 720] };
    function load() {
        try { return JSON.parse(localStorage.getItem(KEY)) || {}; } catch (_) { return {}; }
    }
    const saved = load();
    let right = $state(Number.isFinite(saved.right) ? saved.right : 16);
    let bottom = $state(Number.isFinite(saved.bottom) ? saved.bottom : 96);
    let widths = $state({ audio: 320, video: 360, ...(saved.widths || {}) });
    let soundPref = $state(!!saved.soundOnly);
    function save() {
        try { localStorage.setItem(KEY, JSON.stringify({ right, bottom, widths, soundOnly: soundPref })); } catch (_) {}
    }
    let winW = $state(window.innerWidth), winH = $state(window.innerHeight);
    // A video can shrink to sound behind a thumbnail: a button, not a size, so a resize
    // never changes what the box is.
    const soundOnly = $derived(isVideo && soundPref);
    const kindKey = $derived(isVideo && !soundOnly ? 'video' : 'audio');
    const width = $derived(Math.min(widths[kindKey], winW - 24));
    function toggleSound() { soundPref = !soundPref; save(); }

    let box = $state(null);
    function chromeHeight() {
        const v = parseFloat(getComputedStyle(document.body).getPropertyValue('--chrome-h'));
        return Number.isFinite(v) ? v : 0;
    }
    function clampPlace() {
        if (!box) return;
        const w = box.offsetWidth, hgt = box.offsetHeight;
        right = Math.min(Math.max(8, right), Math.max(8, winW - w - 8));
        bottom = Math.min(Math.max(8, bottom), Math.max(8, winH - hgt - chromeHeight() - 8));
    }
    $effect(() => {
        const keep = () => { winW = window.innerWidth; winH = window.innerHeight; clampPlace(); };
        window.addEventListener('resize', keep);
        return () => window.removeEventListener('resize', keep);
    });
    // A panel opening (songs, lyrics, a transcript) or a change of shape grows the box
    // toward the open space: a box in the upper half keeps its top edge and grows down, one
    // in the lower half keeps its bottom edge and grows up (the anchor it already has). Kept
    // edge by edge as the height animates, so opening and closing retrace the same path.
    let lastH = 0;
    $effect(() => {
        if (!box) return;
        const ro = new ResizeObserver(() => {
            const h = box.offsetHeight;
            if (lastH && h !== lastH && !gesture) {
                const prevTop = winH - bottom - lastH;
                if (prevTop + lastH / 2 < winH / 2) bottom = Math.max(8, winH - prevTop - h);
            }
            lastH = h;
        });
        ro.observe(box);
        return () => { ro.disconnect(); lastH = 0; };
    });

    // A new size or kind can push the box past an edge; never mid-resize, where the
    // pinned corner decides the place.
    $effect(() => { width; soundOnly; item; untrack(() => requestAnimationFrame(() => { if (!gesture?.corner) clampPlace(); })); });

    // A move drags the box; a resize pins the corner opposite the grabbed one and sizes
    // the box from where the pointer is relative to it. Absolute, not a running delta: a
    // box that changes shape mid-drag (video into sound-only) can never leave the pointer
    // chasing a corner that jumped away, because only the pointer's place counts.
    let gesture = null;
    // The native controls' strip along a video's bottom stays the video's.
    const VIDEO_CONTROLS_H = 44;
    function onPointerDown(e) {
        if (e.button !== 0 || e.target.closest('button, canvas, .popout-track, .transcription-result, .lyrics')) return;
        const onVideo = e.target.closest('video');
        if (onVideo && !soundOnly && e.clientY > onVideo.getBoundingClientRect().bottom - VIDEO_CONTROLS_H) return;
        const grip = e.target.closest('.popout-grip');
        const r = box.getBoundingClientRect();
        if (grip) {
            const corner = grip.dataset.corner;
            const pinRight = corner.endsWith('l'), pinBottom = corner.startsWith('t');
            gesture = { corner, pinRight, pinBottom, ax: pinRight ? r.right : r.left, ay: pinBottom ? r.bottom : r.top };
        } else {
            // On the picture a press is still the video's until it travels: a tap reaches
            // the video, a drag moves the box.
            gesture = { sx: e.clientX, sy: e.clientY, right, bottom, pending: !!onVideo };
        }
        if (!gesture.pending) {
            e.currentTarget.setPointerCapture(e.pointerId);
            // A press here is the box's: never the start of a native drag of what lies under it.
            e.preventDefault();
        }
    }
    // Only the corner nearest the pointer shows its grip mark.
    let nearCorner = $state(null);
    function onPointerMove(e) {
        // No button held means the release happened where this box never heard it (a native
        // drag swallowed it): the gesture is over, not still following the pointer.
        if (gesture && e.buttons === 0) { onPointerUp(); return; }
        if (!gesture) {
            const r = box.getBoundingClientRect();
            nearCorner = (e.clientY - r.top < r.bottom - e.clientY ? 't' : 'b') + (e.clientX - r.left < r.right - e.clientX ? 'l' : 'r');
            return;
        }
        if (!gesture.corner) {
            if (gesture.pending) {
                if (Math.hypot(e.clientX - gesture.sx, e.clientY - gesture.sy) < 4) return;
                gesture.pending = false;
                box.setPointerCapture(e.pointerId);
                // The release after a drag must not reach the video as a click; dropped right
                // after the release, so a drag that ends off the box eats no later click.
                gesture.swallow = (c) => { c.stopPropagation(); c.preventDefault(); };
                box.addEventListener('click', gesture.swallow, { capture: true, once: true });
            }
            right = gesture.right - (e.clientX - gesture.sx);
            bottom = gesture.bottom - (e.clientY - gesture.sy);
            clampPlace();
            return;
        }
        const g = gesture;
        const dx = g.pinRight ? g.ax - e.clientX : e.clientX - g.ax;
        const dy = g.pinBottom ? g.ay - e.clientY : e.clientY - g.ay;
        // A video's height follows its width (w / aspect + the caption below it), so its
        // corner can only travel one line: take the point on that line nearest the pointer.
        // Continuous in the pointer, so a small move is only ever a small change.
        let w = dx;
        if (kindKey === 'video' && video?.offsetHeight) {
            const a = video.offsetWidth / video.offsetHeight;
            const chrome = box.offsetHeight - video.offsetHeight;
            w = (dx + (dy - chrome) / a) / (1 + 1 / (a * a));
        }
        const room = (g.pinRight ? g.ax : winW - g.ax) - 8;
        const [min, max] = LIMITS[kindKey];
        widths[kindKey] = Math.round(Math.max(min, Math.min(max, room, w)));
        // Lay the new size out now, then put the pinned corner back where it was.
        flushSync();
        const bw = box.offsetWidth, bh = box.offsetHeight;
        right = g.pinRight ? winW - g.ax : winW - g.ax - bw;
        bottom = g.pinBottom ? winH - g.ay : winH - g.ay - bh;
    }
    function onPointerUp() {
        if (!gesture) return;
        if (gesture.pending) { gesture = null; return; }
        const { swallow } = gesture, el = box;
        if (swallow) setTimeout(() => el?.removeEventListener('click', swallow, { capture: true }));
        const wasResize = !!gesture.corner;
        gesture = null;
        if (wasResize) clampPlace();
        save();
    }

    // ── audio: the waveform, drawn on a canvas at the session's clock ──
    let canvas = $state(null);
    let positionMs = $state(0);
    let raf = null, bins = null, colour = '';
    function draw() {
        raf = null;
        if (!canvas || !session) return;
        const dpr = window.devicePixelRatio || 1;
        const W = canvas.clientWidth, H = canvas.clientHeight;
        if (!W || !H) return;
        // Both sides: a width that happens to match the canvas default (300) would otherwise
        // leave the default height (150), squashing the bars into the top of the box.
        if (canvas.width !== Math.round(W * dpr) || canvas.height !== Math.round(H * dpr)) {
            canvas.width = Math.round(W * dpr);
            canvas.height = Math.round(H * dpr);
        }
        const ctx = canvas.getContext('2d');
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        ctx.clearRect(0, 0, W, H);
        const n = Math.max(12, Math.floor(W / 5)), gap = 2, bw = (W - gap * (n - 1)) / n;
        if (!bins || bins.length !== n) bins = new Float32Array(n);
        const pos = session.position();
        const sp = session.span(isAlbum ? Math.max(0, session.track) : -1);
        const progress = sp.end > sp.start ? (pos - sp.start) / (sp.end - sp.start) : 0;
        const wf = session.waveform;
        let moving = false;
        for (let i = 0; i < n; i++) {
            let target = 0;
            if (session.playing && wf?.data) {
                const v = (wf.data[Math.floor((pos / 1000) * wf.fps) * wf.bins + Math.floor(i * wf.bins / n)] ?? 0) / 255;
                target = Math.min(1, v * Math.sqrt(v));
            }
            bins[i] = target > bins[i] ? bins[i] * 0.7 + target * 0.3 : bins[i] * 0.85 + target * 0.15;
            if (bins[i] > 0.01) moving = true;
        }
        ctx.fillStyle = colour;
        for (let i = 0; i < n; i++) {
            const val = Math.max(0.15, bins[i]);
            const bh = Math.max(3, val * H);
            ctx.globalAlpha = (0.45 + val * 0.55) * ((i + 0.5) / n <= progress ? 1 : 0.35);
            ctx.beginPath();
            ctx.roundRect(i * (bw + gap), (H - bh) / 2, bw, bh, Math.min(1.5, bw / 2));
            ctx.fill();
        }
        positionMs = pos;
        if (session.playing || moving) raf = requestAnimationFrame(draw);
    }
    function kick() {
        if (!canvas) return;
        colour = getComputedStyle(canvas).color;
        if (!raf) raf = requestAnimationFrame(draw);
    }
    $effect(() => { session?.playing; session?.pausedAt; canvas; accent; untrack(kick); });
    $effect(() => () => { if (raf) cancelAnimationFrame(raf); });
    // A resize while paused redraws too: nothing else would until playback moves.
    $effect(() => {
        if (!canvas) return;
        const ro = new ResizeObserver(() => kick());
        ro.observe(canvas);
        return () => ro.disconnect();
    });

    function seekAt(e, el) {
        const r = el.getBoundingClientRect();
        return Math.max(0, Math.min(1, (e.clientX - r.left) / r.width));
    }
    let scrubbing = false;
    function scrubStart(e) {
        scrubbing = true;
        e.currentTarget.setPointerCapture(e.pointerId);
        scrub(e);
    }
    function scrub(e) {
        if (!scrubbing) return;
        const f = seekAt(e, e.currentTarget);
        if (session && session.durationMs) {
            session.seek(Math.floor(curSpan.start + f * (curSpan.end - curSpan.start)));
            positionMs = session.position();
            kick();
        } else if (video && vDur) {
            video.currentTime = f * vDur;
            vTime = video.currentTime;
        }
    }
    function scrubEnd() { scrubbing = false; }

    // A finished item with nothing after it leaves on its own. A voice run hands on in the
    // tick the engine reports the end, swapping the session; only the same session going
    // from playing to its start means nothing followed.
    let watched = null, wasPlaying = false;
    $effect(() => {
        const s = session;
        const on = !!s?.playing;
        const done = !!s && s === watched && wasPlaying && !on && s.pausedAt === 0;
        watched = s;
        wasPlaying = on;
        if (done) untrack(closePopout);
    });

    // ── video ──
    let video = $state(null);
    let vPlaying = $state(false), vTime = $state(0), vDur = $state(0);
    const vKey = $derived(isVideo ? `video:${item.id}` : '');
    function onVideoMeta() {
        if (!item || !video) return;
        vDur = video.duration || 0;
        video.muted = !!item.muted;
        video.volume = item.volume ?? 1;
        video.currentTime = item.time;
        if (item.playing) video.play().catch(() => {});
    }
    function onVideoPlay() {
        if (!item) return;
        vPlaying = true;
        item.playing = true;
        const el = video;
        claimPlayback(vKey, () => el.pause());
    }
    function onVideoPause() {
        vPlaying = false;
        if (item) item.playing = false;
        releasePlayback(vKey);
    }
    function onVideoTime() {
        // A leaving box's video still reports while it fades; the item has already gone.
        if (!item || !video) return;
        vTime = video.currentTime;
        // Kept on the item, so a row that takes it back resumes from here.
        item.time = vTime;
        item.muted = video.muted;
        item.volume = video.volume;
    }
    $effect(() => {
        const key = vKey;
        return () => { if (key) releasePlayback(key); };
    });

    const playing = $derived(isVideo ? vPlaying : !!session?.playing);
    const loading = $derived(!!session?.loading);
    function toggle() {
        if (isVideo) { if (video.paused) video.play().catch(() => {}); else video.pause(); }
        else if (session.playing) session.pause();
        else session.play();
    }
    // The modal cards' pop, both ways: a fade that grows in and shrinks out. A leaving
    // video is silenced at once rather than playing on under the animation.
    function cardPop(_, { duration }) {
        return { duration, easing: (t) => t * (2 - t), css: (t) => `opacity: ${t}; transform: scale(${0.92 + 0.08 * t});` };
    }
    $effect(() => { if (!item && video) video.pause(); });

    const fmt = (ms) => h.audio.formatTime(ms / 1000);
    const timeText = $derived(isVideo ? `${fmt(vTime * 1000)} / ${fmt(vDur * 1000)}` : `${fmt(Math.max(0, positionMs - curSpan.start))} / ${fmt(Math.max(0, curSpan.end - curSpan.start))}`);

    function openInChat() {
        const { chatId, msg } = ref;
        h.openAt(chatId, msg.id);
    }
</script>

{#if item}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="media-popout" class:is-album={isAlbum} bind:this={box} in:cardPop={{ duration: 160 }} out:cardPop={{ duration: 150 }} class:is-video={isVideo && !soundOnly} class:is-sound={!isVideo || soundOnly}
         style:width="{width}px" style:right="{right}px" style:bottom="{bottom}px" style:--icon-color-primary={accent}
         onpointerdown={onPointerDown} onpointermove={onPointerMove} onpointerup={onPointerUp} onpointercancel={onPointerUp}
         onlostpointercapture={() => { if (gesture && !gesture.pending) onPointerUp(); }} ondragstart={(e) => e.preventDefault()}
         onpointerleave={() => { if (!gesture) nearCorner = null; }}>
        <!-- Its own rounded clip: WebKit leaves a blurred layer's square corners outside the
             box's rounding, and clipping the box itself would cut off its shadow. -->
        {#if art}<div class="popout-glow"><img class="audio-art-glow" src={art} alt="" aria-hidden="true"></div>{/if}
        {#each ['tl', 'tr', 'bl', 'br'] as corner (corner)}
            <div class="popout-grip" data-corner={corner} class:is-near={nearCorner === corner}></div>
        {/each}

        {#if isVideo}
            {#key item.id}
                <!-- svelte-ignore a11y_media_has_caption -->
                <video class="popout-thumb popout-video" bind:this={video} src={item.src} playsinline controls={!soundOnly} controlsList="nodownload"
                       onloadedmetadata={onVideoMeta} onplay={onVideoPlay} onpause={onVideoPause} ontimeupdate={onVideoTime}
                       onended={() => closePopout()}></video>
            {/key}
        {:else if art}
            <button class="popout-thumb" aria-label="View cover art" onclick={() => h.audio.viewImage(art)}><img src={art} alt=""></button>
        {:else if words?.avatar}
            <img class="popout-thumb is-round" src={words.avatar} alt="" onerror={(e) => { e.currentTarget.onerror = null; e.currentTarget.src = 'icons/user-placeholder.svg'; }}>
        {:else}
            <span class="popout-thumb is-glyph"><span class="icon icon-volume-max"></span></span>
        {/if}

        <div class="popout-text">
            <div class="popout-title cutoff" title={words?.title}>{words?.title}</div>
            {#if words?.place}
                <div class="popout-sub popout-place">
                    {#if words.place.icon}<img class="popout-place-icon" src={words.place.icon} alt="">{/if}
                    <span class="cutoff popout-place-community">{words.place.community}</span>
                    {#if words.place.channel}<span class="popout-place-sep">›</span><span class="cutoff">{words.place.channel}</span>{/if}
                </div>
            {:else if words?.sub}
                <div class="popout-sub cutoff">{words.sub}</div>
            {/if}
        </div>
        <div class="popout-actions">
            {#if isAlbum && !compact}
                <button class="popout-action" class:is-open={tracksOpen} aria-label={tracksOpen ? 'Hide songs' : 'Songs'} title={tracksOpen ? 'Hide songs' : 'Songs'}
                        onclick={() => (tracksOpen = !tracksOpen)}>
                    <span class="icon icon-list-music"></span>
                </button>
            {/if}
            {#if lyrics && !compact}
                <button class="popout-action" class:is-open={lyricsOpen} aria-label={lyricsOpen ? 'Hide lyrics' : 'Show lyrics'}
                        title={lyricsOpen ? 'Hide lyrics' : 'Lyrics'} onclick={() => setLyricsOpen(session.id, !lyricsOpen)}>
                    <span class="icon icon-align-left"></span>
                </button>
            {/if}
            {#if canTranscribe}
                <button class="popout-action" class:is-open={transcription?.phase === 'ready' && transcription.open} disabled={download.active}
                        aria-label={transcription?.phase === 'ready' ? (transcription.open ? 'Hide transcript' : 'Show transcript') : 'Transcribe'}
                        title={transcription?.phase === 'ready' ? (transcription.open ? 'Hide transcript' : 'Show transcript') : 'Transcribe'} onclick={onTranscribe}>
                    <span class="icon {transcribing ? 'icon-loading spin' : (transcription?.phase === 'ready' && transcription.open ? 'icon-file-minus' : 'icon-file-plus')}"></span>
                </button>
            {/if}
            {#if isVideo}
                <button class="popout-action" aria-label={soundOnly ? 'Show video' : 'Sound only'} title={soundOnly ? 'Show video' : 'Sound only'} onclick={toggleSound}>
                    <span class="icon {soundOnly ? 'icon-video' : 'icon-volume-max'}"></span>
                </button>
            {/if}
            <button class="popout-action" aria-label="Show in chat" title="Show in chat" onclick={openInChat}><span class="icon icon-chat-bubble"></span></button>
            <button class="popout-action" aria-label="Close" title="Close" onclick={() => closePopout()}><span class="icon icon-x"></span></button>
        </div>

        {#if !isVideo || soundOnly}
            <span class="popout-play-group">
                {#if isAlbum}<button class="audio-skip" aria-label="Previous" onclick={() => session.prev()}><span class="icon icon-skip-back"></span></button>{/if}
                <button class="audio-play-btn popout-play" aria-label={playing ? 'Pause' : 'Play'} onclick={toggle}>
                    <span class="icon {loading ? 'icon-loading spin' : (playing ? 'icon-pause' : 'icon-play')}"></span>
                </button>
                {#if isAlbum}<button class="audio-skip" aria-label="Next" onclick={() => session.next()}><span class="icon icon-skip-forward"></span></button>{/if}
            </span>
            {#if isVideo}
                <div class="popout-track" onpointerdown={scrubStart} onpointermove={scrub} onpointerup={scrubEnd} onpointercancel={scrubEnd}>
                    <div class="popout-track-fill" style:width="{vDur ? (vTime / vDur) * 100 : 0}%"></div>
                </div>
            {:else}
                <canvas class="popout-wave" bind:this={canvas} onpointerdown={scrubStart} onpointermove={scrub} onpointerup={scrubEnd} onpointercancel={scrubEnd}></canvas>
            {/if}
            {#if !(compact && isAlbum)}<span class="popout-time">{timeText}</span>{/if}
        {/if}

        {#if isAlbum}
            <div class="lyrics-panel popout-tracks" class:is-open={tracksOpen && !compact}>
                <div class="lyrics-panel-inner">
                    <ol class="album-tracks">
                        {#each session.tracks as t, i (i)}
                            {@const name = splitTitle(t.title)}
                            <li>
                                <button class="album-track" class:is-current={i === trackIdx} onclick={() => session.playTrack(i)}>
                                    <span class="album-track-no">{#if i === trackIdx && playing}<span class="album-eq"><span></span><span></span><span></span></span>{:else}{i + 1}{/if}</span>
                                    <span class="album-track-title cutoff">{name.main}{#if name.note}<span class="title-note">{name.note}</span>{/if}</span>
                                    <span class="album-track-len">{fmt((t.end ?? session.durationMs) - t.start)}</span>
                                </button>
                            </li>
                        {/each}
                    </ol>
                </div>
            </div>
        {/if}

        {#if lyrics}
            {#key session.id}
                <div class="lyrics-panel" class:is-open={lyricsOpen && !compact}>
                    <div class="lyrics-panel-inner"><Lyrics {lyrics} {positionMs} playing={session.playing} onSeek={lyricsFrom} /></div>
                </div>
            {/key}
        {/if}

        {#if transcription && transcription.phase !== 'loading'}
            {#key session.id}
                <Transcription t={transcription} {positionMs} playing={session.playing} h={transcriptH}
                               onSeek={(ms) => { session.seek(ms); positionMs = ms; kick(); }}
                               onSettled={() => patchTranscription(session.id, { fresh: false })} />
            {/key}
        {/if}
    </div>
{/if}
