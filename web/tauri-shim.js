// Vector Web: stands in for `window.__TAURI__` so the desktop frontend runs in a
// browser. Commands and backend events go to vector-core running as WebAssembly
// in a worker; window, dialog and OS APIs map to browser equivalents or no-ops.
// Loaded as a classic script ahead of every deferred app script.
(() => {
    'use strict';

    const worker = new Worker('/web/worker.js', { type: 'module' });
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

    worker.onmessage = ({ data }) => {
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

    function settle(id, ok, value) {
        const p = pending.get(id);
        if (!p) return;
        pending.delete(id);
        if (!ok) return p.reject(value);
        p.resolve(value === undefined || value === '' ? undefined : JSON.parse(value));
    }

    function invoke(cmd, args = {}) {
        return new Promise((resolve, reject) => {
            if (fatal) return reject(fatal);
            const id = nextId++;
            pending.set(id, { resolve, reject });
            const msg = { t: 'invoke', id, cmd, args: JSON.stringify(args ?? {}) };
            if (ready) worker.postMessage(msg); else queued.push(msg);
        });
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
            convertFileSrc: (path) => path,
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
        webview: { getCurrentWebview: () => appWindow },
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
            open: async () => null,
            save: async () => null,
            message: async (msg) => { alert(msg); },
            ask: async (msg) => confirm(msg),
            confirm: async (msg) => confirm(msg),
        },
        process: { exit: async () => location.reload(), relaunch: async () => location.reload() },
        updater: { check: async () => null },
    };

    // The desktop reloads its webview on `session_reload`; here a reload also
    // restarts the worker, which boots whichever account is marked active.
    document.documentElement.classList.add('vector-web');
})();
