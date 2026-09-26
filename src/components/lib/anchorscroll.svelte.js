// Scroll spy and jumps for a scroller of `[data-anchor]` blocks, behind a SectionNav.
// The block crossing the scroller's midline is the one being read; the last one wins
// once the scroll bottoms out, however short its block.
export function anchorScroll(first = null) {
    const s = $state({ active: first, el: null });
    let mutedUntil = 0;
    return {
        get active() { return s.active; },
        set active(id) { s.active = id; },
        get el() { return s.el; },
        set el(node) { s.el = node; },
        spy() {
            const el = s.el;
            if (!el || performance.now() < mutedUntil) return;
            const blocks = [...el.querySelectorAll('[data-anchor]')];
            if (!blocks.length) return;
            const mid = el.getBoundingClientRect().top + el.clientHeight / 2;
            let current = blocks[0];
            for (const b of blocks) if (b.getBoundingClientRect().top <= mid) current = b;
            if (el.scrollTop + el.clientHeight >= el.scrollHeight - 2) current = blocks[blocks.length - 1];
            s.active = current.dataset.anchor;
        },
        jump(id) {
            const el = s.el;
            const block = el?.querySelector(`[data-anchor="${id}"]`);
            if (!block) return;
            s.active = id;
            // The smooth scroll passes every block between here and there; the spy would
            // strobe the timeline through each of them.
            mutedUntil = performance.now() + 700;
            const offset = block.getBoundingClientRect().top - el.getBoundingClientRect().top;
            el.scrollTo({ top: el.scrollTop + offset - 28, behavior: 'smooth' });
        },
        top(id) {
            if (s.el) s.el.scrollTop = 0;
            s.active = id;
        },
    };
}
