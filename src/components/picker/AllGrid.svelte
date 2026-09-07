<script>
    // Every stock emoji as ONE static span per cell, built in a fragment and twemojified
    // lazily in chunks as it scrolls into view. Deliberately not a keyed each: a reactive
    // node per cell for ~1900 cells (each with its own branch anchors) tripled the DOM and
    // made every interaction inside the picker stall for hundreds of milliseconds.
    let { grid, h } = $props();   // h: all(), twemojify(el), stockTitle(e), scrollRoot()

    const CHUNK = 36;   // 6 columns x 6 rows

    function build(host) {
        const items = h.all();
        const frag = document.createDocumentFragment();
        const leaders = [];
        items.forEach((e, i) => {
            const span = document.createElement('span');
            span.textContent = e.emoji;
            span.dataset.emoji = e.emoji;
            span.dataset.emojiTooltip = h.stockTitle(e);
            if (i % CHUNK === 0) { span.dataset.chunkIndex = String(leaders.length); leaders.push(span); }
            frag.appendChild(span);
        });
        host.appendChild(frag);
        const spans = [...host.querySelectorAll('span[data-emoji]')];
        const io = new IntersectionObserver((entries) => {
            for (const entry of entries) {
                if (!entry.isIntersecting) continue;
                const leader = entry.target;
                if (!leader.dataset.twemojified) {
                    leader.dataset.twemojified = '1';
                    const start = Number(leader.dataset.chunkIndex) * CHUNK;
                    for (let i = start; i < Math.min(start + CHUNK, spans.length); i++) h.twemojify(spans[i]);
                }
                io.unobserve(leader);
            }
        }, { root: h.scrollRoot(), rootMargin: '0px 0px 200px 0px' });
        leaders.forEach(l => io.observe(l));
        return { destroy: () => { io.disconnect(); host.replaceChildren(); } };
    }
    $effect(() => build(grid).destroy);
</script>
