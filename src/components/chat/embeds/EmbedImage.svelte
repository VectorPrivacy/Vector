<script>
    // A picture fetched through the backend cache. WebKit draws an <img> with no source yet as
    // a broken image, so it stays out of sight behind a shimmer until it lands; one that never
    // arrives leaves nothing behind.
    let { url, h, cls = '', ratio = null, preview = false, onload = null } = $props();   // h: NostrEmbedHelpers
    let phase = $state('loading');   // loading | loaded | failed

    function bind(img) {
        h.backendCachedImg(img, url);
        if (preview) h.attachImagePreview(img);
    }
</script>

{#if phase === 'loading'}<span class="pack-skel ne-img-skel {cls}" style:aspect-ratio={ratio}></span>{/if}
{#if phase !== 'failed'}
    <img class={cls} class:ne-img-pending={phase === 'loading'} alt="" use:bind
         onload={() => { phase = 'loaded'; onload?.(); }} onerror={() => { phase = 'failed'; }}>
{/if}
