<script>
    // A downloaded picture: the real image, or a thumbhash blur behind a Spoiler veil
    // until tapped. An upload in flight dims it under a progress ring.
    import UploadOverlay from './UploadOverlay.svelte';
    let { att, msg, ctx, sender, h } = $props();
    // h: assetUrl(path), isSpoiler(att), thumbhash(npub, msgId), onImageLoad(), onThumbLoad(), attachImagePreview(img),
    //    attachFileExtBadge(img, container, ext), cancelUpload

    const uploading = $derived(msg.mine && msg.pending);
    // svelte-ignore state_referenced_locally
    const spoiler = h.isSpoiler(att);
    // Mount-time: a row's chat and author never change under it.
    // svelte-ignore state_referenced_locally
    const npub = ctx.isGroupChat ? h.openChat() : (sender?.id || h.openChat());
    const real = $derived(h.assetUrl(att.path));

    // Spoiler: the blur arrives async; a failed blur shows the real image instead.
    let blur = $state(null);
    let blurFailed = $state(false);
    let revealed = $state(false);
    if (spoiler) {
        // svelte-ignore state_referenced_locally
    h.thumbhash(npub, msg.id).then((b64) => { blur = b64; }).catch(() => { blurFailed = true; });
    }
    const fit = $derived.by(() => {
        const m = att.img_meta;
        if (!m?.width || !m?.height) return null;
        const scale = Math.min(450 / m.width, 350 / m.height, 1);
        return { w: Math.round(m.width * scale), h: Math.round(m.height * scale), ratio: `${m.width} / ${m.height}` };
    });

    let container = $state(null);
    function preview(img) { h.attachImagePreview(img); }
    function badge(img) { h.attachFileExtBadge(img, container, att.extension); }
    function badgeOnly(node) { h.attachFileExtBadge(null, container, att.extension); }
    // Uploading media pins its wrapper to the rendered width so the ring centres on it.
    // The ring covers the media, not the wrapper: the overlay is sized to the media's rendered
    // box for as long as it is on screen. Pinning the wrapper instead would cap the media
    // through its percentage max-width and freeze it at its pre-metadata size.
    function pin(media) {
        const set = () => {
            container.style.setProperty('--media-w', media.offsetWidth + 'px');
            container.style.setProperty('--media-h', media.offsetHeight + 'px');
        };
        const ro = new ResizeObserver(set);
        ro.observe(media);
        return { destroy: () => ro.disconnect() };
    }
</script>

{#if spoiler && !blurFailed}
    <div style="position: relative; display: inline-block;" bind:this={container} data-spoiler-upload={uploading ? '1' : undefined} use:badgeOnly>
        {#if blur}
            <img class={revealed ? 'dmsg-image-attachment' : 'spoiler-img'} src={revealed ? real : blur} alt=""
                 width={!revealed && fit ? fit.w : undefined} height={!revealed && fit ? fit.h : undefined}
                 style="max-width: 100%; height: auto; border-radius: 8px;"
                 style:aspect-ratio={!revealed && fit ? fit.ratio : null}
                 style:opacity={uploading ? '0.25' : null}
                 onload={() => h.onThumbLoad()}
                 use:preview={revealed}>
            {#if uploading}
                <UploadOverlay pendingId={msg.id} {h} />
            {:else if !revealed}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="spoiler-overlay" onclick={() => { revealed = true; }}><span class="icon icon-eye-off"></span><span class="spoiler-label">Spoiler</span></div>
            {/if}
        {/if}
    </div>
{:else}
    <div style="position: relative; display: inline-block; line-height: 0; max-width: 100%;" bind:this={container}>
        {#if att.extension === 'svg'}
            <img data-attachment-type="svg" src={real} alt="" style="width: 25vw; height: auto; border-radius: 8px;" style:opacity={uploading ? '0.25' : null} onload={() => h.onImageLoad()} use:preview use:badge use:pin>
        {:else}
            <img class="dmsg-image-attachment" src={real} alt="" style="max-width: 100%; height: auto; border-radius: 8px;" style:opacity={uploading ? '0.25' : null} onload={() => h.onImageLoad()} use:preview use:badge use:pin>
        {/if}
        {#if uploading}
            <UploadOverlay pendingId={msg.id} {h} />
        {/if}
    </div>
{/if}
