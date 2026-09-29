// Vector Web mini app origin: serves one app's files straight out of its .xdc.
// Every mini app gets its own origin (`<partition>.xdc.<vector host>`), so its
// storage is its own. The host page (host.js) stores the package; this serves it
// under an offline policy: no network beyond the origin itself.
'use strict';

const STORE = 'vector-xdc';
const PKG = '/__vector/package';
const META = '/__vector/meta';

// A policy the app can't loosen: responses carry it, and nothing else serves this origin.
// No frames at all: a child frame is a fresh window with its own WebRTC (see bridge.js).
const BASE_CSP = [
    "default-src 'self'",
    "script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval' blob:",
    "style-src 'self' 'unsafe-inline' blob:",
    "font-src 'self' data: blob:",
    "img-src 'self' data: blob:",
    "media-src 'self' data: blob:",
    "connect-src 'self' data: blob:",
    "worker-src 'self' blob:",
    "frame-src 'none'",
    "fenced-frame-src 'none'",
    "child-src 'self' blob:",
    "form-action 'none'",
    "base-uri 'self'",
    "object-src 'none'",
    "webrtc 'block'",
];
// Documents: HTML strings only through the guard's policy, which is the only one.
const DOCUMENT_CSP = [...BASE_CSP, "require-trusted-types-for 'script'", 'trusted-types default'].join('; ');
// Scripts, which may start workers: workers have no DOM and no WebRTC.
const SCRIPT_CSP = BASE_CSP.join('; ');
// Anything else opened as a document (an SVG, some XML) runs no script at all.
const INERT_CSP = "sandbox; default-src 'none'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; font-src 'self' data:; media-src 'self' data: blob:";

const TYPES = {
    html: 'text/html; charset=utf-8', htm: 'text/html; charset=utf-8', js: 'text/javascript; charset=utf-8',
    mjs: 'text/javascript; charset=utf-8', css: 'text/css; charset=utf-8', json: 'application/json',
    wasm: 'application/wasm', svg: 'image/svg+xml', png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg',
    gif: 'image/gif', webp: 'image/webp', avif: 'image/avif', ico: 'image/x-icon', bmp: 'image/bmp',
    mp3: 'audio/mpeg', ogg: 'audio/ogg', oga: 'audio/ogg', opus: 'audio/ogg', wav: 'audio/wav', m4a: 'audio/mp4',
    aac: 'audio/aac', flac: 'audio/flac', weba: 'audio/webm', mp4: 'video/mp4', webm: 'video/webm', ogv: 'video/ogg',
    woff: 'font/woff', woff2: 'font/woff2', ttf: 'font/ttf', otf: 'font/otf', txt: 'text/plain; charset=utf-8',
    xml: 'application/xml', toml: 'text/plain; charset=utf-8', md: 'text/plain; charset=utf-8',
};

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

// The package is parsed once per load; the host says when a new one lands.
let current = null;
self.addEventListener('message', (e) => {
    if (e.data?.t === 'reset') current = null;
    e.ports?.[0]?.postMessage('ok');
});

async function load() {
    if (current) return current;
    const cache = await caches.open(STORE);
    const [pkg, meta] = await Promise.all([cache.match(PKG), cache.match(META)]);
    if (!pkg || !meta) throw new Error('no package');
    const blob = await pkg.blob();
    current = { blob, meta: await meta.json(), entries: await readDirectory(blob) };
    return current;
}

// ─── Zip reading ────────────────────────────────────────────────────────────

const u16 = (v, o) => v.getUint16(o, true);
const u32 = (v, o) => v.getUint32(o, true);
const decoder = new TextDecoder();

async function readDirectory(blob) {
    const tailLen = Math.min(blob.size, 65557);
    const tail = new DataView(await blob.slice(blob.size - tailLen).arrayBuffer());
    let eocd = -1;
    for (let i = tailLen - 22; i >= 0; i--) {
        if (u32(tail, i) === 0x06054b50) { eocd = i; break; }
    }
    if (eocd < 0) throw new Error('not a zip');
    const count = u16(tail, eocd + 10);
    const size = u32(tail, eocd + 12);
    const offset = u32(tail, eocd + 16);
    if (offset === 0xffffffff || count === 0xffff) throw new Error('zip64 packages are not supported');
    const dir = new DataView(await blob.slice(offset, offset + size).arrayBuffer());
    const entries = new Map();
    for (let p = 0, n = 0; n < count && u32(dir, p) === 0x02014b50; n++) {
        const method = u16(dir, p + 10);
        const compressed = u32(dir, p + 20);
        const uncompressed = u32(dir, p + 24);
        const nameLen = u16(dir, p + 28);
        const extraLen = u16(dir, p + 30);
        const commentLen = u16(dir, p + 32);
        const local = u32(dir, p + 42);
        const name = decoder.decode(new Uint8Array(dir.buffer, dir.byteOffset + p + 46, nameLen));
        if (!name.endsWith('/')) entries.set(name, { method, compressed, uncompressed, local });
        p += 46 + nameLen + extraLen + commentLen;
    }
    return entries;
}

function find(entries, path) {
    if (entries.has(path)) return entries.get(path);
    const lower = path.toLowerCase();
    for (const [name, e] of entries) if (name.toLowerCase() === lower) return e;
    return null;
}

// Decompressed-size ceiling, as desktop: a small entry must not inflate to gigabytes.
const ENTRY_CAP = 512 * 1024 * 1024;

async function extract(blob, e) {
    const head = new DataView(await blob.slice(e.local, e.local + 30).arrayBuffer());
    if (u32(head, 0) !== 0x04034b50) throw new Error('bad entry');
    const start = e.local + 30 + u16(head, 26) + u16(head, 28);
    const raw = blob.slice(start, start + e.compressed);
    if (e.method === 0) return raw;
    if (e.method !== 8) throw new Error(`unsupported compression ${e.method}`);
    const reader = raw.stream().pipeThrough(new DecompressionStream('deflate-raw')).getReader();
    const parts = [];
    let total = 0;
    for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        total += value.length;
        if (total > ENTRY_CAP) { reader.cancel(); throw new Error('entry too large'); }
        parts.push(value);
    }
    return new Blob(parts);
}

// ─── Serving ────────────────────────────────────────────────────────────────

const isDocument = (type) => type.startsWith('text/html');
const isScript = (type) => type.startsWith('text/javascript') || type.startsWith('application/wasm');

function headers(meta, type) {
    return {
        'Content-Type': type,
        'Content-Security-Policy': isDocument(type) ? DOCUMENT_CSP : isScript(type) ? SCRIPT_CSP : INERT_CSP,
        'Permissions-Policy': meta.policy,
        'X-Content-Type-Options': 'nosniff',
        'X-DNS-Prefetch-Control': 'off',
        'Referrer-Policy': 'no-referrer',
        'Cache-Control': 'no-store',
    };
}

// Markup the parser would turn into a frame is disarmed before it is served;
// the guard does the same for markup made at runtime.
const FRAMES = ['iframe', 'frame', 'frameset', 'object', 'embed', 'fencedframe', 'portal'];
const FRAME_TAG = new RegExp(`<(/?)([\\w.-]+:)?(${FRAMES.join('|')})(?=[\\s/>]|$)`, 'gi');
const disarm = (text) => text.replace(FRAME_TAG, '<$1$2vector-blocked-$3');

// The guard must run before anything else in the document, so it goes first,
// after the doctype if there is one. A later `<script src="webxdc.js">` of the
// app's own finds the API already there.
const BRIDGE_TAG = '<script src="/webxdc.js"></script>';

function inject(html) {
    const doctype = /^\uFEFF?\s*<!doctype[^>]*>/i.exec(html);
    const at = doctype ? doctype[0].length : 0;
    return html.slice(0, at) + BRIDGE_TAG + html.slice(at);
}

const MARKUP = /^(text\/html|image\/svg\+xml|application\/xml)/;

async function serve(request, path) {
    let pkg;
    try { pkg = await load(); } catch { return new Response('Mini app not loaded', { status: 503 }); }
    const { blob, meta, entries } = pkg;
    if (path === '/webxdc.js') {
        const src = (await (await fetch('/__vector/bridge.js')).text())
            .replace('__VECTOR_META__', JSON.stringify({ selfAddr: meta.selfAddr, selfName: meta.selfName, parent: meta.parent }));
        return new Response(src, { headers: headers(meta, TYPES.js) });
    }
    let name = path.replace(/^\/+/, '') || 'index.html';
    if (name.split('/').includes('..')) return new Response('Bad path', { status: 400 });
    let entry = null;
    for (const candidate of [name, `${name}.html`, `${name.replace(/\/$/, '')}/index.html`]) {
        entry = find(entries, candidate);
        if (entry) { name = candidate; break; }
    }
    if (!entry) return new Response('Not found', { status: 404, headers: headers(meta, 'text/plain') });
    let body;
    try { body = await extract(blob, entry); } catch (e) { return new Response(String(e), { status: 500 }); }
    const ext = name.split('.').pop().toLowerCase();
    const type = TYPES[ext] || 'application/octet-stream';
    if (MARKUP.test(type)) {
        const text = disarm(await body.text());
        body = new Blob([isDocument(type) ? inject(text) : text]);
    }

    const range = request.headers.get('Range');
    const m = range && /bytes=(\d*)-(\d*)/.exec(range);
    if (m) {
        const size = body.size;
        const startAt = m[1] === '' ? Math.max(0, size - Number(m[2])) : Number(m[1]);
        const end = m[1] !== '' && m[2] !== '' ? Math.min(Number(m[2]), size - 1) : size - 1;
        if (startAt > end) return new Response(null, { status: 416, headers: { 'Content-Range': `bytes */${size}` } });
        return new Response(body.slice(startAt, end + 1), {
            status: 206,
            headers: { ...headers(meta, type), 'Accept-Ranges': 'bytes', 'Content-Range': `bytes ${startAt}-${end}/${size}`, 'Content-Length': String(end - startAt + 1) },
        });
    }
    return new Response(body, { headers: { ...headers(meta, type), 'Accept-Ranges': 'bytes', 'Content-Length': String(body.size) } });
}

self.addEventListener('fetch', (e) => {
    const url = new URL(e.request.url);
    if (url.origin !== location.origin || url.pathname.startsWith('/__vector/')) return;
    let path;
    try { path = decodeURIComponent(url.pathname); } catch { return e.respondWith(new Response('Bad path', { status: 400 })); }
    e.respondWith(serve(e.request, path));
});
