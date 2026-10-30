<script>
    // Sits behind every other chat surface and absorbs the blur + brightness filter so
    // messages stay crisp. The image rides a CSS variable the stylesheet paints.
    import { wallpaperState, wallpaperShown } from '../lib/wallpaper.svelte.js';
    import { moving } from '../lib/motion.js';
    const wp = wallpaperState();
    const image = $derived(wallpaperShown());
    const src = $derived(image.match(/^url\("(.*)"\)$/)?.[1] ?? null);
    let shown = $state(null);
    const painted = $derived(src ? (shown ? `url("${shown}")` : '') : image);
</script>

<div class="chat-wallpaper-layer" style:--wp-image={painted || null} style:filter={painted ? wp.filter || null : null} use:moving={{ src, paint: (s) => shown = s }}></div>
