// Vector Web backend: vector-core as WebAssembly, one dedicated worker per tab.
// OPFS's synchronous access handles, which the SQLite VFS needs, exist only here.
import init, { start, invoke, invoke_bytes, set_event_sink } from './pkg/vector_web.js';

const VERSION = 'web';

// ─── Storage ────────────────────────────────────────────────────────────────
// What the browser keeps decides where everything lives:
//   persistent: OPFS, kept until the user clears it;
//   session:    IndexedDB, which private browsers keep until they close;
//   memory:     nothing, and a reload starts over.
// Loose files (attachments, avatars, picked files) go to the same place as the
// databases, through `globalThis.vectorFiles`, which the core's file layer calls.

async function probeStorage() {
    try {
        const root = await navigator.storage.getDirectory();
        const handle = await (await root.getFileHandle('.probe', { create: true })).createSyncAccessHandle();
        handle.close();
        await root.removeEntry('.probe');
        return 'persistent';
    } catch {}
    try {
        await idb();
        return 'session';
    } catch {}
    return 'memory';
}

const opfsFiles = {
    async dirFor(path, create) {
        let dir = await (await navigator.storage.getDirectory()).getDirectoryHandle('files', { create });
        const parts = path.split('/').filter(Boolean);
        const name = parts.pop();
        for (const p of parts) dir = await dir.getDirectoryHandle(p, { create });
        return [dir, name];
    },
    async dirAt(path) {
        let dir = await (await navigator.storage.getDirectory()).getDirectoryHandle('files');
        for (const p of path.split('/').filter(Boolean)) dir = await dir.getDirectoryHandle(p);
        return dir;
    },
    async write(path, bytes) {
        const [dir, name] = await this.dirFor(path, true);
        const handle = await (await dir.getFileHandle(name, { create: true })).createSyncAccessHandle();
        try {
            handle.truncate(0);
            handle.write(bytes, { at: 0 });
            handle.flush();
        } finally {
            handle.close();
        }
    },
    async read(path) {
        try {
            const [dir, name] = await this.dirFor(path, false);
            return new Uint8Array(await (await (await dir.getFileHandle(name)).getFile()).arrayBuffer());
        } catch { return null; }
    },
    async size(path) {
        try {
            const [dir, name] = await this.dirFor(path, false);
            return (await (await dir.getFileHandle(name)).getFile()).size;
        } catch { return -1; }
    },
    async remove(path) {
        try {
            const [dir, name] = await this.dirFor(path, false);
            await dir.removeEntry(name, { recursive: true });
            return true;
        } catch { return false; }
    },
    // [name, size] of the files under `path`; with `recursive`, names are relative paths.
    async list(path, recursive) {
        const out = [];
        let root;
        try { root = await this.dirAt(path); } catch { return out; }
        const walk = async (dir, prefix) => {
            for await (const [name, handle] of dir.entries()) {
                if (handle.kind === 'file') out.push([prefix + name, (await handle.getFile()).size]);
                else if (recursive) await walk(handle, prefix + name + '/');
            }
        };
        try { await walk(root, ''); } catch {}
        return out;
    },
};

let idbHandle = null;
function idb() {
    if (idbHandle) return idbHandle;
    idbHandle = new Promise((resolve, reject) => {
        const req = indexedDB.open('vector-files', 1);
        req.onupgradeneeded = () => req.result.createObjectStore('files');
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => { idbHandle = null; reject(req.error); };
    });
    return idbHandle;
}

async function idbRun(mode, fn) {
    const db = await idb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction('files', mode);
        const req = fn(tx.objectStore('files'));
        tx.oncomplete = () => resolve(req?.result);
        tx.onerror = tx.onabort = () => reject(tx.error);
    });
}

const norm = (path) => '/' + path.split('/').filter(Boolean).join('/');
const under = (dir) => { const d = norm(dir); return d === '/' ? '/' : d + '/'; };

// Session and memory keep files flat, keyed by path.
function keyedFiles(store) {
    return {
        write: (path, bytes) => store.put(norm(path), bytes.slice()),
        async read(path) { return (await store.get(norm(path))) ?? null; },
        async size(path) { return (await store.len(norm(path))) ?? -1; },
        async remove(path) {
            const key = norm(path);
            const prefix = under(key);
            const keys = (await store.keys()).filter((k) => k === key || k.startsWith(prefix));
            for (const k of keys) await store.delete(k);
            return keys.length > 0;
        },
        async list(path, recursive) {
            const prefix = under(path);
            const out = [];
            for (const k of await store.keys()) {
                if (!k.startsWith(prefix)) continue;
                const rest = k.slice(prefix.length);
                if (!recursive && rest.includes('/')) continue;
                out.push([rest, (await store.len(k)) ?? 0]);
            }
            return out;
        },
    };
}

const idbStore = {
    put: (k, v) => idbRun('readwrite', (s) => s.put(v, k)),
    get: (k) => idbRun('readonly', (s) => s.get(k)),
    len: async (k) => (await idbRun('readonly', (s) => s.get(k)))?.byteLength,
    delete: (k) => idbRun('readwrite', (s) => s.delete(k)),
    keys: async () => (await idbRun('readonly', (s) => s.getAllKeys())).map(String),
};

const memoryMap = new Map();
const memoryStore = {
    put: async (k, v) => { memoryMap.set(k, v); },
    // A copy: what is read may be transferred to the page, which detaches it.
    get: async (k) => memoryMap.get(k)?.slice(),
    len: async (k) => memoryMap.get(k)?.byteLength,
    delete: async (k) => { memoryMap.delete(k); },
    keys: async () => [...memoryMap.keys()],
};

const booted = (async () => {
    const level = await probeStorage();
    globalThis.vectorFiles = level === 'persistent' ? opfsFiles : keyedFiles(level === 'session' ? idbStore : memoryStore);
    await init();
    set_event_sink((name, json, bytes) => postMessage({ t: 'event', name, json, bytes }, bytes ? [bytes.buffer] : []));
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
