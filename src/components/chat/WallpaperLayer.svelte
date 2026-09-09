<script>
    // Sits behind every other chat surface and absorbs the blur + brightness filter so
    // messages stay crisp. The image rides a CSS variable the stylesheet paints.
    import { wallpaperState } from '../lib/wallpaper.svelte.js';
    const wp = wallpaperState();
</script>

<div class="chat-wallpaper-layer" style:--wp-image={wp.image || null} style:filter={wp.filter || null}></div>
