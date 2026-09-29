// Vector Web backend: vector-core as WebAssembly, one dedicated worker per tab.
// OPFS's synchronous access handles, which the SQLite VFS needs, exist only here.
import init, { start, invoke, set_event_sink } from './pkg/vector_web.js';

const VERSION = 'web';

const booted = (async () => {
    await init();
    set_event_sink((name, json) => postMessage({ t: 'event', name, json }));
    await start(VERSION);
})();

booted.then(
    () => postMessage({ t: 'ready' }),
    (e) => postMessage({ t: 'fatal', error: String(e) }),
);

onmessage = async ({ data }) => {
    if (data.t !== 'invoke') return;
    try {
        await booted;
        const value = await invoke(data.cmd, data.args);
        postMessage({ t: 'result', id: data.id, ok: true, value });
    } catch (e) {
        postMessage({ t: 'result', id: data.id, ok: false, error: typeof e === 'string' ? e : String(e?.message ?? e) });
    }
};
