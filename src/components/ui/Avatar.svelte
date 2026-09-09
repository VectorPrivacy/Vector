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

    let failed = $state(false);
    $effect(() => { src; failed = false; });

    const dims = $derived(size == null ? '' : `width:${size}px;height:${size}px;`);
    const box = $derived(size == null ? '' : `min-width:${size}px;min-height:${size}px;max-width:${size}px;max-height:${size}px;`);
</script>

{#if src && !failed}
    <img
        class={cls}
        {src}
        alt=""
        draggable="false"
        style="{dims}object-fit:cover;border-radius:50%;{style}"
        onerror={() => (failed = true)}
    />
{:else}
    <div class="placeholder-avatar {cls}" style="{box}background-image:url(&quot;icons/{group ? 'group' : 'user'}-placeholder.svg&quot;);background-size:cover;background-position:center;{style}"></div>
{/if}
