// Countdowns for what ends on its own: a dissolving message, a Timeout.

/** mm:ss under an hour, else Hh Mm, else Dd Hh. */
export function fmtCountdown(secs) {
    if (secs >= 86400) { const d = Math.floor(secs / 86400); const h = Math.floor((secs % 86400) / 3600); return d + 'd ' + h + 'h'; }
    if (secs >= 3600) { const h = Math.floor(secs / 3600); const m = Math.floor((secs % 3600) / 60); return h + 'h ' + m + 'm'; }
    const m = Math.floor(secs / 60), s = secs % 60;
    return m + ':' + String(s).padStart(2, '0');
}

/**
 * A tooltip counting down to `until` (unix seconds) while `node` is hovered, ticking each
 * second: `label(secondsLeft)` words it. `ui` is { showTooltip, hideTooltip }.
 */
export function countdownTip(node, opts) {
    let cur = opts;
    let timer = 0;
    const tick = () => cur.ui.showTooltip(cur.label(cur.until - Math.floor(Date.now() / 1000)), node);
    const enter = () => { tick(); clearInterval(timer); timer = setInterval(tick, 1000); };
    const leave = () => {
        if (!timer) return;
        clearInterval(timer);
        timer = 0;
        cur.ui.hideTooltip();
    };
    node.addEventListener('mouseenter', enter);
    node.addEventListener('mouseleave', leave);
    return {
        update(next) { cur = next; },
        destroy() {
            leave();
            node.removeEventListener('mouseenter', enter);
            node.removeEventListener('mouseleave', leave);
        },
    };
}
