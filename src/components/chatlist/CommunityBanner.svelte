<script>
    // The community's banner over the foot of its channel pane (widescreen). The list keeps a
    // band of the banner's height at its end, so nothing is ever stuck behind it. In a list
    // long enough to need the room, scrolling away from either end fades the banner out and
    // lends the list the whole pane; at the top, and in its band at the bottom, it is there.
    import { paneState } from '../lib/signals.svelte.js';
    import { bannerState } from '../lib/banners.svelte.js';
    import BannerArt from '../community/BannerArt.svelte';

    let { h, scroller } = $props();   // h: openMenu(communityId, x, y); scroller: the channel list's scroll box

    const pane = paneState();
    const banners = bannerState();
    const id = $derived(pane.communityId);
    const src = $derived(id ? banners.src[id] : null);
    const shown = $derived(!!src && !banners.hidden.includes(id));

    let el = $state(null);
    let height = $state(0);

    $effect(() => {
        const list = scroller, node = el, band = height;
        if (!list || !node || !shown || !band) return;
        list.style.setProperty('--ws-banner-h', band + 'px');
        const sync = () => {
            const room = list.scrollHeight - list.clientHeight;
            let fade = 0;
            // A list that overflows by less than the banner only scrolls its last rows clear;
            // fading over a few pixels of that would flash the banner on and off.
            if (room >= band) {
                const span = Math.min(band * 0.7, room / 2);
                const away = Math.min(list.scrollTop, room - list.scrollTop);
                fade = Math.min(1, Math.max(0, away / span));
            }
            // The top goes first, at twice the pace, so the channels coming down meet it fading.
            node.style.setProperty('--banner-top', String(Math.max(0, 1 - fade * 2)));
            node.style.setProperty('--banner-bottom', String(1 - fade));
            node.classList.toggle('faded', fade > 0.5);
        };
        const later = () => requestAnimationFrame(sync);
        list.addEventListener('scroll', sync, { passive: true });
        const ro = new ResizeObserver(sync);
        ro.observe(list);
        const mo = new MutationObserver(later);
        mo.observe(list, { childList: true, subtree: true });
        sync();
        return () => {
            list.removeEventListener('scroll', sync);
            ro.disconnect();
            mo.disconnect();
            list.style.removeProperty('--ws-banner-h');
        };
    });
</script>

{#if shown}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="ws-banner" bind:this={el} bind:clientHeight={height}
         oncontextmenu={(e) => { e.preventDefault(); h.openMenu(id, e.clientX, e.clientY); }}>
        <BannerArt {src} />
    </div>
{/if}
