/**
 * Overscan for the chat list's skipped rows.
 *
 * Rows carry `content-visibility: auto`, so the browser lays out and paints only
 * the ones intersecting the viewport, and a row scrolled into view would render
 * on arrival. This marks every row within NEAR_PX of the viewport as `cv-near`,
 * which forces it visible while it is still off screen, so a scroll never
 * reaches a row that has yet to render.
 */

(function () {
    const NEAR_PX = 400;

    function attach() {
        const list = VectorSvelte.shellElements().chatList;
        if (!list) {
            console.warn('[chatlist-window] #chat-list not found');
            return;
        }

        const io = new IntersectionObserver((entries) => {
            for (const entry of entries) {
                entry.target.classList.toggle('cv-near', entry.isIntersecting);
            }
        }, { root: list, rootMargin: `${NEAR_PX}px 0px`, threshold: 0 });

        list.querySelectorAll('.chatlist-contact').forEach((el) => io.observe(el));

        // The keyed list reuses rows, so only additions need observing; a removed
        // row is dropped by the observer with the node.
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
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', attach, { once: true });
    } else {
        attach();
    }
})();
