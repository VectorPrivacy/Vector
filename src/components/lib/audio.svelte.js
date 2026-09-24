// Audio attachments: what outlives the player component. A row scrolls out of the window
// and back, so its duration, tag metadata and transcription are kept by attachment id;
// playback itself belongs to the mounted player. The Whisper model download is one
// app-wide state the player that triggered it renders.
import { SvelteMap } from 'svelte/reactivity';
import { artAccent } from './artcolor.js';

const byId = new SvelteMap();   // att.id → { durationMs, meta, transcription, lyricsOpen }

function entry(id) {
    let e = byId.get(id);
    if (!e) {
        e = { durationMs: 0, meta: null, transcription: null, lyricsOpen: false };
        byId.set(id, e);
    }
    return e;
}

export function audioInfo(id) { return byId.get(id) ?? null; }
export function setAudioDuration(id, ms) { if (ms > 0) byId.set(id, { ...entry(id), durationMs: ms }); }
/** meta: { track, artist, album, coverArt } strings, set once read even when the file has no tags;
 *  `accent` is the art's colour once measured, null for grey art; `lyrics` as src-tauri lyrics.rs
 *  returns them, or null. */
export function setAudioMeta(id, meta) { byId.set(id, { ...entry(id), meta: { track: '', artist: '', album: '', coverArt: '', ...meta } }); }
/** transcription: { phase: 'loading' | 'ready' | 'error', sections, lang, error, open } */
export function setTranscription(id, t) { byId.set(id, { ...entry(id), transcription: t }); }
// An edition note ("2012 Remaster", "Live", "Radio Edit") is split off so the song's own
// name can lead and the note be dimmed.
const EDITION_NOTE = /\s*(?:\(([^()]*\b(?:remaster(?:ed)?|remix|live|mono|stereo|edit|version|demo|acoustic|deluxe|bonus)\b[^()]*)\)|-\s*((?:\d{4}\s*)?remaster(?:ed)?(?:\s*\d{4})?|live|radio edit))\s*$/i;
export function splitTitle(t) {
    const m = (t || '').match(EDITION_NOTE);
    return m ? { main: t.slice(0, m.index).trim(), note: (m[1] || m[2]).trim() } : { main: t || '', note: '' };
}

/** An album's lyrics while one of its songs plays: the lines within that song's stretch. */
export function songLyrics(lyrics, span) {
    if (!lyrics?.synced || !span) return lyrics;
    return { synced: true, lines: lyrics.lines.filter((l) => l.at_ms >= span.start - 50 && l.at_ms < span.end - 50) };
}

/** The engine's waveform, which travels as base64 (a JSON number array is several times larger). */
export function waveformBytes(b64) {
    const raw = atob(b64);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
    return out;
}

/** An album card's control colour: the art's own, white where the art has no colour. */
export function accentOf(meta) {
    return meta?.coverArt ? (meta.accent || 'rgb(242, 242, 242)') : null;
}
/** Measure the art's colour once per attachment, whichever player shows it first. */
const accentRuns = new Set();
export function ensureAccent(id) {
    const m = byId.get(id)?.meta;
    if (!m?.coverArt || m.accent !== undefined || accentRuns.has(id)) return;
    accentRuns.add(id);
    artAccent(m.coverArt).then((accent) => {
        accentRuns.delete(id);
        // Onto the metadata as it stands now: the art may have failed to load meanwhile.
        const now = byId.get(id)?.meta;
        if (now?.coverArt === m.coverArt) setAudioMeta(id, { ...now, accent });
    });
}

/** One waveform bar's next height from its last and the band's level (0-1): a gentle curve,
 *  quick to rise and slower to fall. The engine has already spread each band over its range. */
export function smoothBin(prev, level) {
    const target = Math.min(1, level * Math.sqrt(level));
    return target > prev ? prev * 0.7 + target * 0.3 : prev * 0.85 + target * 0.15;
}

/** The transcript button: open or shut one that's ready, or make it. */
export function toggleTranscript(id, path, transcribe) {
    const t = byId.get(id)?.transcription;
    if (t?.phase === 'loading') return;
    if (t?.phase === 'ready') { patchTranscription(id, { open: !t.open }); return; }
    transcribeAudio(id, path, transcribe);
}
/** How that button looks for transcript `t`. */
export function transcriptButton(t) {
    const ready = t?.phase === 'ready';
    return {
        icon: t?.phase === 'loading' ? 'icon-loading spin' : (ready && t.open ? 'icon-file-minus' : 'icon-file-plus'),
        label: ready ? (t.open ? 'Hide transcript' : 'Show transcript') : 'Transcribe',
        open: ready && !!t.open,
    };
}

/** The lyrics sheet is the attachment's: open in the chat's card means open in the pop-out. */
export function setLyricsOpen(id, open) { byId.set(id, { ...entry(id), lyricsOpen: !!open }); }
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

// One at a time, per lane: a song stops the song before it and a video the video before
// it, but a video watched over music leaves the music playing. A key is whatever plays:
// an audio session itself (the same file in two chats is two sessions), a video's id.
const holders = { audio: null, video: null };   // lane → { key, stop }
export function claimPlayback(key, stop, lane = 'audio') {
    const prev = holders[lane];
    holders[lane] = { key, stop };
    if (prev && prev.key !== key) prev.stop();
}
export function releasePlayback(key, lane = 'audio') { if (holders[lane]?.key === key) holders[lane] = null; }
export function holdsPlayback(key, lane = 'audio') { return holders[lane]?.key === key; }

// Mounted players, by attachment id: a finished voice message hands on to the next one.
const players = new Map();
export function registerPlayer(id, player) {
    players.set(id, player);
    return () => { if (players.get(id) === player) players.delete(id); };
}
export function playerFor(id) { return players.get(id) ?? null; }
