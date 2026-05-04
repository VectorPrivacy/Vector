/**
 * Dev-only chat-list perf probe. Call `__measureChatlist()` from the console
 * to capture: render wall-time, scroll FPS, DOM image count, heap (where
 * available). Logs JSON for copy-paste baseline.
 *
 *   await __measureChatlist();
 *   await __measureChatlist({ scrollDurationMs: 2000 });
 */

function _mcl_scrollFps(list, durationMs) {
    return new Promise(resolve => {
        const startScroll = list.scrollTop;
        const maxScroll = list.scrollHeight - list.clientHeight;
        if (maxScroll <= 0) {
            resolve({ frames: 0, avgFrameMs: 0, worstFrameMs: 0, fps: 0, note: 'not scrollable' });
            return;
        }
        const startT = performance.now();
        const samples = [];
        let lastT = startT;
        function tick(t) {
            const dt = t - lastT;
            lastT = t;
            const elapsed = t - startT;
            const progress = Math.min(1, elapsed / durationMs);
            list.scrollTop = startScroll + (maxScroll - startScroll) * progress;
            samples.push(dt);
            if (progress < 1) {
                requestAnimationFrame(tick);
            } else {
                list.scrollTop = startScroll;
                const usable = samples.slice(1);
                const avg = usable.reduce((a, b) => a + b, 0) / usable.length;
                const worst = Math.max.apply(null, usable);
                resolve({
                    frames: usable.length,
                    avgFrameMs: +avg.toFixed(2),
                    worstFrameMs: +worst.toFixed(2),
                    fps: +(1000 / avg).toFixed(1),
                });
            }
        }
        requestAnimationFrame(tick);
    });
}

window.__measureChatlist = async function (opts) {
    opts = opts || {};
    const scrollDurationMs = opts.scrollDurationMs || 1000;

    const list = document.getElementById('chat-list');
    if (!list) {
        console.warn('[measureChatlist] #chat-list not found');
        return null;
    }
    if (typeof renderChatlist !== 'function') {
        console.warn('[measureChatlist] renderChatlist() not available');
        return null;
    }

    // Force the next renderChatlist() to actually render (bust the hash gate).
    // `lastChatlistStateHash` is a top-level `let` in main.js — classic scripts
    // share the same global lexical environment, so we can reassign it here.
    try { lastChatlistStateHash = ''; } catch (_) {}

    // 1. Render wall-time — full re-render of current state.
    const tRenderStart = performance.now();
    renderChatlist();
    const renderMs = performance.now() - tRenderStart;

    // 2. Static snapshot.
    const rowCount = list.querySelectorAll('.chatlist-contact').length;
    const imgCount = list.querySelectorAll('img').length;
    const heap = performance.memory ? performance.memory.usedJSHeapSize : null;

    // 3. Scroll FPS — programmatic top-to-bottom over the requested window.
    const scroll = await _mcl_scrollFps(list, scrollDurationMs);

    const result = {
        rowCount,
        imgCount,
        renderMs: +renderMs.toFixed(2),
        heapMB: heap ? +(heap / 1024 / 1024).toFixed(1) : null,
        scroll,
        platform: navigator.platform,
        ua: navigator.userAgent,
    };
    console.log('[measureChatlist]', result);
    console.log(JSON.stringify(result));
    return result;
};
