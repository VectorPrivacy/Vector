#!/usr/bin/env node
/**
 * tauri.mjs — `npm run tauri` resolves to whichever Tauri CLI is present.
 *
 * The npm package on a dev machine; `cargo tauri` where prebuilt binaries are
 * off the table (F-Droid builds from source only). Gradle re-enters the CLI
 * through `npm run -- tauri android android-studio-script`, so this is the one
 * place the choice is made. TAURI_CLI="cargo tauri" forces it.
 */
import { spawnSync } from 'child_process';
import { existsSync } from 'fs';
import { createRequire } from 'module';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const args = process.argv.slice(2);
enableVideo(args);
let command;
let commandArgs;

if (process.env.TAURI_CLI) {
    const [cmd, ...rest] = process.env.TAURI_CLI.split(' ').filter(Boolean);
    command = cmd;
    commandArgs = [...rest, ...args];
} else {
    try {
        const entry = createRequire(import.meta.url).resolve('@tauri-apps/cli/tauri.js');
        command = process.execPath;
        commandArgs = [entry, ...args];
    } catch {
        command = 'cargo';
        commandArgs = ['tauri', ...args];
    }
}

const result = spawnSync(command, commandArgs, { stdio: 'inherit' });
if (result.error) {
    console.error(`[tauri] could not run ${command}: ${result.error.message}`);
    process.exit(1);
}
process.exit(result.status ?? 1);

/**
 * A desktop dev/build turns on video compression when scripts/build-ffmpeg.sh has built FFmpeg
 * for its target (or FFMPEG_DIR names one). VECTOR_VIDEO=0 opts out.
 */
function enableVideo(args) {
    if (!['dev', 'build'].includes(args[0]) || process.env.VECTOR_VIDEO === '0') return;
    const sep = args.indexOf('--');
    const ours = sep < 0 ? args : args.slice(0, sep);
    const t = ours.findIndex(a => a === '--target' || a === '-t');
    const target = t >= 0 ? ours[t + 1] : hostTarget();
    if (!process.env.FFMPEG_DIR) {
        const root = join(dirname(fileURLToPath(import.meta.url)), '..');
        const dir = join(root, 'src-tauri', 'native-deps', 'ffmpeg', target || '');
        if (!target || !['libavcodec.a', 'avcodec.lib'].some(f => existsSync(join(dir, 'lib', f)))) return;
        process.env.FFMPEG_DIR = dir;
    }
    // bindgen needs the macOS SDK named when another clang (the NDK's) is first on PATH.
    if (target?.endsWith('-apple-darwin') && !process.env.SDKROOT) {
        const sdk = spawnSync('xcrun', ['--sdk', 'macosx', '--show-sdk-path'], { encoding: 'utf8' });
        if (sdk.status === 0) process.env.SDKROOT = sdk.stdout.trim();
    }
    console.log(`[tauri] video compression on: FFmpeg at ${process.env.FFMPEG_DIR}`);
    args.splice(sep < 0 ? args.length : sep, 0, '--features', 'video');
}

function hostTarget() {
    const r = spawnSync('rustc', ['-vV'], { encoding: 'utf8' });
    return r.status === 0 ? r.stdout.match(/^host: (\S+)/m)?.[1] : undefined;
}
