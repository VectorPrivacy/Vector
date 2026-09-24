<script>
    // An audio attachment: play/pause on the Rust engine, the spectral waveform, the time,
    // the file's tag metadata, the upload ring while it is still sending, and the
    // transcription. The bars are Svelte's; the visualizer paints their styles per frame
    // (animation, not markup), the same way a canvas would.
    import { transfer } from '../../lib/attachments.svelte.js';
    import { messageVersion } from '../../lib/chatview.svelte.js';
    import { untrack } from 'svelte';
    import { audioInfo, setAudioDuration, setAudioMeta, patchTranscription, transcribeAudio, modelDownloadState, registerPlayer, setLyricsOpen,
             splitTitle, songLyrics } from '../../lib/audio.svelte.js';
    import { AudioSession, popOut, takeBack, yieldPopout } from '../../lib/popout.svelte.js';
    import { lockSelection } from '../../lib/draglock.js';
    import Transcription from './Transcription.svelte';
    import Lyrics from './Lyrics.svelte';

    let { att, msg, h } = $props();   // h: AudioPlayerHelpers (js/voice.js)
    //    listen(event, fn) → unlisten, glowColor(), transcriptionSupported(att, msg), transcribe(path), autoTranscribe(msg),
    //    cancelUpload(pendingId), formatTime(seconds), flag(lang), twemojify(el), holdScroll(),
    //    nextVoice(chatId, msgId), reveal(el), viewImage(src), openChat()

    // svelte-ignore state_referenced_locally
    const isVoiceMessage = !att.name;   // an attachment's name never changes under a mounted player
    const info = $derived(audioInfo(att.id));
    const uploading = $derived.by(() => { messageVersion(msg.id); return !!(msg.mine && msg.pending); });
    const up = $derived(uploading ? transfer(msg.id) : null);
    const download = modelDownloadState();
    const transcription = $derived(info?.transcription ?? null);
    const canTranscribe = $derived(!uploading && h.transcriptionSupported(att));

    // ── playback: a session this row owns, or takes back from the pop-out ──
    // svelte-ignore state_referenced_locally
    let session = $state(takeBack(att.id, 'audio', h.openChat())?.session ?? null);
    const playing = $derived(!!session?.playing);
    const loading = $derived(!!session?.loading);
    // svelte-ignore state_referenced_locally
    let positionMs = $state(session?.position() ?? 0);   // what the time display shows
    let durationMs = $derived(info?.durationMs || 0);
    const meta = $derived(info?.meta ?? null);
    const title = $derived(meta?.track ? (meta.artist ? `${meta.artist} — ${meta.track}` : meta.track) : (att.name || ''));
    // The controls wear the art's colour on an album card; grey art leaves them white.
    const accent = $derived(meta?.coverArt ? (meta.accent || 'rgb(242, 242, 242)') : null);
    $effect(() => {
        if (!meta?.coverArt || meta.accent !== undefined) return;
        const m = meta;
        artAccent(m.coverArt).then((accent) => setAudioMeta(att.id, { ...m, accent }));
    });
    // ── an album in one file ──
    const chapters = $derived(meta?.chapters ?? []);
    const isAlbum = $derived(chapters.length > 1);
    const albumTitle = $derived(splitTitle(meta?.album || meta?.track || att.name));
    const albumByline = $derived([meta?.artist, meta?.year].filter(Boolean).join(' · '));
    const trackIdx = $derived(session && session.tracks.length ? session.track : -1);
    const trackLen = (i) => (chapters[i].end_ms ?? durationMs) - chapters[i].start_ms;
    const albumStats = $derived(`${chapters.length} songs${durationMs ? ` · ${Math.round(durationMs / 60000)} min` : ''}`);
    let showAllTracks = $state(false);
    const shownTracks = $derived(showAllTracks ? chapters.length : Math.min(4, chapters.length));
    // The stretch of the file the controls show: the current song of an album, or all of it.
    function spanNow() {
        if (!isAlbum) return { start: 0, end: durationMs };
        const c = chapters[session ? Math.max(0, session.track) : 0];
        return { start: c.start_ms, end: c.end_ms ?? durationMs };
    }
    const curSpan = $derived.by(() => { session?.track; durationMs; return spanNow(); });
    const nowTitle = $derived(isAlbum ? splitTitle(chapters[Math.max(0, trackIdx)]?.title) : null);

    const byline = $derived([meta?.artist, meta?.album !== meta?.track ? meta?.album : ''].filter(Boolean).join(' · '));

    let binDisplay = null;
    // A bar at rest: this short, lifted this far. It is also the floor while playing, so
    // a finished or paused wave settles into it rather than dipping below and snapping back.
    const REST_SCALE = 0.15, REST_Y = -9;
    let barOffsetY = REST_Y;
    let animationId = null, windDownId = null;

    // Header-only probe for the duration, and the tags for a named file. An upload is read
    // too: the file is already on this device, so the card is whole from its first frame
    // and only the ring becomes a play button when the upload lands.
    $effect(() => {
        if (!att.path) return;
        if (!durationMs) h.probe(att.path).then((ms) => setAudioDuration(att.id, ms)).catch(() => {});
        if (!isVoiceMessage && att.path && !meta) {
            h.metadata(att.path).then((m) => {
                setAudioMeta(att.id, { track: m?.title || '', artist: m?.artist || '', album: m?.album || '', coverArt: m?.cover_art || '',
                    lyrics: m?.lyrics || null, year: m?.year || null, chapters: m?.chapters || [] });
            }).catch(() => setAudioMeta(att.id, {}));
        }
    });

    // The art's most prominent colourful hue, lifted to a brightness the controls read at.
    function artAccent(src) {
        return new Promise((resolve) => {
            const img = new Image();
            img.onerror = () => resolve(null);
            img.onload = () => {
                const N = 24, canvas = document.createElement('canvas');
                canvas.width = canvas.height = N;
                const ctx = canvas.getContext('2d', { willReadFrequently: true });
                ctx.drawImage(img, 0, 0, N, N);
                const px = ctx.getImageData(0, 0, N, N).data;
                const bins = Array.from({ length: 12 }, () => ({ w: 0, r: 0, g: 0, b: 0 }));
                let total = 0;
                for (let i = 0; i < px.length; i += 4) {
                    const r = px[i] / 255, g = px[i + 1] / 255, b = px[i + 2] / 255;
                    const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2;
                    const sat = max === min ? 0 : (max - min) / (1 - Math.abs(2 * l - 1));
                    // Near-black and near-white carry no colour worth naming.
                    const w = sat * sat * (1 - Math.abs(2 * l - 1));
                    total += 1;
                    if (w < 0.02) continue;
                    let hue = max === r ? ((g - b) / (max - min)) % 6 : max === g ? (b - r) / (max - min) + 2 : (r - g) / (max - min) + 4;
                    const bin = bins[Math.floor(((hue * 60 + 360) % 360) / 30)];
                    bin.w += w; bin.r += px[i] * w; bin.g += px[i + 1] * w; bin.b += px[i + 2] * w;
                }
                const top = bins.reduce((a, b) => (b.w > a.w ? b : a));
                if (top.w / total < 0.03) { resolve(null); return; }
                const [hh, ss] = toHsl(top.r / top.w, top.g / top.w, top.b / top.w);
                resolve(`hsl(${Math.round(hh)}, ${Math.round(Math.min(0.85, Math.max(0.45, ss)) * 100)}%, 68%)`);
            };
            img.src = src;
        });
    }
    function toHsl(r, g, b) {
        r /= 255; g /= 255; b /= 255;
        const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2, d = max - min;
        if (!d) return [0, 0, l];
        const s = d / (1 - Math.abs(2 * l - 1));
        const h = max === r ? ((g - b) / d + 6) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
        return [h * 60, s, l];
    }

    // ── bars ──
    let waveform = $state(null);
    let barCount = $state(0);
    let bars = [];                        // the rendered bar elements, by index
    // Styles survive a re-count: each new bar takes the state of the old bar at its position.
    let barState = [];                    // [{ transform, opacity, boxShadow }]
    function bar(node, i) {
        bars[i] = node;
        const s = barState[i];
        if (s) { node.style.transform = s.transform; node.style.opacity = s.opacity; node.style.boxShadow = s.boxShadow; }
        return { destroy() { if (bars[i] === node) bars[i] = undefined; } };
    }
    $effect(() => {
        if (!waveform) return;
        const sync = () => {
            const w = waveform.clientWidth;
            if (w === 0) return;
            const target = Math.max(16, Math.min(64, Math.floor(w / 5)));
            if (target === barCount) return;
            const old = barState.slice(), oldCount = barCount;
            barState = Array.from({ length: target }, (_, i) => oldCount > 0 ? old[Math.min(Math.round(i * oldCount / target), oldCount - 1)] : undefined);
            // The bars are keyed by position, so those below the new count stay mounted and
            // never re-register: keep them (the painter finds nothing otherwise) and give
            // them their remapped styles. Bars past the count unregister as they unmount.
            bars.length = Math.min(bars.length, target);
            bars.forEach((node, i) => {
                const st = barState[i];
                if (node && st) { node.style.transform = st.transform; node.style.opacity = st.opacity; node.style.boxShadow = st.boxShadow; }
            });
            barCount = target;
        };
        requestAnimationFrame(sync);
        const ro = new ResizeObserver(sync);
        ro.observe(waveform);
        return () => ro.disconnect();
    });
    function paint(i, transform, opacity, boxShadow) {
        const el = bars[i];
        barState[i] = { transform, opacity, boxShadow };
        if (!el) return;
        el.style.transform = transform;
        el.style.opacity = opacity;
        el.style.boxShadow = boxShadow;
    }

    function frame() {
        if (!durationMs || !session) { animationId = requestAnimationFrame(frame); return; }
        const posMs = Math.min(session.position(), durationMs);
        const sp = spanNow();
        const progress = (posMs - sp.start) / Math.max(1, sp.end - sp.start);
        const { data: waveformData, fps: waveformFps = 30, bins: waveformBins = 64 } = session.waveform || {};
        if (waveformData && waveformData.length > 0) {
            const offset = Math.floor((posMs / 1000) * waveformFps) * waveformBins;
            if (!binDisplay || binDisplay.length !== waveformBins) binDisplay = new Float32Array(waveformBins);
            barOffsetY *= 0.92;
            for (let i = 0; i < waveformBins; i++) {
                // Each band arrives already spread over its own range across the file (the
                // engine's waveform), so a gentle curve is all it needs: quiet detail stays
                // low and peaks stand out, without pinning the loud bands to the top.
                const v = (offset + i < waveformData.length) ? waveformData[offset + i] / 255 : 0;
                const target = Math.min(1, v * Math.sqrt(v));
                binDisplay[i] = target > binDisplay[i] ? binDisplay[i] * 0.7 + target * 0.3 : binDisplay[i] * 0.85 + target * 0.15;
            }
            const glow = accent || h.glowColor();
            for (let i = 0; i < barCount; i++) {
                const val = binDisplay[Math.floor(i * waveformBins / barCount)];
                const yOff = Math.abs(barOffsetY) > 0.5 ? `translateY(${barOffsetY}px) ` : '';
                const barProgress = (i + 0.5) / barCount;
                const opacity = (0.3 + val * 0.7) * (barProgress <= progress ? 1 : 0.4);
                const shadow = val > 0.7 && barProgress <= progress ? `0 0 ${(val - 0.7) * 8}px ${glow}` : 'none';
                paint(i, `${yOff}scaleY(${Math.max(REST_SCALE, val)})`, String(opacity), shadow);
            }
        } else {
            for (let i = 0; i < barCount; i++) {
                paint(i, 'translateY(-7px) scaleY(0.25)', (i + 0.5) / barCount <= progress ? '0.5' : '0.2', 'none');
            }
        }
        positionMs = posMs;
        if (posMs >= durationMs) { animationId = null; return; }   // the engine's ended event settles the rest
        animationId = requestAnimationFrame(frame);
    }

    function windDown(progressOpacity) {
        const run = () => {
            barOffsetY = barOffsetY * 0.92 + REST_Y * 0.08;
            let settled = Math.abs(barOffsetY - REST_Y) < 0.2;
            const waveformBins = session?.waveform?.bins || 64;
            for (let i = 0; i < barCount; i++) {
                const binIdx = Math.floor(i * waveformBins / barCount);
                if (binDisplay && binIdx < binDisplay.length) {
                    binDisplay[binIdx] *= 0.94;
                    if (binDisplay[binIdx] > REST_SCALE) settled = false;
                }
                const scale = Math.max(REST_SCALE, binDisplay ? binDisplay[binIdx] || 0 : 0);
                const yOff = Math.abs(barOffsetY) > 0.5 ? `translateY(${barOffsetY}px) ` : '';
                paint(i, `${yOff}scaleY(${scale})`, progressOpacity(i), 'none');
            }
            if (!settled) { windDownId = requestAnimationFrame(run); return; }
            windDownId = null;
            barOffsetY = REST_Y;
            for (let i = 0; i < barCount; i++) paint(i, `translateY(${REST_Y}px) scaleY(${REST_SCALE})`, barState[i]?.opacity ?? '0.3', 'none');
        };
        run();
    }

    function ensureSession() {
        if (!session) session = new AudioSession(h, att, msg, h.openChat());
        if (isAlbum && !session.tracks.length) session.setTracks(chapters);
        return session;
    }
    function play() {
        if (uploading) return;
        ensureSession();
        yieldPopout('audio', session);
        session.play();
    }
    function playAlbum(shuffled) {
        if (uploading) return;
        const s = ensureSession();
        yieldPopout('audio', s);
        s.setShuffle(shuffled);
        s.playTrack(shuffled ? Math.floor(Math.random() * chapters.length) : 0);
    }
    function playTrackAt(i) {
        if (uploading) return;
        const s = ensureSession();
        yieldPopout('audio', s);
        s.playTrack(i);
    }
    // A session that was handed over, or that began before the tags landed, learns its tracks.
    $effect(() => { if (isAlbum && session && !session.tracks.length) untrack(() => session.setTracks(chapters)); });
    function pause() { session?.pause(); }

    // The bars follow the session, whoever started or stopped it: this row, the one-at-a-time
    // rule, the engine's end, or the pop-out before this row took it back.
    let wasPlaying = false;
    $effect(() => {
        const on = playing;
        untrack(() => {
            if (on) {
                if (windDownId) { cancelAnimationFrame(windDownId); windDownId = null; }
                if (!animationId) frame();
            } else if (wasPlaying) {
                if (animationId) { cancelAnimationFrame(animationId); animationId = null; }
                const at = session?.pausedAt ?? 0;
                positionMs = at;
                const sp = spanNow();
                const progress = sp.end > sp.start ? (at - sp.start) / (sp.end - sp.start) : 0;
                windDown(at === 0 ? () => '0.3' : (i) => (i + 0.5) / barCount <= progress ? '0.3' : '0.15');
            }
        });
        wasPlaying = on;
    });

    let root;
    $effect(() => registerPlayer(att.id, {
        start: () => { if (playing || uploading) return; h.reveal(root); play(); },
    }));

    // ── seek: visuals now, the engine throttled so a drag does not glitch the audio ──
    let dragging = false;
    let seekTimer = null, pendingSeekMs = null;
    function engineSeek(ms) { session?.seek(ms); }
    function seekVisual(clientX) {
        if (!session || !durationMs || !waveform) return;
        const rect = waveform.getBoundingClientRect();
        const x = Math.max(0, Math.min(clientX - rect.left, rect.width));
        const sp = spanNow();
        const posMs = Math.floor(sp.start + (x / rect.width) * (sp.end - sp.start));
        positionMs = posMs;
        if (!playing) {
            const progress = (posMs - sp.start) / Math.max(1, sp.end - sp.start);
            for (let i = 0; i < barCount; i++) {
                const s = barState[i] || { transform: `translateY(${REST_Y}px) scaleY(${REST_SCALE})`, boxShadow: 'none' };
                paint(i, s.transform, (i + 0.5) / barCount <= progress ? '0.3' : '0.15', s.boxShadow);
            }
        }
        pendingSeekMs = posMs;
        if (!seekTimer) {
            seekTimer = setTimeout(() => {
                seekTimer = null;
                if (pendingSeekMs != null) engineSeek(pendingSeekMs);
            }, 50);
        }
    }
    function flushSeek() {
        if (pendingSeekMs != null) { engineSeek(pendingSeekMs); pendingSeekMs = null; }
        if (seekTimer) { clearTimeout(seekTimer); seekTimer = null; }
    }
    let stopDrag = null;   // the in-flight scrub's document listeners, for an unmount mid-drag
    function onMouseDown(e) {
        dragging = true;
        seekVisual(e.clientX);
        const unlock = lockSelection();
        const move = (ev) => { if (dragging) seekVisual(ev.clientX); };
        const stop = () => { stopDrag = null; dragging = false; unlock(); flushSeek(); document.removeEventListener('mousemove', move); document.removeEventListener('mouseup', stop); };
        document.addEventListener('mousemove', move);
        document.addEventListener('mouseup', stop);
        stopDrag = stop;
    }
    function onTouchStart(e) { dragging = true; seekVisual(e.touches[0].clientX); }
    function onTouchMove(e) { if (dragging) { e.preventDefault(); seekVisual(e.touches[0].clientX); } }
    function onTouchEnd() { dragging = false; flushSeek(); }

    // A transcription section click.
    function seekTo(ms) {
        if (!session) return;
        session.seek(ms);
        positionMs = ms;
    }

    // ── lyrics ──
    const allLyrics = $derived(meta?.lyrics ?? null);
    const lyrics = $derived(isAlbum ? songLyrics(allLyrics, curSpan) : allLyrics);
    const lyricsOpen = $derived(!!info?.lyricsOpen);
    // A line is a place in the song: play from it, starting playback if need be.
    function playFrom(ms) {
        if (uploading) return;
        ensureSession();
        session.seek(ms);
        positionMs = ms;
        if (!session.playing) play();
    }
    // The sheet slides open under the card; the conversation holds its distance from the
    // bottom for as long as the slide runs.
    function toggleLyrics() {
        setLyricsOpen(att.id, !lyricsOpen);
        const hold = h.holdScroll();
        const until = performance.now() + 450;
        const step = () => { hold(); if (performance.now() < until) requestAnimationFrame(step); };
        requestAnimationFrame(step);
    }

    // ── transcription ──
    const transcribing = $derived(transcription?.phase === 'loading');
    function onTranscribe() {
        if (transcribing || download.active) return;
        if (transcription?.phase === 'ready') { patchTranscription(att.id, { open: !transcription.open }); return; }
        transcribeAudio(att.id, att.path, h.transcribe);
    }
    // A fresh voice message transcribes itself when the setting is on and the model is here.
    $effect(() => {
        if (canTranscribe && !transcription && h.autoTranscribe(msg)) onTranscribe();
    });

    $effect(() => () => {
        stopDrag?.();
        if (animationId) cancelAnimationFrame(animationId);
        if (windDownId) cancelAnimationFrame(windDownId);
        if (seekTimer) clearTimeout(seekTimer);
        // Playing media follows you: the pop-out carries it on until this row is back.
        if (!(session?.playing && popOut({ kind: 'audio', session }))) session?.dispose();
    });

    const currentText = $derived(h.formatTime(Math.max(0, positionMs - curSpan.start) / 1000));
    const durationText = $derived(h.formatTime(Math.max(0, curSpan.end - curSpan.start) / 1000));
    const transcribeIcon = $derived(transcribing ? 'icon-loading spin' : (transcription?.phase === 'ready' && transcription.open ? 'icon-file-minus' : 'icon-file-plus'));
</script>

<div class="audio-message-container custom-audio-player" bind:this={root} class:has-metadata={!isVoiceMessage && !!att.name}
     style:--icon-color-primary={accent}>
    {#if meta?.coverArt}
        <img class="audio-art-glow" src={meta.coverArt} alt="" aria-hidden="true">
    {/if}
    <div class:audio-album={!!meta?.coverArt || isAlbum} class:is-full={isAlbum} class:no-art={!meta?.coverArt}>
    {#if meta?.coverArt}
        <button class="audio-cover-art" aria-label="View cover art" onclick={() => h.viewImage(meta.coverArt)}>
            <img src={meta.coverArt} alt="" onerror={() => setAudioMeta(att.id, { ...meta, coverArt: '' })}>
        </button>
    {/if}
    {#if isAlbum}
        <div class="audio-track-text album-head">
            <div class="audio-track-title cutoff" title={meta.album || meta.track}>{albumTitle.main}{#if albumTitle.note}<span class="title-note">{albumTitle.note}</span>{/if}</div>
            {#if albumByline}<div class="audio-track-byline cutoff">{albumByline}</div>{/if}
            <div class="album-stats">{albumStats}</div>
            <div class="album-actions">
                <button class="album-play" disabled={uploading} onclick={() => playAlbum(false)}><span class="icon icon-play"></span>Play</button>
                <button class="album-shuffle" disabled={uploading} onclick={() => playAlbum(true)}><span class="icon icon-shuffle"></span>Shuffle</button>
                <span class="album-toggles">
                    <button class="album-toggle" class:is-on={session?.shuffle} aria-label="Shuffle" title="Shuffle" onclick={() => ensureSession().setShuffle(!session.shuffle)}><span class="icon icon-shuffle"></span></button>
                    <button class="album-toggle" class:is-on={session && session.repeat !== 'off'} aria-label="Repeat" title={session?.repeat === 'one' ? 'Repeat one' : session?.repeat === 'all' ? 'Repeat all' : 'Repeat'}
                            onclick={() => ensureSession().cycleRepeat()}><span class="icon {session?.repeat === 'one' ? 'icon-repeat-one' : 'icon-repeat'}"></span></button>
                </span>
            </div>
        </div>
        <ol class="album-tracks">
            {#each chapters.slice(0, shownTracks) as c, i (i)}
                {@const t = splitTitle(c.title)}
                <li>
                    <button class="album-track" class:is-current={i === trackIdx} onclick={() => playTrackAt(i)}>
                        <span class="album-track-no">{#if i === trackIdx && playing}<span class="album-eq"><span></span><span></span><span></span></span>{:else}{i + 1}{/if}</span>
                        <span class="album-track-title cutoff">{t.main}{#if t.note}<span class="title-note">{t.note}</span>{/if}</span>
                        <span class="album-track-len">{h.formatTime(trackLen(i) / 1000)}</span>
                    </button>
                </li>
            {/each}
            {#if chapters.length > 4}
                <li><button class="album-more" onclick={() => (showAllTracks = !showAllTracks)}>{showAllTracks ? 'Show fewer' : `Show all ${chapters.length} songs`}</button></li>
            {/if}
        </ol>
    {:else if meta?.coverArt}
        <div class="audio-track-text">
            <div class="audio-track-title cutoff" title={meta.track || att.name}>{meta.track || att.name}</div>
            {#if byline}<div class="audio-track-byline cutoff" title={byline}>{byline}</div>{/if}
        </div>
    {/if}
    <div class="custom-audio-player-inner" class:playing>
        {#if uploading}
            <div style="position: relative; width: 40px; height: 40px; min-width: 40px; flex-shrink: 0;">
                <div class="miniapp-downloading-spinner" style="width: 40px; height: 40px;" style:--progress={up?.pct != null ? `${up.pct}%` : null}></div>
                {#if up?.phase !== 'publishing'}
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                    <div class="upload-cancel-btn audio-upload-cancel" onclick={(e) => { e.stopPropagation(); h.cancelUpload(msg.id); }}></div>
                {/if}
            </div>
        {:else}
            {#if isAlbum}<button class="audio-skip" aria-label="Previous" onclick={() => ensureSession().prev()}><span class="icon icon-skip-back"></span></button>{/if}
            <button class="audio-play-btn" class:loading disabled={download.active} aria-label={playing ? 'Pause' : 'Play'} onclick={() => { if (playing) pause(); else play(); }}>
                <span class="icon {loading ? 'icon-loading spin' : (playing ? 'icon-pause' : 'icon-play')}"></span>
            </button>
            {#if isAlbum}<button class="audio-skip" aria-label="Next" onclick={() => ensureSession().next()}><span class="icon icon-skip-forward"></span></button>{/if}
        {/if}
        {#snippet waveformEl()}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="audio-waveform" bind:this={waveform} style:display={download.active ? 'none' : null}
                 onmousedown={onMouseDown} ontouchstart={onTouchStart} ontouchmove={onTouchMove} ontouchend={onTouchEnd}>
                {#each { length: barCount } as _, i (i)}
                    <div class="waveform-bar" use:bar={i}></div>
                {/each}
            </div>
        {/snippet}
        {#if isAlbum}
            <div class="audio-waveform-wrapper">
                <div class="audio-filename cutoff">{nowTitle.main}</div>
                {@render waveformEl()}
            </div>
        {:else if !isVoiceMessage && att.name && !meta?.coverArt}
            <div class="audio-waveform-wrapper">
                <div class="audio-filename cutoff" title={title}>{title}</div>
                {@render waveformEl()}
            </div>
        {:else}
            {@render waveformEl()}
        {/if}
        <div class="audio-time-display" style:display={download.active ? 'none' : null}>
            <span class="current-time" class:is-moving={positionMs > 0}>{currentText}</span><span class="audio-time-sep">/</span><span class="duration">{durationText}</span>
        </div>
        {#if canTranscribe}
            {#if download.active}
                <!-- A model download is progress with its own Cancel, not a button. -->
                <div class="audio-transcribe-btn downloading" style:margin-left="auto" style:margin-right="auto">
                    <div class="transcribe-progress-container">
                        <div class="transcribe-progress-text">{download.text}</div>
                        <div class="transcribe-progress-bar"><div class="transcribe-progress-fill" style:width="{download.pct}%" style:background={download.failed ? '#ff5e5e' : null}></div></div>
                        <button class="cancel-download-inline" onclick={(e) => { e.stopPropagation(); h.cancelModelDownload(); }}>Cancel</button>
                    </div>
                </div>
            {:else}
                <button class="audio-transcribe-btn" class:loading={transcribing} class:is-open={transcription?.phase === 'ready' && transcription.open}
                        style:cursor={transcribing ? 'default' : null}
                        aria-label={transcription?.phase === 'ready' ? (transcription.open ? 'Hide transcript' : 'Show transcript') : 'Transcribe'} onclick={onTranscribe}>
                    <span class="icon {transcribeIcon}"></span>
                </button>
            {/if}
        {/if}
        {#if allLyrics}
            <button class="audio-transcribe-btn" class:is-open={lyricsOpen} aria-label={lyricsOpen ? 'Hide lyrics' : 'Show lyrics'}
                    title={lyricsOpen ? 'Hide lyrics' : 'Lyrics'} onclick={toggleLyrics}>
                <span class="icon icon-align-left"></span>
            </button>
        {/if}
    </div>
    </div>
    {#if allLyrics}
        <div class="lyrics-panel" class:is-open={lyricsOpen}>
            <div class="lyrics-panel-inner"><Lyrics {lyrics} {positionMs} {playing} onSeek={playFrom} /></div>
        </div>
    {/if}
    {#if canTranscribe}
        <div class="transcribe-container"></div>
        {#if transcription && transcription.phase !== 'loading'}
            <Transcription t={transcription} {positionMs} {playing} {h} onSeek={seekTo} onSettled={() => patchTranscription(att.id, { fresh: false })} />
        {/if}
    {/if}
</div>
