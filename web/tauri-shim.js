// Vector Web: stands in for `window.__TAURI__` so the desktop frontend runs in a
// browser. Commands and backend events go to vector-core running as WebAssembly
// in a worker; window, dialog and OS APIs map to browser equivalents or no-ops.
// Loaded as a classic script ahead of every deferred app script.
(() => {
    'use strict';

    if ('serviceWorker' in navigator) {
        navigator.serviceWorker.register('/sw.js').catch((e) => console.error('[web] service worker failed:', e));
    }

    let worker = null;
    let nextId = 1;
    const pending = new Map();
    const listeners = new Map();
    let ready = false;
    let fatal = null;
    const queued = [];

    function dispatchEvent(name, payload) {
        const set = listeners.get(name);
        if (!set) return;
        for (const cb of [...set]) {
            try { cb({ event: name, id: 0, payload }); } catch (e) { console.error(`[web] listener for ${name} threw`, e); }
        }
    }

    const onWorkerMessage = ({ data }) => {
        switch (data.t) {
            case 'ready':
                ready = true;
                for (const msg of queued.splice(0)) worker.postMessage(msg);
                break;
            case 'fatal':
                fatal = data.error;
                console.error('[web] backend failed to start:', fatal);
                for (const msg of queued.splice(0)) settle(msg.id, false, fatal);
                break;
            case 'result':
                settle(data.id, data.ok, data.ok ? data.value : data.error);
                break;
            case 'event':
                dispatchEvent(data.name, data.json === undefined ? null : JSON.parse(data.json));
                break;
        }
    };

    // One tab per origin owns the storage: OPFS access handles are exclusive.
    // A second tab offers to take over, and the first steps aside when asked.
    const channel = new BroadcastChannel('vector-web');
    let holdLock = null;

    function startWorker() {
        worker = new Worker('/web/worker.js', { type: 'module' });
        worker.onmessage = onWorkerMessage;
    }

    function claim() {
        navigator.locks.request('vector-web-instance', { ifAvailable: true }, (lock) => {
            if (!lock) return blocked();
            startWorker();
            return new Promise((resolve) => { holdLock = resolve; });
        }).catch(() => {});
    }

    channel.onmessage = ({ data }) => {
        if (data !== 'takeover' || !worker) return;
        worker.terminate();
        worker = null;
        ready = false;
        holdLock?.();
        overlay('Vector is open in another tab.', 'Use here', () => { channel.postMessage('takeover'); setTimeout(() => location.reload(), 600); });
    };

    function blocked() {
        overlay('Vector is already open in another tab.', 'Use here', () => {
            channel.postMessage('takeover');
            setTimeout(() => location.reload(), 600);
        });
    }

    function overlay(text, action, onAction) {
        const show = () => {
            const el = document.createElement('div');
            el.style.cssText = 'position:fixed;inset:0;z-index:2147483647;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:16px;background:#030303;color:#e5e5e5;font:16px system-ui,sans-serif';
            const p = document.createElement('p');
            p.textContent = text;
            const b = document.createElement('button');
            b.textContent = action;
            b.style.cssText = 'padding:10px 22px;border-radius:8px;border:0;background:#59fcb3;color:#030303;font-weight:600;cursor:pointer';
            b.onclick = onAction;
            el.append(p, b);
            document.body.appendChild(el);
        };
        if (document.body) show(); else addEventListener('DOMContentLoaded', show);
    }

    if (navigator.locks) claim(); else startWorker();

    function settle(id, ok, value) {
        const p = pending.get(id);
        if (!p) return;
        pending.delete(id);
        if (!ok) return p.reject(value);
        p.resolve(value === undefined || value === '' ? undefined : JSON.parse(value));
    }

    function post(msg) {
        return new Promise((resolve, reject) => {
            if (fatal) return reject(fatal);
            const id = nextId++;
            pending.set(id, { resolve, reject });
            msg.id = id;
            if (ready) worker.postMessage(msg); else queued.push(msg);
        });
    }

    // Commands answered in the page itself (media, clipboard), registered by web/media.js.
    const local = new Map();

    // Raw-body IPC (Tauri's `invoke(cmd, bytes, { headers })`) keeps the bytes binary.
    function invoke(cmd, args = {}, options = {}) {
        if (local.has(cmd)) {
            try { return Promise.resolve(local.get(cmd)(args ?? {}, options)); } catch (e) { return Promise.reject(String(e?.message ?? e)); }
        }
        return backend(cmd, args, options);
    }

    function backend(cmd, args = {}, options = {}) {
        if (args instanceof ArrayBuffer) args = new Uint8Array(args);
        if (args instanceof Uint8Array) return post({ t: 'invoke-bytes', cmd, bytes: args, headers: options.headers });
        return post({ t: 'invoke', cmd, args: JSON.stringify(args ?? {}) });
    }

    const storeFiles = (files) => post({ t: 'store', files: [...files] });

    function pickFiles({ multiple = false, filters } = {}) {
        return new Promise((resolve) => {
            const input = document.createElement('input');
            input.type = 'file';
            input.multiple = multiple;
            const exts = (filters || []).flatMap((f) => f.extensions || []).filter((e) => e && e !== '*');
            if (exts.length) input.accept = exts.map((e) => '.' + e).join(',');
            input.addEventListener('change', () => resolve([...input.files]));
            input.addEventListener('cancel', () => resolve([]));
            input.click();
        });
    }

    async function openDialog(opts = {}) {
        if (opts.directory) return null;
        const files = await pickFiles(opts);
        if (!files.length) return null;
        const paths = await storeFiles(files);
        return opts.multiple ? paths : paths[0];
    }

    // File drops, delivered the way Tauri's webview reports them: paths the backend can read.
    async function onDragDropEvent(cb) {
        const pos = (e) => new PhysicalPosition(e.clientX * dpr(), e.clientY * dpr());
        const over = (e) => {
            if (!e.dataTransfer?.types?.includes('Files')) return;
            e.preventDefault();
            cb({ payload: { type: 'over', position: pos(e) } });
        };
        const leave = () => cb({ payload: { type: 'leave' } });
        const drop = async (e) => {
            if (!e.dataTransfer?.files?.length) return;
            e.preventDefault();
            const paths = await storeFiles(e.dataTransfer.files);
            cb({ payload: { type: 'drop', paths, position: pos(e) } });
        };
        addEventListener('dragover', over);
        addEventListener('dragleave', leave);
        addEventListener('drop', drop);
        return () => {
            removeEventListener('dragover', over);
            removeEventListener('dragleave', leave);
            removeEventListener('drop', drop);
        };
    }

    async function listen(name, cb) {
        if (!listeners.has(name)) listeners.set(name, new Set());
        listeners.get(name).add(cb);
        return () => listeners.get(name)?.delete(cb);
    }

    async function once(name, cb) {
        const unlisten = await listen(name, (e) => { unlisten(); cb(e); });
        return unlisten;
    }

    // A stand-in object whose every method resolves: window/webview handles the
    // frontend drives for chrome it doesn't have in a tab.
    const noop = () => {};
    function stub(overrides = {}) {
        return new Proxy(overrides, {
            get(target, prop) {
                if (prop in target) return target[prop];
                if (prop === 'then' || typeof prop === 'symbol') return undefined;
                if (/^on[A-Z]/.test(prop) || prop === 'listen' || prop === 'once') return async () => noop;
                return async () => undefined;
            },
        });
    }

    const dpr = () => window.devicePixelRatio || 1;
    class PhysicalSize { constructor(width, height) { this.type = 'Physical'; this.width = width; this.height = height; } toLogical(f) { return new LogicalSize(this.width / f, this.height / f); } }
    class LogicalSize { constructor(width, height) { this.type = 'Logical'; this.width = width; this.height = height; } toPhysical(f) { return new PhysicalSize(this.width * f, this.height * f); } }
    class PhysicalPosition { constructor(x, y) { this.type = 'Physical'; this.x = x; this.y = y; } toLogical(f) { return new LogicalPosition(this.x / f, this.y / f); } }
    class LogicalPosition { constructor(x, y) { this.type = 'Logical'; this.x = x; this.y = y; } toPhysical(f) { return new PhysicalPosition(this.x * f, this.y * f); } }

    const appWindow = stub({
        label: 'main',
        isMaximized: async () => false,
        isMinimized: async () => false,
        isFullscreen: async () => !!document.fullscreenElement,
        isFocused: async () => document.hasFocus(),
        isVisible: async () => document.visibilityState === 'visible',
        scaleFactor: async () => dpr(),
        innerSize: async () => new PhysicalSize(innerWidth * dpr(), innerHeight * dpr()),
        outerSize: async () => new PhysicalSize(outerWidth * dpr(), outerHeight * dpr()),
        outerPosition: async () => new PhysicalPosition(screenX * dpr(), screenY * dpr()),
        theme: async () => 'dark',
        onFocusChanged: async (cb) => {
            const focus = () => cb({ payload: true });
            const blur = () => cb({ payload: false });
            addEventListener('focus', focus);
            addEventListener('blur', blur);
            return () => { removeEventListener('focus', focus); removeEventListener('blur', blur); };
        },
        onResized: async (cb) => {
            const h = () => cb({ payload: new PhysicalSize(innerWidth * dpr(), innerHeight * dpr()) });
            addEventListener('resize', h);
            return () => removeEventListener('resize', h);
        },
        close: async () => window.close(),
    });

    const webview = stub({ onDragDropEvent });

    const monitor = () => ({
        name: 'browser',
        scaleFactor: dpr(),
        size: new PhysicalSize(screen.width * dpr(), screen.height * dpr()),
        position: new PhysicalPosition(0, 0),
        workArea: { size: new PhysicalSize(screen.availWidth * dpr(), screen.availHeight * dpr()), position: new PhysicalPosition(0, 0) },
    });

    window.__TAURI__ = {
        core: {
            invoke,
            // Backend files are served from OPFS by the service worker (web/sw.js).
            convertFileSrc: (path) => (!path || /^[a-z]+:/i.test(path) ? path : '/vfs' + encodeURI(path.startsWith('/') ? path : '/' + path)),
            Channel: class { constructor() { this.onmessage = null; } },
        },
        event: { listen, once, emit: async (name, payload) => dispatchEvent(name, payload) },
        window: {
            getCurrentWindow: () => appWindow,
            currentMonitor: async () => monitor(),
            primaryMonitor: async () => monitor(),
            availableMonitors: async () => [monitor()],
        },
        webviewWindow: { getCurrentWebviewWindow: () => appWindow },
        webview: { getCurrentWebview: () => webview },
        dpi: { PhysicalSize, LogicalSize, PhysicalPosition, LogicalPosition },
        app: {
            getVersion: async () => document.documentElement.dataset.version || 'web',
            getName: async () => 'Vector',
        },
        opener: {
            openUrl: async (url) => { window.open(url, '_blank', 'noopener,noreferrer'); },
            openPath: async () => {},
            revealItemInDir: async () => {},
        },
        dialog: {
            open: openDialog,
            save: async () => null,
            message: async (msg) => { alert(msg); },
            ask: async (msg) => confirm(msg),
            confirm: async (msg) => confirm(msg),
        },
        process: { exit: async () => location.reload(), relaunch: async () => location.reload() },
        updater: { check: async () => null },
    };

    const convertFileSrc = window.__TAURI__.core.convertFileSrc;
    window.__vectorWeb = {
        register: (cmd, fn) => local.set(cmd, fn),
        emit: dispatchEvent,
        backend,
        storeFiles,
        convertFileSrc,
    };

    // The desktop reloads its webview on `session_reload`; here a reload also
    // restarts the worker, which boots whichever account is marked active.
    document.documentElement.classList.add('vector-web');
})();
