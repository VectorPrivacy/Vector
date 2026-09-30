// Where Vector Web keeps what it stores, decided by what the browser allows:
//   persistent: OPFS, kept until the user clears it;
//   session:    IndexedDB, which private browsers keep until they close;
//   memory:     nothing, and a reload starts over.
// Loose files (attachments, avatars, picked files) live beside the databases, behind
// one interface the core's file layer calls through `globalThis.vectorFiles`.

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function probeStorage() {
    for (let attempt = 0; ; attempt++) {
        try {
            const root = await navigator.storage.getDirectory();
            const name = `.probe-${crypto.randomUUID()}`;
            const handle = await (await root.getFileHandle(name, { create: true })).createSyncAccessHandle();
            handle.close();
            await root.removeEntry(name);
            return 'persistent';
        } catch (e) {
            // Private browsing refuses OPFS outright. Any other failure may pass (a tab
            // stepping aside still holds handles), and must never quietly move a
            // normal browser's account out of reach.
            if (e?.name === 'SecurityError' || !navigator.storage?.getDirectory) break;
            if (attempt >= 4) throw new Error(`OPFS storage unavailable: ${e?.message ?? e}`);
            await sleep(250 * (attempt + 1));
        }
    }
    try {
        await openIdb();
        return 'session';
    } catch {}
    return 'memory';
}

// ─── OPFS ───────────────────────────────────────────────────────────────────

export const opfsFiles = {
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

// ─── Keyed stores: IndexedDB and memory ─────────────────────────────────────
// Flat, keyed by normalised path, with sizes kept apart so asking one never
// reads the file.

const norm = (path) => '/' + String(path).split('/').filter(Boolean).join('/');
const under = (dir) => { const d = norm(dir); return d === '/' ? '/' : d + '/'; };

/** The file interface over a store of { put, get, size, delete, keys }. */
export function keyedFiles(store) {
    const safe = (fallback, fn) => async (...args) => { try { return await fn(...args); } catch { return fallback; } };
    return {
        write: (path, bytes) => store.put(norm(path), bytes.slice()),
        read: safe(null, async (path) => (await store.get(norm(path))) ?? null),
        size: safe(-1, async (path) => (await store.size(norm(path))) ?? -1),
        remove: safe(false, async (path) => {
            const key = norm(path);
            if (key === '/') return false;
            const prefix = under(key);
            const keys = (await store.keys()).filter((k) => k === key || k.startsWith(prefix));
            for (const k of keys) await store.delete(k);
            return keys.length > 0;
        }),
        list: safe([], async (path, recursive) => {
            const prefix = under(path);
            const out = [];
            for (const k of await store.keys()) {
                if (!k.startsWith(prefix)) continue;
                const rest = k.slice(prefix.length);
                if (!recursive && rest.includes('/')) continue;
                out.push([rest, (await store.size(k)) ?? 0]);
            }
            return out;
        }),
    };
}

let idbHandle = null;
function openIdb() {
    if (idbHandle) return idbHandle;
    idbHandle = new Promise((resolve, reject) => {
        const req = indexedDB.open('vector-files', 2);
        req.onupgradeneeded = () => {
            const db = req.result;
            if (!db.objectStoreNames.contains('files')) db.createObjectStore('files');
            if (!db.objectStoreNames.contains('sizes')) {
                db.createObjectStore('sizes');
                // Files written before sizes were kept apart.
                const tx = req.transaction;
                tx.objectStore('files').openCursor().onsuccess = (e) => {
                    const cursor = e.target.result;
                    if (!cursor) return;
                    tx.objectStore('sizes').put(cursor.value?.byteLength ?? 0, cursor.key);
                    cursor.continue();
                };
            }
        };
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => { idbHandle = null; reject(req.error); };
    });
    return idbHandle;
}

async function idbRun(stores, mode, fn) {
    const db = await openIdb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(stores, mode);
        const req = fn(tx);
        tx.oncomplete = () => resolve(req?.result);
        tx.onerror = tx.onabort = () => reject(tx.error);
    });
}

export const idbStore = {
    put: (k, v) => idbRun(['files', 'sizes'], 'readwrite', (tx) => {
        tx.objectStore('sizes').put(v.byteLength, k);
        return tx.objectStore('files').put(v, k);
    }),
    get: (k) => idbRun(['files'], 'readonly', (tx) => tx.objectStore('files').get(k)),
    size: (k) => idbRun(['sizes'], 'readonly', (tx) => tx.objectStore('sizes').get(k)),
    delete: (k) => idbRun(['files', 'sizes'], 'readwrite', (tx) => {
        tx.objectStore('sizes').delete(k);
        return tx.objectStore('files').delete(k);
    }),
    keys: async () => (await idbRun(['sizes'], 'readonly', (tx) => tx.objectStore('sizes').getAllKeys())).map(String),
};

/** Files held in this worker's memory, up to `budget` bytes. */
export function memoryStore(budget = 512 * 1024 * 1024) {
    const map = new Map();
    let used = 0;
    return {
        async put(k, v) {
            const before = map.get(k)?.byteLength ?? 0;
            if (used - before + v.byteLength > budget) throw new Error('Storage full: this private browser keeps files in memory only');
            map.set(k, v);
            used += v.byteLength - before;
        },
        // A copy: what is read may be transferred to the page, which detaches it.
        get: async (k) => map.get(k)?.slice(),
        size: async (k) => map.get(k)?.byteLength,
        async delete(k) {
            used -= map.get(k)?.byteLength ?? 0;
            map.delete(k);
        },
        keys: async () => [...map.keys()],
    };
}

/**
 * The file interface for `level`. `changed(path)` hears every write and removal,
 * so the page can drop a copy it holds of a file that is no longer what it was.
 */
export function filesFor(level, changed = () => {}) {
    const files = level === 'persistent' ? opfsFiles : keyedFiles(level === 'session' ? idbStore : memoryStore());
    return {
        ...files,
        async write(path, bytes) {
            await files.write(path, bytes);
            changed(path);
        },
        async remove(path) {
            const removed = await files.remove(path);
            if (removed) changed(path);
            return removed;
        },
    };
}
