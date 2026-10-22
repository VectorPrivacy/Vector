// Vector Web mini app origin: the first page loaded here. It installs the
// origin's service worker, takes the package from Vector, and hands over to it.
(async () => {
    'use strict';
    // Vector itself lives on the host this origin is a subdomain of.
    const vector = `${location.protocol}//${location.host.replace(/^[a-z0-9-]+\.xdc\./, '')}`;
    if (window.parent === window) return;

    const reg = await navigator.serviceWorker.register('/__vector/sw.js', { scope: '/' });
    // A newer worker than the running one must take over before it's handed the package.
    await reg.update().catch(() => {});
    const pending = reg.installing || reg.waiting;
    if (pending) {
        await new Promise((resolve) => {
            if (pending.state === 'activated' || pending.state === 'redundant') return resolve();
            pending.addEventListener('statechange', () => {
                if (pending.state === 'activated' || pending.state === 'redundant') resolve();
            });
        });
    }
    await navigator.serviceWorker.ready;

    // One database as Vector kept it: its stores and indexes made in the upgrade, which then
    // writes every record back under its own key.
    const rebuild = (db) => new Promise((resolve, reject) => {
        const req = indexedDB.open(db.name, db.version);
        req.onupgradeneeded = () => {
            for (const s of db.stores) {
                const store = req.result.createObjectStore(s.name, { keyPath: s.keyPath, autoIncrement: s.autoIncrement });
                for (const i of s.indexes) store.createIndex(i.name, i.keyPath, { unique: i.unique, multiEntry: i.multiEntry });
                for (let n = 0; n < s.values.length; n++) {
                    if (s.keyPath === null) store.put(s.values[n], s.keys[n]);
                    else store.put(s.values[n]);
                }
            }
        };
        req.onsuccess = () => { req.result.close(); resolve(); };
        req.onerror = () => reject(req.error);
        req.onblocked = () => reject(new Error('blocked'));
    });

    const fail = (why) => {
        document.body.textContent = `This mini app could not start: ${why}`;
        document.body.style.cssText = 'color:#ccc;font:14px system-ui;padding:24px';
    };

    addEventListener('message', async (e) => {
        if (e.source !== window.parent || e.origin !== vector || e.data?.t !== 'xdc-load') return;
        const { bytes, meta, href, local, idb } = e.data;
        const sw = reg.active || navigator.serviceWorker.controller;
        if (!sw) return fail('service worker unavailable');
        // The worker stores and parses the package itself, so its storage is the one it reads.
        const result = await new Promise((resolve) => {
            const ch = new MessageChannel();
            ch.port1.onmessage = ({ data }) => resolve(data);
            sw.postMessage({ t: 'store', bytes, meta: { ...meta, parent: vector } }, [ch.port2, bytes]);
            setTimeout(() => resolve({ ok: false, error: 'service worker did not answer' }), 30000);
        });
        if (!result?.ok) return fail(result?.error || 'unknown error');
        // What Vector kept of this app's localStorage, put back when the origin lost its own.
        if (local && typeof local === 'object') {
            try {
                if (localStorage.length === 0) for (const [k, v] of Object.entries(local)) localStorage.setItem(k, String(v));
            } catch { /* full or blocked: the app starts as it would have */ }
        }
        // And its IndexedDB, rebuilt whole when the origin holds no database at all.
        if (idb?.v === 1 && Array.isArray(idb.dbs) && idb.dbs.length && typeof indexedDB?.databases === 'function') {
            try {
                if ((await indexedDB.databases()).length === 0) for (const db of idb.dbs) await rebuild(db);
            } catch { /* full or blocked: the app starts as it would have */ }
        }
        location.replace(typeof href === 'string' && href.startsWith('/') ? href : '/index.html');
    });
    window.parent.postMessage({ t: 'xdc-host-ready' }, vector);
})().catch((e) => {
    document.body.textContent = `This mini app could not start: host: ${e?.name || 'Error'}: ${e?.message || e}`;
    document.body.style.cssText = 'color:#ccc;font:14px system-ui;padding:24px';
});
