// Vector Web: mini app windows. Each app runs in a sandboxed iframe on its own
// origin, `<partition>.xdc.<vector host>`, so its storage is its own and it can
// reach nothing of Vector's. The origin's service worker serves the package
// (web/xdc/); this side hands it over and relays the webxdc bridge.
(() => {
    'use strict';

    const { listen, convertFileSrc } = { listen: window.__TAURI__.event.listen, convertFileSrc: window.__TAURI__.core.convertFileSrc };
    const { backend, register } = window.__vectorWeb;

    // label -> { el, frame, origin, port, info, filePath }
    const windows = new Map();
    let zTop = 1000;
    let cascade = 0;

    // App origins are subdomains of Vector's host, which an IP address has none of.
    function requireHostName() {
        const host = location.hostname;
        if (/^[\d.]+$/.test(host) || host.includes(':') || host.startsWith('[')) {
            throw new Error('Mini apps need Vector opened by a host name (for example localhost), not an IP address.');
        }
    }

    const appOrigin = (partition) => `${location.protocol}//${partition}.xdc.${location.host}`;

    function focus(w) {
        w.el.style.zIndex = String(++zTop);
    }

    function close(label) {
        const w = windows.get(label);
        if (!w) return;
        windows.delete(label);
        try { w.port?.close(); } catch { /* already closed */ }
        w.el.remove();
        backend('miniapp_closed', { label }).catch(() => {});
    }

    // Both title bar glyphs drawn on one 12px grid, so the buttons match.
    const SVG = 'http://www.w3.org/2000/svg';
    function icon(kind) {
        const svg = document.createElementNS(SVG, 'svg');
        svg.setAttribute('viewBox', '0 0 12 12');
        svg.setAttribute('width', '12');
        svg.setAttribute('height', '12');
        svg.setAttribute('aria-hidden', 'true');
        const shape = document.createElementNS(SVG, kind === 'max' ? 'rect' : 'path');
        if (kind === 'max') {
            for (const [k, v] of Object.entries({ x: 1.5, y: 1.5, width: 9, height: 9, rx: 1.5 })) shape.setAttribute(k, v);
        } else {
            shape.setAttribute('d', 'M2 2L10 10M10 2L2 10');
        }
        shape.setAttribute('fill', 'none');
        shape.setAttribute('stroke', 'currentColor');
        shape.setAttribute('stroke-width', '1.5');
        shape.setAttribute('stroke-linecap', 'round');
        svg.appendChild(shape);
        return svg;
    }

    function makeWindow(info) {
        const el = document.createElement('div');
        el.className = 'xdc-window';
        const bar = document.createElement('div');
        bar.className = 'xdc-bar';
        if (info.icon_data) {
            const icon = document.createElement('img');
            icon.className = 'xdc-icon';
            icon.src = info.icon_data;
            icon.alt = '';
            bar.appendChild(icon);
        }
        const title = document.createElement('span');
        title.className = 'xdc-title';
        title.textContent = info.name || 'Mini App';
        bar.appendChild(title);
        const max = document.createElement('button');
        max.type = 'button';
        max.className = 'xdc-btn';
        max.title = 'Maximize';
        max.appendChild(icon('max'));
        const x = document.createElement('button');
        x.type = 'button';
        x.className = 'xdc-btn xdc-close';
        x.title = 'Close';
        x.appendChild(icon('close'));
        bar.append(max, x);

        const frame = document.createElement('iframe');
        frame.className = 'xdc-frame';
        frame.setAttribute('sandbox', 'allow-scripts allow-same-origin allow-pointer-lock allow-modals');
        frame.setAttribute('allow', info.allow || '');
        frame.setAttribute('referrerpolicy', 'no-referrer');
        frame.title = info.name || 'Mini App';

        el.append(bar, frame);
        const offset = (cascade++ % 6) * 28;
        el.style.left = `${Math.max(8, (innerWidth - 480) / 2 + offset)}px`;
        el.style.top = `${Math.max(8, (innerHeight - 640) / 2 + offset)}px`;
        document.body.appendChild(el);

        el.addEventListener('pointerdown', () => focus({ el }), true);
        max.onclick = () => el.classList.toggle('xdc-max');
        x.onclick = () => close(info.label);
        drag(el, bar);
        return { el, frame };
    }

    // Dragging by the title bar; the frame would swallow the pointer mid-drag.
    function drag(el, bar) {
        bar.addEventListener('pointerdown', (e) => {
            if (e.target.closest('button') || el.classList.contains('xdc-max')) return;
            const startX = e.clientX - el.offsetLeft;
            const startY = e.clientY - el.offsetTop;
            bar.setPointerCapture(e.pointerId);
            el.classList.add('xdc-dragging');
            const move = (m) => {
                el.style.left = `${Math.min(innerWidth - 80, Math.max(-el.offsetWidth + 80, m.clientX - startX))}px`;
                el.style.top = `${Math.min(innerHeight - 40, Math.max(0, m.clientY - startY))}px`;
            };
            const up = () => {
                el.classList.remove('xdc-dragging');
                bar.removeEventListener('pointermove', move);
                bar.removeEventListener('pointerup', up);
                bar.removeEventListener('pointercancel', up);
            };
            bar.addEventListener('pointermove', move);
            bar.addEventListener('pointerup', up);
            bar.addEventListener('pointercancel', up);
        });
    }

    async function open({ filePath, chatId = '', messageId = '', href = null, topicId = null }) {
        requireHostName();
        const info = await backend('miniapp_prepare', { filePath, chatId, messageId, topicId });
        const existing = windows.get(info.label);
        if (existing) {
            focus(existing);
            if (href && href.startsWith('/')) existing.frame.src = existing.origin + href;
            return;
        }
        const origin = appOrigin(info.partition);
        const { el, frame } = makeWindow(info);
        const w = { el, frame, origin, port: null, info, filePath, href, chatId, messageId };
        windows.set(info.label, w);
        focus(w);
        frame.src = `${origin}/__vector/host.html`;
    }

    // The package goes over once the origin's host page is ready for it.
    async function deliver(w) {
        let bytes;
        try {
            bytes = await (await fetch(convertFileSrc(w.filePath))).arrayBuffer();
        } catch (e) {
            window.showToast?.(`Could not open ${w.info.name}: ${e?.message || e}`);
            return close(w.info.label);
        }
        const meta = { selfAddr: w.info.self_addr, selfName: w.info.self_name, policy: w.info.policy };
        w.frame.contentWindow.postMessage({ t: 'xdc-load', bytes, meta, href: w.href }, w.origin, [bytes]);
    }

    // The app page asked for its bridge: a fresh port per page load.
    function connect(w) {
        try { w.port?.close(); } catch { /* replaced */ }
        const ch = new MessageChannel();
        w.port = ch.port1;
        w.port.onmessage = ({ data }) => {
            switch (data?.t) {
                case 'rt-join':
                    backend('miniapp_rt_join', { label: w.info.label }).catch((e) => console.warn('[xdc] realtime join failed:', e));
                    break;
                case 'rt-send':
                    if (data.data instanceof Uint8Array) {
                        backend('miniapp_rt_send', data.data, { headers: { label: w.info.label } }).catch(() => {});
                    }
                    break;
                case 'rt-leave':
                    break;
            }
        };
        w.frame.contentWindow.postMessage({ t: 'xdc-port' }, w.origin, [ch.port2]);
    }

    addEventListener('message', (e) => {
        for (const w of windows.values()) {
            if (e.source !== w.frame.contentWindow || e.origin !== w.origin) continue;
            if (e.data?.t === 'xdc-host-ready') deliver(w);
            else if (e.data?.t === 'xdc-hello') connect(w);
            return;
        }
    });

    listen('miniapp_rt', (e) => {
        const p = e.payload;
        const w = windows.get(p?.label);
        if (!w?.port || p.event !== 'data' || !p.bytes) return;
        w.port.postMessage({ t: 'rt-data', data: p.bytes }, [p.bytes.buffer]);
    });

    register('miniapp_open', (args) => open(args));
    register('miniapp_close', ({ chatId, messageId }) => {
        for (const [label, w] of windows) if (w.chatId === chatId && w.messageId === messageId) close(label);
    });
    register('marketplace_open_app', async ({ appId }) => {
        const filePath = await backend('marketplace_resolve_app', { appId });
        await open({ filePath });
    });
})();
