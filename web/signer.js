// Vector Web: the page half of the NIP-07 signer. Browser extensions inject
// `window.nostr` into pages only, so the backend asks here and the answer goes back.
(() => {
    'use strict';

    const { listen } = window.__TAURI__.event;
    const { backend, register } = window.__vectorWeb;

    // Long enough for a person to read and approve an extension prompt.
    const TIMEOUT_MS = 120_000;

    const METHODS = {
        getPublicKey: (n) => n.getPublicKey(),
        signEvent: (n, [event]) => n.signEvent(event),
        'nip44.encrypt': (n, [pk, text]) => n.nip44.encrypt(pk, text),
        'nip44.decrypt': (n, [pk, text]) => n.nip44.decrypt(pk, text),
        'nip04.encrypt': (n, [pk, text]) => n.nip04.encrypt(pk, text),
        'nip04.decrypt': (n, [pk, text]) => n.nip04.decrypt(pk, text),
    };

    // Gift-wrapped DMs need NIP-44, so an extension without it can't sign in.
    const usable = () => typeof window.nostr?.signEvent === 'function' && typeof window.nostr?.nip44?.decrypt === 'function';

    // Extensions inject after load, sometimes well after.
    async function available() {
        for (let i = 0; i < 20 && !usable(); i++) await new Promise((r) => setTimeout(r, 100));
        return usable();
    }

    async function answer({ id, method, params }) {
        const reply = (ok, value, error) => backend('nip07_reply', { id, ok, value, error }).catch(() => {});
        const run = METHODS[method];
        if (!run) return reply(false, null, `unsupported signer method ${method}`);
        if (!usable()) return reply(false, null, 'No browser signer extension found. Enable it and reload.');
        let timer;
        try {
            const value = await Promise.race([
                run(window.nostr, params || []),
                new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Browser signer did not answer in time')), TIMEOUT_MS); }),
            ]);
            reply(true, value);
        } catch (e) {
            reply(false, null, String(e?.message ?? e ?? 'Browser signer refused'));
        } finally {
            clearTimeout(timer);
        }
    }

    listen('nip07_request', (e) => { answer(e.payload); });
    register('is_nip07_available', () => available());
})();
