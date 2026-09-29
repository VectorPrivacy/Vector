// Vector Web mini app origin: the first page loaded here. It installs the
// origin's service worker, takes the package from Vector, and hands over to it.
(async () => {
    'use strict';
    // Vector itself lives on the host this origin is a subdomain of.
    const vector = `${location.protocol}//${location.host.replace(/^[a-z0-9-]+\.xdc\./, '')}`;
    if (window.parent === window) return;

    const reg = await navigator.serviceWorker.register('/__vector/sw.js', { scope: '/' });
    await navigator.serviceWorker.ready;

    addEventListener('message', async (e) => {
        if (e.source !== window.parent || e.origin !== vector || e.data?.t !== 'xdc-load') return;
        const { bytes, meta, href } = e.data;
        const cache = await caches.open('vector-xdc');
        await cache.put('/__vector/package', new Response(new Blob([bytes])));
        await cache.put('/__vector/meta', new Response(JSON.stringify({ ...meta, parent: vector })));
        const sw = reg.active || navigator.serviceWorker.controller;
        if (sw) {
            await new Promise((resolve) => {
                const ch = new MessageChannel();
                ch.port1.onmessage = resolve;
                sw.postMessage({ t: 'reset' }, [ch.port2]);
                setTimeout(resolve, 1000);
            });
        }
        location.replace(typeof href === 'string' && href.startsWith('/') ? href : '/index.html');
    });
    window.parent.postMessage({ t: 'xdc-host-ready' }, vector);
})().catch((e) => {
    document.body.textContent = `This mini app could not start: ${e?.message || e}`;
    document.body.style.cssText = 'color:#ccc;font:14px system-ui;padding:24px';
});
