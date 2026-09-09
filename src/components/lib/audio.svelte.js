// Audio attachments: what outlives the player component. A row scrolls out of the window
// and back, so its duration, tag metadata and transcription are kept by attachment id;
// playback itself belongs to the mounted player. The Whisper model download is one
// app-wide state the player that triggered it renders.
import { SvelteMap } from 'svelte/reactivity';

const byId = new SvelteMap();   // att.id → { durationMs, title, coverArt, transcription }

function entry(id) {
    let e = byId.get(id);
    if (!e) {
        e = { durationMs: 0, title: '', coverArt: '', transcription: null };
        byId.set(id, e);
    }
    return e;
}

export function audioInfo(id) { return byId.get(id) ?? null; }
export function setAudioDuration(id, ms) { if (ms > 0) byId.set(id, { ...entry(id), durationMs: ms }); }
export function setAudioMeta(id, { title, coverArt }) { byId.set(id, { ...entry(id), title: title || '', coverArt: coverArt || '' }); }
/** transcription: { phase: 'loading' | 'ready' | 'error', sections, lang, error, open } */
export function setTranscription(id, t) { byId.set(id, { ...entry(id), transcription: t }); }
export function patchTranscription(id, fields) {
    const e = entry(id);
    if (!e.transcription) return;
    byId.set(id, { ...e, transcription: { ...e.transcription, ...fields } });
}

const modelDownload = $state({ active: false, pct: 0, text: '', failed: false });
export function modelDownloadState() { return modelDownload; }
export function setModelDownload(fields) { Object.assign(modelDownload, fields); }
