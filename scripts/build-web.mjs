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
for (const f of ['tauri-shim.js', 'media.js', 'worker.js', 'web.css']) cpSync(join(ROOT, 'web', f), join(OUT, 'web', f));
cpSync(join(ROOT, 'web', 'pkg'), join(OUT, 'web', 'pkg'), { recursive: true });
// Root scope, so it can answer `/vfs/…` for the whole page.
cpSync(join(ROOT, 'web', 'sw.js'), join(OUT, 'sw.js'));

// Desktop's policy, minus Tauri's schemes. Remote images are allowed because
// without a media proxy the page loads them directly, as desktop does.
export const CSP = [
    "default-src 'self'",
    "script-src 'self'",
    "img-src 'self' data: blob: https:",
    "media-src 'self' blob: https://gifverse.net",
    "style-src 'self' 'unsafe-inline'",
    "connect-src 'self' https://gifverse.net",
    "worker-src 'self'",
    "base-uri 'self'",
    "object-src 'none'",
    "form-action 'none'",
].join('; ');

// The shim must define window.__TAURI__ before any app script runs.
const indexPath = join(OUT, 'index.html');
const html = readFileSync(indexPath, 'utf8');
writeFileSync(indexPath, html
    .replace('<head>', `<head>\n    <meta http-equiv="Content-Security-Policy" content="${CSP}">\n    <script src="/web/tauri-shim.js"></script>\n    <script src="/web/media.js"></script>`)
    .replace('</head>', '    <link rel="stylesheet" href="/web/web.css" />\n  </head>'));

console.log(`[build-web] → ${OUT}`);
