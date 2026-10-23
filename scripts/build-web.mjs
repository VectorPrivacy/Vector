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

import { cpSync, rmSync, readFileSync, writeFileSync, existsSync, readdirSync, statSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import { execFileSync } from 'child_process';
import { homedir } from 'os';
import { createHash } from 'crypto';
import { buildSvelte } from './build-svelte.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const OUT = join(ROOT, 'dist-web');
const release = process.argv.includes('--release');

if (!process.argv.includes('--no-wasm')) {
    // A release links the whole program at once (fat LTO, one codegen unit): ~7% smaller and
    // optimised across crate boundaries, for a slower build.
    const lto = release ? { CARGO_PROFILE_RELEASE_LTO: 'fat', CARGO_PROFILE_RELEASE_CODEGEN_UNITS: '1' } : {};
    execFileSync('wasm-pack', [
        'build', '--target', 'web', release ? '--release' : '--dev',
        '--no-typescript', '--no-pack', '--out-dir', join(ROOT, 'web', 'pkg'),
    ], { cwd: join(ROOT, 'crates', 'vector-web'), stdio: 'inherit', env: { ...process.env, ...lto } });
    if (release) stripWasmNames(join(ROOT, 'web', 'pkg', 'vector_web_bg.wasm'));
}

/** Drop the function-name section (~10% of the module): nothing at runtime reads it, only
 *  stack traces do. No optimisation passes: `-O3` inlined enough to compress worse. */
function stripWasmNames(wasm) {
    const cached = join(homedir(), 'Library', 'Caches', '.wasm-pack');
    const candidates = ['wasm-opt', ...(existsSync(cached) ? readdirSync(cached).filter((d) => d.startsWith('wasm-opt-')).map((d) => join(cached, d, 'bin', 'wasm-opt')) : [])];
    for (const tool of candidates) {
        try {
            execFileSync(tool, [wasm, '--all-features', '--strip-debug', '--strip-producers', '-o', wasm], { stdio: 'inherit' });
            return;
        } catch (e) {
            if (e.code !== 'ENOENT') throw e;
        }
    }
    console.warn('[build-web] wasm-opt not found: the wasm keeps its name section');
}
if (!existsSync(join(ROOT, 'web', 'pkg', 'vector_web_bg.wasm'))) {
    console.error('[build-web] no wasm build in web/pkg — run without --no-wasm first');
    process.exit(1);
}

await buildSvelte({ dev: !release });

rmSync(OUT, { recursive: true, force: true });
cpSync(join(ROOT, 'src'), OUT, { recursive: true, dereference: true });
for (const f of ['preflight.js', 'tauri-shim.js', 'chrome.js', 'media.js', 'push.js', 'whisper.js', 'signer.js', 'miniapps.js', 'calls.js', 'calls-media.js', 'calls-worklet.js', 'worker.js', 'storage.js', 'web.css']) cpSync(join(ROOT, 'web', f), join(OUT, 'web', f));
// Whisper's worker and its two builds (scripts/whisper-web/build.sh); the models are served apart.
cpSync(join(ROOT, 'web', 'whisper'), join(OUT, 'web', 'whisper'), { recursive: true });
cpSync(join(ROOT, 'src-tauri', 'src', 'calls', 'ended.wav'), join(OUT, 'web', 'ended.wav'));
// Icons and the link-preview card, at the root where browsers and crawlers look.
cpSync(join(ROOT, 'web', 'meta'), OUT, { recursive: true });
// The app's own version, for Settings and the database's downgrade record.
const VERSION = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8')).version;
const workerPath = join(OUT, 'web', 'worker.js');
writeFileSync(workerPath, readFileSync(workerPath, 'utf8').replace("const VERSION = 'web';", `const VERSION = ${JSON.stringify(VERSION)};`));
cpSync(join(ROOT, 'web', 'pkg'), join(OUT, 'web', 'pkg'), { recursive: true });
// The probe vectorapp.io/go frames to learn whether this browser uses Vector Web.
for (const f of ['go-probe.html', 'go-probe.js']) cpSync(join(ROOT, 'web', f), join(OUT, f));
// Root scope, so it can answer `/vfs/…` for the whole page.
cpSync(join(ROOT, 'web', 'sw.js'), join(OUT, 'sw.js'));
// Mini app origins are served from here; see serve.mjs.
cpSync(join(ROOT, 'web', 'xdc'), join(OUT, '__vector'), { recursive: true });

// What the page warms once signed in, so a screen opened for the first time has
// its icons already: the UI's own images and fonts. The login art is on screen
// before this list is read.
const warm = [];
const collect = (dir, url) => {
    for (const name of readdirSync(dir)) {
        const path = join(dir, name);
        if (statSync(path).isDirectory()) {
            if (name !== 'login') collect(path, `${url}${name}/`);
        } else if (/\.(svg|png|gif|webp|woff2)$/i.test(name)) {
            warm.push(url + encodeURIComponent(name));
        }
    }
};
collect(join(OUT, 'icons'), '/icons/');
collect(join(OUT, 'fonts'), '/fonts/');
writeFileSync(join(OUT, 'web', 'warm.json'), JSON.stringify(warm));
// The service worker caches everything under these by a name that changes only when a file does.
{
    const hash = createHash('sha256');
    const digest = (dir) => {
        for (const name of readdirSync(dir).sort()) {
            const path = join(dir, name);
            if (statSync(path).isDirectory()) digest(path);
            else hash.update(path.slice(OUT.length)).update(readFileSync(path));
        }
    };
    digest(join(OUT, 'icons'));
    digest(join(OUT, 'fonts'));
    const swPath = join(OUT, 'sw.js');
    const sw = readFileSync(swPath, 'utf8');
    const named = sw.replace("const ASSETS = 'vector-assets-dev';", `const ASSETS = 'vector-assets-${hash.digest('hex').slice(0, 12)}';`);
    if (named === sw) throw new Error('[build-web] sw.js lost its ASSETS marker');
    writeFileSync(swPath, named);
}

// Desktop's policy, minus Tauri's schemes. Nothing remote: every remote file reaches the page
// as a local copy the worker fetched and checked. Frames are the exception: Mini Apps, and a
// video player a click starts (the server's header narrows them).
export const CSP = [
    "default-src 'self'",
    "script-src 'self'",
    "img-src 'self' data: blob:",
    "media-src 'self' blob:",
    "style-src 'self' 'unsafe-inline'",
    // A model's embedded textures and buffers are fetched as blob: and data: URLs.
    "connect-src 'self' blob: data:",
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

// With JavaScript off (Tor Browser's Safest level) no script can explain, so the page itself does.
const NOSCRIPT = '<noscript><div style="position:fixed;inset:0;z-index:2147483647;display:flex;align-items:center;justify-content:center;padding:28px 22px;box-sizing:border-box;background:#030303;text-align:center;font-family:Rubik,system-ui,sans-serif">'
    + '<div style="max-width:400px"><img src="/icons/vector_warning.svg" alt="" style="width:76px;height:76px;margin-bottom:14px">'
    + '<h2 style="margin:0 0 8px;color:#fff;font-size:21px;font-weight:600">Turn on JavaScript for Vector</h2>'
    + '<p style="margin:0;color:#9a9a9a;font-size:15px;line-height:1.5">Vector runs in your browser on JavaScript. In Tor Browser, open the shield menu and set the Security Level to Standard.</p>'
    + '</div></div></noscript>';

const indexPath = join(OUT, 'index.html');
const html = readFileSync(indexPath, 'utf8');
writeFileSync(indexPath, html
    .replace('<html', `<html data-version="${VERSION}"`)
    // Link previews (Discord's embed stripe) take the first theme-color; media="print"
    // keeps it off the browser chrome, which falls through to the page's dark one.
    .replace('<meta name="theme-color"', '<meta name="theme-color" content="#59fcb3" media="print">\n    <meta name="theme-color"')
    .replace('<meta charset="UTF-8" />', `<meta charset="UTF-8" />\n    <meta http-equiv="Content-Security-Policy" content="${CSP}">\n    <script src="/web/preflight.js"></script>\n    <script src="/web/tauri-shim.js"></script>\n    <script src="/web/chrome.js"></script>\n    <script src="/web/media.js"></script>\n    <script src="/web/push.js"></script>\n    <script src="/web/whisper.js"></script>\n    <script src="/web/calls.js"></script>\n    <script src="/web/signer.js"></script>\n    <script src="/web/miniapps.js"></script>`)
    .replace('</head>', `    <link rel="stylesheet" href="/web/web.css" />\n${META}  </head>`)
    .replace('<body>', `<body>\n    <div class="edge-cap" aria-hidden="true" hidden></div>\n    ${NOSCRIPT}\n  `));

// Every injection hangs off a marker in index.html; a missed one must not ship a page without its policy.
const built = readFileSync(indexPath, 'utf8');
for (const needed of ['Content-Security-Policy', '/web/preflight.js', '<noscript>', '/web/tauri-shim.js', '/web/web.css', 'og:image', 'edge-cap']) {
    if (!built.includes(needed)) {
        console.error(`[build-web] index.html is missing ${needed}: an injection marker moved`);
        process.exit(1);
    }
}

// Release: the page's scripts as a few bundles, joined in document order so they run exactly as
// before. The onion is plain HTTP/1.1, where every file is another Tor round trip.
if (release) await bundleScripts();

async function bundleScripts() {
    const { minify } = await import('terser');
    // build-frontend's options: a bare property read (`store.seq;`) is a Svelte subscription.
    const options = () => ({ compress: { dead_code: true, drop_console: true, passes: 3 }, mangle: true, format: { comments: false } });
    const probe = await minify('function f(o) { o.seq; return 1; }', options());
    if (!/\.seq/.test(probe.code)) throw new Error('[build-web] terser options drop bare property reads');

    let page = readFileSync(indexPath, 'utf8');
    // A directive counts only at the top of a script: inside a bundle it is lost, or leaks
    // onto every file after it, so such a file keeps its own tag and splits the run.
    const strict = (src) => /^\s*(\/\*[\s\S]*?\*\/\s*|\/\/[^\n]*\n\s*)*['"]use strict['"]/.test(readFileSync(join(OUT, src), 'utf8'));
    const groups = [];
    const runs = { true: null, false: null };
    for (const [tag, src, defer] of page.matchAll(/ *<script src="([^"]+)"( defer)?><\/script>\n?/g)) {
        const kind = !!defer;
        if (strict(src)) { runs[kind] = null; continue; }
        if (!runs[kind]) groups.push(runs[kind] = { defer: kind, srcs: [], tags: [] });
        runs[kind].srcs.push(src);
        runs[kind].tags.push(tag);
    }
    let n = 0;
    for (const g of groups.filter((g) => g.srcs.length > 1)) {
        const joined = g.srcs.map((src) => readFileSync(join(OUT, src), 'utf8')).join('\n;\n');
        const { code } = await minify(joined, options());
        const name = `/web/bundle-${n++}.js`;
        writeFileSync(join(OUT, name), code);
        page = page.replace(g.tags[0], `    <script src="${name}"${g.defer ? ' defer' : ''}></script>\n`);
        for (const tag of g.tags.slice(1)) page = page.replace(tag, '');
        console.log(`[build-web] ${name}: ${g.srcs.length} scripts${g.defer ? ' (deferred)' : ''}, ${(joined.length / 1024).toFixed(0)} KB → ${(code.length / 1024).toFixed(0)} KB`);
    }
    writeFileSync(indexPath, page);
}

console.log(`[build-web] → ${OUT}`);
