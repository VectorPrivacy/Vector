<script>
    // Fullscreen QR overlay, shared by the Profile QR and the bunker login QR. Closes via
    // the button, a backdrop tap or Escape; the opener owns the back stack entry.
    import { qrOverlay } from '../lib/dialogs.svelte.js';
    import { popIn } from '../lib/popin.js';
    let { h } = $props();   // h: renderQr(host, text), close()
    const st = qrOverlay.state();
    function qr(host, text) {
        h.renderQr(host, text);
        return { update(next) { h.renderQr(host, next); } };
    }
</script>

<svelte:window onkeydown={(e) => { if (st.active && e.key === 'Escape') h.close(); }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div id="qr-overlay" class="qr-overlay" class:active={st.active} class:closing={st.closing}
     onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
    <div class="qr-overlay-card" use:popIn={st.pop}>
        <div class="qr-overlay-tile">
            <div id="qr-overlay-full" aria-label="QR code" use:qr={st.text}></div>
        </div>
        <button id="qr-overlay-close" class="qr-overlay-close" onclick={() => h.close()}>Close</button>
    </div>
</div>
