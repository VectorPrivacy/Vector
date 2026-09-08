<script>
    // A blocking card with a spinner and a line of text, faded in and out by class.
    import { processingState } from '../lib/overlays.svelte.js';
    const st = processingState();
</script>

<div id="processing-overlay" class="processing-overlay" class:visible={st.visible}>
    <div class="processing-overlay-card">
        <div class="processing-overlay-spinner"></div>
        <div class="processing-overlay-text">{st.message}</div>
    </div>
</div>
