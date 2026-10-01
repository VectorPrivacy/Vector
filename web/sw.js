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
