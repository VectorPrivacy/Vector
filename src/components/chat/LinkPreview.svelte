<script>
    // The OpenGraph card under a message with a link: favicon and title, the description
    // with its line breaks, the image. Both images come through the backend cache: the
    // linked host is attacker-chosen, and a raw src would be a clearnet fetch past Tor.
    let { data, h } = $props();   // data: linkPreviewData(msg); h: backendCachedImg(img, url), onThumbLoad(), openUrl(url)

    // Description text arrives with <br> and newline breaks; both render as line breaks.
    const lines = $derived((data.description || '').split(/<br\s*\/?>/i).flatMap((part, i, parts) => {
        const subs = part.split('\n');
        return i < parts.length - 1 ? [...subs, ''] : subs;
    }));

    let faviconHidden = $state(false);
    let imageHidden = $state(false);

    function cached(img, url) {
        h.backendCachedImg(img, url);
    }
    function onLoad(img) {
        if (img.isConnected) h.onThumbLoad();
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="dmsg-preview btn" url={data.url} style:padding-bottom={data.description && data.image ? '0' : null} onclick={() => h.openUrl(data.url)}>
    <span>
        <img class="favicon" alt="" style:display={faviconHidden ? 'none' : null} use:cached={data.favicon}
             onload={(e) => onLoad(e.currentTarget)} onerror={() => { faviconHidden = true; }}>{data.title}</span>
    {#if data.description}
        <span class="dmsg-preview-description" style:border-radius={data.image ? '0' : null}>
            {#each lines as line, i}{line}{#if i < lines.length - 1}<br>{/if}{/each}
        </span>
    {/if}
    {#if data.image && !imageHidden}
        <img class="dmsg-preview-img" alt="" use:cached={data.image}
             onload={(e) => onLoad(e.currentTarget)} onerror={() => { imageHidden = true; }}>
    {/if}
</div>
