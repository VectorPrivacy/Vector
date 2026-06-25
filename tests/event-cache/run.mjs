/**
 * Alternate entrypoint: `node tests/event-cache/run.mjs`.
 * Programmatically drives Node's built-in test runner over this directory so
 * the harness runs without remembering the `--test` flag.
 */
import { run } from 'node:test';
import { spec as Spec } from 'node:test/reporters';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import process from 'node:process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const testFile = resolve(__dirname, 'event-cache.test.mjs');

let failed = false;
run({ files: [testFile] })
    .on('test:fail', () => { failed = true; })
    .compose(new Spec())
    .pipe(process.stdout);

process.on('exit', () => process.exit(failed ? 1 : 0));
