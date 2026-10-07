<script>
    // Sits behind every other chat surface and absorbs the blur + brightness filter so
    // messages stay crisp. The image rides a CSS variable the stylesheet paints.
    import { wallpaperState, wallpaperShown } from '../lib/wallpaper.svelte.js';
    const wp = wallpaperState();
    const image = $derived(wallpaperShown());
</script>

<div class="chat-wallpaper-layer" style:--wp-image={image || null} style:filter={image ? wp.filter || null : null}></div>
