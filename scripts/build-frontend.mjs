#!/usr/bin/env node
/**
 * build-frontend.mjs — Minify frontend assets for release builds
 *
 * Copies src/ → dist/ and minifies JS (terser) + CSS (lightningcss) in-place.
 * Skips files already minified (*.min.js, *.min.css).
 * HTML is minified by collapsing whitespace and removing comments.
 *
 * Usage:
 *   node scripts/build-frontend.mjs          # full minified build
 *   node scripts/build-frontend.mjs --dev    # plain copy, no minification
 */

import { cpSync, rmSync, readdirSync, readFileSync, writeFileSync, statSync, symlinkSync, lstatSync, unlinkSync, readlinkSync } from 'fs';
import { join, dirname, extname, basename } from 'path';
import { fileURLToPath } from 'url';
import { buildSvelte } from './build-svelte.mjs';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
const SRC = join(ROOT, 'src');
const DIST = join(ROOT, 'dist');

const isDev = process.argv.includes('--dev');
const isCopy = process.argv.includes('--copy');

console.log(`[build-frontend] ${isDev ? 'DEV' : isCopy ? 'COPY' : 'RELEASE'} build`);

// ── Step 0: compile Svelte islands → src/components.bundle.js ─────────────
// Runs first in every mode so the bundle exists in src/ before the symlink/copy.
// Release compiles Svelte in production mode (strips dev validation / a11y warnings / HMR +
// FILENAME machinery); desktop-dev + android-dev keep the dev bundle. (Active .svelte dev uses
// `npm run svelte:watch` for incremental rebuilds.)
await buildSvelte({ dev: isDev || isCopy });

// Remove dist safely — if it's a symlink, just unlink it (never follow into src/)
function cleanDist() {
    try {
        const st = lstatSync(DIST);
        if (st.isSymbolicLink()) {
            unlinkSync(DIST);
        } else {
            rmSync(DIST, { recursive: true });
        }
    } catch {}
}

// True when dist is already the dev symlink pointing at src.
function distIsDevLink() {
    try {
        return lstatSync(DIST).isSymbolicLink() && readlinkSync(DIST) === SRC;
    } catch {
        return false;
    }
}

if (isDev) {
    // Symlink dist → src so Tauri watches src/ changes directly.
    // Windows: 'junction' — real symlinks need Developer Mode or elevation; junctions don't.
    //
    // Idempotent on purpose: `tauri dev`'s static server resolves frontendDist ONCE
    // at startup, and replacing the inode under it makes it answer index.html to
    // every asset request — a 200 the webview then fails to parse ("Unexpected
    // token '<'"), leaving a running app whose window (born hidden) is never shown.
    //
    // Which is why `npm run dev` runs this BEFORE `tauri dev`, not only as its
    // beforeDevCommand: after a release build dist/ is a real directory, and by the
    // time the beforeDevCommand replaces it the path is already resolved. Relinking
    // first means the launch never swaps the inode at all, and this run is the no-op
    // below. Invoking `tauri dev` directly still costs the one bad launch.
    if (distIsDevLink()) {
        console.log('  dist/ → src/ already linked (left alone; a live dev server holds this inode)');
        process.exit(0);
    }
    cleanDist();
    symlinkSync(SRC, DIST, process.platform === 'win32' ? 'junction' : 'dir');
    console.log('  Symlinked dist/ → src/ (hot-reload enabled)');
    process.exit(0);
}

// Replacing the dev symlink with a real dist/ is what a release or android build
// does, and it strands any `tauri dev` still running against it — the reason a
// build-type switch is followed by a launch that compiles, boots and shows no GUI.
const replacedDevLink = distIsDevLink();

if (isCopy) {
    // Plain copy without minification (for Android dev builds where symlinks don't work)
    cleanDist();
    cpSync(SRC, DIST, { recursive: true });
    console.log('  Copied src/ → dist/ (no minification)');
    process.exit(0);
}

// ── Step 1: Copy src → dist ─────────────────────────────────────────────

// Clean and copy
cleanDist();
cpSync(SRC, DIST, { recursive: true });
console.log(`  Copied src/ → dist/`);

// ── Step 2: Minify JS with terser ────────────────────────────────────────

const { minify } = await import('terser');

async function minifyJs(filePath) {
    const code = readFileSync(filePath, 'utf-8');
    const result = await minify(code, {
        compress: {
            dead_code: true,
            drop_console: false, // keep console.log for debugging in production
            passes: 3,
            pure_getters: true,
        },
        mangle: true,
        format: {
            comments: false,
        },
    });
    if (result.code) {
        writeFileSync(filePath, result.code);
        return { before: code.length, after: result.code.length };
    }
    return null;
}

// ── Step 3: Minify CSS with lightningcss ─────────────────────────────────

const { transform } = await import('lightningcss');

function minifyCss(filePath) {
    const code = readFileSync(filePath);
    const result = transform({
        filename: filePath,
        code,
        minify: true,
    });
    writeFileSync(filePath, result.code);
    return { before: code.length, after: result.code.length };
}

// ── Step 4: Minify HTML ──────────────────────────────────────────────────

function minifyHtml(filePath) {
    const html = readFileSync(filePath, 'utf-8');
    const minified = html
        // Remove HTML comments (but not IE conditionals)
        .replace(/<!--(?!\[if)[\s\S]*?-->/g, '')
        // Collapse whitespace between tags
        .replace(/>\s+</g, '><')
        // Trim leading/trailing whitespace per line
        .replace(/^\s+/gm, '')
        // Collapse multiple newlines
        .replace(/\n{2,}/g, '\n')
        .trim();
    writeFileSync(filePath, minified);
    return { before: html.length, after: minified.length };
}

// ── Step 5: Walk dist/ and minify ────────────────────────────────────────

function walk(dir) {
    const entries = [];
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const fullPath = join(dir, entry.name);
        if (entry.isDirectory()) {
            entries.push(...walk(fullPath));
        } else {
            entries.push(fullPath);
        }
    }
    return entries;
}

console.log('  Minifying...');

let totalBefore = 0;
let totalAfter = 0;

const files = walk(DIST);
const jsFiles = files.filter(f => extname(f) === '.js' && !basename(f).endsWith('.min.js'));
const cssFiles = files.filter(f => extname(f) === '.css' && !basename(f).endsWith('.min.css'));
const htmlFiles = files.filter(f => extname(f) === '.html');

// Minify JS (async)
for (const file of jsFiles) {
    const rel = file.replace(DIST + '/', '');
    const result = await minifyJs(file);
    if (result) {
        totalBefore += result.before;
        totalAfter += result.after;
        const pct = ((1 - result.after / result.before) * 100).toFixed(1);
        console.log(`    JS  ${rel}: ${(result.before / 1024).toFixed(1)}K → ${(result.after / 1024).toFixed(1)}K (${pct}%)`);
    }
}

// Minify CSS (sync)
for (const file of cssFiles) {
    const rel = file.replace(DIST + '/', '');
    const result = minifyCss(file);
    totalBefore += result.before;
    totalAfter += result.after;
    const pct = ((1 - result.after / result.before) * 100).toFixed(1);
    console.log(`    CSS ${rel}: ${(result.before / 1024).toFixed(1)}K → ${(result.after / 1024).toFixed(1)}K (${pct}%)`);
}

// Minify HTML (sync)
for (const file of htmlFiles) {
    const rel = file.replace(DIST + '/', '');
    const result = minifyHtml(file);
    totalBefore += result.before;
    totalAfter += result.after;
    const pct = ((1 - result.after / result.before) * 100).toFixed(1);
    console.log(`    HTML ${rel}: ${(result.before / 1024).toFixed(1)}K → ${(result.after / 1024).toFixed(1)}K (${pct}%)`);
}

const totalPct = ((1 - totalAfter / totalBefore) * 100).toFixed(1);
console.log(`\n  Total: ${(totalBefore / 1024).toFixed(1)}K → ${(totalAfter / 1024).toFixed(1)}K (${totalPct}% reduction)`);
console.log('  Done!');

if (replacedDevLink) {
    console.log('\n  NOTE: this replaced the dev symlink with a real dist/.');
    console.log('  A `tauri dev` still running resolved the old one and will now answer');
    console.log('  index.html to every asset — its window loads nothing. Restart it.');
}
