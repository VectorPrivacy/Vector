#!/usr/bin/env node
/**
 * build-svelte.mjs — Compile src/components/*.svelte into one global bundle.
 *
 * Output: src/components.bundle.js — an IIFE exposing `window.VectorSvelte`, loaded as a
 * plain <script> in index.html to fit the frontend's one-global-scope model, then
 * symlinked (dev) / terser-minified (release) like every other src/ file.
 *
 * Usage:
 *   node scripts/build-svelte.mjs           # one-shot compile
 *   node scripts/build-svelte.mjs --watch   # recompile on .svelte change (dev)
 *
 * build-frontend.mjs also imports buildSvelte() and runs it as step 0.
 */
// esbuild is a native binary. Where prebuilt binaries are not allowed (the
// F-Droid recipe removes the package) the same entry is bundled with rollup,
// which is pure JS. Same IIFE, same global; only the bundler differs.
async function loadEsbuild() {
    try {
        return (await import('esbuild')).default;
    } catch {
        return null;
    }
}
const esbuild = await loadEsbuild();

async function buildWithRollup(options, dev) {
    const { rollup } = await import('rollup');
    const svelte = (await import('rollup-plugin-svelte')).default;
    const { nodeResolve } = await import('@rollup/plugin-node-resolve');
    const bundle = await rollup({
        input: options.entryPoints[0],
        plugins: [
            svelte({ compilerOptions: { css: 'injected', dev }, emitCss: false }),
            nodeResolve({
                browser: true,
                exportConditions: options.conditions,
                mainFields: options.mainFields,
            }),
        ],
    });
    await bundle.write({ file: options.outfile, format: 'iife', name: options.globalName });
    await bundle.close();
}
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');

// dev=true keeps Svelte's dev machinery (runtime prop/binding validation, a11y warnings,
// HMR + FILENAME hooks, dev warnings). dev=false compiles a lean production bundle with all of
// that stripped. build-frontend passes dev:false for release. Minification stays terser's job.
export async function buildSvelte({ watch = false, dev = true } = {}) {
    const options = {
        entryPoints: [join(ROOT, 'src/components/index.js')],
        outfile: join(ROOT, 'src/components.bundle.js'),
        bundle: true,
        format: 'iife',
        globalName: 'VectorSvelte',
        // Svelte package resolution (per esbuild-svelte docs). The dev/production condition drives
        // esm-env's DEV flag (svelte/internal/client dev machinery), so the runtime's `if (DEV)`
        // branches — dev_fallback, HMR/FILENAME hooks, validation, a11y warnings — tree-shake out
        // of release. `browser` stays first in esm-env's export map, so BROWSER remains true.
        mainFields: ['svelte', 'browser', 'module', 'main'],
        conditions: [dev ? 'development' : 'production', 'svelte', 'browser'],
        logLevel: 'warning',
        plugins: [],
    };
    if (!esbuild) {
        if (watch) throw new Error('[build-svelte] watch mode needs esbuild');
        await buildWithRollup(options, dev);
        console.log(`[build-svelte] → src/components.bundle.js via rollup (${dev ? 'dev' : 'production'})`);
        return;
    }
    const esbuildSvelte = (await import('esbuild-svelte')).default;
    options.plugins = [esbuildSvelte({ compilerOptions: { css: 'injected', dev } })];
    if (watch) {
        const ctx = await esbuild.context(options);
        await ctx.watch();
        console.log('[build-svelte] watching src/components/ …');
    } else {
        await esbuild.build(options);
        console.log(`[build-svelte] → src/components.bundle.js (${dev ? 'dev' : 'production'})`);
    }
}

// Standalone invocation (npm run svelte:build / svelte:watch).
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
    buildSvelte({ watch: process.argv.includes('--watch'), dev: !process.argv.includes('--production') }).catch((e) => {
        console.error(e);
        process.exit(1);
    });
}
