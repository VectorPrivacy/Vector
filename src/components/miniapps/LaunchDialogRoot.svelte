<script>
    import { launchDialogHandlers } from '../lib/dialogs.svelte.js';
    import LaunchDialog from './LaunchDialog.svelte';
    const h = $derived(launchDialogHandlers());
</script>

{#if h}<LaunchDialog {h} />{/if}
