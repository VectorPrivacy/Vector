// Fetching a model into a file: resumes where a dropped connection left off, and ends only at
// exactly the expected size. The file, the clock and fetch come in, so the tests can stand in.

const MAX_FAILURES = 8;

const failure = (message, retry) => Object.assign(new Error(message), { retry });

function waitOrAbort(ms, signal) {
    return new Promise((resolve, reject) => {
        const timer = setTimeout(resolve, ms);
        signal?.addEventListener('abort', () => { clearTimeout(timer); reject(signal.reason); }, { once: true });
    });
}

/**
 * Downloads `url` into `sink` ({ truncate(n), write(bytes, at) }), `total` bytes long.
 * Network failures and short bodies retry from what is already written; a server that ignores
 * the range restarts the file; a 4xx, a body longer than expected or a failed write ends it.
 */
export async function fetchInto({ url, total, sink, signal, onProgress, fetch = globalThis.fetch, wait = waitOrAbort, now = () => performance.now() }) {
    sink.truncate(0);
    let done = 0;
    let failures = 0;
    let last = -1;
    const started = now();
    for (;;) {
        try {
            const res = await fetch(url, { signal, cache: 'no-store', headers: done ? { Range: `bytes=${done}-` } : {} });
            const ranged = done > 0 && res.status === 206 && (res.headers.get('content-range') || '').startsWith(`bytes ${done}-`);
            if (done > 0 && res.status === 200) {
                done = 0;
                sink.truncate(0);
            } else if (!(done > 0 ? ranged : res.ok)) {
                await res.body?.cancel().catch(() => {});
                throw failure(`HTTP error: ${res.status}`, res.status >= 500 || res.status === 429 || (done > 0 && res.status === 206));
            }
            const reader = res.body.getReader();
            for (;;) {
                const { done: end, value } = await reader.read();
                if (end) break;
                if (done + value.byteLength > total) {
                    await reader.cancel().catch(() => {});
                    throw failure('The model on the server is not the one expected', false);
                }
                sink.write(value, done);
                done += value.byteLength;
                const progress = Math.min(100, Math.floor((done * 100) / total));
                if (progress !== last) {
                    last = progress;
                    const secs = (now() - started) / 1000;
                    onProgress?.({ progress, downloaded_bytes: done, total_bytes: total, speed_bps: secs > 0 ? Math.round(done / secs) : 0 });
                }
            }
            if (done === total) return;
            throw failure('The download ended early', true);
        } catch (e) {
            if (signal?.aborted) throw new Error('Download cancelled');
            const retry = e?.retry ?? e instanceof TypeError;
            if (!retry || ++failures > MAX_FAILURES) throw e;
            try {
                await wait(Math.min(30_000, 1000 * 2 ** failures), signal);
            } catch {
                throw new Error('Download cancelled');
            }
        }
    }
}
