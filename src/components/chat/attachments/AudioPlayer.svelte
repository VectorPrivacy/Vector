<script>
    // An audio attachment: play/pause on the Rust engine, the spectral waveform, the time,
    // the file's tag metadata, the upload ring while it is still sending, and the
    // transcription. The bars are Svelte's; the visualizer paints their styles per frame
    // (animation, not markup), the same way a canvas would.
    import { transfer } from '../../lib/attachments.svelte.js';
    import { messageVersion } from '../../lib/chatview.svelte.js';
    import { audioInfo, setAudioDuration, setAudioMeta, setTranscription, patchTranscription, modelDownloadState } from '../../lib/audio.svelte.js';
    import Transcription from './Transcription.svelte';

    let { att, msg, h } = $props();   // h: AudioPlayerHelpers (js/voice.js)
    //    listen(event, fn) → unlisten, glowColor(), transcriptionSupported(att, msg), transcribe(path), autoTranscribe(msg),
    //    cancelUpload(pendingId), formatTime(seconds), autoTranslate(), flag(lang), twemojify(el), scrollBy(px)

    // svelte-ignore state_referenced_locally
    const isVoiceMessage = !att.name;   // an attachment's name never changes under a mounted player
    const info = $derived(audioInfo(att.id));
    const uploading = $derived.by(() => { messageVersion(msg.id); return !!(msg.mine && msg.pending); });
    const up = $derived(uploading ? transfer(msg.id) : null);
    const download = modelDownloadState();
    const transcription = $derived(info?.transcription ?? null);
    const canTranscribe = $derived(!uploading && h.transcriptionSupported(att));

    // ── playback ──
    let sourceId = null;
    let playing = $state(false);
    let loading = $state(false);
    let positionMs = $state(0);          // what the time display shows
    let durationMs = $derived(info?.durationMs || 0);
    const title = $derived(info?.title || att.name || '');

    let waveformData = null, waveformFps = 30, waveformBins = 64;
    let binDisplay = null;
    let barOffsetY = -9;
    let playStartTime = 0, playStartPos = 0;
    let animationId = null, windDownId = null;
    let unlisteners = [];

    // Header-only probe for the duration, and the tags for an uploaded file.
    $effect(() => {
        if (uploading) return;
        if (!durationMs) h.probe(att.path).then((ms) => setAudioDuration(att.id, ms)).catch(() => {});
        if (!isVoiceMessage && att.path && !info?.title && !info?.coverArt) {
            h.metadata(att.path).then((meta) => {
                if (!meta) return;
                setAudioMeta(att.id, {
                    title: meta.title ? (meta.artist ? `${meta.artist} — ${meta.title}` : meta.title) : '',
                    coverArt: meta.cover_art || '',
                });
            }).catch(() => {});
        }
    });

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
            bars = [];
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
        if (!durationMs) { animationId = requestAnimationFrame(frame); return; }
        const posMs = Math.min(playStartPos + (performance.now() - playStartTime), durationMs);
        const progress = posMs / durationMs;
        if (waveformData && waveformData.length > 0) {
            const offset = Math.floor((posMs / 1000) * waveformFps) * waveformBins;
            if (!binDisplay || binDisplay.length !== waveformBins) binDisplay = new Float32Array(waveformBins);
            barOffsetY *= 0.92;
            // Mean deviation per frame: the spectral shape, with every bin weighted alike.
            let frameMean = 0;
            for (let i = 0; i < waveformBins; i++) frameMean += (offset + i < waveformData.length) ? waveformData[offset + i] / 255 : 0;
            frameMean /= waveformBins;
            const level = Math.min(1, Math.sqrt(frameMean * 2));
            for (let i = 0; i < waveformBins; i++) {
                const v = (offset + i < waveformData.length) ? waveformData[offset + i] / 255 : 0;
                const target = Math.max(0.05, Math.min(1, (v - frameMean) * 2.5 + 0.5)) * level;
                binDisplay[i] = target > binDisplay[i] ? binDisplay[i] * 0.7 + target * 0.3 : binDisplay[i] * 0.85 + target * 0.15;
            }
            const glow = h.glowColor();
            for (let i = 0; i < barCount; i++) {
                const val = binDisplay[Math.floor(i * waveformBins / barCount)];
                const yOff = Math.abs(barOffsetY) > 0.5 ? `translateY(${barOffsetY}px) ` : '';
                const barProgress = (i + 0.5) / barCount;
                const opacity = (0.3 + val * 0.7) * (barProgress <= progress ? 1 : 0.4);
                const shadow = val > 0.7 && barProgress <= progress ? `0 0 ${(val - 0.7) * 8}px ${glow}` : 'none';
                paint(i, `${yOff}scaleY(${Math.max(0.1, val)})`, String(opacity), shadow);
            }
        } else {
            for (let i = 0; i < barCount; i++) {
                paint(i, 'translateY(-7px) scaleY(0.25)', (i + 0.5) / barCount <= progress ? '0.5' : '0.2', 'none');
            }
        }
        positionMs = posMs;
        if (posMs >= durationMs) return;   // the engine's ended event settles the rest
        animationId = requestAnimationFrame(frame);
    }

    function windDown(progressOpacity) {
        const run = () => {
            barOffsetY = barOffsetY * 0.92 + -9 * 0.08;
            let settled = true;
            for (let i = 0; i < barCount; i++) {
                const binIdx = Math.floor(i * waveformBins / barCount);
                if (binDisplay && binIdx < binDisplay.length) {
                    binDisplay[binIdx] *= 0.94;
                    if (binDisplay[binIdx] > 0.02) settled = false;
                }
                const scale = Math.max(0.1, binDisplay ? binDisplay[binIdx] || 0.1 : 0.1);
                const yOff = Math.abs(barOffsetY) > 0.5 ? `translateY(${barOffsetY}px) ` : '';
                paint(i, `${yOff}scaleY(${scale})`, progressOpacity(i), 'none');
            }
            if (!settled) { windDownId = requestAnimationFrame(run); return; }
            windDownId = null;
            barOffsetY = -9;
            for (let i = 0; i < barCount; i++) paint(i, 'translateY(-9px) scaleY(0.15)', barState[i]?.opacity ?? '0.3', 'none');
        };
        run();
    }

    async function play() {
        if (uploading || loading) return;
        if (!sourceId) {
            loading = true;
            try {
                // Listeners before the load: a WAV's FFT can finish before the load returns.
                unlisteners.push(await h.listen('audio_ended', (e) => { if (e.payload.id === sourceId) onEnded(); }));
                unlisteners.push(await h.listen('audio_waveform', (e) => {
                    if (e.payload.id !== sourceId) return;
                    waveformData = new Uint8Array(e.payload.waveform);
                    waveformFps = e.payload.waveform_fps;
                    waveformBins = e.payload.bins;
                }));
                unlisteners.push(await h.listen('audio_duration', (e) => { if (e.payload.id === sourceId) setAudioDuration(att.id, e.payload.duration_ms); }));
                const result = await h.load(att.path);
                sourceId = result.id;
                if (result.duration_ms > 0) setAudioDuration(att.id, result.duration_ms);
                waveformFps = result.waveform_fps;
                waveformBins = result.bins;
            } catch (err) {
                console.error('Audio load failed:', err);
                loading = false;
                return;
            }
            loading = false;
        }
        try {
            const posMs = await h.play(sourceId);
            playStartTime = performance.now();
            playStartPos = posMs;
            if (windDownId) { cancelAnimationFrame(windDownId); windDownId = null; }
            playing = true;
            frame();
        } catch (err) {
            console.error('Audio play failed:', err);
        }
    }

    async function pause() {
        if (!sourceId) return;
        try { await h.pause(sourceId); } catch (err) { console.error('Audio pause failed:', err); }
        playing = false;
        if (animationId) { cancelAnimationFrame(animationId); animationId = null; }
        if (durationMs > 0) {
            const progress = Math.min(playStartPos + (performance.now() - playStartTime), durationMs) / durationMs;
            windDown((i) => (i + 0.5) / barCount <= progress ? '0.3' : '0.15');
        } else {
            barOffsetY = -9;
        }
    }

    function onEnded() {
        playing = false;
        positionMs = 0;
        if (animationId) { cancelAnimationFrame(animationId); animationId = null; }
        windDown(() => '0.3');
    }

    // ── seek: visuals now, the engine throttled so a drag does not glitch the audio ──
    let dragging = false;
    let seekTimer = null, pendingSeekMs = null;
    function engineSeek(ms) {
        h.seek(sourceId, ms).catch(() => {});
        if (playing) { playStartTime = performance.now(); playStartPos = ms; }
    }
    function seekVisual(clientX) {
        if (!sourceId || !durationMs || !waveform) return;
        const rect = waveform.getBoundingClientRect();
        const x = Math.max(0, Math.min(clientX - rect.left, rect.width));
        const posMs = Math.floor((x / rect.width) * durationMs);
        positionMs = posMs;
        if (!playing) {
            const progress = posMs / durationMs;
            for (let i = 0; i < barCount; i++) {
                const s = barState[i] || { transform: 'translateY(-9px) scaleY(0.15)', boxShadow: 'none' };
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
        const move = (ev) => { if (dragging) seekVisual(ev.clientX); };
        const stop = () => { stopDrag = null; dragging = false; flushSeek(); document.removeEventListener('mousemove', move); document.removeEventListener('mouseup', stop); };
        document.addEventListener('mousemove', move);
        document.addEventListener('mouseup', stop);
        stopDrag = stop;
    }
    function onTouchStart(e) { dragging = true; seekVisual(e.touches[0].clientX); }
    function onTouchMove(e) { if (dragging) { e.preventDefault(); seekVisual(e.touches[0].clientX); } }
    function onTouchEnd() { dragging = false; flushSeek(); }

    // A transcription section click.
    async function seekTo(ms) {
        if (!sourceId) return;
        await h.seek(sourceId, ms);
        if (playing) { playStartTime = performance.now(); playStartPos = ms; }
        positionMs = ms;
    }

    // ── transcription ──
    const transcribing = $derived(transcription?.phase === 'loading');
    async function onTranscribe() {
        if (transcribing || download.active) return;
        if (transcription?.phase === 'ready') { patchTranscription(att.id, { open: !transcription.open }); return; }
        setTranscription(att.id, { phase: 'loading', sections: [], lang: '', error: '', open: false });
        try {
            const data = await h.transcribe(att.path);
            setTranscription(att.id, { phase: 'ready', sections: data.sections || [], lang: data.lang || '', error: '', open: true });
        } catch (err) {
            console.error('Transcription error:', err);
            setTranscription(att.id, { phase: 'error', sections: [], lang: '', error: err?.message || 'Transcription failed', open: true });
        }
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
        if (sourceId) h.stop(sourceId).catch(() => {});
        for (const off of unlisteners) off();
        unlisteners = [];
    });

    const currentText = $derived(h.formatTime(positionMs / 1000));
    const durationText = $derived(h.formatTime(durationMs / 1000));
    const transcribeIcon = $derived(transcribing ? 'icon-loading spin' : (transcription?.phase === 'ready' && transcription.open ? 'icon-file-minus' : 'icon-file-plus'));
</script>

<div class="audio-message-container custom-audio-player" class:has-metadata={!isVoiceMessage && !!att.name}>
    {#if info?.coverArt}
        <div class="audio-cover-art-wrap"><img class="audio-cover-art" src={info.coverArt} alt="" onerror={() => setAudioMeta(att.id, { title: info.title, coverArt: '' })}></div>
    {/if}
    <div class="custom-audio-player-inner" class:playing>
        {#if uploading}
            <div style="position: relative; width: 40px; height: 40px; min-width: 40px; flex-shrink: 0;">
                <div class="miniapp-downloading-spinner" style="width: 40px; height: 40px;" style:--progress={up?.pct != null ? `${up.pct}%` : null}></div>
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="upload-cancel-btn audio-upload-cancel" onclick={(e) => { e.stopPropagation(); h.cancelUpload(msg.id); }}></div>
            </div>
        {:else}
            <button class="audio-play-btn" class:loading disabled={download.active} onclick={() => { if (playing) pause(); else play(); }}>
                <span class="icon {loading ? 'icon-loading spin' : (playing ? 'icon-pause' : 'icon-play')}"></span>
            </button>
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
        {#if !isVoiceMessage && att.name}
            <div class="audio-waveform-wrapper">
                <div class="audio-filename cutoff" title={title}>{title}</div>
                {@render waveformEl()}
            </div>
        {:else}
            {@render waveformEl()}
        {/if}
        <div class="audio-time-display" style:display={download.active ? 'none' : null}>
            <span class="current-time" style:color={positionMs > 0 ? '#ffffffb3' : null}>{currentText}</span> / <span class="duration">{durationText}</span>
        </div>
        {#if canTranscribe}
            <button class="audio-transcribe-btn" class:loading={transcribing} class:downloading={download.active}
                    style:cursor={transcribing ? 'default' : null} style:margin-left={download.active ? 'auto' : null} style:margin-right={download.active ? 'auto' : null}
                    onclick={onTranscribe}>
                {#if download.active}
                    <div class="transcribe-progress-container">
                        <div class="transcribe-progress-text">{download.text}</div>
                        <div class="transcribe-progress-bar"><div class="transcribe-progress-fill" style:width="{download.pct}%" style:background={download.failed ? '#ff5e5e' : null}></div></div>
                        <button class="cancel-download-inline" onclick={(e) => { e.stopPropagation(); h.cancelModelDownload(); }}>Cancel</button>
                    </div>
                {:else}
                    <span class="icon {transcribeIcon}"></span>
                {/if}
            </button>
        {/if}
    </div>
    {#if canTranscribe}
        <div class="transcribe-container"></div>
        {#if transcription && transcription.phase !== 'loading'}
            <Transcription t={transcription} {positionMs} {playing} {h} onSeek={seekTo} />
        {/if}
    {/if}
</div>
