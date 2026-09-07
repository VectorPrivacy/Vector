<script>
    // The GIF grid: skeletons while a page loads, one item per result with its thumbhash
    // placeholder, media loaded lazily as items scroll in (the app's format fallback
    // chain), videos paused off-screen, and a skeleton tail while more loads.
    import { gifState, gifItems } from '../lib/gifs.svelte.js';

    let { grid, h } = $props();   // h: loadMedia(item, el, placeholderEl)

    const g = gifState();
    const items = $derived(gifItems());

    // One observer for the grid, made before any cell mounts: loads media on first sight,
    // plays / pauses after.
    // svelte-ignore state_referenced_locally
    const io = new IntersectionObserver((entries) => {
            for (const entry of entries) {
                const el = entry.target;
                if (entry.isIntersecting && !el.dataset.mediaLoaded) {
                    el.dataset.mediaLoaded = 'true';
                    h.loadMedia(el.__gif, el, el.querySelector('.gif-placeholder'));
                }
                const video = el.querySelector('video');
                if (video && video.dataset.ready) {
                    if (entry.isIntersecting) video.play().catch(() => {}); else video.pause();
                }
            }
    }, { root: grid, rootMargin: '0px 0px 200px 0px', threshold: 0.1 });
    $effect(() => () => io.disconnect());
    function watch(el, item) {
        el.__gif = item;
        io.observe(el);
        return { destroy: () => io.unobserve(el) };
    }
</script>

{#if g.phase === 'loading'}
    {#each Array(g.pageSize) as _, i (i)}<div class="gif-item gif-skeleton"></div>{/each}
{:else if g.phase === 'empty'}
    <div class="gif-empty-state" style="grid-column: 1 / -1;"><span class="icon icon-image"></span><span>{g.message}</span></div>
{:else if g.phase === 'ok'}
    {#each items as item (item.id)}
        <div class="gif-item" data-gif-id={item.id} data-gif-title={item.title || ''} use:watch={item}>
            <div class="gif-placeholder" style:background-image={item.thumb ? `url(${item.thumb})` : null} style:background-size={item.thumb ? 'cover' : null}><span class="loading-spinner"></span></div>
        </div>
    {/each}
    {#if g.loadingMore}
        {#each Array(g.pageSize) as _, i ('more' + i)}<div class="gif-item gif-skeleton gif-loading-more"></div>{/each}
    {/if}
{/if}
