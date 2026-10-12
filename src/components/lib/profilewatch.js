// Profiles on screen, or about to be, are reported in batches; the backend fetches only the ones
// whose last check has gone stale. One observer serves every row that shows a person.

const NEAR = '300px 0px';
const FLUSH_MS = 1500;
// The backend dedupes too; this keeps a scroll back and forth from re-asking at all.
const RESEND_MS = 10 * 60 * 1000;

const sent = new Map();          // npub -> when it was last reported
const pending = new Set();
const npubOf = new WeakMap();    // element -> npub
let io = null;
let timer = 0;

function flush() {
    timer = 0;
    const npubs = [...pending];
    pending.clear();
    if (npubs.length) window.__TAURI__.core.invoke('queue_visible_profiles', { npubs }).catch(() => {});
}

function seen(npub) {
    const now = Date.now();
    if (now - (sent.get(npub) || 0) < RESEND_MS) return;
    sent.set(npub, now);
    pending.add(npub);
    if (!timer) timer = setTimeout(flush, FLUSH_MS);
}

function observer() {
    io ??= new IntersectionObserver((entries) => {
        for (const e of entries) {
            const npub = e.isIntersecting && npubOf.get(e.target);
            if (npub) seen(npub);
        }
    }, { rootMargin: NEAR, threshold: 0 });
    return io;
}

/** `use:watchProfile={npub}`: keeps this person's profile fresh while the node is near view. */
export function watchProfile(node, npub) {
    const o = observer();
    const set = (n) => {
        o.unobserve(node);
        if (!n) { npubOf.delete(node); return; }
        npubOf.set(node, n);
        o.observe(node);
    };
    set(npub);
    return {
        update: set,
        destroy() { o.unobserve(node); npubOf.delete(node); },
    };
}
