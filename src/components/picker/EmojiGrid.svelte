<script>
    // One grid of emoji cells, rendered straight into the host grid element: stock emoji
    // (twemojified, lazily in chunks for the big grid) and custom pack emoji (cached image).
    // The picker's delegated click handlers read the cells' data attributes and expect
    // them as the grid's direct children, so there is no wrapper here.
    let { items, grid, lazy = false, h } = $props();   // h: twemojify(el), bindCachedImg(img, url, kind), stockTitle(e), scrollRoot()

    const CHUNK = 36;   // 6 columns x 6 rows

    function stock(span, e) {
        span.textContent = e.emoji;
        if (!lazy) h.twemojify(span);
    }
    function custom(img, url) { h.bindCachedImg(img, url, 'emoji'); }

    // Every span exists up front (cheap text) so the scroll height is right; twemoji runs
    // per chunk as its first span scrolls into view.
    $effect(() => {
        if (!lazy) return;
        const spans = [...grid.querySelectorAll('span[data-emoji]')];
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
        spans.forEach((s, i) => { if (i % CHUNK === 0) { s.dataset.chunkIndex = String(i / CHUNK); io.observe(s); } });
        return () => io.disconnect();
    });
</script>

{#each items as item, i (item.isCustom ? 'c:' + item.shortcode : 's:' + item.emoji + ':' + i)}
    {#if item.isCustom}
        <span class="emoji-pack-emoji" data-pack-shortcode={item.shortcode} data-pack-url={item.url} data-emoji-tooltip=":{item.shortcode}:">
            <img alt=":{item.shortcode}:" use:custom={item.url}>
        </span>
    {:else}
        <span data-emoji={item.emoji} data-emoji-tooltip={h.stockTitle(item)} use:stock={item}></span>
    {/if}
{/each}
