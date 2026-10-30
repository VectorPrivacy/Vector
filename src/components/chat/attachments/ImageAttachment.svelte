<script>
    // A downloaded picture: the real image, or a thumbhash blur behind a Spoiler veil
    // until tapped. An upload in flight dims it under a progress ring.
    import UploadOverlay from './UploadOverlay.svelte';
    import FileBox from './FileBox.svelte';
    import { messageVersion } from '../../lib/chatview.svelte.js';
    import { mediaPath } from '../../lib/media.js';
    let { att, msg, ctx, sender, h } = $props();
    // h: assetUrl(path), displaySrc(path, dim), svgRender(path, dim), lost(att, msg), isSpoiler(att), thumbhash(chatId, msgId), onImageLoad(), onThumbLoad(), attachImagePreview(img),
    //    attachFileExtBadge(img, container, ext), cancelUpload

    const uploading = $derived.by(() => { messageVersion(msg.id); return !!(msg.mine && msg.pending); });
    // svelte-ignore state_referenced_locally
    const spoiler = h.isSpoiler(att);
    // Mount-time: a row's chat never changes under it. The blur is keyed by chat, not
    // author: an own message's author is not a participant of its DM.
    // svelte-ignore state_referenced_locally
    const chatId = h.openChat();
    // An SVG shows as the backend's render of it, never as its own markup; one that refuses to
    // render shows as a file.
    const isSvg = $derived(att.extension?.toLowerCase() === 'svg');
    let svgSrc = $state(null);
    let svgRefused = $state(false);
    $effect(() => {
        if (!isSvg) return;
        const path = mediaPath(att);
        let live = true;
        h.svgRender(path, 1024).then((src) => {
            if (!live) return;
            svgSrc = src;
            svgRefused = !src;
        });
        return () => { live = false; };
    });
    // A photo larger than a bubble ever draws (450 x 350 at up to 2x) shows a copy at that size:
    // WebKit decodes a picture whole however small it draws. The viewer opens the original.
    const DISPLAY_DIM = 1024;
    const original = $derived(isSvg ? null : h.assetUrl(mediaPath(att)));
    const sized = $derived(!isSvg && !uploading && Math.max(att.img_meta?.width || 0, att.img_meta?.height || 0) > DISPLAY_DIM);
    let display = $state(undefined);
    $effect(() => {
        if (!sized) return;
        display = undefined;
        let live = true;
        h.displaySrc(mediaPath(att), DISPLAY_DIM).then((src) => { if (live) display = src; }, () => { if (live) display = null; });
        return () => { live = false; };
    });
    const real = $derived(isSvg ? svgSrc : !sized ? original : display === undefined ? null : display ?? original);

    // Spoiler: the blur arrives async; a failed blur shows the real image instead.
    let blur = $state(null);
    let blurFailed = $state(false);
    let revealed = $state(false);
    if (spoiler) {
        // svelte-ignore state_referenced_locally
    h.thumbhash(chatId, msg.id).then((b64) => { blur = b64; }).catch(() => { blurFailed = true; });
    }
    const fit = $derived.by(() => {
        const m = att.img_meta;
        if (!m?.width || !m?.height) return null;
        const scale = Math.min(450 / m.width, 350 / m.height, 1);
        return { w: Math.round(m.width * scale), h: Math.round(m.height * scale), ratio: `${m.width} / ${m.height}` };
    });

    let container = $state(null);
    // The fullscreen previewer attaches once the picture is revealed; before that a click reveals.
    // A plain image attaches at once; a spoiler once revealed, so a click on the blur reveals.
    function preview(img, revealed = true) { if (revealed) h.attachImagePreview(img); return { update(r) { if (r) h.attachImagePreview(img); } }; }
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

{#if svgRefused}
    <FileBox {att} {msg} {sender} phase="downloaded" {h} />
{:else if spoiler && !blurFailed}
    <div style="position: relative; display: inline-block;" bind:this={container} data-spoiler-upload={uploading ? '1' : undefined} use:badgeOnly>
        {#if blur}
            <img class={revealed ? 'dmsg-image-attachment' : 'spoiler-img'} src={revealed ? real : blur} data-full={revealed ? original : undefined} onerror={() => { if (revealed) h.lost(att, msg); }} alt=""
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
        {#if isSvg}
            {#if real}<img data-attachment-type="svg" data-svg-path={mediaPath(att)} src={real} onerror={() => h.lost(att, msg)} alt="" style="width: 25vw; height: auto; border-radius: 8px;" style:opacity={uploading ? '0.25' : null} onload={() => h.onImageLoad()} use:preview use:badge use:pin>{/if}
        {:else}
            <img class="dmsg-image-attachment" src={real} data-full={original} onerror={() => h.lost(att, msg)} alt="" style="max-width: 100%; height: auto; border-radius: 8px;" style:opacity={uploading ? '0.25' : null} onload={() => h.onImageLoad()} use:preview use:badge use:pin>
        {/if}
        {#if uploading}
            <UploadOverlay pendingId={msg.id} {h} />
        {/if}
    </div>
{/if}
