// Vector Web mini app origin: the first page loaded here. It installs the
// origin's service worker, takes the package from Vector, and hands over to it.
(async () => {
    'use strict';
    // Vector itself lives on the host this origin is a subdomain of.
    const vector = `${location.protocol}//${location.host.replace(/^[a-z0-9-]+\.xdc\./, '')}`;
    if (window.parent === window) return;

    const reg = await navigator.serviceWorker.register('/__vector/sw.js', { scope: '/' });
    // A newer worker than the running one must take over before it's handed the package.
    await reg.update().catch(() => {});
    const pending = reg.installing || reg.waiting;
    if (pending) {
        await new Promise((resolve) => {
            if (pending.state === 'activated' || pending.state === 'redundant') return resolve();
            pending.addEventListener('statechange', () => {
                if (pending.state === 'activated' || pending.state === 'redundant') resolve();
            });
        });
    }
    await navigator.serviceWorker.ready;

    const fail = (why) => {
        document.body.textContent = `This mini app could not start: ${why}`;
        document.body.style.cssText = 'color:#ccc;font:14px system-ui;padding:24px';
    };

    addEventListener('message', async (e) => {
        if (e.source !== window.parent || e.origin !== vector || e.data?.t !== 'xdc-load') return;
        const { bytes, meta, href } = e.data;
        const sw = reg.active || navigator.serviceWorker.controller;
        if (!sw) return fail('service worker unavailable');
        // The worker stores and parses the package itself, so its storage is the one it reads.
        const result = await new Promise((resolve) => {
            const ch = new MessageChannel();
            ch.port1.onmessage = ({ data }) => resolve(data);
            sw.postMessage({ t: 'store', bytes, meta: { ...meta, parent: vector } }, [ch.port2, bytes]);
            setTimeout(() => resolve({ ok: false, error: 'service worker did not answer' }), 30000);
        });
        if (!result?.ok) return fail(result?.error || 'unknown error');
        location.replace(typeof href === 'string' && href.startsWith('/') ? href : '/index.html');
    });
    window.parent.postMessage({ t: 'xdc-host-ready' }, vector);
})().catch((e) => {
    document.body.textContent = `This mini app could not start: host: ${e?.name || 'Error'}: ${e?.message || e}`;
    document.body.style.cssText = 'color:#ccc;font:14px system-ui;padding:24px';
});
