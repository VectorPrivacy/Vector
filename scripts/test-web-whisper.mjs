// Vector Web's Whisper: the audio it hears, how it reads a pass, and how a model downloads.
// Run: node --test scripts/test-web-whisper.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { RATE, checkLength, isWav, to16k, wavChannels } from '../web/whisper/audio.js';
import { COUNTRY, MIN_CONFIDENCE, bestOf, repeats, summarize } from '../web/whisper/results.js';
import { fetchInto } from '../web/whisper/download.js';

// ─── Audio ──────────────────────────────────────────────────────────────────

function wav({ rate = 16000, channels = 1, samples = [], format = 1, bits = 16, extra = null }) {
    const data = samples.length * 2;
    const extraLen = extra ? 8 + extra.length + (extra.length & 1) : 0;
    const buf = new ArrayBuffer(12 + 24 + extraLen + 8 + data);
    const v = new DataView(buf);
    const str = (at, s) => { for (let i = 0; i < s.length; i++) v.setUint8(at + i, s.charCodeAt(i)); };
    str(0, 'RIFF'); v.setUint32(4, buf.byteLength - 8, true); str(8, 'WAVE');
    str(12, 'fmt '); v.setUint32(16, 16, true); v.setUint16(20, format, true); v.setUint16(22, channels, true);
    v.setUint32(24, rate, true); v.setUint32(28, rate * channels * 2, true); v.setUint16(32, channels * 2, true); v.setUint16(34, bits, true);
    let at = 36;
    if (extra) {
        str(at, 'LIST'); v.setUint32(at + 4, extra.length, true); str(at + 8, extra);
        at += extraLen;
    }
    str(at, 'data'); v.setUint32(at + 4, data, true);
    samples.forEach((s, i) => v.setInt16(at + 8 + i * 2, s, true));
    return buf;
}

test('a PCM16 WAV reads as floats per channel', () => {
    const r = wavChannels(wav({ samples: [0, 16384, -32768, 32767] }));
    assert.equal(r.rate, 16000);
    assert.deepEqual([...r.channels[0]], [0, 0.5, -1, 32767 / 32768]);
    const st = wavChannels(wav({ channels: 2, samples: [100, -100, 200, -200] }));
    assert.equal(st.channels.length, 2);
    assert.deepEqual([...st.channels[1]].map((x) => Math.round(x * 32768)), [-100, -200]);
});

test('chunks before the samples are skipped, odd lengths padded', () => {
    const r = wavChannels(wav({ samples: [1000, 2000], extra: 'abc' }));
    assert.deepEqual([...r.channels[0]].map((x) => Math.round(x * 32768)), [1000, 2000]);
});

test('a WAV that is not PCM16 is left to the browser; not a WAV at all is told apart', () => {
    assert.equal(wavChannels(wav({ format: 3, bits: 32, samples: [1, 2] })), null);
    assert.equal(wavChannels(wav({ bits: 8, samples: [1, 2] })), null);
    assert.equal(isWav(wav({ samples: [1] })), true);
    assert.equal(isWav(new TextEncoder().encode('ID3\u0003 mp3 bytes').buffer), false);
    assert.equal(isWav(new ArrayBuffer(4)), false);
});

test('no samples, an absurd rate or hours of audio are refused before any work', () => {
    assert.throws(() => wavChannels(wav({ samples: [] })), /No audio/);
    assert.throws(() => wavChannels(wav({ rate: 1000, samples: [1, 2, 3] })), /sample rate/);
    assert.throws(() => checkLength(RATE * 60 * 60, RATE), /too long/);
    assert.doesNotThrow(() => checkLength(RATE * 60 * 19, RATE));
});

test('16 kHz mono passes through; stereo mixes down', () => {
    const mono = new Float32Array([0.1, 0.2, 0.3]);
    assert.equal(to16k([mono], 16000), mono);
    const mixed = to16k([new Float32Array([1, 0]), new Float32Array([0, 1])], 16000);
    assert.deepEqual([...mixed], [0.5, 0.5]);
});

test('48 kHz averages each three samples; 24 kHz interpolates', () => {
    const at48 = to16k([new Float32Array([0, 0.3, 0.6, 0.9, 0.9, 0.9])], 48000);
    assert.equal(at48.length, 2);
    assert.ok(Math.abs(at48[0] - 0.3) < 1e-6 && Math.abs(at48[1] - 0.9) < 1e-6);
    const ramp = Float32Array.from({ length: 24 }, (_, i) => i);
    const at24 = to16k([ramp], 24000);
    assert.equal(at24.length, 16);
    at24.forEach((x, i) => assert.ok(Math.abs(x - i * 1.5) < 1e-5));
});

// ─── Reading a pass ─────────────────────────────────────────────────────────

const sections = (...texts) => texts.map((text) => ({ text, at: 0, confidence: 1 }));

test('a phrase on repeat is a loop; rhetorical repetition is not', () => {
    assert.equal(repeats(sections('het verbanden van het verbanden van het verbanden van het verbanden van het verbanden van')), true);
    assert.equal(repeats(sections('na'.repeat(20))), true);
    const speech = 'we shall fight on the beaches, ' + 'and some other words here that vary a lot, '.repeat(1)
        + ['we shall fight on the landing grounds', 'we shall fight in the fields and in the streets', 'we shall fight in the hills',
            'we shall never surrender', 'whatever the cost may be to us and to our children'].join(', ');
    assert.equal(repeats(sections(speech)), false);
    assert.equal(repeats(sections('Hey, are we still meeting for lunch tomorrow? Let me know.')), false);
});

test('a pass reads as desktop reads it', () => {
    const r = summarize([
        { text: ' Hello there', t0: 0, p: 0.9 },
        { text: ' [BLANK_AUDIO]', t0: 150, p: 0.99 },
        { text: ' .', t0: 160, p: 0.99 },
        { text: ' how are you', t0: 210, p: 0.7 },
    ], 3, 'es');
    assert.deepEqual(r.sections.map((s) => [s.text, s.at]), [[' Hello there', 0], [' how are you', 2100]]);
    assert.ok(Math.abs(r.confidence - 0.8) < 1e-9);
    assert.equal(r.lang, 'ES');
    assert.equal(r.language, 'es');
    assert.equal(COUNTRY.length, 100);
    const none = summarize([], -1, 'xx');
    assert.deepEqual([none.lang, none.language, none.confidence], ['auto', '', 0]);
});

test('a loop reads as no confidence at all', () => {
    const r = summarize([{ text: 'na'.repeat(20), t0: 0, p: 0.99 }], 12, 'nl');
    assert.equal(r.confidence, 0);
});

const pass = (confidence, n = 1) => ({ sections: Array(n).fill({ text: 'x', at: 0, confidence }), lang: 'GB', language: 'en', confidence });

test('a confident first pass is the answer', async () => {
    const calls = [];
    const r = await bestOf(async (beam, t) => { calls.push([beam, t]); return pass(0.9); });
    assert.deepEqual(calls, [[0, 0]]);
    assert.equal(r.confidence, 0.9);
});

test('an unsure pass retries with beam search, then warmer, keeping the surest', async () => {
    const calls = [];
    const results = [pass(0.2), pass(0.3), pass(0.25)];
    const r = await bestOf(async (beam, t) => { calls.push([beam, t]); return results[calls.length - 1]; });
    assert.deepEqual(calls, [[0, 0], [5, 0], [5, 0.6]]);
    assert.equal(r.confidence, 0.3);
});

test('retries stop once one is sure enough; silence never retries', async () => {
    const calls = [];
    const results = [pass(0.1), pass(MIN_CONFIDENCE + 0.01)];
    await bestOf(async (beam, t) => { calls.push([beam, t]); return results[calls.length - 1]; });
    assert.equal(calls.length, 2);
    let n = 0;
    await bestOf(async () => { n++; return pass(0, 0); });
    assert.equal(n, 1);
});

// ─── Downloads ──────────────────────────────────────────────────────────────

const MODEL = Uint8Array.from({ length: 1000 }, (_, i) => i % 251);

function memorySink() {
    let buf = new Uint8Array(0);
    return {
        truncate(n) { buf = buf.slice(0, n); },
        write(b, at) {
            if (at + b.length > buf.length) { const grown = new Uint8Array(at + b.length); grown.set(buf); buf = grown; }
            buf.set(b, at);
        },
        get bytes() { return buf; },
    };
}

/** A body that sends `bytes` in chunks and then, optionally, fails like a dropped connection. */
function body(bytes, { dropAfter = null } = {}) {
    let at = 0;
    return new ReadableStream({
        pull(c) {
            if (dropAfter !== null && at >= dropAfter) return c.error(new TypeError('network connection was lost'));
            if (at >= bytes.length) return c.close();
            const end = Math.min(bytes.length, at + 128, dropAfter ?? Infinity);
            c.enqueue(bytes.slice(at, end));
            at = end;
        },
    });
}

/** fetch that answers each call from `script`, recording the Range asked for. */
function scripted(script) {
    const asked = [];
    const fetch = async (url, init) => {
        asked.push(init.headers.Range ?? null);
        const next = script.shift();
        if (next instanceof Error) throw next;
        return next(init.headers.Range ?? null);
    };
    return { fetch, asked };
}

const full = () => new Response(body(MODEL), { status: 200 });
const fromRange = (range) => {
    const start = Number(/bytes=(\d+)-/.exec(range)[1]);
    return new Response(body(MODEL.slice(start)), { status: 206, headers: { 'content-range': `bytes ${start}-${MODEL.length - 1}/${MODEL.length}` } });
};
const opts = (extra) => ({ url: '/m', total: MODEL.length, wait: async () => {}, ...extra });

test('a download writes the model and reports progress up to 100', async () => {
    const sink = memorySink();
    const seen = [];
    await fetchInto(opts({ sink, fetch: scripted([full]).fetch, onProgress: (p) => seen.push(p.progress) }));
    assert.deepEqual(sink.bytes, MODEL);
    assert.equal(seen.at(-1), 100);
    assert.deepEqual(seen, [...new Set(seen)].sort((a, b) => a - b));
});

test('a dropped connection resumes from the bytes already written', async () => {
    const sink = memorySink();
    const { fetch, asked } = scripted([() => new Response(body(MODEL, { dropAfter: 384 }), { status: 200 }), fromRange]);
    await fetchInto(opts({ sink, fetch }));
    assert.deepEqual(asked, [null, 'bytes=384-']);
    assert.deepEqual(sink.bytes, MODEL);
});

test('a failure before any byte is retried too', async () => {
    const sink = memorySink();
    const { fetch, asked } = scripted([new TypeError('offline'), full]);
    await fetchInto(opts({ sink, fetch }));
    assert.equal(asked.length, 2);
    assert.deepEqual(sink.bytes, MODEL);
});

test('a server that ignores the range starts the file over', async () => {
    const sink = memorySink();
    const { fetch } = scripted([() => new Response(body(MODEL, { dropAfter: 256 }), { status: 200 }), full]);
    await fetchInto(opts({ sink, fetch }));
    assert.deepEqual(sink.bytes, MODEL);
});

test('a range answered from the wrong offset is never written', async () => {
    const sink = memorySink();
    const wrong = () => new Response(body(MODEL), { status: 206, headers: { 'content-range': `bytes 0-999/1000` } });
    const { fetch } = scripted([() => new Response(body(MODEL, { dropAfter: 256 }), { status: 200 }), wrong, fromRange]);
    await fetchInto(opts({ sink, fetch }));
    assert.deepEqual(sink.bytes, MODEL);
});

test('a missing model fails at once; a busy server is retried', async () => {
    const { fetch, asked } = scripted([() => new Response('gone', { status: 404 })]);
    await assert.rejects(fetchInto(opts({ sink: memorySink(), fetch })), /HTTP error: 404/);
    assert.equal(asked.length, 1);
    const busy = scripted([() => new Response('busy', { status: 503 }), full]);
    const sink = memorySink();
    await fetchInto(opts({ sink, fetch: busy.fetch }));
    assert.deepEqual(sink.bytes, MODEL);
});

test('a body longer than the model is refused', async () => {
    const big = () => new Response(body(new Uint8Array(1200)), { status: 200 });
    await assert.rejects(fetchInto(opts({ sink: memorySink(), fetch: scripted([big]).fetch })), /not the one expected/);
});

test('a short body is retried from where it stopped', async () => {
    const sink = memorySink();
    const short = () => new Response(body(MODEL.slice(0, 600)), { status: 200 });
    const { fetch, asked } = scripted([short, fromRange]);
    await fetchInto(opts({ sink, fetch }));
    assert.deepEqual(asked, [null, 'bytes=600-']);
    assert.deepEqual(sink.bytes, MODEL);
});

test('a failed write ends the download without retrying', async () => {
    const { fetch, asked } = scripted([full, full]);
    const sink = { truncate() {}, write() { throw Object.assign(new Error('quota'), { name: 'QuotaExceededError' }); } };
    await assert.rejects(fetchInto(opts({ sink, fetch })), /quota/);
    assert.equal(asked.length, 1);
});

test('cancelling during the wait between retries ends it as cancelled', async () => {
    const abort = new AbortController();
    const { fetch } = scripted([new TypeError('offline'), full]);
    const wait = (ms, signal) => new Promise((_, reject) => {
        signal.addEventListener('abort', () => reject(signal.reason), { once: true });
        abort.abort();
    });
    await assert.rejects(fetchInto(opts({ sink: memorySink(), fetch, wait, signal: abort.signal })), /Download cancelled/);
});

test('repeated network failures give up', async () => {
    const { fetch, asked } = scripted(Array.from({ length: 12 }, () => new TypeError('offline')));
    await assert.rejects(fetchInto(opts({ sink: memorySink(), fetch })), /offline/);
    assert.equal(asked.length, 9);
});
