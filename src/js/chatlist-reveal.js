/**
 * Soft fade-in for chat list rows when they SCROLL into the viewport.
 *
 * Preferred path: `contentvisibilityautostatechange` (Chromium) — fires only
 * when content-visibility:auto's skipped state flips back to rendered.
 *
 * Fallback path: IntersectionObserver — fires on geometric viewport entry.
 * Used on WebKit (macOS WKWebView) where the cv event isn't shipped yet.
 *
 * Animation is opacity-only (GPU compositor) — no layout/paint retrigger.
 *
 * Insertion vs. scroll. renderChatlist() rebuilds the list via
 * replaceChildren(), so every render swaps in fresh DOM nodes. Both paths
 * fire their initial callback for each new row on observe, which would
 * animate the entire visible list on every render (the black flash). We
 * gate on data-cv-was-off: a row only animates on transition to visible
 * AFTER it has been observed off-screen at least once. Freshly inserted
 * rows don't have the flag, so the initial in-view callback is a no-op.
 */

(function () {
    let revealCount = 0;
    let skipCount = 0;
    let mode = 'none';

    function triggerReveal(el) {
        // remove → reflow → add restarts the CSS animation on the same element.
        el.classList.remove('cv-revealed');
        void el.offsetWidth;
        el.classList.add('cv-revealed');
        revealCount++;
    }

    function attach() {
        const list = document.getElementById('chat-list');
        if (!list) {
            console.warn('[chatlist-reveal] #chat-list not found');
            return;
        }

        // Probe support: dispatch a fake event listener and let the browser
        // decide. Easier: just attach both paths but gate IO on first event.
        let cvEventSeen = false;

        list.addEventListener('contentvisibilityautostatechange', (e) => {
            cvEventSeen = true;
            mode = 'contentvisibility';
            const target = e.target;
            if (!(target instanceof HTMLElement)) return;
            if (!target.classList.contains('chatlist-contact')) return;
            if (e.skipped) {
                // Row is now off-screen: arm it for fade-in on next scroll-in.
                target.dataset.cvWasOff = '1';
                target.classList.remove('cv-revealed');
                skipCount++;
            } else if (target.dataset.cvWasOff === '1') {
                // Row was previously off-screen and is now scrolling back in.
                triggerReveal(target);
            }
            // Otherwise: initial in-viewport render after insertion. Skip.
        });

        // IntersectionObserver fallback. Observe each row; animate on entry
        // only after the row has been off-screen (otherwise initial-observe
        // callbacks would fade-in the whole visible list on every render).
        // Suppressed on Chromium once the cv event takes over.
        const io = new IntersectionObserver((entries) => {
            if (cvEventSeen) return;
            mode = 'intersection';
            for (const entry of entries) {
                const el = entry.target;
                if (!(el instanceof HTMLElement)) continue;
                if (!el.classList.contains('chatlist-contact')) continue;
                if (entry.isIntersecting) {
                    if (el.dataset.cvWasOff === '1') {
                        triggerReveal(el);
                    }
                } else {
                    el.dataset.cvWasOff = '1';
                    skipCount++;
                }
            }
        }, { root: list, threshold: 0 });

        function observeAll() {
            list.querySelectorAll('.chatlist-contact').forEach((el) => io.observe(el));
        }
        observeAll();

        // renderChatlist() does `replaceChildren(fragment)` — old observers
        // become noops on detached nodes (auto-cleaned by browser), but new
        // rows need to be observed. MutationObserver picks up added rows.
        const mo = new MutationObserver((mutations) => {
            for (const m of mutations) {
                for (const node of m.addedNodes) {
                    if (node instanceof HTMLElement && node.classList.contains('chatlist-contact')) {
                        io.observe(node);
                    }
                }
            }
        });
        mo.observe(list, { childList: true });

        window.__chatlistRevealStats = () => ({
            reveals: revealCount,
            skips: skipCount,
            mode,
        });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', attach, { once: true });
    } else {
        attach();
    }
})();
