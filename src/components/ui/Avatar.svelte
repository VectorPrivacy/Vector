<script>
    // A round avatar: the real image as a direct <img> (a direct flex child, so no inline
    // baseline gap and no nested-in-a-div WKWebView re-composite flicker), or the app's
    // placeholder when there is no source or the image fails to load. `size` null leaves
    // the sizing to the stylesheet.
    let {
        src = null,
        size = 25,
        group = false,
        class: cls = '',
        style = '',
    } = $props();

    import { avatarFallback } from '../lib/avatar.js';

    let failed = $state(false);
    let fell = $state(false);   // the thumb failed; showing the original
    $effect(() => { src; failed = false; fell = false; });
    const shown = $derived(fell ? avatarFallback(src) : src);
    function onError() {
        if (!fell && avatarFallback(src)) fell = true;
        else failed = true;
    }

    const dims = $derived(size == null ? '' : `width:${size}px;height:${size}px;`);
    const box = $derived(size == null ? '' : `min-width:${size}px;min-height:${size}px;max-width:${size}px;max-height:${size}px;`);
</script>

{#if src && !failed}
    <img
        class={cls}
        src={shown}
        alt=""
        draggable="false"
        style="{dims}object-fit:cover;border-radius:50%;{style}"
        onerror={onError}
    />
{:else}
    <div class="placeholder-avatar {cls}" style="{box}background-image:url(&quot;icons/{group ? 'group' : 'user'}-placeholder.svg&quot;);background-size:cover;background-position:center;{style}"></div>
{/if}
