<script>
    // The progress ring and cancel over a picture or video still uploading. Past 100%
    // there is nothing left to stop, so the button goes; if the publish then drags on,
    // a line under the ring says so rather than leaving a full ring with no explanation.
    import { uploadProgress, transferPublishing, transferSlow } from '../../lib/attachments.svelte.js';
    let { pendingId, size = 48, h } = $props();   // h: cancelUpload(pendingId)
    const pct = $derived(uploadProgress(pendingId));
    const publishing = $derived(transferPublishing(pendingId));
    const sending = $derived(transferSlow(pendingId));

    // Measured off the media: a thumbnail too small for a line under the ring gets none.
    let boxW = $state(0);
    let boxH = $state(0);
    const room = $derived(boxH >= 118 && boxW >= 120);
</script>

{#snippet ring()}
    <div class="miniapp-downloading-spinner" id="{pendingId}_file" style="width: {size}px; height: {size}px;" style:--progress={pct != null ? `${pct}%` : null}></div>
{/snippet}

<div class="attachment-progress-overlay" bind:clientWidth={boxW} bind:clientHeight={boxH}
     style="right: auto; bottom: auto; width: var(--media-w, 100%); height: var(--media-h, 100%);">
    {#if room}
        <div class="media-progress">
            {@render ring()}
            <span class="media-progress-note" class:is-visible={sending}>Sending</span>
        </div>
    {:else}
        <!-- No room for a note means the original DOM, so the overlay's own rule still
             keeps the ring round on media too small to hold it. -->
        {@render ring()}
    {/if}
    {#if !publishing}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="upload-cancel-btn" onclick={(e) => { e.stopPropagation(); h.cancelUpload(pendingId); }}></div>
    {/if}
</div>
