// Vector Web: serves the backend's files to the page, as asset:// does on desktop.
// `convertFileSrc(path)` yields `/vfs<path>`; the file lives in OPFS under `files<path>`.
const PREFIX = '/vfs/';

const TYPES = {
    png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp',
    avif: 'image/avif', svg: 'image/svg+xml', bmp: 'image/bmp', ico: 'image/x-icon',
    mp4: 'video/mp4', webm: 'video/webm', mov: 'video/quicktime', mkv: 'video/x-matroska',
    mp3: 'audio/mpeg', m4a: 'audio/mp4', aac: 'audio/aac', ogg: 'audio/ogg', opus: 'audio/ogg',
    wav: 'audio/wav', flac: 'audio/flac', weba: 'audio/webm',
    pdf: 'application/pdf', txt: 'text/plain; charset=utf-8', json: 'application/json',
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
    try { file = await openFile(path); } catch { return new Response('Not found', { status: 404 }); }
    const ext = path.split('.').pop().toLowerCase();
    const type = TYPES[ext] || file.type || 'application/octet-stream';
    const headers = { 'Content-Type': type, 'Accept-Ranges': 'bytes', 'Cache-Control': 'no-cache' };
    const range = request.headers.get('Range');
    const m = range && /bytes=(\d*)-(\d*)/.exec(range);
    if (m) {
        const size = file.size;
        let start = m[1] === '' ? size - Number(m[2]) : Number(m[1]);
        let end = m[1] !== '' && m[2] !== '' ? Number(m[2]) : size - 1;
        start = Math.max(0, start);
        end = Math.min(end, size - 1);
        if (start > end) return new Response(null, { status: 416, headers: { 'Content-Range': `bytes */${size}` } });
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
    e.respondWith(serve(e.request, decodeURIComponent(url.pathname.slice(PREFIX.length - 1))));
});
