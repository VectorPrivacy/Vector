// Voice audio as Whisper hears it: 16 kHz mono, as floats. Pure, so the worker and the tests share it.

export const RATE = 16000;
export const MAX_SECONDS = 20 * 60;

/** Refuses what would take a sender's header to allocate or process without bound. */
export function checkLength(frames, rate) {
    if (!(rate >= 8000 && rate <= 384000)) throw new Error(`Unsupported sample rate: ${rate}`);
    if (frames <= 0) throw new Error('No audio to transcribe');
    if (frames / rate > MAX_SECONDS) throw new Error('Voice message too long to transcribe');
}

export function isWav(bytes) {
    if (bytes.byteLength < 12) return false;
    const v = new DataView(bytes instanceof ArrayBuffer ? bytes : bytes.buffer, bytes.byteOffset ?? 0);
    return v.getUint32(0) === 0x52494646 && v.getUint32(8) === 0x57415645; // RIFF … WAVE
}

/** The channels of a PCM16 WAV and its rate; null for any other WAV. */
export function wavChannels(buffer) {
    const v = new DataView(buffer);
    const tag = (at) => String.fromCharCode(v.getUint8(at), v.getUint8(at + 1), v.getUint8(at + 2), v.getUint8(at + 3));
    if (!isWav(buffer)) return null;
    let fmt = null;
    for (let at = 12; at + 8 <= buffer.byteLength;) {
        const id = tag(at);
        const len = v.getUint32(at + 4, true);
        if (id === 'fmt ' && at + 24 <= buffer.byteLength) {
            fmt = { format: v.getUint16(at + 8, true), channels: v.getUint16(at + 10, true), rate: v.getUint32(at + 12, true), bits: v.getUint16(at + 22, true) };
        } else if (id === 'data' && fmt) {
            if (fmt.format !== 1 || fmt.bits !== 16 || !fmt.channels) return null;
            const frames = Math.floor(Math.min(len, buffer.byteLength - at - 8) / (2 * fmt.channels));
            checkLength(frames, fmt.rate);
            const channels = Array.from({ length: fmt.channels }, () => new Float32Array(frames));
            for (let f = 0, p = at + 8; f < frames; f++) {
                for (let c = 0; c < fmt.channels; c++, p += 2) channels[c][f] = v.getInt16(p, true) / 32768;
            }
            return { channels, rate: fmt.rate };
        }
        at += 8 + len + (len & 1);
    }
    return null;
}

/** Mono at 16 kHz: integer ratios average, the rest interpolate, as desktop does. */
export function to16k(channels, rate) {
    const len = channels[0].length;
    checkLength(len, rate);
    let mono = channels[0];
    if (channels.length > 1) {
        mono = new Float32Array(len);
        for (const ch of channels) for (let i = 0; i < len; i++) mono[i] += ch[i] / channels.length;
    }
    if (rate === RATE) return mono;
    const ratio = rate / RATE;
    const out = new Float32Array(Math.max(1, Math.floor(len / ratio)));
    if (Number.isInteger(ratio)) {
        for (let i = 0; i < out.length; i++) {
            let sum = 0;
            for (let j = 0; j < ratio; j++) sum += mono[i * ratio + j] ?? 0;
            out[i] = sum / ratio;
        }
    } else {
        for (let i = 0; i < out.length; i++) {
            const pos = i * ratio;
            const i0 = Math.min(Math.floor(pos), len - 1);
            const t = pos - i0;
            out[i] = mono[i0] * (1 - t) + (mono[i0 + 1] ?? mono[i0]) * t;
        }
    }
    return out;
}
