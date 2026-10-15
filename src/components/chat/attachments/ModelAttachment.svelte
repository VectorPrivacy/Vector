<script>
    // A downloaded 3D model: a viewport to turn it in (drag; pinch to zoom), opened full screen
    // by a tap, its animation behind a Play button. The file's box shows only when the preview
    // can't stand in for it: while it uploads, or when the model won't open here.
    import FileBox from './FileBox.svelte';
    import { loadModelEngine } from '../../lib/model3d.js';
    import { messageVersion } from '../../lib/chatview.svelte.js';
    let { att, msg, sender, h } = $props();   // h: MediaHelpers (js/render/chat/message-row.js)

    let phase = $state('loading');
    let error = $state('');
    let animated = $state(false);
    let clips = $state([]);
    // No viewer here after all: the file alone, as anywhere else.
    let fallback = $state(false);
    const uploading = $derived.by(() => { messageVersion(msg.id); return !!(msg.mine && msg.pending); });
    let playing = $state(false);
    let view = null;

    function viewport(canvas) {
        let live = true;
        loadModelEngine().then((engine) => {
            if (!live) return;
            if (!engine.available()) { fallback = true; return; }
            view = engine.createView(canvas, {
                url: h.assetUrl(att.path),
                ext: (att.extension || '').toLowerCase(),
                onState: (s) => { phase = s.phase; error = s.text || ''; animated = !!s.animated; clips = s.clips || []; },
                onTap: () => h.openModel(att),
            });
        }).catch(() => { if (live) fallback = true; });
        return { destroy() { live = false; view?.dispose(); view = null; } };
    }

    function togglePlay() { playing = !!view?.setPlaying(!playing); }
</script>

<div class="model3d">
    {#if !fallback}
    <div class="model3d-stage" class:is-ready={phase === 'ready'}>
        <canvas use:viewport aria-label="3D model {att.name || ''}"></canvas>
        {#if phase === 'loading'}
            <span class="model3d-note">Loading model…</span>
        {:else if phase === 'error'}
            <span class="model3d-note is-error">{error}</span>
        {:else}
            <div class="model3d-top">
            <span class="model3d-badge">3D</span>
            <!-- More than one animation: say which plays here, and that full screen has the rest. -->
            {#if clips.length > 1}
                <button type="button" class="model3d-clip" title="{clips.length} animations: open to choose" onclick={() => h.openModel(att)}>
                    <span class="model3d-clip-name">{clips[0].name}</span>
                    <span class="model3d-clip-more">+{clips.length - 1}</span>
                </button>
            {/if}
            </div>
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
                <button type="button" class="model3d-btn" title="Full screen" onclick={() => h.openModel(att)}>
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                        <path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7" />
                    </svg>
                </button>
                <button type="button" class="model3d-btn" title="Reset view" onclick={() => view?.reset()}>
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                        <path d="M3 12a9 9 0 1 0 3-6.7L3 8" /><path d="M3 3v5h5" />
                    </svg>
                </button>
            </div>
        {/if}
    </div>
    {/if}
    {#if fallback || uploading || phase === 'error'}
        <FileBox {att} {msg} {sender} phase="downloaded" {h} />
    {/if}
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
    .model3d-top {
        position: absolute;
        top: 8px;
        left: 8px;
        right: 8px;
        display: flex;
        align-items: center;
        gap: 6px;
        pointer-events: none;
    }
    .model3d-badge {
        display: flex;
        align-items: center;
        height: 22px;
        padding: 0 7px;
        border-radius: 6px;
        background: rgba(0, 0, 0, 0.45);
        color: rgba(255, 255, 255, 0.75);
        font-size: 11px;
        font-weight: 700;
        letter-spacing: 0.06em;
    }
    .model3d-clip {
        min-width: 0;
        display: flex;
        align-items: center;
        gap: 6px;
        height: 22px;
        padding: 0 8px;
        border: none;
        border-radius: 6px;
        background: rgba(0, 0, 0, 0.45);
        color: rgba(255, 255, 255, 0.85);
        font-size: 12px;
        cursor: pointer;
        pointer-events: auto;
    }
    .model3d-clip:hover { background: rgba(0, 0, 0, 0.65); }
    .model3d-clip-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .model3d-clip-more { flex-shrink: 0; color: rgba(255, 255, 255, 0.5); font-variant-numeric: tabular-nums; }
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
