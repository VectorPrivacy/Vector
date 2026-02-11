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

import { cpSync, rmSync, readdirSync, readFileSync, writeFileSync, statSync } from 'fs';
import { join, dirname, extname, basename } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
const SRC = join(ROOT, 'src');
const DIST = join(ROOT, 'dist');

const isDev = process.argv.includes('--dev');

// ── Step 1: Copy src → dist ─────────────────────────────────────────────

console.log(`[build-frontend] ${isDev ? 'DEV' : 'RELEASE'} build`);

// Clean and copy
rmSync(DIST, { recursive: true, force: true });
cpSync(SRC, DIST, { recursive: true });
console.log(`  Copied src/ → dist/`);

if (isDev) {
    console.log('  Dev mode — skipping minification');
    process.exit(0);
}

// ── Step 2: Minify JS with terser ────────────────────────────────────────

const { minify } = await import('terser');

async function minifyJs(filePath) {
    const code = readFileSync(filePath, 'utf-8');
    const result = await minify(code, {
        compress: {
            dead_code: true,
            drop_console: false, // keep console.log for debugging in production
            passes: 2,
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
