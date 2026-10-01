// Vector Web calls, the worker's half: Opus through WebCodecs, between the page's
// audio worklets and the engine in Rust (crates/vector-web/src/calls.rs), which
// owns the datagrams, the jitter buffer and the rate ladder.
import { call_audio_frame, call_audio_level, call_audio_pull, call_audio_ready, call_audio_starved, call_link_close, call_link_open, call_link_send } from './pkg/vector_web.js';

const RATE = 48000;
const FRAME = 960;
const FRAME_US = 20000;
/** A slot the speaker asked for that has nothing yet. */
const NONE = new Float32Array(0);

let call = null;
/** The page video worker's link: `{ id, port }`. */
let link = null;

/** A frame's loudness on a 0 to 1 scale, as desktop meters it: -60 dBFS is silence. */
function level(pcm) {
    let sum = 0;
    for (let i = 0; i < pcm.length; i++) sum += pcm[i] * pcm[i];
    const rms = Math.sqrt(sum / Math.max(1, pcm.length));
    if (rms <= 0) return 0;
    return Math.min(1, Math.max(0, (20 * Math.log10(rms) + 60) / 60));
}

const unit = (v) => (Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 1);

function applySettings(s) {
    if (!call || !s) return;
    call.micGain = unit(s.mic_volume);
    call.speakerGain = unit(s.speaker_volume);
}

/** The engine's instructions, from `set_call_sink`. */
export function onSink(op, json, bytes) {
    const p = json ? JSON.parse(json) : null;
    switch (op) {
        case 'link-frame':
            if (link?.id === p.id) link.port.postMessage(bytes, [bytes.buffer]);
            break;
        case 'link-close':
            if (link?.id === p.id) closeLink(false);
            break;
        case 'start': begin(p); break;
        case 'stop':
            end();
            postMessage({ t: 'call', op: 'stop' });
            break;
        case 'rate':
            if (call) {
                call.kbps = p.kbps;
                call.fec = p.fec;
                if (call.enc?.state === 'configured') configureEncoder(call.enc);
            }
            break;
        case 'volume':
            if (call) call.volume = p.volume;
            break;
        case 'settings':
            applySettings(p.settings);
            postMessage({ t: 'call', op: 'settings', settings: p.settings });
            break;
        case 'ended':
            postMessage({ t: 'call', op: 'ended' });
            break;
    }
}

function begin(p) {
    end();
    if (typeof AudioEncoder !== 'function' || typeof AudioDecoder !== 'function') {
        call_audio_ready("This browser can't encode call audio");
        return;
    }
    call = { kbps: p.kbps, fec: p.fec, volume: p.volume ?? 1, micGain: 1, speakerGain: 1, enc: null, dec: null, mic: null, spk: null, ts: 0, dts: 0, slots: [], decoding: [], last: null };
    applySettings(p.settings);
    postMessage({ t: 'call', op: 'start', settings: p.settings });
}

function end() {
    if (!call) return;
    const c = call;
    call = null;
    try { c.enc?.close(); } catch (_) {}
    try { c.dec?.close(); } catch (_) {}
    c.mic?.close();
    c.spk?.close();
}

/** The video link: the page's video worker on one end, the call's video track on the other. */
function openLink(port) {
    closeLink(true);
    const id = call_link_open();
    if (!id) {
        port.postMessage('close');
        port.close();
        return;
    }
    link = { id, port };
    port.onmessage = ({ data }) => {
        if (link?.port !== port) return;
        if (data === 'close') closeLink(true);
        else call_link_send(id, data instanceof Uint8Array ? data : new Uint8Array(data));
    };
}

/** `tellBackend` false when the backend closed it: it already knows. */
function closeLink(tellBackend) {
    if (!link) return;
    const { id, port } = link;
    link = null;
    if (tellBackend) call_link_close(id);
    port.postMessage('close');
    port.close();
}

/** Messages from the page: its worklets' ports, why it could not open the microphone, a video link. */
export function onPageMessage(data) {
    if (data.t === 'call-link') {
        openLink(data.port);
        return;
    }
    if (data.t === 'call-failed') {
        call_audio_ready(data.error || 'The microphone did not start');
        return;
    }
    if (data.t !== 'call-ports') return;
    if (!call) {
        data.mic.close();
        data.spk.close();
        return;
    }
    try {
        call.enc = makeEncoder();
        call.dec = makeDecoder();
    } catch (e) {
        call_audio_ready(String(e?.message ?? e));
        return;
    }
    call.mic = data.mic;
    call.spk = data.spk;
    call.mic.onmessage = ({ data }) => onMic(data);
    call.spk.onmessage = ({ data }) => {
        if (data.starved) call_audio_starved(data.starved);
        else pull(data.ahead ?? 0);
    };
    call_audio_ready(undefined);
}

function configureEncoder(enc) {
    enc.configure({
        codec: 'opus',
        sampleRate: RATE,
        numberOfChannels: 1,
        bitrate: call.kbps * 1000,
        // In-band FEC lets a decoder with the API for it rebuild a lost frame from its successor.
        opus: { frameDuration: FRAME_US, useinbandfec: true, packetlossperc: call.fec },
    });
}

function makeEncoder() {
    const enc = new AudioEncoder({
        output: (chunk) => {
            const bytes = new Uint8Array(chunk.byteLength);
            chunk.copyTo(bytes);
            call_audio_frame(bytes, true);
        },
        error: (e) => {
            console.warn('[calls] encoder:', e?.message ?? e);
            if (call?.enc === enc) call.enc = null;
        },
    });
    configureEncoder(enc);
    return enc;
}

function makeDecoder() {
    const dec = new AudioDecoder({
        output: (data) => {
            const pcm = toMono48k(data);
            data.close();
            const slot = call?.decoding.shift();
            if (!slot) return;
            call.last = pcm.slice();
            slot.pcm = pcm;
            flush();
        },
        error: (e) => {
            console.warn('[calls] decoder:', e?.message ?? e);
            if (call?.dec !== dec) return;
            call.dec = null;
            for (const slot of call.decoding.splice(0)) slot.pcm = new Float32Array(FRAME);
            flush();
        },
    });
    dec.configure({ codec: 'opus', sampleRate: RATE, numberOfChannels: 1 });
    return dec;
}

/** Whatever layout and rate the decoder chose, as mono floats at the call's rate. */
function toMono48k(data) {
    const n = data.numberOfFrames;
    let pcm = new Float32Array(n);
    try {
        data.copyTo(pcm, { planeIndex: 0, format: 'f32-planar' });
    } catch (_) {
        const interleaved = data.format === 'f32' || data.format === 's16';
        const stride = interleaved ? data.numberOfChannels : 1;
        const raw = data.format.startsWith('s16') ? new Int16Array(n * stride) : new Float32Array(n * stride);
        data.copyTo(raw, { planeIndex: 0 });
        const scale = raw instanceof Int16Array ? 1 / 32768 : 1;
        for (let i = 0; i < n; i++) pcm[i] = raw[i * stride] * scale;
    }
    if (data.sampleRate !== RATE && data.sampleRate > 0) {
        const out = new Float32Array(Math.round((n * RATE) / data.sampleRate));
        const step = data.sampleRate / RATE;
        for (let i = 0; i < out.length; i++) {
            const t = i * step;
            const j = Math.min(n - 1, Math.floor(t));
            const b = pcm[Math.min(n - 1, j + 1)];
            out[i] = pcm[j] + (b - pcm[j]) * (t - j);
        }
        pcm = out;
    }
    return pcm;
}

function onMic(pcm) {
    if (!call) return;
    if (call.micGain !== 1) for (let i = 0; i < pcm.length; i++) pcm[i] *= call.micGain;
    call_audio_level(false, level(pcm));
    const enc = call.enc;
    // A frame goes out every 20 ms whatever happens here: the peer's liveness check counts them.
    if (!enc || enc.state !== 'configured' || enc.encodeQueueSize > 4) {
        call_audio_frame(new Uint8Array(0), false);
        return;
    }
    const data = new AudioData({ format: 'f32-planar', sampleRate: RATE, numberOfFrames: pcm.length, numberOfChannels: 1, timestamp: call.ts, data: pcm });
    call.ts += FRAME_US;
    enc.encode(data);
    data.close();
}

/** The speaker's next frame, answered in the order asked even though decodes finish later. */
function pull(ahead) {
    if (!call) return;
    const r = call_audio_pull(ahead);
    const slot = { pcm: null };
    call.slots.push(slot);
    switch (r?.k) {
        case 1:
            if (!call.dec) {
                try { call.dec = makeDecoder(); } catch (_) {}
            }
            if (call.dec) {
                call.decoding.push(slot);
                call.dec.decode(new EncodedAudioChunk({ type: 'key', timestamp: call.dts, data: r.d }));
                call.dts += FRAME_US;
            } else {
                slot.pcm = new Float32Array(FRAME);
            }
            break;
        case 2:
            slot.pcm = new Float32Array(FRAME);
            break;
        case 3:
            slot.pcm = conceal(r.run);
            break;
        default:
            slot.pcm = NONE;
    }
    flush();
}

/** WebCodecs offers no packet loss concealment. Looping the last frame buzzes, so the
 *  first missing frame is the last one fading to nothing, and any after it are silence. */
function conceal(run) {
    const last = call.last;
    const out = new Float32Array(last?.length ?? FRAME);
    if (!last || run > 1) return out;
    for (let i = 0; i < out.length; i++) out[i] = last[i] * (1 - i / out.length);
    return out;
}

function flush() {
    const c = call;
    if (!c) return;
    while (c.slots.length && c.slots[0].pcm) {
        const { pcm } = c.slots.shift();
        if (pcm === NONE) {
            c.spk?.postMessage({});
            continue;
        }
        call_audio_level(true, level(pcm));
        const gain = c.speakerGain * c.volume;
        if (gain !== 1) for (let i = 0; i < pcm.length; i++) pcm[i] = Math.max(-1, Math.min(1, pcm[i] * gain));
        c.spk?.postMessage({ pcm }, [pcm.buffer]);
    }
}
