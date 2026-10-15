<script>
    // A downloaded 3D model: a viewport to turn it in (drag; pinch to zoom; double-click to
    // reset), its animation behind a Play button, and the file's own box below for saving.
    import FileBox from './FileBox.svelte';
    import { loadModelEngine } from '../../lib/model3d.js';
    let { att, msg, sender, h } = $props();   // h: MediaHelpers (js/render/chat/message-row.js)

    let phase = $state('loading');
    let error = $state('');
    let animated = $state(false);
    let playing = $state(false);
    let view = null;

    function viewport(canvas) {
        let live = true;
        loadModelEngine().then((engine) => {
            if (!live) return;
            if (!engine.available()) { phase = 'error'; error = "This device can't show 3D models."; return; }
            view = engine.createView(canvas, {
                url: h.assetUrl(att.path),
                ext: (att.extension || '').toLowerCase(),
                onState: (s) => { phase = s.phase; error = s.text || ''; animated = !!s.animated; },
            });
        }).catch(() => {
            if (!live) return;
            phase = 'error';
            error = "The 3D viewer couldn't load.";
        });
        return { destroy() { live = false; view?.dispose(); view = null; } };
    }

    function togglePlay() { playing = !!view?.setPlaying(!playing); }
</script>

<div class="model3d">
    <div class="model3d-stage" class:is-ready={phase === 'ready'}>
        <canvas use:viewport aria-label="3D model {att.name || ''}"></canvas>
        {#if phase === 'loading'}
            <span class="model3d-note">Loading model…</span>
        {:else if phase === 'error'}
            <span class="model3d-note is-error">{error}</span>
        {:else}
            <span class="model3d-badge">3D</span>
            <div class="model3d-tools">
                {#if animated}
                    <button type="button" class="model3d-btn" title={playing ? 'Pause' : 'Play'} onclick={togglePlay}>
                        {#if playing}
                            <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="5" width="4" height="14" rx="1" /><rect x="14" y="5" width="4" height="14" rx="1" /></svg>
                        {:else}
                            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5.5v13a1 1 0 0 0 1.5.86l10.5-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5z" /></svg>
                        {/if}
                    </button>
                {/if}
                <button type="button" class="model3d-btn" title="Reset view" onclick={() => view?.reset()}>
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                        <path d="M3 12a9 9 0 1 0 3-6.7L3 8" /><path d="M3 3v5h5" />
                    </svg>
                </button>
            </div>
        {/if}
    </div>
    <FileBox {att} {msg} {sender} phase="downloaded" {h} />
</div>

<style>
    .model3d {
        display: flex;
        flex-direction: column;
        gap: 6px;
        width: min(100%, 320px);
    }
    .model3d-stage {
        position: relative;
        aspect-ratio: 1;
        border-radius: 12px;
        overflow: hidden;
        background: radial-gradient(circle at 50% 40%, rgba(255, 255, 255, 0.07), rgba(255, 255, 255, 0.015) 70%);
        border: 1px solid rgba(255, 255, 255, 0.06);
    }
    canvas {
        display: block;
        width: 100%;
        height: 100%;
        cursor: grab;
        opacity: 0;
        transition: opacity 0.25s ease;
    }
    canvas:active { cursor: grabbing; }
    .is-ready canvas { opacity: 1; }
    .model3d-note {
        position: absolute;
        inset: 0;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: 16px;
        text-align: center;
        color: rgba(255, 255, 255, 0.5);
        font-size: 13px;
        pointer-events: none;
    }
    .model3d-note.is-error { color: rgba(255, 140, 117, 0.9); }
    .model3d-badge {
        position: absolute;
        top: 8px;
        left: 8px;
        padding: 2px 7px;
        border-radius: 6px;
        background: rgba(0, 0, 0, 0.45);
        color: rgba(255, 255, 255, 0.75);
        font-size: 11px;
        font-weight: 700;
        letter-spacing: 0.06em;
        pointer-events: none;
    }
    .model3d-tools {
        position: absolute;
        right: 8px;
        bottom: 8px;
        display: flex;
        gap: 6px;
    }
    .model3d-btn {
        display: flex;
        align-items: center;
        justify-content: center;
        width: 32px;
        height: 32px;
        padding: 0;
        border: none;
        border-radius: 50%;
        background: rgba(0, 0, 0, 0.5);
        color: #fff;
        cursor: pointer;
    }
    .model3d-btn:hover { background: rgba(0, 0, 0, 0.7); }
    .model3d-btn svg { width: 16px; height: 16px; fill: currentColor; }
    .model3d-btn svg[fill='none'] { fill: none; }
</style>
