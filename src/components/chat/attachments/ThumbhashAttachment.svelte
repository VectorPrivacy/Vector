<script>
    // A picture not on disk yet: its thumbhash blur sized to the box the real image will
    // take, under a download ring (downloading or auto-download) or a Download box.
    // A missing blur falls back to the file box leaf.
    //
    // Both overlays size themselves off the rendered blur, because a thumbnail can be
    // anything from a wide banner to a sliver: the box drops its text when there is no
    // room for it, and the ring shows a rate only when one will fit under it.
    import { downloadProgress, transfer, transferStageText } from '../../lib/attachments.svelte.js';
    import FileBox from './FileBox.svelte';
    import { messageVersion } from '../../lib/chatview.svelte.js';
    let { att, msg, ctx, sender, auto, h } = $props();
    // h: thumbhash(chatId, msgId), onThumbLoad(), formatBytes, startDownload(att, msg, sender), openChat(), and FileBox's

    // Mount-time: a row's chat never changes under it. The blur is keyed by chat, not
    // author: an own message's author is not a participant of its DM.
    const chatId = h.openChat();
    const downloading = $derived.by(() => { messageVersion(msg.id); return !!att.downloading || auto || started; });
    let started = $state(false);
    let blur = $state(null);
    let blurFailed = $state(false);
    // svelte-ignore state_referenced_locally
    h.thumbhash(chatId, msg.id).then((b64) => { blur = b64; }).catch(() => { blurFailed = true; });

    const fit = $derived.by(() => {
        const m = att.img_meta;
        if (!m?.width || !m?.height) return null;
        const scale = Math.min(450 / m.width, 350 / m.height, 1);
        return { w: Math.round(m.width * scale), h: Math.round(m.height * scale), ratio: `${m.width} / ${m.height}` };
    });

    // Measured, not derived from the metadata: the blur is capped by the column it lands
    // in, and a thumbnail with no metadata at all still has to place its overlay.
    let boxW = $state(0);
    let boxH = $state(0);
    // The full box measures 191x68 at its widest wording and sits at 94% of the media,
    // so below this it is the icon alone with no panel around it.
    const compact = $derived(boxW > 0 && (boxW < 205 || boxH < 76));
    // The ring plus a line under it, with air around both.
    const showRate = $derived(boxH >= 118 && boxW >= 120);

    const pct = $derived(downloadProgress(att.id));
    const rate = $derived.by(() => {
        const stage = transferStageText(att.id);
        if (stage) return stage;
        const t = transfer(att.id);
        if (!t || !(t.bps > 0)) return '';
        return `${h.formatBytes(t.bps, t.bps >= 1048576 ? 2 : 0, true)}/s`;
    });

    // Part of the file is already here: the next download picks up from it.
    const paused = $derived.by(() => { messageVersion(msg.id); return h.pausedAt?.(att) || null; });
    const failed = $derived(!paused && !!att.download_failed);
    const title = $derived(paused ? 'Resume Download' : failed ? 'Download Failed' : `Download ${(att.extension || '').toUpperCase()}`.trim());
    // A failed download carries the backend's reason, so a red box is diagnosable at a glance.
    const sub = $derived.by(() => {
        if (paused) return `${h.formatBytes(paused.offset)}${paused.total ? ` of ${h.formatBytes(paused.total)}` : ''}`;
        if (failed) return (att.download_error || '').slice(0, 64) || 'Tap to Retry';
        return att.size > 0 ? h.formatBytes(att.size) : 'Unknown Size';
    });

    function download(e) {
        e.stopPropagation();
        if (started) return;
        started = true;
        h.startDownload(att, msg, sender);
    }
</script>

{#snippet ring()}
    <div class="miniapp-downloading-spinner" data-attachment-id={att.id} style="width: 48px; height: 48px;" style:--progress={pct != null ? `${pct}%` : null}></div>
{/snippet}

{#if blurFailed}
    <FileBox {att} {msg} {sender} phase={downloading ? 'downloading' : 'download'} {h} />
{:else if blur}
    <div style="position: relative; display: inline-block;" style:line-height={downloading ? '0' : null}>
        <img src={blur} alt="" width={fit?.w} height={fit?.h}
             bind:clientWidth={boxW} bind:clientHeight={boxH}
             style="max-width: min(100%, 450px); max-height: 350px; height: auto; border-radius: 8px;"
             style:aspect-ratio={fit?.ratio ?? null}
             style:opacity={downloading ? (att.downloading ? '0.7' : '0.8') : '0.6'}
             onload={() => h.onThumbLoad()}>
        {#if downloading}
            <div class="attachment-progress-overlay">
                <!-- No rate to show means the original DOM, so the overlay's own rule still
                     keeps the ring round on media too small to hold it. -->
                {#if showRate}
                    <div class="media-progress">
                        {@render ring()}
                        <span class="media-progress-note" class:is-visible={!!rate}>{rate}</span>
                    </div>
                {:else}
                    {@render ring()}
                {/if}
            </div>
        {:else}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="thumb-download" class:is-compact={compact} class:is-failed={failed}
                 data-attachment-id={att.id} role="button" tabindex="-1" title={compact ? `${title} · ${sub}` : null}
                 onclick={download}>
                <span class="thumb-download-icon"><span class="icon icon-{failed ? 'refresh' : 'download'}"></span></span>
                {#if !compact}
                    <span class="thumb-download-text">
                        <span class="thumb-download-title cutoff">{title}</span>
                        <span class="thumb-download-sub cutoff">{sub}</span>
                    </span>
                {/if}
            </div>
        {/if}
    </div>
{/if}
