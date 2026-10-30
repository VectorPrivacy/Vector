<script>
    // Every stock emoji as ONE static span per cell, built in a fragment, each a slice of a
    // sprite sheet: a few images decode as the grid scrolls, where an <img> per cell kept a
    // WebKit document per SVG. Deliberately not a keyed each: a reactive node per cell for
    // ~1900 cells (each with its own branch anchors) tripled the DOM and made every
    // interaction inside the picker stall for hundreds of milliseconds.
    let { grid, h } = $props();   // h: all(), sprite(cell, emoji), twemojify(el), stockTitle(e)

    function build(host) {
        const frag = document.createDocumentFragment();
        for (const e of h.all()) {
            const span = document.createElement('span');
            span.dataset.emoji = e.emoji;
            span.dataset.emojiTooltip = h.stockTitle(e);
            if (!h.sprite(span, e.emoji)) {
                span.textContent = e.emoji;
                h.twemojify(span);
            }
            frag.appendChild(span);
        }
        host.appendChild(frag);
        return { destroy: () => host.replaceChildren() };
    }
    $effect(() => build(grid).destroy);
</script>
