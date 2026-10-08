// Vector Web: voice transcription, answered in the page under desktop's command names.
// Whisper runs in a worker of its own (web/whisper/worker.js), downloads in another
// (web/whisper/download-worker.js); each starts on first use and stops once idle, which frees
// the model with it.
(() => {
    'use strict';
    const { register, emit, backend, fileUrl } = window.__vectorWeb;

    // FUTO's ACFT models (Apache-2.0), as on Android, served by Vector Web's own host.
    const MODELS = [
        { name: 'base', display_name: 'Good Quality - Fast', file: 'base_acft_q8_0.bin', bytes: 81768602, ram_required: 400, supports_translate: false },
        { name: 'small', display_name: 'Best Quality - Moderate', file: 'small_acft_q8_0.bin', bytes: 264464624, ram_required: 770, supports_translate: true },
    ];
    const MODEL_DIR = 'whisper';
    const ENGINE_IDLE_MS = 3 * 60 * 1000;
    const FETCHER_IDLE_MS = 10 * 1000;
    const MAX_SECONDS = 20 * 60;

    const modelOf = (name) => {
        const model = MODELS.find((m) => m.name === name);
        if (!model) throw new Error(`Unknown model: ${name}`);
        return model;
    };

    function formatBytes(bytes) {
        if (bytes < 1024) return `${bytes} B`;
        if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
        if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
        return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
    }

    // --- Workers -------------------------------------------------------------
    /** A worker started on first use and ended `idleMs` after its last job settles. */
    function lazyWorker(url, idleMs, onEvent) {
        let worker = null;
        let nextId = 1;
        let busy = 0;
        let idle = 0;
        const pending = new Map();

        function stop(error) {
            worker?.terminate();
            worker = null;
            for (const job of pending.values()) job.reject(error);
            pending.clear();
        }

        function spawn() {
            if (worker) return worker;
            worker = new Worker(url, { type: 'module' });
            worker.onmessage = ({ data }) => {
                if (data.t !== 'result') return onEvent?.(data);
                const job = pending.get(data.id);
                if (!job) return;
                pending.delete(data.id);
                if (data.ok) job.resolve(data.value);
                else job.reject(new Error(data.error));
                // A module that threw mid-call keeps nothing it held: the next job gets a fresh one.
                if (data.fatal) stop(new Error(data.error));
            };
            worker.onerror = (e) => stop(new Error(e.message || 'Voice transcription stopped'));
            return worker;
        }

        function run(t, args = {}, transfer = []) {
            clearTimeout(idle);
            busy++;
            return new Promise((resolve, reject) => {
                const id = nextId++;
                spawn().postMessage({ t, id, ...args }, transfer);
                pending.set(id, { resolve, reject });
            }).finally(() => {
                if (--busy === 0) idle = setTimeout(() => stop(new Error('Voice transcription went idle')), idleMs);
            });
        }

        return { run, stop, get running() { return !!worker; } };
    }

    const engine = lazyWorker('/web/whisper/worker.js', ENGINE_IDLE_MS);
    const fetcher = lazyWorker('/web/whisper/download-worker.js', FETCHER_IDLE_MS, (data) => {
        if (data.t === 'progress') {
            const { t, ...progress } = data;
            emit('whisper_download_progress', progress);
        }
    });

    // Another tab has taken over: its own workers take the models from here.
    addEventListener('vector-web-step-aside', () => {
        engine.stop(new Error('Vector is open in another tab'));
        fetcher.stop(new Error('Vector is open in another tab'));
    });

    // --- Models on disk ------------------------------------------------------
    async function modelDir() {
        try {
            return await (await navigator.storage.getDirectory()).getDirectoryHandle(MODEL_DIR);
        } catch {
            return null;
        }
    }

    /** name → size of every file in the model folder, partial downloads included. */
    async function onDisk() {
        const files = new Map();
        const dir = await modelDir();
        if (!dir) return files;
        for await (const [name, handle] of dir.entries()) {
            if (handle.kind !== 'file') continue;
            try {
                files.set(name, (await handle.getFile()).size);
            } catch {
                // Open for reading or writing by a worker: a model mid-load is whole.
                files.set(name, MODELS.find((m) => m.file === name)?.bytes ?? 0);
            }
        }
        return files;
    }

    const isWhole = (files, model) => files.get(model.file) === model.bytes;

    async function removeFile(name) {
        try {
            await (await modelDir())?.removeEntry(name);
            return true;
        } catch {
            return false;
        }
    }

    async function remove(model) {
        if (engine.running) await engine.run('forget', { file: model.file });
        return removeFile(model.file);
    }

    // One download per model, however many callers wait on it.
    const downloads = new Map();
    function ensureDownloaded(model) {
        if (downloads.has(model.file)) return downloads.get(model.file);
        const done = (async () => {
            if (isWhole(await onDisk(), model)) return;
            await fetcher.run('download', { file: model.file, url: `/models/whisper/${model.file}`, bytes: model.bytes });
        })().finally(() => downloads.delete(model.file));
        downloads.set(model.file, done);
        return done;
    }

    // --- Audio ---------------------------------------------------------------
    const isWav = (bytes) => {
        const v = new DataView(bytes);
        return bytes.byteLength >= 12 && v.getUint32(0) === 0x52494646 && v.getUint32(8) === 0x57415645;
    };

    /** Decoded by the browser, resampled to 16 kHz where it can, mixed to mono. */
    async function decoded(bytes) {
        let ctx;
        try {
            ctx = new OfflineAudioContext(1, 1, 16000);
        } catch {
            ctx = new OfflineAudioContext(1, 1, 44100);
        }
        const buffer = await ctx.decodeAudioData(bytes);
        if (!buffer.length) throw new Error('No audio to transcribe');
        if (buffer.duration > MAX_SECONDS) throw new Error('Voice message too long to transcribe');
        const pcm = new Float32Array(buffer.length);
        for (let c = 0; c < buffer.numberOfChannels; c++) {
            const ch = buffer.getChannelData(c);
            for (let i = 0; i < ch.length; i++) pcm[i] += ch[i] / buffer.numberOfChannels;
        }
        return { pcm, rate: buffer.sampleRate };
    }

    async function transcribeFile(path, args) {
        const url = await fileUrl(path);
        if (!url) throw new Error('File not found');
        const read = async () => {
            const res = await fetch(url);
            if (!res.ok) throw new Error('File not found');
            return res.arrayBuffer();
        };
        // A PCM16 WAV, every Vector voice message, is read in the worker; anything else is
        // decoded here, where the browser's decoders are.
        const bytes = await read();
        if (isWav(bytes)) {
            try {
                return await engine.run('transcribe', { ...args, wav: bytes }, [bytes]);
            } catch (e) {
                if (e.message !== 'unsupported-wav') throw e;
            }
        }
        const { pcm, rate } = await decoded(bytes.byteLength ? bytes : await read());
        return engine.run('transcribe', { ...args, pcm, rate }, [pcm.buffer]);
    }

    // --- Commands ------------------------------------------------------------
    register('list_models', async () => {
        const files = await onDisk();
        return MODELS.map((m) => ({
            model: { name: m.name, display_name: m.display_name, size: Math.round(m.bytes / 1024 ** 2), ram_required: m.ram_required, supports_translate: m.supports_translate },
            downloaded: isWhole(files, m),
        }));
    });

    register('download_whisper_model', async ({ modelName }) => {
        const model = modelOf(modelName);
        await ensureDownloaded(model);
        return `${MODEL_DIR}/${model.file}`;
    });

    register('cancel_whisper_download', async () => {
        if (fetcher.running) await fetcher.run('cancel').catch(() => {});
    });

    register('delete_whisper_model', async ({ modelName }) => remove(modelOf(modelName)));

    register('transcribe', async ({ filePath, modelName, translate }) => {
        const model = modelOf(modelName);
        await ensureDownloaded(model);
        return transcribeFile(filePath, { file: model.file, translate: !!translate && model.supports_translate, acft: true });
    });

    // Bucketed and capped by the browser; unknown reads as no limit.
    register('get_device_memory', () => (navigator.deviceMemory ? navigator.deviceMemory * 1024 ** 3 : 0));

    // The storage page's "AI" slice: models live beside the account's files, not among them.
    register('get_storage_info', async () => {
        const info = await backend('get_storage_info');
        let models = 0;
        for (const size of (await onDisk()).values()) models += size;
        if (models > 0) {
            info.type_distribution['/ai_models'] = models;
            info.total_bytes += models;
            info.total_formatted = formatBytes(info.total_bytes);
        }
        return info;
    });

    register('clear_storage_category', async (args) => {
        if (args.category !== 'ai') return backend('clear_storage_category', args);
        if (engine.running) for (const m of MODELS) await engine.run('forget', { file: m.file });
        let freed = 0;
        for (const [name, size] of await onDisk()) {
            if (downloads.has(name.replace(/\.part$/, ''))) continue;
            if (await removeFile(name)) freed += size;
        }
        return { freed_bytes: freed, freed_formatted: formatBytes(freed) };
    });
})();
