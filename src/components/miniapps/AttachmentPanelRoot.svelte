<script>
    // The attachment panel mounts once its owner has registered the bag.
    import { attachmentHandlers } from '../lib/attachmentpanel.svelte.js';
    import AttachmentPanel from './AttachmentPanel.svelte';
    const h = $derived(attachmentHandlers());
</script>

{#if h}<AttachmentPanel {h} />{/if}
