<script>
    // Fullscreen 3D model viewer, in the image viewer's frame: drag to turn, wheel or pinch to
    // zoom, double-click to reset. An animated model gets its animations, a scrubber and speed.
    import { modelViewerState, modelViewerHandlers } from '../lib/modelviewer.svelte.js';
    import { loadModelEngine } from '../lib/model3d.js';
    const v = modelViewerState();
    const h = () => modelViewerHandlers();

    let phase = $state('loading');
    let error = $state('');
    let clips = $state([]);
    let clip = $state(0);
    let playing = $state(false);
    let time = $state(0);
    let speed = $state(1);
    let view = null;
    const SPEEDS = [0.25, 0.5, 1, 2];

    function viewport(canvas) {
        let live = true;
        phase = 'loading';
        clips = [];
        clip = 0;
        playing = false;
        time = 0;
        speed = 1;
        loadModelEngine().then((engine) => {
            if (!live) return;
            view = engine.createView(canvas, {
                url: v.url,
                ext: v.ext,
                fullscreen: true,
                onState: (s) => {
                    phase = s.phase;
                    error = s.text || '';
                    if (s.phase === 'ready') {
                        clips = s.clips || [];
                        if (clips.length) playing = !!view?.setPlaying(true);
                    }
                },
            });
        }).catch(() => { if (live) { phase = 'error'; error = "The 3D viewer couldn't load."; } });
        return { destroy() { live = false; view?.dispose(); view = null; } };
    }

    // The scrubber follows the animation only while it plays.
    $effect(() => {
        if (!playing || !v.open) return;
        let raf = requestAnimationFrame(function follow() {
            time = view?.time() ?? 0;
            raf = requestAnimationFrame(follow);
        });
        return () => cancelAnimationFrame(raf);
    });

    const duration = $derived(clips[clip]?.duration || 0);
    function togglePlay() { playing = !!view?.setPlaying(!playing); }
    function pick(i) { clip = i; view?.setClip(i); time = 0; }
    function scrub(t) { time = t; view?.seek(t); }
    function cycleSpeed() {
        speed = SPEEDS[(SPEEDS.indexOf(speed) + 1) % SPEEDS.length];
        view?.setSpeed(speed);
    }
    const fmt = (t) => `${Math.floor(t / 60)}:${String(Math.floor(t % 60)).padStart(2, '0')}`;
    const FILE_LABEL = { reveal: 'Show in folder', share: 'Share', save: 'Save' };
</script>

<svelte:window onkeydown={(e) => { if (v.open && e.key === 'Escape') h().close?.(); }} />

{#if v.open}
    <div class="image-viewer-overlay model-viewer" class:active={v.active} style="display: flex;">
        <canvas class="model-viewer-canvas" use:viewport aria-label="3D model {v.name}"></canvas>
        {#if phase === 'loading'}
            <span class="model-viewer-note">Loading model…</span>
        {:else if phase === 'error'}
            <span class="model-viewer-note is-error">{error}</span>
        {/if}
        <span class="model-viewer-name">{v.name}</span>
        <button class="image-viewer-close" aria-label="Close" onclick={() => h().close?.()}></button>
        <div class="image-viewer-controls">
            {#if v.fileAction}
                <button class="image-viewer-ctrl-btn" aria-label={FILE_LABEL[v.fileAction]} title={FILE_LABEL[v.fileAction]} onclick={() => h().file?.()}>
                    {#if v.fileAction === 'reveal'}
                        <svg viewBox="0 0 24 24" fill="none"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/></svg>
                    {:else if v.fileAction === 'share'}
                        <svg viewBox="0 0 24 24" fill="none"><path d="M12 15V3M12 3L7 8M12 3l5 5M4 14v5a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
                    {:else}
                        <svg viewBox="0 0 24 24" fill="none"><path d="M12 3V15M12 15L7 10M12 15L17 10M4 17V19C4 20.1 4.9 21 6 21H18C19.1 21 20 20.1 20 19V17" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
                    {/if}
                </button>
            {/if}
            <button class="image-viewer-ctrl-btn" aria-label="Reset view" title="Reset view" onclick={() => view?.reset()}>
                <svg viewBox="0 0 24 24" fill="none"><path d="M2 10C2 10 4.00498 7.26822 5.63384 5.63824C7.26269 4.00827 9.5136 3 12 3C16.9706 3 21 7.02944 21 12C21 16.9706 16.9706 21 12 21C7.89691 21 4.43511 18.2543 3.35177 14.5M2 10V4M2 10H8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
            </button>
        </div>
        {#if clips.length}
            <div class="model-viewer-anim">
                <button class="model-viewer-play" aria-label={playing ? 'Pause' : 'Play'} onclick={togglePlay}>
                    {#if playing}
                        <svg viewBox="0 0 24 24"><rect x="6" y="5" width="4" height="14" rx="1" /><rect x="14" y="5" width="4" height="14" rx="1" /></svg>
                    {:else}
                        <svg viewBox="0 0 24 24"><path d="M8 5.5v13a1 1 0 0 0 1.5.86l10.5-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5z" /></svg>
                    {/if}
                </button>
                <input class="model-viewer-scrub" type="range" min="0" max={duration} step="0.01" value={time}
                       aria-label="Animation position" oninput={(e) => scrub(Number(e.currentTarget.value))}>
                <span class="model-viewer-time">{fmt(time)} / {fmt(duration)}</span>
                <button class="model-viewer-speed" aria-label="Speed" onclick={cycleSpeed}>{speed}×</button>
                {#if clips.length > 1}
                    <select class="model-viewer-clip" aria-label="Animation" value={clip} onchange={(e) => pick(Number(e.currentTarget.value))}>
                        {#each clips as c, i (i)}<option value={i}>{c.name}</option>{/each}
                    </select>
                {/if}
            </div>
        {/if}
    </div>
{/if}

<style>
    .model-viewer-canvas {
        position: absolute;
        inset: 0;
        width: 100%;
        height: 100%;
        cursor: grab;
    }
    .model-viewer-canvas:active { cursor: grabbing; }
    .model-viewer-note {
        position: absolute;
        inset: 0;
        display: flex;
        align-items: center;
        justify-content: center;
        color: rgba(255, 255, 255, 0.55);
        font-size: 14px;
        pointer-events: none;
    }
    .model-viewer-note.is-error { color: rgba(255, 140, 117, 0.9); }
    .model-viewer-name {
        position: absolute;
        top: 28px;
        left: 24px;
        right: 90px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: rgba(255, 255, 255, 0.7);
        font-size: 14px;
        pointer-events: none;
    }
    .model-viewer-anim {
        position: absolute;
        left: 50%;
        bottom: max(24px, env(safe-area-inset-bottom));
        transform: translateX(-50%);
        width: min(560px, calc(100% - 32px));
        box-sizing: border-box;
        display: flex;
        align-items: center;
        gap: 10px;
        padding: 8px 12px;
        border-radius: 14px;
        background: rgba(20, 20, 20, 0.82);
        border: 1px solid rgba(255, 255, 255, 0.08);
        color: #fff;
    }
    .model-viewer-play,
    .model-viewer-speed {
        flex-shrink: 0;
        display: flex;
        align-items: center;
        justify-content: center;
        height: 32px;
        padding: 0;
        border: none;
        border-radius: 8px;
        background: rgba(255, 255, 255, 0.08);
        color: #fff;
        cursor: pointer;
    }
    .model-viewer-play { width: 32px; }
    .model-viewer-play svg { width: 16px; height: 16px; fill: currentColor; }
    .model-viewer-speed { min-width: 44px; padding: 0 8px; font-size: 13px; font-variant-numeric: tabular-nums; }
    .model-viewer-play:hover,
    .model-viewer-speed:hover { background: rgba(255, 255, 255, 0.16); }
    .model-viewer-scrub {
        flex: 1;
        min-width: 60px;
        accent-color: var(--icon-color-primary, #59fcb3);
    }
    .model-viewer-time {
        flex-shrink: 0;
        color: rgba(255, 255, 255, 0.6);
        font-size: 12px;
        font-variant-numeric: tabular-nums;
    }
    .model-viewer-clip {
        flex-shrink: 1;
        min-width: 0;
        max-width: 160px;
        height: 32px;
        padding: 0 8px;
        border: 1px solid rgba(255, 255, 255, 0.12);
        border-radius: 8px;
        background: #111;
        color: #fff;
        font-size: 13px;
    }
    @media (max-width: 480px) {
        .model-viewer-time { display: none; }
    }
</style>
