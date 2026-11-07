// Vector Web: a diagnostics panel for transfers that stop moving. Five quick taps on the version
// in Settings open it. It tells a busy worker (pings wait) from a stalled network (pings answer,
// a download's last bytes keep getting older).
(() => {
    'use strict';

    const { ping, backend } = window.__vectorWeb;
    const WINDOW_MS = 30000;

    let taps = [];
    let panel = null;
    let timer = 0;
    let asked = 0;          // when the unanswered ping went out, 0 when none is out
    let last = null;        // the latest answered ping's round trip
    let samples = [];       // [at, ms] over the last WINDOW_MS
    let flow = [];
    let flowAt = 0;

    document.addEventListener('click', (e) => {
        if (!e.target.closest?.('#current-version')) return;
        const now = Date.now();
        taps = taps.filter((t) => now - t < 2000);
        taps.push(now);
        if (taps.length < 5) return;
        taps = [];
        panel ? close() : open();
    }, true);

    function open() {
        panel = document.createElement('div');
        panel.setAttribute('style', [
            'position:fixed', 'z-index:2147483647', 'left:8px', 'right:8px',
            'top:calc(env(safe-area-inset-top) + 8px)', 'padding:10px 12px', 'border-radius:10px',
            'background:rgba(0,0,0,0.88)', 'color:#e6e6e6', 'font:12px/1.45 ui-monospace,Menlo,monospace',
            'white-space:pre-wrap', 'pointer-events:auto', 'box-shadow:0 4px 18px rgba(0,0,0,0.5)',
        ].join(';'));
        panel.addEventListener('click', close);
        document.body.appendChild(panel);
        tick();
        timer = setInterval(tick, 1000);
    }

    function close() {
        clearInterval(timer);
        panel?.remove();
        panel = null;
    }

    function tick() {
        const now = Date.now();
        if (!asked) {
            asked = now;
            ping().then(() => {
                last = Date.now() - asked;
                samples.push([Date.now(), last]);
                asked = 0;
            }, () => { asked = 0; });
            backend('web_download_flow').then((rows) => { flow = rows || []; flowAt = Date.now(); }, () => {});
        }
        samples = samples.filter(([at]) => now - at < WINDOW_MS);
        render(now);
    }

    function render(now) {
        if (!panel) return;
        const worst = samples.reduce((m, [, ms]) => Math.max(m, ms), 0);
        const waiting = asked ? now - asked : 0;
        const lines = [
            'Vector Web diagnostics (tap to close)',
            `worker ping: ${last == null ? '…' : last + ' ms'}  worst 30s: ${worst} ms${waiting > 1000 ? `  NO ANSWER ${(waiting / 1000).toFixed(1)} s` : ''}`,
            `downloads (${flow.length})${flowAt ? `, read ${((now - flowAt) / 1000).toFixed(1)} s ago` : ''}:`,
        ];
        for (const d of flow) {
            const size = d.total ? `${kb(d.bytes)}/${kb(d.total)}` : kb(d.bytes);
            lines.push(`  ${d.id.slice(0, 8)} ${size}  running ${(d.running_ms / 1000).toFixed(0)} s  last bytes ${(d.quiet_ms / 1000).toFixed(1)} s ago`);
        }
        if (!flow.length) lines.push('  none');
        panel.textContent = lines.join('\n');
    }

    const kb = (n) => `${Math.round(n / 1024)} KB`;
})();
