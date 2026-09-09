// Replay a card's pop-in each time `pop` moves. The class lands after the overlay has
// rendered, because WebKit never starts an animation declared on a subtree emerging
// from display:none; the reflow between remove and add makes the replay reliable.
export function popIn(node, pop) {
    let last = pop;
    const play = () => {
        node.classList.remove('pop-in');
        void node.offsetWidth;
        node.classList.add('pop-in');
    };
    if (pop) play();
    return {
        update(next) { if (next !== last) { last = next; play(); } },
    };
}
