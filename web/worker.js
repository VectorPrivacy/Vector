// Vector Web backend: vector-core as WebAssembly, one dedicated worker per tab.
// OPFS's synchronous access handles, which the SQLite VFS needs, exist only here.
import init, { start, invoke, invoke_bytes, set_event_sink, set_call_sink } from './pkg/vector_web.js';
import { probeStorage, filesFor } from './storage.js';
import { onSink, onPageMessage } from './calls-media.js';

const VERSION = 'web';

const booted = (async () => {
    const level = await probeStorage();
    globalThis.vectorFiles = filesFor(level, (path) => postMessage({ t: 'file-changed', path }));
    await init();
    set_event_sink((name, json, bytes) => postMessage({ t: 'event', name, json, bytes }, bytes ? [bytes.buffer] : []));
    set_call_sink(onSink);
    // A tab that just stepped aside may still be closing its storage handles.
    for (let attempt = 0; ; attempt++) {
        try {
            await start(VERSION, level);
            return level;
        } catch (e) {
            if (attempt >= 8 || !/OPFS storage unavailable/.test(String(e))) throw e;
            await new Promise((r) => setTimeout(r, 250 * (attempt + 1)));
        }
    }
})();

booted.then(
    (level) => postMessage({ t: 'ready', level }),
    (e) => postMessage({ t: 'fatal', error: String(e) }),
);

// Files the user hands the page (picker, drop) land in storage, where the backend
// reads them by path and the page is served them back.
async function storeFile(file) {
    const name = (file.name || 'file').replace(/[\\/]/g, '_');
    const path = `/picked/${crypto.randomUUID()}/${name}`;
    await globalThis.vectorFiles.write(path, new Uint8Array(await file.arrayBuffer()));
    return path;
}

async function run(data) {
    switch (data.t) {
        case 'invoke': return invoke(data.cmd, data.args);
        case 'invoke-bytes': return invoke_bytes(data.cmd, data.bytes, JSON.stringify(data.headers ?? {}));
        case 'store': return JSON.stringify(await Promise.all(data.files.map(storeFile)));
        case 'file': return globalThis.vectorFiles.read(data.path);
    }
}

// A trap aborts the module: nothing it held survives, so the page must restart.
const trapped = (e) => e instanceof WebAssembly.RuntimeError || /unreachable|RuntimeError/.test(String(e?.message ?? e));
const die = (e) => postMessage({ t: 'fatal', error: String(e?.message ?? e) });
addEventListener('error', (e) => { if (trapped(e.error ?? e.message)) die(e.error ?? e.message); });
addEventListener('unhandledrejection', (e) => { if (trapped(e.reason)) die(e.reason); });

onmessage = async ({ data }) => {
    // The page's half of a call: worklet ports and failures, never answered.
    if (data.t?.startsWith('call-')) {
        await booted.catch(() => {});
        onPageMessage(data);
        return;
    }
    try {
        await booted;
        const value = await run(data);
        const transfer = value instanceof Uint8Array ? [value.buffer] : [];
        postMessage({ t: 'result', id: data.id, ok: true, value }, transfer);
    } catch (e) {
        if (trapped(e)) die(e);
        postMessage({ t: 'result', id: data.id, ok: false, error: typeof e === 'string' ? e : String(e?.message ?? e) });
    }
};
