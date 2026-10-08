// Vector Web: voice model downloads, in a worker apart from the one running Whisper, whose CPU
// builds hold their thread for a whole transcription. A model lands in OPFS `whisper/` as
// `<name>.part` and takes its name only once whole.
import { fetchInto } from './download.js';

const MODEL_DIR = 'whisper';
const running = new Map();

async function download({ file, url, bytes }) {
    if (running.has(file)) throw new Error('This voice model is already downloading');
    const abort = new AbortController();
    running.set(file, abort);
    const part = `${file}.part`;
    let dir = null;
    let out = null;
    try {
        dir = await (await navigator.storage.getDirectory()).getDirectoryHandle(MODEL_DIR, { create: true });
        out = await (await dir.getFileHandle(part, { create: true })).createSyncAccessHandle();
        const sink = { truncate: (n) => out.truncate(n), write: (b, at) => out.write(b, { at }) };
        await fetchInto({ url, total: bytes, sink, signal: abort.signal, onProgress: (p) => postMessage({ t: 'progress', ...p }) });
        out.flush();
        out.close();
        out = null;
        await dir.removeEntry(file).catch(() => {});
        await (await dir.getFileHandle(part)).move(dir, file);
    } catch (e) {
        out?.close();
        out = null;
        await dir?.removeEntry(part).catch(() => {});
        throw e;
    } finally {
        running.delete(file);
    }
}

const jobs = {
    download,
    cancel: () => { for (const abort of running.values()) abort.abort(); },
};

onmessage = async ({ data }) => {
    try {
        postMessage({ t: 'result', id: data.id, ok: true, value: await jobs[data.t](data) });
    } catch (e) {
        postMessage({ t: 'result', id: data.id, ok: false, error: String(e?.message ?? e) });
    }
};
