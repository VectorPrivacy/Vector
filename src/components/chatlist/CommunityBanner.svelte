<script>
    // The community's banner, under its header at the top of the channel pane (widescreen).
    import { paneState } from '../lib/signals.svelte.js';
    import { bannerState } from '../lib/banners.svelte.js';
    import BannerArt from '../community/BannerArt.svelte';

    let { h } = $props();   // h: openMenu(communityId, x, y)

    const pane = paneState();
    const banners = bannerState();
    const id = $derived(pane.communityId);
    const src = $derived(id ? banners.src[id] : null);
    const shown = $derived(!!src && !banners.hidden.includes(id));
</script>

{#if shown}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="ws-banner"
         oncontextmenu={(e) => { e.preventDefault(); h.openMenu(id, e.clientX, e.clientY); }}>
        <BannerArt {src} />
    </div>
{/if}
