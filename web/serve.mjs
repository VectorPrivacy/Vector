#!/usr/bin/env node
// Static server for dist-web/. OPFS needs a secure context, which localhost is.
import { createServer } from 'http';
import { readFile, stat } from 'fs/promises';
import { join, extname, normalize, dirname } from 'path';
import { fileURLToPath } from 'url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', 'dist-web');
const PORT = Number(process.env.PORT || 8790);

const TYPES = {
    '.html': 'text/html; charset=utf-8',
    '.js': 'text/javascript; charset=utf-8',
    '.mjs': 'text/javascript; charset=utf-8',
    '.css': 'text/css; charset=utf-8',
    '.json': 'application/json',
    '.wasm': 'application/wasm',
    '.svg': 'image/svg+xml',
    '.png': 'image/png',
    '.jpg': 'image/jpeg',
    '.gif': 'image/gif',
    '.webp': 'image/webp',
    '.ico': 'image/x-icon',
    '.woff': 'font/woff',
    '.woff2': 'font/woff2',
    '.ttf': 'font/ttf',
    '.mp3': 'audio/mpeg',
    '.wav': 'audio/wav',
};

// Mini app origins: `<partition>.xdc.<vector host>`. Only the host page, its
// script, the service worker and the bridge template are served; the worker
// serves the app itself out of its package, so nothing else exists here.
const XDC_FILES = new Set(['host.html', 'host.js', 'sw.js', 'bridge.js']);

function serveMiniAppOrigin(req, res, vectorHost) {
    const name = new URL(req.url, 'http://x').pathname.replace(/^\/__vector\//, '');
    if (!XDC_FILES.has(name) || !req.url.startsWith('/__vector/')) return res.writeHead(404).end('Not found');
    readFile(join(ROOT, '__vector', name)).then((body) => {
        const vector = `http://${vectorHost}`;
        res.writeHead(200, {
            'Content-Type': TYPES[extname(name)],
            'Cache-Control': 'no-cache',
            'X-Content-Type-Options': 'nosniff',
            'Content-Security-Policy': `default-src 'self'; script-src 'self'; style-src 'unsafe-inline'; frame-ancestors ${vector}`,
            'Service-Worker-Allowed': '/',
        });
        res.end(body);
    }, () => res.writeHead(404).end('Not found'));
}

createServer(async (req, res) => {
    const app = /^[a-z0-9-]+\.xdc\.(.+)$/i.exec(req.headers.host || '');
    if (app) return serveMiniAppOrigin(req, res, app[1]);
    try {
        const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname)).replace(/^(\.\.[/\\])+/, '');
        let file = join(ROOT, path);
        if ((await stat(file)).isDirectory()) file = join(file, 'index.html');
        const body = await readFile(file);
        const headers = { 'Content-Type': TYPES[extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-cache', 'X-Content-Type-Options': 'nosniff' };
        // Clickjacking guard for the app page; meta CSP can't carry frame-ancestors.
        if (extname(file) === '.html') {
            headers['Content-Security-Policy'] = `frame-ancestors 'none'; frame-src http://*.xdc.${req.headers.host}`;
        }
        res.writeHead(200, headers);
        res.end(body);
    } catch {
        res.writeHead(404).end('Not found');
    }
}).listen(PORT, () => console.log(`[vector-web] http://localhost:${PORT}`));
