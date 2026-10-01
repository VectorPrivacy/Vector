// The first script in every mini app document (the service worker puts it
// there). Two parts: a guard that keeps WebRTC out of reach, then the
// `window.webxdc` API, talking to Vector over a port the parent page hands over.

// ─── Guard ──────────────────────────────────────────────────────────────────
//
// WebRTC reaches the network past every CSP, so the app must never hold a
// working RTCPeerConnection. Deleting it here is only half: any child frame is a
// fresh window with its own copy, reachable through frames[i]. So no frame may
// ever exist. Frame elements can't be created or inserted, HTML strings reach
// the DOM only through a Trusted Types policy that disarms frame tags, and
// `javascript:` navigations (which would load a fresh window) are refused.
(() => {
    'use strict';

    // Without Trusted Types the string paths can't be closed; refuse to run at all.
    if (!window.trustedTypes || typeof window.trustedTypes.createPolicy !== 'function') {
        window.stop();
        const say = () => { document.documentElement.textContent = 'This browser is too old to run mini apps safely. Please update it.'; };
        if (document.documentElement) say(); else addEventListener('DOMContentLoaded', say);
        throw new Error('Trusted Types unavailable: mini app halted');
    }

    const FRAMES = ['iframe', 'frame', 'frameset', 'object', 'embed', 'fencedframe', 'portal'];
    const FRAME_NAMES = new Set(FRAMES);
    const FRAME_SELECTOR = FRAMES.join(',');
    const FRAME_TAG = new RegExp(`<(/?)([\\w.-]+:)?(${FRAMES.join('|')})(?=[\\s/>]|$)`, 'gi');

    const disarm = (s) => String(s).replace(FRAME_TAG, '<$1$2vector-blocked-$3');
    const isFrameName = (name) => FRAME_NAMES.has(String(name).split(':').pop().toLowerCase());
    const blocked = () => new DOMException('Frames are not available to mini apps', 'NotSupportedError');

    const lock = (obj, name, desc) => Object.defineProperty(obj, name, { ...desc, configurable: false });

    // A node about to enter the DOM: refuse it if it is, or holds, a frame.
    function refuseFrames(node) {
        if (!node || typeof node !== 'object') return;
        const t = node.nodeType;
        if (t === 1 && isFrameName(node.localName)) throw blocked();
        if ((t === 1 || t === 9 || t === 11) && typeof node.querySelector === 'function' && node.querySelector(FRAME_SELECTOR)) throw blocked();
    }

    // Documents parsed outside the policy (XHR, DOMParser results) lose their frames.
    function scrub(root) {
        if (!root || typeof root.getElementsByTagName !== 'function') return;
        for (const el of [...root.getElementsByTagName('*')]) if (isFrameName(el.localName)) el.remove();
    }

    // 1. WebRTC and the other ways to a fresh window are gone from this realm.
    const WINDOW_KILL = ['documentPictureInPicture', 'XSLTProcessor'];
    for (const name of [...Object.getOwnPropertyNames(window), ...WINDOW_KILL]) {
        if (!/^(webkit|moz)?RTC/.test(name) && !WINDOW_KILL.includes(name)) continue;
        try { delete window[name]; } catch { /* not configurable */ }
        try { lock(window, name, { value: undefined, writable: false, enumerable: false }); } catch { /* gone */ }
    }
    lock(window, 'open', { value: () => null, writable: false, enumerable: true });

    // 2. HTML strings become DOM only through this policy, and it is the only one allowed.
    window.trustedTypes.createPolicy('default', {
        createHTML: (s) => disarm(s),
        // A javascript: URL would run in, and may load, a window this guard never saw.
        createScript: (s, ...sink) => (sink.some((x) => /location|javascript|href/i.test(String(x))) ? null : s),
        createScriptURL: (s) => s,
    });

    // 3. Frame elements can't be made.
    const wrap = (proto, name, before) => {
        const d = proto && Object.getOwnPropertyDescriptor(proto, name);
        if (!d || typeof d.value !== 'function') return;
        const orig = d.value;
        lock(proto, name, { value: function (...args) { before(args, this); return orig.apply(this, args); }, writable: false, enumerable: d.enumerable });
    };
    const wrapSetter = (proto, name, before) => {
        const d = proto && Object.getOwnPropertyDescriptor(proto, name);
        if (!d || typeof d.set !== 'function') return;
        lock(proto, name, { get: d.get, set(v) { before([v], this); d.set.call(this, v); }, enumerable: d.enumerable });
    };

    wrap(Document.prototype, 'createElement', ([name]) => { if (isFrameName(name)) throw blocked(); });
    wrap(Document.prototype, 'createElementNS', ([, name]) => { if (isFrameName(name)) throw blocked(); });
    wrap(DOMImplementation.prototype, 'createDocument', ([, name]) => { if (name && isFrameName(name)) throw blocked(); });
    wrap(CustomElementRegistry.prototype, 'define', ([, , opts]) => { if (opts && opts.extends && isFrameName(opts.extends)) throw blocked(); });

    // 4. Nothing holding a frame can be inserted anywhere.
    const nodesIn = (args) => args.forEach(refuseFrames);
    for (const m of ['appendChild', 'insertBefore', 'replaceChild']) wrap(Node.prototype, m, nodesIn);
    for (const proto of [Element.prototype, Document.prototype, DocumentFragment.prototype]) {
        for (const m of ['append', 'prepend', 'replaceChildren', 'moveBefore']) wrap(proto, m, nodesIn);
    }
    for (const proto of [Element.prototype, CharacterData.prototype, DocumentType.prototype]) {
        for (const m of ['before', 'after', 'replaceWith']) wrap(proto, m, nodesIn);
    }
    wrap(Element.prototype, 'insertAdjacentElement', ([, el]) => refuseFrames(el));
    wrap(Range.prototype, 'insertNode', nodesIn);
    wrap(Range.prototype, 'surroundContents', nodesIn);
    wrap(HTMLSelectElement.prototype, 'add', nodesIn);
    wrap(window.HTMLOptionsCollection && HTMLOptionsCollection.prototype, 'add', nodesIn);
    wrapSetter(Document.prototype, 'body', nodesIn);
    for (const p of ['caption', 'tHead', 'tFoot']) wrapSetter(HTMLTableElement.prototype, p, nodesIn);

    // HTML handed to APIs outside Trusted Types' reach is disarmed the same way.
    wrap(Document.prototype, 'execCommand', (args) => { if (typeof args[2] === 'string') args[2] = disarm(args[2]); });
    for (const proto of [Element.prototype, window.ShadowRoot && ShadowRoot.prototype]) {
        wrap(proto, 'setHTML', (args) => { if (typeof args[0] === 'string') args[0] = disarm(args[0]); });
    }
    if (typeof Document.parseHTML === 'function') {
        const parse = Document.parseHTML;
        lock(Document, 'parseHTML', { value: (html, opts) => parse.call(Document, disarm(html), opts), writable: false });
    }
    for (const prop of ['responseXML', 'response']) {
        const d = Object.getOwnPropertyDescriptor(XMLHttpRequest.prototype, prop);
        if (d && d.get) lock(XMLHttpRequest.prototype, prop, { get() { const v = d.get.call(this); if (v instanceof Document) scrub(v); return v; }, enumerable: d.enumerable });
    }
    {
        const parse = DOMParser.prototype.parseFromString;
        lock(DOMParser.prototype, 'parseFromString', { value: function (...args) { const doc = parse.apply(this, args); scrub(doc); return doc; }, writable: false });
    }

    // Pasted or dropped markup is the user's input, and stays frameless too.
    for (const type of ['paste', 'drop', 'beforeinput']) {
        addEventListener(type, (e) => {
            const dt = e.clipboardData || e.dataTransfer;
            const html = dt && dt.getData && dt.getData('text/html');
            if (html && disarm(html) !== html) { e.preventDefault(); e.stopImmediatePropagation(); }
        }, true);
    }

    // A frame element, should one ever exist, opens onto nothing.
    for (const ctor of ['HTMLIFrameElement', 'HTMLFrameElement', 'HTMLObjectElement', 'HTMLEmbedElement', 'HTMLFencedFrameElement']) {
        const proto = window[ctor] && window[ctor].prototype;
        if (!proto) continue;
        for (const prop of ['contentWindow', 'contentDocument']) {
            if (Object.getOwnPropertyDescriptor(proto, prop)) lock(proto, prop, { get: () => null, enumerable: true });
        }
        if (proto.getSVGDocument) lock(proto, 'getSVGDocument', { value: () => null, writable: false });
    }

    // 5. Backstop: anything that slipped through is removed and reported.
    new MutationObserver((records) => {
        for (const r of records) {
            for (const n of r.addedNodes) {
                if (n.nodeType !== 1) continue;
                if (isFrameName(n.localName)) { n.remove(); console.warn('[webxdc] removed a frame'); } else scrub(n);
            }
        }
    }).observe(document, { childList: true, subtree: true });

})();

// ─── Bridge ─────────────────────────────────────────────────────────────────
(() => {
    'use strict';
    if (window.webxdc) return;
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
})();
