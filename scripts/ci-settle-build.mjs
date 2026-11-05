#!/usr/bin/env node
// CI's beforeBundleCommand (src-tauri/tauri.ci.conf.json): lets a retried `tauri build` reuse the
// compiled app instead of re-linking it.
//
// generate_context! writes its compressed assets into OUT_DIR while the crate compiles, so they
// postdate cargo's dep-info and the next build always recompiles once. They are named by content
// hash, so an old mtime can never hide a change. Never fails the build.
import { readdirSync, utimesSync } from 'node:fs';
import { join } from 'node:path';

const OLD = new Date('2000-01-01T00:00:00Z');
const root = process.env.CARGO_TARGET_DIR || join(import.meta.dirname, '..', 'src-tauri', 'target');

const dirs = (p) => {
    try {
        return readdirSync(p, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => join(p, e.name));
    } catch {
        return [];
    }
};

let settled = 0;
try {
    // <target>/release or <target>/<triple>/release
    const profiles = [join(root, 'release'), ...dirs(root).map((d) => join(d, 'release'))];
    for (const build of profiles.map((p) => join(p, 'build'))) {
        for (const unit of dirs(build).filter((d) => /[\\/]vector-[0-9a-f]+$/.test(d))) {
            const assets = join(unit, 'out', 'tauri-codegen-assets');
            for (const file of (() => { try { return readdirSync(assets); } catch { return []; } })()) {
                utimesSync(join(assets, file), OLD, OLD);
                settled++;
            }
        }
    }
} catch (e) {
    console.error(`[ci-settle-build] skipped: ${e.message}`);
}
console.log(`[ci-settle-build] ${settled} codegen assets settled under ${root}`);
