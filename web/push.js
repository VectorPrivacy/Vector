// Vector Web: notifications while Vector is closed (sealed push, vector_core::push).
//
// The browser's push subscription belongs to the device, not an account: it lives here in
// localStorage with the VAPID key it was made with, and is handed to whichever account is
// signed in. The account hands tickets to its contacts; their Vector writes the
// notification and encrypts it to this subscription, and /sw.js shows it.
(() => {
    const { register, backend } = window.__vectorWeb;
    const KEY = 'vector-push-device';
    const CACHE = 'vector-push';
    const STATE_URL = '/__vector/push/state';

    const standalone = matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
    const ios = /iP(hone|ad|od)/.test(navigator.platform) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
    let reg = null;
    let vapid = null;

    const kept = () => { try { return JSON.parse(localStorage.getItem(KEY) || 'null'); } catch { return null; } };
    const keep = (d) => { try { d ? localStorage.setItem(KEY, JSON.stringify(d)) : localStorage.removeItem(KEY); } catch {} };
    const hex = (n) => [...crypto.getRandomValues(new Uint8Array(n))].map((b) => b.toString(16).padStart(2, '0')).join('');
    const b64url = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
    const unb64url = (s) => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));

    function supported() {
        const s = window.__vectorWeb.storage();
        if (s === 'session' || s === 'memory') return 'unsupported';
        if (!('serviceWorker' in navigator)) return 'unsupported';
        // iOS only lets a Home Screen app receive pushes.
        if (!('PushManager' in window)) return ios && !standalone ? 'install' : 'unsupported';
        return 'ok';
    }

    // Everything the subscribe call needs is made ahead of time: iOS only allows it inside
    // the tap, and an await before it can spend that.
    async function prepare() {
        if (supported() !== 'ok') return;
        reg = await navigator.serviceWorker.ready.catch(() => null);
        const d = kept();
        if (d?.vapid && d?.vapidPublic) { vapid = { secret: d.vapid, public: d.vapidPublic }; return; }
        const pair = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify']);
        const jwk = await crypto.subtle.exportKey('jwk', pair.privateKey);
        vapid = { secret: jwk.d, public: b64url(await crypto.subtle.exportKey('raw', pair.publicKey)) };
    }
    const ready = prepare().catch((e) => console.warn('[Push] prepare failed:', e));

    function deviceFrom(sub, previous) {
        const json = sub.toJSON();
        return {
            id: previous?.endpoint === json.endpoint && previous?.id ? previous.id : hex(16),
            endpoint: json.endpoint,
            p256dh: json.keys.p256dh,
            auth: json.keys.auth,
            vapid: vapid.secret,
            vapidPublic: vapid.public,
            origin: location.origin,
        };
    }

    async function adopt(device) {
        keep(device);
        const { vapidPublic, ...forAccount } = device;
        const state = await backend('push_set_device', { device: forAccount });
        await writeState(state);
    }

    async function writeState(state) {
        try {
            const cache = await caches.open(CACHE);
            await cache.put(STATE_URL, new Response(JSON.stringify(state), { headers: { 'Content-Type': 'application/json' } }));
        } catch (e) { console.warn('[Push] state not saved:', e); }
    }

    async function status() {
        const s = supported();
        if (s !== 'ok') return s;
        if (window.Notification?.permission === 'denied') return 'denied';
        await ready;
        const sub = await reg?.pushManager.getSubscription().catch(() => null);
        return sub && kept()?.endpoint === sub.endpoint ? 'on' : 'off';
    }

    register('push_status', () => status());

    // Called straight from the tap, so the subscribe below still holds its user activation.
    register('push_enable', () => {
        if (!reg || !vapid) return Promise.reject('Notifications are still getting ready, try again in a moment.');
        const subscribing = reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: unb64url(vapid.public) });
        return subscribing.then(async (sub) => {
            await adopt(deviceFrom(sub, kept()));
            return 'on';
        }, async (e) => {
            // A subscription made with another key blocks this one: drop it and retry.
            const old = await reg.pushManager.getSubscription().catch(() => null);
            if (old) {
                await old.unsubscribe().catch(() => {});
                const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: unb64url(vapid.public) });
                await adopt(deviceFrom(sub, null));
                return 'on';
            }
            throw String(e?.message || e);
        });
    });

    register('push_disable', async () => {
        await backend('push_disable').catch(() => {});
        const sub = await reg?.pushManager.getSubscription().catch(() => null);
        await sub?.unsubscribe().catch(() => {});
        keep(null);
        await writeState({ v: 1, enabled: false, contacts: {} });
        return 'off';
    });

    // Signed in: re-check the subscription (Safari never says when it changes), hand it to
    // this account, and refresh the names the service worker shows.
    async function sync() {
        if (supported() !== 'ok') return;
        await ready;
        const d = kept();
        if (!d || window.Notification?.permission !== 'granted') return;
        let sub = await reg?.pushManager.getSubscription().catch(() => null);
        if (!sub) {
            sub = await reg?.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: unb64url(d.vapidPublic) }).catch(() => null);
            if (!sub) return;
        }
        await adopt(deviceFrom(sub, d));
    }

    async function refresh() {
        if (!kept()) return;
        const state = await backend('push_worker_state').catch(() => null);
        if (state) await writeState(state);
    }

    let signedIn = false;
    window.__TAURI__.event.listen('init_finished', () => {
        signedIn = true;
        backend('get_synced_settings').then((v) => { live = !!v?.streamer?.on; if (live) closeAll(); }, () => {});
        sync().catch((e) => console.warn('[Push] sync failed:', e));
        openFromHash();
        setTimeout(clearRead, 3000);
    });
    window.__TAURI__.event.listen('push_contacts_changed', () => refresh());
    // Going live takes down what is already shown: it names people the stream must not see.
    let live = false;
    window.__TAURI__.event.listen('synced_settings_updated', ({ payload }) => {
        const on = !!payload?.streamer?.on;
        if (on && !live) closeAll();
        live = on;
    });
    // Names change while open; the worker reads them only after the app is gone.
    addEventListener('pagehide', () => { if (signedIn) refresh(); });
    document.addEventListener('visibilitychange', () => {
        if (document.visibilityState === 'hidden' && signedIn) refresh();
        if (document.visibilityState === 'visible') navigator.clearAppBadge?.().catch(() => {});
    });

    // The chat on screen holds its contact's pushes at the pusher, on a lease renewed while it
    // stays there: iOS shows every push it receives, even for the conversation already open.
    let viewed = null;
    let renewed = 0;
    function watch() {
        if (!signedIn || !kept()) return;
        const chat = document.visibilityState === 'visible' && strOpenChat.startsWith('npub1') ? strOpenChat : null;
        if (chat !== viewed || (chat && Date.now() - renewed > 20000)) {
            if (chat !== viewed) clearRead();
            viewed = chat;
            renewed = Date.now();
            backend('push_viewing', { npub: chat }).catch(() => {});
        }
    }
    setInterval(watch, 5000);
    document.addEventListener('visibilitychange', () => { watch(); if (document.visibilityState === 'visible') clearRead(); });

    // Notifications for chats read since: gone. iOS refuses to close one in its first 30 seconds,
    // so whatever was too young is tried once more after that.
    async function clearRead(retry = true) {
        if (!signedIn) return;
        const reg = await navigator.serviceWorker?.ready.catch(() => null);
        const shown = await reg?.getNotifications().catch(() => []) || [];
        let young = false;
        for (const n of shown) {
            const chat = n.data?.chat;
            const open = document.visibilityState === 'visible' && chat === strOpenChat;
            if (!chat || !(open || arrChats.find((c) => c.id === chat)?.unread === 0)) continue;
            try { n.close(); } catch {}
            young = true;
        }
        if (young && retry) setTimeout(() => clearRead(false), 31000);
    }

    // iOS refuses to close one in its first 30 seconds, so once more after that.
    async function closeAll(retry = true) {
        const reg = await navigator.serviceWorker?.ready.catch(() => null);
        const shown = await reg?.getNotifications().catch(() => []) || [];
        for (const n of shown) {
            try { n.close(); } catch {}
        }
        if (shown.length && retry) setTimeout(() => closeAll(false), 31000);
    }

    // A tapped notification names the chat: in the hash when it opened the app, in a
    // message from the worker when the app was already open.
    function openChatSoon(npub) {
        if (!/^npub1[0-9a-z]{58}$/.test(npub)) return;
        const go = () => openChat(npub);
        if (signedIn) go(); else window.__TAURI__.event.listen('init_finished', () => setTimeout(go, 0));
    }
    function openFromHash() {
        const m = /^#chat=(npub1[0-9a-z]{58})$/.exec(location.hash);
        if (!m) return;
        history.replaceState(null, '', location.pathname + location.search);
        openChatSoon(m[1]);
    }
    addEventListener('hashchange', openFromHash);
    navigator.serviceWorker?.addEventListener('message', (e) => {
        if (e.data?.type === 'vector-open-chat') openChatSoon(e.data.chat);
    });
})();
