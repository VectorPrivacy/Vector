// Vector Web: serves the backend's files to the page, as asset:// does on desktop.
// `convertFileSrc(path)` yields `/vfs<path>`; the file lives in OPFS under `files<path>`.
// These bytes are other people's: every response is sandboxed, and only passive
// types render; anything else downloads.
const PREFIX = '/vfs/';

const TYPES = {
    png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp',
    avif: 'image/avif', svg: 'image/svg+xml', bmp: 'image/bmp', ico: 'image/x-icon',
    mp4: 'video/mp4', webm: 'video/webm', mov: 'video/quicktime', mkv: 'video/x-matroska',
    mp3: 'audio/mpeg', m4a: 'audio/mp4', aac: 'audio/aac', ogg: 'audio/ogg', opus: 'audio/ogg',
    wav: 'audio/wav', flac: 'audio/flac', weba: 'audio/webm',
    txt: 'text/plain; charset=utf-8',
};

const SANDBOX = {
    'Content-Security-Policy': "sandbox; default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'",
    'X-Content-Type-Options': 'nosniff',
    'Cache-Control': 'no-cache',
};

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

async function openFile(path) {
    let dir = await navigator.storage.getDirectory();
    dir = await dir.getDirectoryHandle('files');
    const parts = path.split('/').filter(Boolean);
    const name = parts.pop();
    for (const p of parts) dir = await dir.getDirectoryHandle(p);
    return (await dir.getFileHandle(name)).getFile();
}

async function serve(request, path) {
    let file;
    try { file = await openFile(path); } catch { return new Response('Not found', { status: 404, headers: SANDBOX }); }
    const ext = path.split('.').pop().toLowerCase();
    const type = TYPES[ext];
    const headers = { ...SANDBOX, 'Content-Type': type || 'application/octet-stream', 'Accept-Ranges': 'bytes' };
    if (!type) headers['Content-Disposition'] = 'attachment';
    const range = request.headers.get('Range');
    const m = range && /bytes=(\d*)-(\d*)/.exec(range);
    if (m) {
        const size = file.size;
        let start = m[1] === '' ? size - Number(m[2]) : Number(m[1]);
        let end = m[1] !== '' && m[2] !== '' ? Number(m[2]) : size - 1;
        start = Math.max(0, start);
        end = Math.min(end, size - 1);
        if (start > end) return new Response(null, { status: 416, headers: { ...SANDBOX, 'Content-Range': `bytes */${size}` } });
        return new Response(file.slice(start, end + 1), {
            status: 206,
            headers: { ...headers, 'Content-Range': `bytes ${start}-${end}/${size}`, 'Content-Length': String(end - start + 1) },
        });
    }
    return new Response(file, { headers: { ...headers, 'Content-Length': String(file.size) } });
}

self.addEventListener('fetch', (e) => {
    const url = new URL(e.request.url);
    if (url.origin !== location.origin || !url.pathname.startsWith(PREFIX)) return;
    let path;
    try { path = decodeURIComponent(url.pathname.slice(PREFIX.length - 1)); } catch { return; }
    if (path.split('/').includes('..')) return e.respondWith(new Response('Bad path', { status: 400 }));
    e.respondWith(serve(e.request, path));
});

// --- Notifications while Vector is closed (web/push.js, vector_core::push) ---------------
// A contact's Vector wrote the notification and encrypted it to this device; the browser
// has already decrypted it. It names the sender only by a handle this device issued, and
// carries a MAC made with that contact's key, so a sender can't pose as someone else. The
// name shown is always this device's own. iOS shows something for every push no matter
// what, so anything unverifiable becomes the generic notice rather than nothing.
const PUSH_CACHE = 'vector-push';
const PUSH_STATE = '/__vector/push/state';
const PUSH_BADGE = '/__vector/push/badge';

async function pushState() {
    try {
        const res = await (await caches.open(PUSH_CACHE)).match(PUSH_STATE);
        return res ? await res.json() : null;
    } catch { return null; }
}

const hexBytes = (h) => Uint8Array.from(h.match(/../g) || [], (b) => parseInt(b, 16));
const bytesHex = (b) => [...new Uint8Array(b)].map((x) => x.toString(16).padStart(2, '0')).join('');

async function verified(data, contact) {
    if (typeof data?.a !== 'string' || !/^[0-9a-f]{64}$/.test(contact?.mk || '')) return false;
    const key = await crypto.subtle.importKey('raw', hexBytes(contact.mk), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
    const text = `vector-push/1\n${data.h}\n${data.k}\n${data.m}\n${data.t}\n${data.x}`;
    const mac = bytesHex(await crypto.subtle.sign('HMAC', key, new TextEncoder().encode(text))).slice(0, 32);
    return mac === data.a;
}

async function describe(data) {
    const state = await pushState();
    const contact = state?.contacts?.[data?.h];
    if (!contact || !(await verified(data, contact))) return null;
    const text = String(data.x || '').slice(0, 1000);
    const name = contact.name || 'New message';
    const shown = {
        full: { title: name, body: text },
        hide_content: { title: name, body: data.k === 'file' ? 'Sent you a file' : 'Sent you a message' },
        hide_sender: { title: 'New message', body: text },
        hide_all: { title: 'Vector', body: 'You received a message' },
    }[state.privacy] || { title: name, body: text };
    return { ...shown, chat: contact.npub };
}

async function bumpBadge() {
    try {
        const cache = await caches.open(PUSH_CACHE);
        const res = await cache.match(PUSH_BADGE);
        const n = (res ? Number(await res.text()) || 0 : 0) + 1;
        await cache.put(PUSH_BADGE, new Response(String(n)));
        // Never awaited: during a declarative push WebKit leaves this promise pending.
        self.navigator.setAppBadge?.(n)?.catch?.(() => {});
    } catch {}
}

async function onPush(event) {
    // iOS 18.4+ parses the declarative message itself and hands over its proposal; other
    // browsers deliver the same JSON as data.
    let message = null;
    if (!event.notification) {
        try { message = event.data?.json(); } catch {}
    }
    const data = event.notification?.data ?? message?.notification?.data;
    let shown = null;
    try { shown = await describe(data); } catch {}
    // A window of ours in front already shows the message; only iOS insists on a notice.
    if (!event.notification) {
        const wins = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
        if (wins.some((w) => w.visibilityState === 'visible' && w.focused)) return;
    }
    const origin = self.location.origin;
    const title = shown?.title || 'Vector';
    const options = {
        body: shown?.body || 'New message',
        tag: shown?.chat ? `chat:${shown.chat}` : 'vector',
        icon: '/icon-192.png',
        data: { chat: shown?.chat || null },
        navigate: shown?.chat ? `${origin}/#chat=${shown.chat}` : `${origin}/`,
    };
    await self.registration.showNotification(title, options);
    bumpBadge();
}

self.addEventListener('push', (event) => event.waitUntil(onPush(event)));

self.addEventListener('notificationclick', (event) => {
    event.notification.close();
    const chat = event.notification.data?.chat || null;
    event.waitUntil((async () => {
        try { await (await caches.open(PUSH_CACHE)).delete(PUSH_BADGE); } catch {}
        const wins = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
        if (wins.length) {
            const win = wins.find((w) => w.focused) || wins[0];
            await win.focus().catch(() => {});
            if (chat) win.postMessage({ type: 'vector-open-chat', chat });
            return;
        }
        await self.clients.openWindow(chat ? `/#chat=${chat}` : '/');
    })());
});
