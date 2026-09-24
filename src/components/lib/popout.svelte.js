// Media that follows you. Playback is an object with one owner at a time: the row that
// started it, or the pop-out once that row unmounts (a chat switch, a trimmed window).
// A row that mounts for the same attachment takes it back, so leaving and returning is
// seamless in both directions.
import { claimPlayback, releasePlayback, holdsPlayback, setAudioDuration, playerFor, waveformBytes } from './audio.svelte.js';

/** One audio attachment on the engine, whoever is showing it. */
export class AudioSession {
    playing = $state(false);
    loading = $state(false);
    durationMs = $state(0);
    // The last settled position; while playing, position() runs the clock from it.
    pausedAt = $state(0);
    waveform = null;   // { data: Uint8Array, fps, bins }

    // An album in one file: its tracks, the one under the playhead, and the listener's way
    // through them. Tracks are places in the file, so moving on inside it is gapless.
    tracks = $state([]);      // [{ start, end, title }]; the last one's end is null: the file's end
    track = $state(-1);
    shuffle = $state(false);
    repeat = $state('off');   // off | all | one
    /** Bumped each time playback ends with nothing to follow it, for whoever shows it. */
    finished = $state(0);

    #h; #sourceId = null; #startTime = 0; #startPos = 0; #offs = []; #disposed = false;
    #order = []; #orderAt = 0; #watch = null;

    /** h: AudioPlayerHelpers. */
    constructor(h, att, msg, chatId) {
        this.#h = h;
        this.att = att;
        this.msg = msg;
        this.chatId = chatId;
        this.id = att.id;
        this.voice = !att.name;
    }

    setTracks(chapters) {
        this.tracks = (chapters || []).map((c) => ({ start: c.start_ms, end: c.end_ms ?? null, title: c.title }));
        this.track = this.trackAt(this.position());
        this.#reorder();
        if (this.playing) this.#watchTracks();
    }
    trackAt(ms) {
        let at = -1;
        for (let i = 0; i < this.tracks.length && this.tracks[i].start <= ms + 1; i++) at = i;
        return at;
    }
    /** Where track `i` runs in the file. */
    span(i) {
        const t = this.tracks[i];
        return t ? { start: t.start, end: t.end ?? this.durationMs } : { start: 0, end: this.durationMs };
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
                this.waveform = { data: waveformBytes(e.payload.waveform), fps: e.payload.waveform_fps, bins: e.payload.bins };
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
        // Claimed at the press, not after the load: a later press elsewhere outranks this
        // one, and a pause while it loads calls it off.
        claimPlayback(this, () => this.pause());
        if (!this.#sourceId) {
            this.loading = true;
            try {
                await this.#load();
            } catch (err) {
                console.error('Audio load failed:', err);
                this.loading = false;
                releasePlayback(this);
                if (this.#disposed) this.dispose();
                return;
            }
            this.loading = false;
            if (this.#disposed) { this.dispose(); return; }
            if (!holdsPlayback(this)) return;
            if (this.pausedAt) await h.seek(this.#sourceId, this.pausedAt).catch(() => {});
        }
        let posMs;
        try {
            posMs = await h.play(this.#sourceId);
        } catch (_) {
            if (this.#disposed) { this.dispose(); return; }
            // The engine evicts paused sources past its cap: load it again where it was.
            try {
                h.stop(this.#sourceId).catch(() => {});
                this.#sourceId = null;
                await this.#load();
                if (this.pausedAt) await h.seek(this.#sourceId, this.pausedAt);
                posMs = await h.play(this.#sourceId);
            } catch (err) { console.error('Audio play failed:', err); releasePlayback(this); return; }
        }
        // Disposed or outrun by another player while this one was starting.
        if (this.#disposed) { this.dispose(); return; }
        if (!holdsPlayback(this)) { h.pause(this.#sourceId).catch(() => {}); return; }
        this.#startTime = performance.now();
        this.#startPos = posMs;
        this.playing = true;
        this.#watchTracks();
    }

    async pause() {
        releasePlayback(this);
        this.#unwatch();
        if (!this.playing) return;
        this.pausedAt = this.position();
        this.playing = false;
        try { await this.#h.pause(this.#sourceId); } catch (err) { console.error('Audio pause failed:', err); }
    }

    seek(ms) {
        if (this.#sourceId) this.#h.seek(this.#sourceId, ms).catch(() => {});
        if (this.playing) { this.#startTime = performance.now(); this.#startPos = ms; }
        else this.pausedAt = ms;
        // A seek is a choice, not the song moving on: the track follows it quietly.
        if (this.tracks.length) this.track = this.trackAt(ms);
    }

    // ── moving through an album ──
    /** Play track `i` from its start, whatever the order was. */
    playTrack(i) {
        if (!this.tracks[i]) return;
        this.#goto(i);
        if (this.shuffle) { this.#reorder(); }
        if (!this.playing) this.play();
    }
    next() {
        const n = this.#nextIndex();
        if (n != null) this.#goto(n);
    }
    /** Back to the start of the song, or, near its start already, to the song before. */
    prev() {
        const i = Math.max(0, this.track);
        if (this.position() - this.span(i).start > 3000) { this.#goto(i); return; }
        if (this.shuffle && this.#orderAt > 0) { this.#goto(this.#order[--this.#orderAt], true); return; }
        if (!this.shuffle && i > 0) { this.#goto(i - 1); return; }
        if (!this.shuffle && this.repeat === 'all') { this.#goto(this.tracks.length - 1); return; }
        this.#goto(i);
    }
    setShuffle(on) {
        this.shuffle = !!on;
        this.#reorder();
    }
    cycleRepeat() {
        this.repeat = this.repeat === 'off' ? 'all' : this.repeat === 'all' ? 'one' : 'off';
    }

    // The listener's way through: the tracks in order, or shuffled with the current first.
    #reorder() {
        const n = this.tracks.length, cur = Math.max(0, this.track);
        const rest = [...Array(n).keys()].filter((i) => i !== cur);
        if (this.shuffle) {
            for (let i = rest.length - 1; i > 0; i--) { const j = Math.floor(Math.random() * (i + 1)); [rest[i], rest[j]] = [rest[j], rest[i]]; }
        }
        this.#order = n ? [cur, ...rest] : [];
        this.#orderAt = 0;
    }
    #nextIndex() {
        if (!this.tracks.length) return null;
        if (this.shuffle) {
            if (this.#orderAt + 1 < this.#order.length) return this.#order[this.#orderAt + 1];
            if (this.repeat !== 'all') return null;
            const last = this.#order[this.#orderAt];
            this.#reorder();
            // A fresh round never starts with the song that just ended.
            if (this.#order.length > 1 && this.#order[0] === last) this.#order.push(this.#order.shift());
            this.#orderAt = -1;
            return this.#order[0];
        }
        const i = this.track + 1;
        if (i < this.tracks.length) return i;
        return this.repeat === 'all' ? 0 : null;
    }
    #goto(i, keepOrder = false) {
        if (this.shuffle && !keepOrder) {
            const at = this.#order.indexOf(i);
            if (at >= 0) this.#orderAt = at;
        }
        this.seek(this.span(i).start);
        this.track = i;
    }
    #watchTracks() {
        if (this.tracks.length && !this.#watch) this.#watch = setInterval(() => this.#tick(), 50);
    }
    // While an album plays: note the song moving on, and step in when the listener's way
    // is not simply the next song in the file.
    #tick() {
        const i = this.trackAt(this.position());
        if (i === this.track) return;
        const from = this.track;
        this.track = i;
        if (i !== from + 1) return;
        if (this.repeat === 'one') { this.#goto(from); return; }
        if (this.shuffle) {
            const n = this.#nextIndex();
            if (n == null) { this.pause(); this.#goto(this.#order[0] ?? 0); this.finished++; return; }
            this.#orderAt++;
            // Already there: the shuffle's pick is the song the file moved into.
            if (n !== i) this.#goto(n, true);
        }
    }
    #unwatch() {
        if (this.#watch) { clearInterval(this.#watch); this.#watch = null; }
    }

    #ended() {
        releasePlayback(this);
        this.#unwatch();
        this.playing = false;
        this.pausedAt = 0;
        if (this.tracks.length) {
            // The file's end is the last track's end: the listener's way decides what follows.
            const last = this.track;
            const n = this.repeat === 'one' ? last : this.#nextIndex();
            if (n != null && n >= 0) {
                if (this.shuffle && this.repeat !== 'one') this.#orderAt++;
                this.#goto(n, true);
                this.play();
                return;
            }
            this.track = this.trackAt(0);
        }
        if (this.voice && playNextVoice(this.#h, this)) return;
        this.finished++;
    }

    dispose() {
        this.#disposed = true;
        this.#unwatch();
        releasePlayback(this);
        this.playing = false;
        if (this.#sourceId) this.#h.stop(this.#sourceId).catch(() => {});
        this.#sourceId = null;
        for (const off of this.#offs) off();
        this.#offs = [];
    }
}

// A run of voice messages plays through, like one long one: in the row when it is on
// screen, in the pop-out when it is not.
/** Whether anything followed. */
function playNextVoice(h, done) {
    const next = h.nextVoice(done.chatId, done.msg.id);
    if (!next) return false;
    const row = playerFor(next.att.id);
    if (row) {
        if (popout.item?.session === done) closePopout();
        row.start();
        return true;
    }
    const session = new AudioSession(h, next.att, next.msg, done.chatId);
    if (!popOut({ kind: 'audio', session })) return false;
    session.play();
    return true;
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
/** Media starting in the open chat replaces the pop-out's media of the same kind rather
 *  than queueing behind it; a video over music (or music over a video) leaves it be. */
export function yieldPopout(kind, session = null) {
    const it = popout.item;
    if (it && it.kind === kind && !(kind === 'audio' && it.session === session)) closePopout();
}
/** Whether the pop-out is busy with music: a video leaving its chat then stops rather than
 *  taking the player from it. */
export function popoutPlayingAudio() {
    const it = popout.item;
    return !!(it && it.kind === 'audio' && it.session.playing);
}
export function closePopout() {
    const it = popout.item;
    if (it?.kind === 'audio') it.session.dispose();
    popout.item = null;
}
