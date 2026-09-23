// Audio attachments: what outlives the player component. A row scrolls out of the window
// and back, so its duration, tag metadata and transcription are kept by attachment id;
// playback itself belongs to the mounted player. The Whisper model download is one
// app-wide state the player that triggered it renders.
import { SvelteMap } from 'svelte/reactivity';

const byId = new SvelteMap();   // att.id → { durationMs, meta, transcription }

function entry(id) {
    let e = byId.get(id);
    if (!e) {
        e = { durationMs: 0, meta: null, transcription: null };
        byId.set(id, e);
    }
    return e;
}

export function audioInfo(id) { return byId.get(id) ?? null; }
export function setAudioDuration(id, ms) { if (ms > 0) byId.set(id, { ...entry(id), durationMs: ms }); }
/** meta: { track, artist, album, coverArt } strings, set once read even when the file has no tags;
 *  `accent` is the art's colour once measured, null for grey art. */
export function setAudioMeta(id, meta) { byId.set(id, { ...entry(id), meta: { track: '', artist: '', album: '', coverArt: '', ...meta } }); }
/** transcription: { phase: 'loading' | 'ready' | 'error', sections, lang, error, open } */
export function setTranscription(id, t) { byId.set(id, { ...entry(id), transcription: t }); }
export function patchTranscription(id, fields) {
    const e = entry(id);
    if (!e.transcription) return;
    byId.set(id, { ...e, transcription: { ...e.transcription, ...fields } });
}

/** Transcribe an attachment once, whoever asks: the chat's player and the pop-out share the
 *  result, and a request while one is running (or after one landed) starts nothing. */
export async function transcribeAudio(id, path, transcribe) {
    const phase = byId.get(id)?.transcription?.phase;
    if (phase === 'loading' || phase === 'ready') return;
    setTranscription(id, { phase: 'loading', sections: [], lang: '', error: '', open: false });
    try {
        const data = await transcribe(path);
        setTranscription(id, { phase: 'ready', sections: data.sections || [], lang: data.lang || '', language: data.language || '', error: '', open: true, fresh: true });
    } catch (err) {
        console.error('Transcription error:', err);
        setTranscription(id, { phase: 'error', sections: [], lang: '', error: err?.message || 'Transcription failed', open: true, fresh: true });
    }
}

const modelDownload = $state({ active: false, pct: 0, text: '', failed: false });
export function modelDownloadState() { return modelDownload; }
export function setModelDownload(fields) { Object.assign(modelDownload, fields); }

// One sound at a time: starting a player stops whichever one holds the output.
let holder = null;   // { id, stop }
export function claimPlayback(id, stop) {
    const prev = holder;
    holder = { id, stop };
    if (prev && prev.id !== id) prev.stop();
}
export function releasePlayback(id) { if (holder?.id === id) holder = null; }
export function holdsPlayback(id) { return holder?.id === id; }

// Mounted players, by attachment id: a finished voice message hands on to the next one.
const players = new Map();
export function registerPlayer(id, player) {
    players.set(id, player);
    return () => { if (players.get(id) === player) players.delete(id); };
}
export function playerFor(id) { return players.get(id) ?? null; }
