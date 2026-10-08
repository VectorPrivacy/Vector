// Vector Web: Whisper in a worker of its own. With WebGPU the model lives on the GPU; without
// it, a cross-origin isolated page gets the threaded CPU build, anything else the WebGPU
// build's own single-threaded CPU path. Models are read from OPFS `whisper/`.
import { RATE, checkLength, to16k, wavChannels } from './audio.js';
import { bestOf, summarize } from './results.js';

const MODEL_DIR = 'whisper';

let runtime = null;
let loaded = null;
let current = null;

// Anything thrown out of the module leaves it mid-call: the page starts a fresh worker.
const fatal = (e) => Object.assign(e instanceof Error ? e : new Error(String(e)), { fatal: true });

async function gpuAdapter() {
    try {
        const adapter = await navigator.gpu?.requestAdapter();
        return adapter?.features.has('shader-f16') ? adapter : null;
    } catch {
        return null;
    }
}

async function getRuntime() {
    if (runtime) return runtime;
    const adapter = await gpuAdapter();
    const threaded = !adapter && self.crossOriginIsolated;
    const threads = threaded ? Math.max(1, Math.min(8, navigator.hardwareConcurrency || 4)) : 1;
    const { default: factory } = await import(threaded ? './whisper-cpu.js' : './whisper-gpu.js');
    const rt = { reader: null, threads, suspends: !threaded, flash: !!adapter?.features.has('subgroups') };
    rt.M = await factory({
        vwThreads: threads,
        vwRead: (heap, ptr, n) => rt.reader.read(heap, ptr, n),
        vwEof: () => rt.reader.eof(),
        vwLog: (level, text) => { if (level >= 4) console.warn('[whisper]', text.trimEnd()); },
        onAbort: (what) => {
            if (current !== null) postMessage({ t: 'result', id: current, ok: false, error: `Voice transcription stopped: ${what}`, fatal: true });
        },
    });
    runtime = rt;
    return rt;
}

// The WebGPU build suspends while the GPU works, so its exports are called as promises.
async function call(rt, name, ret, types, args) {
    try {
        return await rt.M.ccall(name, ret, types, args, rt.suspends ? { async: true } : undefined);
    } catch (e) {
        throw fatal(e);
    }
}

async function openModel(file) {
    const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle(MODEL_DIR);
    const handle = await (await dir.getFileHandle(file)).createSyncAccessHandle();
    const size = handle.getSize();
    let pos = 0;
    let scratch = null;
    return {
        read(heap, ptr, n) {
            const want = Math.min(n, size - pos);
            if (want <= 0) return 0;
            let got;
            if (typeof SharedArrayBuffer === 'function' && heap.buffer instanceof SharedArrayBuffer) {
                if (!scratch || scratch.length < want) scratch = new Uint8Array(want);
                got = handle.read(scratch.subarray(0, want), { at: pos });
                heap.set(scratch.subarray(0, got), ptr);
            } else {
                got = handle.read(heap.subarray(ptr, ptr + want), { at: pos });
            }
            pos += got;
            return got;
        },
        eof: () => pos >= size,
        close: () => handle.close(),
    };
}

async function useModel(file) {
    const rt = await getRuntime();
    if (loaded === file) return rt;
    if (loaded) {
        await call(rt, 'vw_release', null, [], []);
        loaded = null;
    }
    rt.reader = await openModel(file);
    try {
        if (!(await call(rt, 'vw_init', 'number', ['number', 'number'], [1, rt.flash ? 1 : 0]))) {
            throw new Error('The voice model failed to load');
        }
    } finally {
        rt.reader.close();
        rt.reader = null;
    }
    loaded = file;
    return rt;
}

/** 16 kHz mono from what the page sent: a PCM16 WAV's bytes, or decoded samples at `rate`. */
function samplesOf({ wav, pcm, rate }) {
    if (wav) {
        const parsed = wavChannels(wav);
        if (!parsed) throw new Error('unsupported-wav');
        return to16k(parsed.channels, parsed.rate);
    }
    if (rate === RATE) {
        checkLength(pcm.length, rate);
        return pcm;
    }
    return to16k([pcm], rate);
}

async function transcribe(job) {
    const samples = samplesOf(job);
    const rt = await useModel(job.file);
    const { M } = rt;
    const ptr = M._malloc(samples.byteLength);
    if (!ptr) throw fatal(new Error('Not enough memory to transcribe this message'));
    try {
        M.HEAPU8.set(new Uint8Array(samples.buffer, samples.byteOffset, samples.byteLength), ptr);
        // ACFT models were tuned to encode only the audio there is, not a padded 30 s window.
        const audioCtx = job.acft ? Math.min(1500, Math.ceil(samples.length / 320) + 32) : 0;
        return await bestOf(async (beam, temperature) => {
            const rc = await call(rt, 'vw_full', 'number', Array(7).fill('number'),
                [ptr, samples.length, beam, temperature, job.translate ? 1 : 0, rt.threads, audioCtx]);
            if (rc !== 0) throw new Error(`Transcription failed (${rc})`);
            const segments = [];
            for (let i = 0; i < M._vw_n_segments(); i++) {
                segments.push({ text: M.UTF8ToString(M._vw_segment_text(i)), t0: M._vw_segment_t0(i), p: M._vw_segment_p(i) });
            }
            const id = M._vw_lang_id();
            return summarize(segments, id, id >= 0 ? M.UTF8ToString(M._vw_lang_str(id)) : '');
        });
    } finally {
        M._free(ptr);
    }
}

async function forget({ file }) {
    if (loaded === file && runtime) {
        await call(runtime, 'vw_release', null, [], []);
        loaded = null;
    }
}

const jobs = { transcribe, forget };

// The module is single-threaded and suspends mid-call: one job at a time.
let queue = Promise.resolve();
onmessage = ({ data }) => {
    queue = queue.then(async () => {
        current = data.id;
        try {
            postMessage({ t: 'result', id: data.id, ok: true, value: await jobs[data.t](data) });
        } catch (e) {
            postMessage({ t: 'result', id: data.id, ok: false, error: String(e?.message ?? e), fatal: !!e?.fatal });
        } finally {
            current = null;
        }
    });
};
