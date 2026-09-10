<script>
    // One grid of emoji cells, rendered straight into the host grid element: stock emoji
    // (twemojified) and custom pack emoji (cached image); the big All grid is AllGrid.
    // The picker's delegated click handlers read the cells' data attributes and expect
    // them as the grid's direct children, so there is no wrapper here.
    let { items, h } = $props();   // h: twemojify(el), bindCachedImg(img, url, kind), stockTitle(e), scrollRoot()


    function stock(span, e) {
        span.textContent = e.emoji;
        h.twemojify(span);
    }
    function custom(img, url) { h.bindCachedImg(img, url, 'emoji'); }

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
