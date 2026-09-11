// Replay a card's pop-in each time `tick` moves. The class lands a frame after the overlay
// has rendered: the action runs in the same flush that flips the overlay's display, and
// WebKit never starts an animation declared on a subtree still emerging from display:none.
export function popIn(node, tick) {
    let last = tick;
    let raf = 0;
    const play = () => {
        cancelAnimationFrame(raf);
        raf = requestAnimationFrame(() => {
            node.classList.remove('pop-in');
            void node.offsetWidth;
            node.classList.add('pop-in');
        });
    };
    if (tick) play();
    return {
        update(next) { if (next !== last) { last = next; play(); } },
        destroy() { cancelAnimationFrame(raf); },
    };
}
