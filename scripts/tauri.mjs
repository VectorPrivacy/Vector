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
import { createRequire } from 'module';

const args = process.argv.slice(2);
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
