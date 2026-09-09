<script>
    // The blur and brightness sliders, in the chat's flex flow directly above the composer
    // so it stays visible while they are tuned. The fill variable keeps WebKit's track in
    // step with the thumb.
    import { wallpaperState } from '../lib/wallpaper.svelte.js';
    import { chatHeaderHandlers } from '../lib/chatpane.svelte.js';
    const wp = wallpaperState();
    const pct = (val, min, max) => `${Math.max(0, Math.min(100, ((val - min) / (max - min)) * 100))}%`;
    function input(field, e) {
        wp[field] = parseInt(e.currentTarget.value, 10);
        chatHeaderHandlers()?.wallpaperSliderInput();
    }
</script>

<div class="wallpaper-preview-bar" style:display={wp.previewing ? null : 'none'}>
    <div class="wallpaper-preview-sliders">
        <label class="wallpaper-slider" title="Blur">
            <span class="icon icon-eye-off wallpaper-slider-icon"></span>
            <input type="range" min="0" max="30" step="1" value={wp.blur} style:--slider-pct={pct(wp.blur, 0, 30)} oninput={(e) => input('blur', e)}>
        </label>
        <label class="wallpaper-slider" title="Brightness">
            <span class="icon icon-bulb wallpaper-slider-icon"></span>
            <input type="range" min="10" max="100" step="1" value={wp.dim} style:--slider-pct={pct(wp.dim, 10, 100)} oninput={(e) => input('dim', e)}>
        </label>
    </div>
</div>
