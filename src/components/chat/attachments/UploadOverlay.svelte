<script>
    // The progress ring and cancel over a picture or video still uploading.
    import { uploadProgress } from '../../lib/attachments.svelte.js';
    let { pendingId, size = 48, h } = $props();   // h: cancelUpload(pendingId)
    const pct = $derived(uploadProgress(pendingId));
</script>

<div class="attachment-progress-overlay">
    <div class="miniapp-downloading-spinner" id="{pendingId}_file" style="width: {size}px; height: {size}px;" style:--progress={pct != null ? `${pct}%` : null}></div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="upload-cancel-btn" onclick={(e) => { e.stopPropagation(); h.cancelUpload(pendingId); }}></div>
</div>
