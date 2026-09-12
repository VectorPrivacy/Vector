<script>
    // The recorder's three pieces, placed where the input row's layout needs them:
    // `strip` (the recording readout and the preview) sits where the editor does,
    // `dot` (the red record dot) rides beside the mic button, `overlays` (the hold hint
    // and the lock target) float over the box. All paint lib/voicerecorder.
    import { recorderState, voiceHandlers, bindVoiceEl } from '../lib/voicerecorder.svelte.js';

    let { part } = $props();   // 'strip' | 'dot' | 'overlays'

    const v = recorderState();
    const bindWaveform = bindVoiceEl('waveform');
    const h = () => voiceHandlers();

    const recording = $derived(v.state === 'recording');
    const locked = $derived(v.state === 'locked');
    const preview = $derived(v.state === 'preview');
    const pct = $derived(v.preview.progress);

    // The chevron's slide animation restarts on every recording, not just the first.
    function restartAnim(node, tick) {
        return { update() { node.style.animation = 'none'; void node.offsetHeight; node.style.animation = ''; } };
    }
</script>

{#if part === 'strip'}
    <div class="voice-recording-ui" class:active={recording || locked} class:cancelling={v.state === 'cancelled'}>
        <div class="voice-recorder-status" style:opacity={v.statusOpacity}>
            <span class="recording-dot"></span>
            <span class="recording-text">{v.statusText}</span>
        </div>
        <div class="voice-recorder-slide-hint" style:display={v.slide.visible ? null : 'none'}
             style:opacity={v.slide.opacity} style:transform={v.slide.offset ? `translateX(${-v.slide.offset}px)` : null}>
            <div class="slide-icon-container">
                <span class="icon icon-chevron-double-left" use:restartAnim={v.slide.animTick} style:background-color={v.slide.hot ? '#ff4444' : null}></span>
            </div>
            <span class="slide-text">Slide to cancel</span>
        </div>
        <div class="voice-recorder-timer" style:opacity={v.timerOpacity}>{v.timer}</div>
    </div>
    <div class="voice-preview-ui" class:active={preview}>
        <button class="voice-preview-delete" aria-label="Delete recording" onclick={() => h()?.previewDelete()}><span class="icon icon-trash"></span></button>
        <div class="voice-preview-center">
            <button class="voice-preview-play" aria-label={v.preview.playing ? 'Pause' : 'Play'} onclick={() => h()?.previewPlayPause()}><span class="icon {v.preview.playing ? 'icon-pause' : 'icon-play'}"></span></button>
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="voice-preview-waveform" use:bindWaveform onpointerdown={(e) => h()?.waveformPointerDown(e)}>
                <div class="voice-preview-progress" style:width="{pct}%"></div>
                <div class="voice-preview-handle" style:left={pct ? `calc(${pct}% - 7px)` : '0'}></div>
            </div>
            <span class="voice-preview-time">{v.preview.time}</span>
        </div>
    </div>
{:else if part === 'dot'}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="voice-recorder-red-circle" class:pending={v.state === 'pending'} class:active={recording || locked} class:locked
         class:returning={v.dot.returning} style:transform={v.dot.transform || null} onclick={() => h()?.dotClick()}></div>
{:else}
    <div class="voice-recorder-tooltip" class:visible={v.tooltip}>Hold to record</div>
    <div class="voice-recorder-lock-zone" class:active={recording || locked} class:locked class:fading-out={v.lock.fading} style:opacity={v.lock.opacity}>
        <div class="lock-indicator" style:transform={v.lock.indicatorTransform || null}>
            <span class="icon icon-locked"></span>
        </div>
        <div class="lock-arrow" style:opacity={v.lock.arrowOpacity}>
            <div class="arrow-icon-container">
                <span class="icon icon-chevron-double-left"></span>
            </div>
        </div>
    </div>
{/if}
