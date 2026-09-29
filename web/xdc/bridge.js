// The `window.webxdc` API, talking to Vector over a port the parent page hands
// the app. Update storage matches desktop: realtime channels carry multiplayer.
(() => {
    'use strict';
    const META = __VECTOR_META__;

    let port = null;
    const portReady = new Promise((resolve) => {
        addEventListener('message', function take(e) {
            if (e.source !== window.parent || e.origin !== META.parent || e.data?.t !== 'xdc-port' || !e.ports[0]) return;
            removeEventListener('message', take);
            port = e.ports[0];
            port.onmessage = onPort;
            resolve(port);
        });
    });
    window.parent.postMessage({ t: 'xdc-hello' }, META.parent);

    let channel = null;
    let listener = null;
    const queued = [];

    function onPort({ data }) {
        if (data?.t !== 'rt-data' || !channel) return;
        if (listener) listener(data.data);
        else if (queued.length < 256) queued.push(data.data);
    }

    const notImplemented = (name) => () => Promise.reject(new Error(`${name} is not implemented`));

    window.webxdc = {
        selfAddr: META.selfAddr,
        selfName: META.selfName,
        sendUpdateInterval: 10000,
        sendUpdateMaxSize: 128000,
        setUpdateListener: () => Promise.resolve(),
        sendUpdate: () => {},
        getAllUpdates: () => Promise.resolve([]),
        sendToChat: notImplemented('sendToChat'),
        importFiles: notImplemented('importFiles'),
        joinRealtimeChannel() {
            if (channel) return channel;
            portReady.then((p) => p.postMessage({ t: 'rt-join' }));
            channel = {
                setListener(fn) {
                    listener = fn;
                    while (listener && queued.length) listener(queued.shift());
                },
                send(data) {
                    if (!(data instanceof Uint8Array)) throw new TypeError('realtime data must be a Uint8Array');
                    if (data.byteLength > 128000) throw new RangeError('realtime data exceeds 128000 bytes');
                    const copy = data.slice();
                    portReady.then((p) => p.postMessage({ t: 'rt-send', data: copy }, [copy.buffer]));
                },
                leave() {
                    portReady.then((p) => p.postMessage({ t: 'rt-leave' }));
                    channel = null;
                    listener = null;
                    queued.length = 0;
                },
            };
            return channel;
        },
    };

    // WebRTC reaches the network past the CSP; the offline promise needs it gone,
    // in this window and in any frame the app makes.
    const RTC = ['RTCPeerConnection', 'webkitRTCPeerConnection', 'RTCDataChannel', 'RTCSessionDescription', 'RTCIceCandidate'];
    const neuter = (w) => {
        for (const name of RTC) {
            try { Object.defineProperty(w, name, { value: undefined, writable: false, configurable: false }); } catch { /* already gone */ }
        }
    };
    neuter(window);
    const frameWindow = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'contentWindow');
    Object.defineProperty(HTMLIFrameElement.prototype, 'contentWindow', {
        configurable: false,
        get() {
            const w = frameWindow.get.call(this);
            if (w) try { neuter(w); } catch { /* cross-origin */ }
            return w;
        },
    });
})();
