#!/usr/bin/env node
// The updater manifest (latest.json) for a release, built once from its uploaded bundles and their
// .sig files. One writer: matrix jobs that each edited the shared asset raced and dropped platforms.
//
//   node scripts/updater-manifest.mjs <assets-dir> <tag> <owner/repo> > latest.json
//
// URLs name the tag: `releases/latest/download` resolves to the newest stable release, so a
// preview's own files would 404 there.
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const [dir, tag, repo] = process.argv.slice(2);
if (!dir || !tag || !repo) {
    console.error('usage: updater-manifest.mjs <assets-dir> <tag> <owner/repo>');
    process.exit(2);
}

const ARCH = { x64: 'x86_64', amd64: 'x86_64', x86_64: 'x86_64', aarch64: 'aarch64', arm64: 'aarch64' };

// Bundle name -> the updater targets it serves. `<os>-<arch>` alone is what older clients read.
const RULES = [
    [/^Vector_(aarch64|x64)\.app\.tar\.gz$/, (a) => [`darwin-${a}`, `darwin-${a}-app`]],
    [/_(amd64|aarch64)\.AppImage$/, (a) => [`linux-${a}`, `linux-${a}-appimage`]],
    [/_(amd64|arm64)\.deb$/, (a) => [`linux-${a}-deb`]],
    [/\.(x86_64|aarch64)\.rpm$/, (a) => [`linux-${a}-rpm`]],
    [/_(x64|arm64)_en-US\.msi$/, (a) => [`windows-${a}`, `windows-${a}-msi`]],
    [/_(x64|arm64)-setup\.exe$/, (a) => [`windows-${a}-nsis`]],
];

// Every desktop build the matrix ships: a release missing one must not go out quietly.
const EXPECTED = [
    'darwin-aarch64', 'darwin-aarch64-app', 'darwin-x86_64', 'darwin-x86_64-app',
    'linux-aarch64', 'linux-aarch64-appimage', 'linux-aarch64-deb', 'linux-aarch64-rpm',
    'linux-x86_64', 'linux-x86_64-appimage', 'linux-x86_64-deb', 'linux-x86_64-rpm',
    'windows-aarch64', 'windows-aarch64-msi', 'windows-aarch64-nsis',
    'windows-x86_64', 'windows-x86_64-msi', 'windows-x86_64-nsis',
];

const files = new Set(readdirSync(dir));
const platforms = {};
const problems = [];
for (const name of [...files].sort()) {
    for (const [pattern, targets] of RULES) {
        const m = pattern.exec(name);
        if (!m) continue;
        if (!files.has(`${name}.sig`)) {
            problems.push(`${name} has no .sig`);
            break;
        }
        const signature = readFileSync(join(dir, `${name}.sig`), 'utf8').trim();
        const url = `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(name)}`;
        for (const target of targets(ARCH[m[1]])) {
            if (platforms[target]) problems.push(`${target} is served by both ${platforms[target].url} and ${name}`);
            platforms[target] = { signature, url };
        }
        break;
    }
}
for (const target of EXPECTED) if (!platforms[target]) problems.push(`no bundle for ${target}`);
if (problems.length) {
    console.error(`latest.json for ${tag} is incomplete:\n  ${problems.join('\n  ')}`);
    process.exit(1);
}

const sorted = Object.fromEntries(Object.keys(platforms).sort().map((k) => [k, platforms[k]]));
const version = tag.replace(/^(rehearsal-)?v/, '');
process.stdout.write(JSON.stringify({ version, notes: '', pub_date: new Date().toISOString(), platforms: sorted }, null, 2) + '\n');
