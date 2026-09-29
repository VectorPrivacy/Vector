// Vector Web backend: vector-core as WebAssembly, one dedicated worker per tab.
// OPFS's synchronous access handles, which the SQLite VFS needs, exist only here.
import init, { start, invoke, invoke_bytes, set_event_sink } from './pkg/vector_web.js';

const VERSION = 'web';

const booted = (async () => {
    await init();
    set_event_sink((name, json, bytes) => postMessage({ t: 'event', name, json, bytes }, bytes ? [bytes.buffer] : []));
    // A tab that just stepped aside may still be closing its storage handles.
    for (let attempt = 0; ; attempt++) {
        try {
            await start(VERSION);
            return;
        } catch (e) {
            if (attempt >= 8 || !/OPFS storage unavailable/.test(String(e))) throw e;
            await new Promise((r) => setTimeout(r, 250 * (attempt + 1)));
        }
    }
})();

booted.then(
    () => postMessage({ t: 'ready' }),
    (e) => postMessage({ t: 'fatal', error: String(e) }),
);

// Files the user hands the page (picker, drop) land in OPFS, where the backend
// reads them by path and the service worker serves them as /vfs.
async function storeFile(file) {
    const dirName = crypto.randomUUID();
    const name = (file.name || 'file').replace(/[\\/]/g, '_');
    let dir = await (await navigator.storage.getDirectory()).getDirectoryHandle('files', { create: true });
    dir = await dir.getDirectoryHandle('picked', { create: true });
    dir = await dir.getDirectoryHandle(dirName, { create: true });
    const handle = await (await dir.getFileHandle(name, { create: true })).createSyncAccessHandle();
    try {
        handle.truncate(0);
        handle.write(new Uint8Array(await file.arrayBuffer()), { at: 0 });
        handle.flush();
    } finally {
        handle.close();
    }
    return `/picked/${dirName}/${name}`;
}

async function run(data) {
    switch (data.t) {
        case 'invoke': return invoke(data.cmd, data.args);
        case 'invoke-bytes': return invoke_bytes(data.cmd, data.bytes, JSON.stringify(data.headers ?? {}));
        case 'store': return JSON.stringify(await Promise.all(data.files.map(storeFile)));
    }
}

// A trap aborts the module: nothing it held survives, so the page must restart.
const trapped = (e) => e instanceof WebAssembly.RuntimeError || /unreachable|RuntimeError/.test(String(e?.message ?? e));
const die = (e) => postMessage({ t: 'fatal', error: String(e?.message ?? e) });
addEventListener('error', (e) => { if (trapped(e.error ?? e.message)) die(e.error ?? e.message); });
addEventListener('unhandledrejection', (e) => { if (trapped(e.reason)) die(e.reason); });

onmessage = async ({ data }) => {
    try {
        await booted;
        postMessage({ t: 'result', id: data.id, ok: true, value: await run(data) });
    } catch (e) {
        if (trapped(e)) die(e);
        postMessage({ t: 'result', id: data.id, ok: false, error: typeof e === 'string' ? e : String(e?.message ?? e) });
    }
};
