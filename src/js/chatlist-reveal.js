/**
 * Soft fade-in for chat list rows when they enter the viewport.
 *
 * Preferred path: `contentvisibilityautostatechange` (Chromium) — fires only
 * when content-visibility:auto's skipped state flips back to rendered.
 *
 * Fallback path: IntersectionObserver — fires on geometric viewport entry.
 * Used on WebKit (macOS WKWebView) where the cv event isn't shipped yet.
 *
 * Animation is opacity-only (GPU compositor) — no layout/paint retrigger.
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
                target.classList.remove('cv-revealed');
                skipCount++;
            } else {
                triggerReveal(target);
            }
        });

        // IntersectionObserver fallback. Observe each row; animate on entry.
        // To avoid double-animating on Chromium (where cv event also fires),
        // we suppress IO callbacks once we've seen a cv event.
        const io = new IntersectionObserver((entries) => {
            if (cvEventSeen) return;
            mode = 'intersection';
            for (const entry of entries) {
                if (!entry.isIntersecting) continue;
                const el = entry.target;
                if (el instanceof HTMLElement && el.classList.contains('chatlist-contact')) {
                    triggerReveal(el);
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
