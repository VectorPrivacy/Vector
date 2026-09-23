// Media that follows you. Playback is an object with one owner at a time: the row that
// started it, or the pop-out once that row unmounts (a chat switch, a trimmed window).
// A row that mounts for the same attachment takes it back, so leaving and returning is
// seamless in both directions.
import { claimPlayback, releasePlayback, holdsPlayback, setAudioDuration, playerFor } from './audio.svelte.js';

/** One audio attachment on the engine, whoever is showing it. */
export class AudioSession {
    playing = $state(false);
    loading = $state(false);
    durationMs = $state(0);
    // The last settled position; while playing, position() runs the clock from it.
    pausedAt = $state(0);
    waveform = null;   // { data: Uint8Array, fps, bins }

    #h; #sourceId = null; #startTime = 0; #startPos = 0; #offs = []; #disposed = false;

    /** h: AudioPlayerHelpers. */
    constructor(h, att, msg, chatId) {
        this.#h = h;
        this.att = att;
        this.msg = msg;
        this.chatId = chatId;
        this.id = att.id;
        this.voice = !att.name;
    }

    position() {
        if (!this.playing) return this.pausedAt;
        const p = this.#startPos + (performance.now() - this.#startTime);
        return this.durationMs ? Math.min(p, this.durationMs) : p;
    }

    async #load() {
        const h = this.#h;
        // Listeners before the load: a WAV's FFT can finish before the load returns.
        if (!this.#offs.length) {
            this.#offs.push(await h.listen('audio_ended', (e) => { if (e.payload.id === this.#sourceId) this.#ended(); }));
            this.#offs.push(await h.listen('audio_waveform', (e) => {
                if (e.payload.id !== this.#sourceId) return;
                this.waveform = { data: new Uint8Array(e.payload.waveform), fps: e.payload.waveform_fps, bins: e.payload.bins };
            }));
            this.#offs.push(await h.listen('audio_duration', (e) => { if (e.payload.id === this.#sourceId) this.#setDuration(e.payload.duration_ms); }));
        }
        const result = await h.load(this.att.path);
        this.#sourceId = result.id;
        if (result.duration_ms > 0) this.#setDuration(result.duration_ms);
        if (!this.waveform) this.waveform = { data: null, fps: result.waveform_fps, bins: result.bins };
    }
    #setDuration(ms) { this.durationMs = ms; setAudioDuration(this.id, ms); }

    async play() {
        if (this.loading || this.playing) return;
        const h = this.#h;
        if (!this.#sourceId) {
            this.loading = true;
            try { await this.#load(); } catch (err) { console.error('Audio load failed:', err); this.loading = false; return; }
            this.loading = false;
            if (this.#disposed) { this.dispose(); return; }
            if (this.pausedAt) await h.seek(this.#sourceId, this.pausedAt).catch(() => {});
        }
        claimPlayback(this.id, () => this.pause());
        let posMs;
        try {
            posMs = await h.play(this.#sourceId);
        } catch (_) {
            // The engine evicts paused sources past its cap: load it again where it was.
            try {
                await this.#load();
                if (this.pausedAt) await h.seek(this.#sourceId, this.pausedAt);
                posMs = await h.play(this.#sourceId);
            } catch (err) { console.error('Audio play failed:', err); releasePlayback(this.id); return; }
        }
        // Disposed or outrun by another player while this one was starting.
        if (this.#disposed) { this.dispose(); return; }
        if (!holdsPlayback(this.id)) { h.pause(this.#sourceId).catch(() => {}); return; }
        this.#startTime = performance.now();
        this.#startPos = posMs;
        this.playing = true;
    }

    async pause() {
        releasePlayback(this.id);
        if (!this.playing) return;
        this.pausedAt = this.position();
        this.playing = false;
        try { await this.#h.pause(this.#sourceId); } catch (err) { console.error('Audio pause failed:', err); }
    }

    seek(ms) {
        if (this.#sourceId) this.#h.seek(this.#sourceId, ms).catch(() => {});
        if (this.playing) { this.#startTime = performance.now(); this.#startPos = ms; }
        else this.pausedAt = ms;
    }

    #ended() {
        releasePlayback(this.id);
        this.playing = false;
        this.pausedAt = 0;
        if (this.voice) playNextVoice(this.#h, this);
    }

    dispose() {
        this.#disposed = true;
        releasePlayback(this.id);
        this.playing = false;
        if (this.#sourceId) this.#h.stop(this.#sourceId).catch(() => {});
        this.#sourceId = null;
        for (const off of this.#offs) off();
        this.#offs = [];
    }
}

// A run of voice messages plays through, like one long one: in the row when it is on
// screen, in the pop-out when it is not.
function playNextVoice(h, done) {
    const next = h.nextVoice(done.chatId, done.msg.id);
    if (!next) return;
    const row = playerFor(next.att.id);
    if (row) {
        if (popout.item?.session === done) closePopout();
        row.start();
        return;
    }
    const session = new AudioSession(h, next.att, next.msg, done.chatId);
    if (popOut({ kind: 'audio', session })) session.play();
}

// ── the pop-out's one item ──
// audio: { kind, session }
// video: { kind, id, att, msg, chatId, src, time, playing, aspect }
const popout = $state({ item: null });
export function popoutState() { return popout; }

// Display → Floating Player. Off, leaving a chat stops its media as it always did.
let enabled = true;
export function setPopoutEnabled(on) {
    enabled = !!on;
    if (!enabled) closePopout();
}

/** Hand playback to the pop-out, replacing (and ending) whatever it held. False when the
 *  Floating Player is off: the caller keeps the media, and ends it. */
export function popOut(item) {
    if (!enabled) return false;
    const prev = popout.item;
    if (prev && prev !== item && prev.kind === 'audio' && prev.session !== item.session) prev.session.dispose();
    popout.item = item;
    return true;
}
/** A row for attachment `id` in `chatId` mounted: it takes the pop-out's playback, if it is
 *  this one. The chat matters: the same file sent to two chats is one attachment id. */
export function takeBack(id, kind, chatId) {
    const it = popout.item;
    const held = it?.kind === 'audio' ? it.session : it;
    if (!it || it.kind !== kind || held.id !== id || held.chatId !== chatId) return null;
    popout.item = null;
    return it;
}
/** Media starting in the open chat replaces the pop-out rather than queueing behind it. */
export function yieldPopout(session = null) {
    const it = popout.item;
    if (it && !(it.kind === 'audio' && it.session === session)) closePopout();
}
export function closePopout() {
    const it = popout.item;
    if (it?.kind === 'audio') it.session.dispose();
    popout.item = null;
}
