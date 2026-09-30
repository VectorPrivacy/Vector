// Vector Web: stands in for `window.__TAURI__` so the desktop frontend runs in a
// browser. Commands and backend events go to vector-core running as WebAssembly
// in a worker; window, dialog and OS APIs map to browser equivalents or no-ops.
// Loaded as a classic script ahead of every deferred app script.
(() => {
    'use strict';

    // The service worker serves stored files where storage is OPFS. Private browsers
    // refuse one, which also rules out mini apps: each runs behind its own.
    let serviceWorker = 'serviceWorker' in navigator ? 'pending' : 'none';
    if (serviceWorker === 'pending') {
        navigator.serviceWorker.register('/sw.js').then(
            () => { serviceWorker = 'ok'; },
            (e) => { serviceWorker = 'none'; applyCapabilities(); console.warn('[web] no service worker:', e); },
        );
    }
    const hasServiceWorker = () => serviceWorker !== 'none';

    // How long the browser keeps what Vector stores ('persistent' | 'session' |
    // 'memory'), as the worker found it; null until it has booted.
    let storage = null;
    function applyCapabilities() {
        const root = document.documentElement.classList;
        if (storage) root.add(`storage-${storage}`);
        root.toggle('no-miniapps', !hasServiceWorker());
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
                storage = data.level || 'persistent';
                applyCapabilities();
                // Ahead of the queue: the backend decides what to warm up by it.
                post({ t: 'invoke', cmd: 'web_capabilities', args: JSON.stringify({ miniApps: hasServiceWorker() }) }).catch(() => {});
                for (const msg of queued.splice(0)) worker.postMessage(msg);
                break;
            case 'fatal':
                fatal = data.error;
                console.error('[web] backend stopped:', fatal);
                for (const msg of queued.splice(0)) settle(msg.id, false, fatal);
                for (const id of [...pending.keys()]) settle(id, false, fatal);
                overlay('Vector hit an error and needs to restart.', 'Reload', () => location.reload());
                break;
            case 'file-changed':
                forget(data.path);
                break;
            case 'result':
                settle(data.id, data.ok, data.ok ? data.value : data.error);
                break;
            case 'event': {
                const payload = data.json === undefined ? null : JSON.parse(data.json);
                if (data.bytes) payload.bytes = data.bytes;
                dispatchEvent(data.name, payload);
                break;
            }
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
        overlay('Vector is open in another tab.', 'Use here', () => { takeOver(); });
    };

    // Ask the owner to step aside, then reload once its lock is actually free.
    function takeOver() {
        channel.postMessage('takeover');
        navigator.locks.request('vector-web-instance', () => location.reload());
    }

    function blocked() {
        overlay('Vector is already open in another tab.', 'Use here', () => {
            takeOver();
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

    // Tor Browser's Safer and Safest levels turn WebAssembly off, and Vector's core is WebAssembly.
    if (typeof WebAssembly !== 'object') {
        fatal = 'WebAssembly is turned off';
        overlay('Vector needs WebAssembly, which this browser has turned off. In Tor Browser, open the shield menu, set the security level to Standard, then reload.', 'Reload', () => location.reload());
    } else if (navigator.locks) claim(); else startWorker();

    function settle(id, ok, value) {
        const p = pending.get(id);
        if (!p) return;
        pending.delete(id);
        if (!ok) return p.reject(value);
        if (typeof value !== 'string') return p.resolve(value);
        p.resolve(value === '' ? undefined : JSON.parse(value));
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

    // Only the page knows the device: on a touch screen the app takes its mobile
    // behaviour (press-and-hold menus, swipe to reply, touch styles), as on Android.
    // What the browser allows decides the rest: another account only where switching
    // (a reload) keeps the one before, mini apps only behind a service worker.
    local.set('get_platform_features', async () => {
        const features = await backend('get_platform_features');
        return {
            ...features,
            is_mobile: matchMedia('(pointer: coarse)').matches,
            multi_account: features.storage !== 'memory',
            mini_apps: hasServiceWorker(),
        };
    });

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

    // ─── Stored files ───────────────────────────────────────────────────────
    // With OPFS and a service worker, a stored file is `/vfs<path>`, served by
    // web/sw.js. Anywhere else the page holds it as a blob: URL of the worker's
    // bytes. `convertFileSrc` must answer at once, so there it hands out a stable
    // placeholder per file, and whatever is given one (an element's src or poster,
    // a style, a fetch) is pointed at the file's current URL once its bytes are here.
    const vfsUrl = (path) => '/vfs' + ('/' + path.replace(/^\/+/, '')).split('/').map(encodeURIComponent).join('/');
    const blobMode = () => storage !== null && (storage !== 'persistent' || !hasServiceWorker());

    // Only passive types render, as web/sw.js serves them; anything else is bytes to
    // save. No SVG: a blob is a document on this origin, with no sandbox header to
    // disarm one opened on its own.
    const TYPES = {
        png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp',
        avif: 'image/avif', bmp: 'image/bmp', ico: 'image/x-icon',
        mp4: 'video/mp4', webm: 'video/webm', mov: 'video/quicktime', mkv: 'video/x-matroska',
        mp3: 'audio/mpeg', m4a: 'audio/mp4', aac: 'audio/aac', ogg: 'audio/ogg', opus: 'audio/ogg',
        wav: 'audio/wav', flac: 'audio/flac', weba: 'audio/webm',
        txt: 'text/plain; charset=utf-8',
    };
    const typeOf = (path) => TYPES[path.split('.').pop().toLowerCase()] || 'application/octet-stream';

    // Least recently used first; past the budget the oldest URLs nothing shows are released.
    const BLOB_BUDGET = 256 * 1024 * 1024;
    const blobs = new Map();
    let blobBytes = 0;
    const loading = new Map();
    // A file found missing is asked for again only after a moment.
    const missing = new Map();
    const MISSING_FOR = 5000;
    // URLs minted here: the app releases blob URLs it made itself, never these.
    const owned = new Set();
    const revoke = URL.revokeObjectURL.bind(URL);

    const inUse = (url) => !!document.querySelector(`[src="${url}"],[poster="${url}"],[style*="${url}"]`);

    function release(path) {
        const b = blobs.get(path);
        if (!b) return;
        blobs.delete(path);
        blobBytes -= b.size;
        if (!inUse(b.url)) {
            owned.delete(b.url);
            revoke(b.url);
        }
    }

    function remember(path, url, size) {
        owned.add(url);
        blobs.set(path, { url, size });
        blobBytes += size;
        for (const [p, b] of blobs) {
            if (blobBytes <= BLOB_BUDGET) break;
            if (p !== path && !inUse(b.url)) release(p);
        }
    }

    // A file rewritten or removed in place: its old bytes must not be served again.
    function forget(path) {
        const prefix = path.replace(/\/+$/, '') + '/';
        for (const p of [...blobs.keys()]) if (p === path || p.startsWith(prefix)) release(p);
        for (const p of [...missing.keys()]) if (p === path || p.startsWith(prefix)) missing.delete(p);
    }

    /** The URL a stored file can be loaded from now; null when it doesn't exist. */
    function fileUrl(path) {
        if (!blobMode()) return Promise.resolve(vfsUrl(path));
        const hit = blobs.get(path);
        if (hit) {
            blobs.delete(path);
            blobs.set(path, hit);
            return Promise.resolve(hit.url);
        }
        if (Date.now() - (missing.get(path) || 0) < MISSING_FOR) return Promise.resolve(null);
        if (!loading.has(path)) {
            loading.set(path, post({ t: 'file', path })
                .then((bytes) => {
                    if (!bytes) {
                        missing.set(path, Date.now());
                        return null;
                    }
                    const url = URL.createObjectURL(new Blob([bytes], { type: typeOf(path) }));
                    remember(path, url, bytes.byteLength);
                    return url;
                })
                .catch(() => null)
                .finally(() => loading.delete(path)));
        }
        return loading.get(path);
    }

    // A transparent pixel with an unguessable name, so an image shows nothing rather
    // than a broken icon meanwhile, and no message can name someone else's file.
    const PLACEHOLDER = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
    // A placeholder, plus whatever a caller appended to it (a cache-busting query).
    const TOKEN = /data:image\/gif;base64,[A-Za-z0-9+/=]+#vfs-[0-9a-f-]{36}(?:[?&][^"')\s]*)?/g;
    const tokens = new Map();
    const tokenOf = new Map();
    const tokenIn = (value) => {
        const base = typeof value === 'string' && value.startsWith(PLACEHOLDER) ? /^[^#]+#vfs-[0-9a-f-]{36}/.exec(value)?.[0] : null;
        return base && tokens.has(base) ? base : null;
    };

    function convertFileSrc(path) {
        if (!path || /^[a-z]+:/i.test(path)) return path;
        if (!blobMode()) return vfsUrl(path);
        let token = tokenOf.get(path);
        if (!token) {
            token = `${PLACEHOLDER}#vfs-${crypto.randomUUID()}`;
            tokenOf.set(path, token);
            tokens.set(token, path);
            installBinding();
            fileUrl(path);
        }
        return token;
    }

    /** The stored path behind a URL `convertFileSrc` handed out, if it is one. */
    const pathOf = (url) => tokens.get(tokenIn(url)) ?? null;

    // What each element was last given and hasn't got yet: a file arriving late must
    // not overwrite a newer value, or a cleared one.
    const awaiting = new WeakMap();
    const decodeWait = new WeakMap();
    let setNative = null;
    let removeNative = null;

    const isMedia = (el) => el instanceof HTMLMediaElement || el instanceof HTMLSourceElement;

    function wanted(el, attr, needle) {
        if (attr === 'style') return !!el.getAttribute('style')?.includes(needle);
        return awaiting.get(el)?.get(attr) === needle;
    }

    function apply(el, attr, needle, url) {
        if (attr === 'style') {
            const style = el.getAttribute('style');
            if (style?.includes(needle)) setNative(el, 'style', style.split(needle).join(url));
        } else {
            awaiting.get(el)?.delete(attr);
            setNative(el, attr, url);
        }
    }

    // `needle` is the text handed out: the placeholder and any suffix.
    function bind(el, attr, token, needle) {
        const path = tokens.get(token);
        const hit = blobs.get(path);
        if (hit) return apply(el, attr, needle, hit.url);
        if (attr !== 'style') {
            if (!awaiting.has(el)) awaiting.set(el, new Map());
            awaiting.get(el).set(attr, needle);
            // A GIF is no media: never let a player try it and fail. An image on
            // screen shows the pixel at once rather than the file it showed before.
            if (isMedia(el)) removeNative(el, attr);
            else if (el.isConnected && el.getAttribute(attr) !== needle) setNative(el, attr, needle);
        }
        const done = fileUrl(path).then((url) => {
            if (!wanted(el, attr, needle)) return;
            if (url) apply(el, attr, needle, url);
            else if (attr !== 'style') {
                awaiting.get(el)?.delete(attr);
                el.dispatchEvent(new Event('error'));
            }
        });
        if (attr === 'src' && el instanceof HTMLImageElement) {
            decodeWait.set(el, done);
            done.then(() => { if (decodeWait.get(el) === done) decodeWait.delete(el); });
        }
    }

    function check(el, attr) {
        const value = el.getAttribute(attr);
        if (!value || !value.includes('#vfs-')) return;
        if (attr === 'style') {
            for (const m of value.match(TOKEN) || []) { const t = tokenIn(m); if (t) bind(el, 'style', t, m); }
            return;
        }
        if (awaiting.get(el)?.get(attr) === value) return;
        const token = tokenIn(value);
        if (token) bind(el, attr, token, value);
    }

    let bound = false;
    function installBinding() {
        if (bound) return;
        bound = true;
        URL.revokeObjectURL = (url) => { if (!owned.has(url)) revoke(url); };

        const setAttribute = Element.prototype.setAttribute;
        const removeAttribute = Element.prototype.removeAttribute;
        const natives = new Map();
        const targets = [
            [HTMLImageElement.prototype, 'src'], [HTMLMediaElement.prototype, 'src'],
            [HTMLSourceElement.prototype, 'src'], [HTMLVideoElement.prototype, 'poster'],
        ];
        for (const [proto, attr] of targets) {
            const d = Object.getOwnPropertyDescriptor(proto, attr);
            natives.set(proto, d);
            Object.defineProperty(proto, attr, {
                ...d,
                set(v) {
                    const token = tokenIn(v);
                    if (token) return bind(this, attr, token, v);
                    awaiting.get(this)?.delete(attr);
                    d.set.call(this, v);
                },
            });
        }
        setNative = (el, attr, value) => setAttribute.call(el, attr, value);
        removeNative = (el, attr) => removeAttribute.call(el, attr);
        Element.prototype.setAttribute = function (name, value) {
            if (name === 'src' || name === 'poster') {
                const token = tokenIn(value);
                if (token) return bind(this, name, token, value);
                awaiting.get(this)?.delete(name);
            }
            return setAttribute.call(this, name, value);
        };
        Element.prototype.removeAttribute = function (name) {
            if (name === 'src' || name === 'poster') awaiting.get(this)?.delete(name);
            return removeAttribute.call(this, name);
        };
        const decode = HTMLImageElement.prototype.decode;
        HTMLImageElement.prototype.decode = function () {
            const wait = decodeWait.get(this);
            return wait ? wait.then(() => decode.call(this)) : decode.call(this);
        };
        const fetch0 = window.fetch;
        window.fetch = function (input, init) {
            const u = typeof input === 'string' ? input : input instanceof URL ? input.href : null;
            const token = tokenIn(u);
            if (token) {
                return fileUrl(tokens.get(token)).then((url) => (url ? fetch0(url, init) : Promise.reject(new TypeError('File not found'))));
            }
            return fetch0.call(this, input, init);
        };
        // Markup built as a string never goes through the setters above.
        const visit = (el) => { for (const a of ['src', 'poster', 'style']) if (el.hasAttribute(a)) check(el, a); };
        new MutationObserver((records) => {
            for (const r of records) {
                if (r.type === 'attributes') { check(r.target, r.attributeName); continue; }
                for (const n of r.addedNodes) {
                    if (n.nodeType !== 1) continue;
                    visit(n);
                    for (const el of n.querySelectorAll('[src*="#vfs-"],[poster*="#vfs-"],[style*="#vfs-"]')) visit(el);
                }
            }
        }).observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ['src', 'poster', 'style'] });
    }

    // Nothing is kept here: leaving signs out, so say so first. The app's own
    // reloads (an account switch, logging out) are the user's choice already.
    let signedIn = false;
    let leaving = false;
    listen('init_finished', () => { signedIn = true; });
    listen('session_reload', () => { leaving = true; });
    addEventListener('beforeunload', (e) => {
        if (storage !== 'memory' || !signedIn || leaving || fatal) return;
        e.preventDefault();
        e.returnValue = '';
    });

    window.__TAURI__ = {
        core: {
            invoke,
            convertFileSrc,
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
        process: { exit: async () => { leaving = true; location.reload(); }, relaunch: async () => { leaving = true; location.reload(); } },
        updater: { check: async () => null },
    };

    window.__vectorWeb = {
        register: (cmd, fn) => local.set(cmd, fn),
        emit: dispatchEvent,
        backend,
        storeFiles,
        convertFileSrc,
        fileUrl,
        pathOf,
        storage: () => storage,
        hasServiceWorker,
    };

    // Back in the foreground: the OS froze the worker's sockets while away.
    let hiddenAt = 0;
    document.addEventListener('visibilitychange', () => {
        if (document.visibilityState === 'hidden') { hiddenAt = Date.now(); return; }
        const away = hiddenAt ? Date.now() - hiddenAt : 0;
        hiddenAt = 0;
        if (ready && away >= 2000) backend('web_resume', { hiddenMs: away }).catch(() => {});
    });
    addEventListener('online', () => { if (ready) backend('web_resume', { hiddenMs: 0 }).catch(() => {}); });

    // The desktop reloads its webview on `session_reload`; here a reload also
    // restarts the worker, which boots whichever account is marked active.
    document.documentElement.classList.add('vector-web');
})();
