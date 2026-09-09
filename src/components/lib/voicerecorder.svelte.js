// The voice recorder's view: what the hold-to-record gesture, the recording timer, the
// lock drag and the preview show. VoiceRecorder (voice.js) owns the gesture, the
// engine calls and the state machine and writes here; VoiceRecorderUI paints it.
const v = $state({
    state: 'idle',            // idle | pending | recording | locked | preview | cancelled
    statusText: 'Recording...',
    timer: '0:00',
    // The cancel drag fades the timer and status; null is the stylesheet's own opacity.
    timerOpacity: null,
    statusOpacity: null,
    slide: { visible: true, offset: 0, opacity: null, hot: false, animTick: 0 },
    dot: { transform: '', returning: false },
    lock: { opacity: null, indicatorTransform: '', arrowOpacity: null, fading: false },
    tooltip: false,
    preview: { playing: false, progress: 0, time: '0:00' },
    // Bumped when the composer's controls return: each plays the fade-in once.
    fadeTick: 0,
});

export function recorderState() { return v; }

/** Enter `state`, clearing every per-gesture visual the previous state left behind. */
export function setVoiceState(state) {
    v.state = state;
    v.timerOpacity = null;
    v.statusOpacity = null;
    v.slide = { visible: state !== 'locked', offset: 0, opacity: null, hot: false, animTick: v.slide.animTick + (state === 'recording' ? 1 : 0) };
    v.lock = { opacity: null, indicatorTransform: '', arrowOpacity: null, fading: false };
    // The clock keeps running through a lock; only a fresh recording starts it over.
    if (state !== 'locked') v.timer = '0:00';
    v.preview = { playing: false, progress: 0, time: '0:00' };
}
export function setVoiceStatusText(text) { v.statusText = text; }
export function setVoiceTimer(text) { v.timer = text; }
export function setVoiceDrag({ timerOpacity, statusOpacity, slide, dot, lock }) {
    if (timerOpacity !== undefined) v.timerOpacity = timerOpacity;
    if (statusOpacity !== undefined) v.statusOpacity = statusOpacity;
    if (slide) Object.assign(v.slide, slide);
    if (dot) Object.assign(v.dot, dot);
    if (lock) Object.assign(v.lock, lock);
}
export function setVoiceDot(fields) { Object.assign(v.dot, fields); }
export function setVoiceLockFading(on) { v.lock.fading = !!on; }
export function setVoiceTooltip(on) { v.tooltip = !!on; }
export function setVoicePreview(fields) { Object.assign(v.preview, fields); }
export function voiceFadeIn() { v.fadeTick++; }

// The recorder's handlers for the preview controls and the red dot, and the waveform
// element its seek gesture measures. Registered by VoiceRecorder once it exists.
let handlers = $state.raw(null);   // { dotClick, previewDelete, previewPlayPause, waveformPointerDown }
export function voiceHandlers() { return handlers; }
export function setVoiceHandlers(h) { handlers = h; }
const els = {};
export function voiceEls() { return els; }
