<script>
    // A round avatar: the real image as a direct <img> (a direct flex child, so no inline
    // baseline gap and no nested-in-a-div WKWebView re-composite flicker), or the app's
    // default-avatar element when there is no source or the image fails to load.
    let {
        src = null,
        size = 25,
        class: cls = '',
        style = '',
        placeholder = () => document.createElement('div'), // () => the default-avatar element
    } = $props();

    let failed = $state(false);
    $effect(() => { src; failed = false; });

    function placeholderInto(node) {
        node.replaceChildren(placeholder());
    }
</script>

{#if src && !failed}
    <img
        class={cls}
        {src}
        alt=""
        style="width:{size}px;height:{size}px;object-fit:cover;border-radius:50%;{style}"
        onerror={() => (failed = true)}
    />
{:else}
    <div class={cls} {style} use:placeholderInto></div>
{/if}
