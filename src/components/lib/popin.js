// Replay a card's tick-in each time `tick` moves. The class lands after the overlay has
// rendered, because WebKit never starts an animation declared on a subtree emerging
// from display:none; the reflow between remove and add makes the replay reliable.
export function popIn(node, tick) {
    let last = tick;
    const play = () => {
        node.classList.remove('tick-in');
        void node.offsetWidth;
        node.classList.add('tick-in');
    };
    if (tick) play();
    return {
        update(next) { if (next !== last) { last = next; play(); } },
    };
}
