<script>
    // Fullscreen QR scanner (mobile). The camera stream and the decode loop live in
    // scan.js, which needs the video element itself; it is handed over once on mount.
    import { qrScanner } from '../lib/dialogs.svelte.js';
    let { h } = $props();   // h: video(el), close()
    const st = qrScanner;
    function video(el) { h.video(el); }
</script>

<svelte:window onkeydown={(e) => { if (st.active && e.key === 'Escape') h.close(); }} />

<div id="qr-scanner" class="qr-scanner" class:active={st.active}>
    <!-- svelte-ignore a11y_media_has_caption -->
    <video class="qr-scanner-video" class:live={st.live} autoplay playsinline muted use:video></video>
    <div class="qr-scanner-veil">
        <div class="qr-scanner-window"></div>
        <span class="qr-scanner-hint">Point at a Vector QR code</span>
    </div>
    <button id="qr-scanner-cancel" class="qr-overlay-close qr-scanner-cancel" onclick={() => h.close()}>Cancel</button>
</div>
