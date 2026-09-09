<script>
    // A picture not on disk yet: its thumbhash blur sized to the box the real image will
    // take, under a download ring (downloading or auto-download) or a Download button.
    // A missing blur falls back to the file box leaf.
    import { downloadProgress } from '../../lib/attachments.svelte.js';
    import FileBox from './FileBox.svelte';
    import { messageVersion } from '../../lib/chatview.svelte.js';
    let { att, msg, ctx, sender, auto, h } = $props();
    // h: thumbhash(npub, msgId), onThumbLoad(), formatBytes, startDownload(att, msg, sender), openChat(), and FileBox's

    // Mount-time: a row's chat and author never change under it.
    // svelte-ignore state_referenced_locally
    const npub = ctx.isGroupChat ? h.openChat() : (sender?.id || h.openChat());
    const downloading = $derived.by(() => { messageVersion(msg.id); return !!att.downloading || auto || started; });
    let started = $state(false);
    let blur = $state(null);
    let blurFailed = $state(false);
    // svelte-ignore state_referenced_locally
    h.thumbhash(npub, msg.id).then((b64) => { blur = b64; }).catch(() => { blurFailed = true; });

    const fit = $derived.by(() => {
        const m = att.img_meta;
        if (!m?.width || !m?.height) return null;
        const scale = Math.min(450 / m.width, 350 / m.height, 1);
        return { w: Math.round(m.width * scale), h: Math.round(m.height * scale), ratio: `${m.width} / ${m.height}` };
    });
    const pct = $derived(downloadProgress(att.id));
    // A failed download carries the backend's reason, so a red box is diagnosable at a glance.
    const label = $derived.by(() => {
        if (att.download_failed) {
            const reason = (att.download_error || '').slice(0, 64);
            return reason ? `Failed: ${reason} · Tap to Retry` : 'Download Failed · Tap to Retry';
        }
        return `Download ${(att.extension || '').toUpperCase()} (${att.size > 0 ? h.formatBytes(att.size) : 'Unknown Size'})`;
    });

    function download(e) {
        e.stopPropagation();
        if (started) return;
        started = true;
        h.startDownload(att, msg, sender);
    }
</script>

{#if blurFailed}
    <FileBox {att} {msg} {sender} phase={downloading ? 'downloading' : 'download'} {h} />
{:else if blur}
    <div style="position: relative; display: inline-block;" style:line-height={downloading ? '0' : null}>
        <img src={blur} alt="" width={fit?.w} height={fit?.h}
             style="max-width: min(100%, 450px); max-height: 350px; height: auto; border-radius: 8px;"
             style:aspect-ratio={fit?.ratio ?? null}
             style:opacity={downloading ? (att.downloading ? '0.7' : '0.8') : '0.6'}
             onload={() => h.onThumbLoad()}>
        {#if downloading}
            <div class="attachment-progress-overlay">
                <div class="miniapp-downloading-spinner" data-attachment-id={att.id} style="width: 48px; height: 48px;" style:--progress={pct != null ? `${pct}%` : null}></div>
            </div>
        {:else}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <i class="btn" data-attachment-id={att.id} onclick={download}
               style="position:absolute;top:50%;left:50%;transform:translate(-50%,-50%);background-color:rgba(0,0,0,0.8);padding:8px 15px;border-radius:6px;color:white;cursor:pointer;font-size:12px;white-space:nowrap;text-align:center;max-width:90%;overflow:hidden;text-overflow:ellipsis;">{label}</i>
        {/if}
    </div>
{/if}
