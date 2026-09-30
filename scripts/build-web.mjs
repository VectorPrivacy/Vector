#!/usr/bin/env node
/**
 * build-web.mjs — Vector Web: the desktop frontend plus vector-core as WebAssembly.
 *
 *   node scripts/build-web.mjs            # dev wasm (fast compile, slow crypto)
 *   node scripts/build-web.mjs --release  # optimised wasm
 *   node scripts/build-web.mjs --no-wasm  # frontend only, reuse web/pkg
 *
 * Output: dist-web/, served by `node web/serve.mjs`.
 */

import { cpSync, rmSync, readFileSync, writeFileSync, existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import { execFileSync } from 'child_process';
import { buildSvelte } from './build-svelte.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const OUT = join(ROOT, 'dist-web');
const release = process.argv.includes('--release');

if (!process.argv.includes('--no-wasm')) {
    execFileSync('wasm-pack', [
        'build', '--target', 'web', release ? '--release' : '--dev',
        '--no-typescript', '--no-pack', '--out-dir', join(ROOT, 'web', 'pkg'),
    ], { cwd: join(ROOT, 'crates', 'vector-web'), stdio: 'inherit' });
}
if (!existsSync(join(ROOT, 'web', 'pkg', 'vector_web_bg.wasm'))) {
    console.error('[build-web] no wasm build in web/pkg — run without --no-wasm first');
    process.exit(1);
}

await buildSvelte({ dev: !release });

rmSync(OUT, { recursive: true, force: true });
cpSync(join(ROOT, 'src'), OUT, { recursive: true, dereference: true });
for (const f of ['tauri-shim.js', 'chrome.js', 'media.js', 'signer.js', 'miniapps.js', 'worker.js', 'web.css']) cpSync(join(ROOT, 'web', f), join(OUT, 'web', f));
// Icons and the link-preview card, at the root where browsers and crawlers look.
cpSync(join(ROOT, 'web', 'meta'), OUT, { recursive: true });
// The app's own version, for Settings and the database's downgrade record.
const VERSION = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8')).version;
const workerPath = join(OUT, 'web', 'worker.js');
writeFileSync(workerPath, readFileSync(workerPath, 'utf8').replace("const VERSION = 'web';", `const VERSION = ${JSON.stringify(VERSION)};`));
cpSync(join(ROOT, 'web', 'pkg'), join(OUT, 'web', 'pkg'), { recursive: true });
// Root scope, so it can answer `/vfs/…` for the whole page.
cpSync(join(ROOT, 'web', 'sw.js'), join(OUT, 'sw.js'));
// Mini app origins are served from here; see serve.mjs.
cpSync(join(ROOT, 'web', 'xdc'), join(OUT, '__vector'), { recursive: true });

// Desktop's policy, minus Tauri's schemes. Remote images are allowed because
// without a media proxy the page loads them directly, as desktop does.
export const CSP = [
    "default-src 'self'",
    "script-src 'self'",
    "img-src 'self' data: blob: https:",
    "media-src 'self' blob: https://gifverse.net",
    "style-src 'self' 'unsafe-inline'",
    "connect-src 'self' blob: https://gifverse.net",
    "worker-src 'self'",
    // Mini apps run on subdomains of whatever host serves Vector; the server narrows
    // this to its own `*.xdc.<host>` with a header, and both policies apply.
    "frame-src http://*.localhost:* https:",
    "base-uri 'self'",
    "object-src 'none'",
    "form-action 'none'",
].join('; ');

// The shim must define window.__TAURI__ before any app script runs.
const SITE = 'https://web.vectorapp.io';
const TITLE = 'Vector Web - Private Messaging';
const DESCRIPTION = 'Vector, the private and end-to-end encrypted messenger, right in your browser. No install, no phone number: your keys stay on your device.';
const META = [
    `<meta name="description" content="${DESCRIPTION}">`,
    `<link rel="canonical" href="${SITE}/">`,
    '<link rel="icon" href="/favicon.ico" sizes="32x32">',
    '<link rel="icon" type="image/png" sizes="32x32" href="/favicon-32.png">',
    '<link rel="icon" type="image/png" sizes="192x192" href="/icon-192.png">',
    '<link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png">',
    '<link rel="manifest" href="/manifest.webmanifest">',
    '<meta name="apple-mobile-web-app-title" content="Vector">',
    '<meta property="og:type" content="website">',
    '<meta property="og:site_name" content="Vector">',
    '<meta property="og:locale" content="en_US">',
    `<meta property="og:url" content="${SITE}/">`,
    `<meta property="og:title" content="${TITLE}">`,
    `<meta property="og:description" content="${DESCRIPTION}">`,
    `<meta property="og:image" content="${SITE}/og.png">`,
    '<meta property="og:image:type" content="image/png">',
    '<meta property="og:image:width" content="1200">',
    '<meta property="og:image:height" content="630">',
    '<meta property="og:image:alt" content="Vector: private messaging, right in your browser.">',
    '<meta name="twitter:card" content="summary_large_image">',
    '<meta name="twitter:site" content="@VectorPrivacy">',
    `<meta name="twitter:title" content="${TITLE}">`,
    `<meta name="twitter:description" content="${DESCRIPTION}">`,
    `<meta name="twitter:image" content="${SITE}/og.png">`,
].map((m) => `    ${m}\n`).join('');

const indexPath = join(OUT, 'index.html');
const html = readFileSync(indexPath, 'utf8');
writeFileSync(indexPath, html
    .replace('<html', `<html data-version="${VERSION}"`)
    // Link previews (Discord's embed stripe) take the first theme-color; media="print"
    // keeps it off the browser chrome, which falls through to the page's dark one.
    .replace('<meta name="theme-color"', '<meta name="theme-color" content="#59fcb3" media="print">\n    <meta name="theme-color"')
    .replace('<meta charset="UTF-8" />', `<meta charset="UTF-8" />\n    <meta http-equiv="Content-Security-Policy" content="${CSP}">\n    <script src="/web/tauri-shim.js"></script>\n    <script src="/web/chrome.js"></script>\n    <script src="/web/media.js"></script>\n    <script src="/web/signer.js"></script>\n    <script src="/web/miniapps.js"></script>`)
    .replace('</head>', `    <link rel="stylesheet" href="/web/web.css" />\n${META}  </head>`)
    .replace('<body>', '<body>\n    <div class="edge-cap" aria-hidden="true" hidden></div>\n  '));

// Every injection hangs off a marker in index.html; a missed one must not ship a page without its policy.
const built = readFileSync(indexPath, 'utf8');
for (const needed of ['Content-Security-Policy', '/web/tauri-shim.js', '/web/web.css', 'og:image', 'edge-cap']) {
    if (!built.includes(needed)) {
        console.error(`[build-web] index.html is missing ${needed}: an injection marker moved`);
        process.exit(1);
    }
}

console.log(`[build-web] → ${OUT}`);
