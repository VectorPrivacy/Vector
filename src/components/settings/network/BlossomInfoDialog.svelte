<script>
    // One media server: whether it is enabled, what it has been seen to accept, and
    // remove (custom) or enable / disable (default).
    import { blossomInfoDialog } from '../../lib/network.svelte.js';
    import BlossomCaps from './BlossomCaps.svelte';
    let { h } = $props();   // h: close(), action(), formatBytes
    const st = blossomInfoDialog.state();
    const actionLabel = $derived(st.isCustom ? 'Remove Server' : (st.enabled ? 'Disable Server' : 'Enable Server'));
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="relay-dialog-overlay" class:active={st.active}
         onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="relay-dialog relay-info-dialog">
            <div class="relay-dialog-header">
                <h3>{st.url}</h3>
                <button class="relay-dialog-close" onclick={h.close}>&times;</button>
            </div>
            <div class="relay-dialog-content">
                <div class="relay-metrics-section">
                    <div class="relay-metrics-header">
                        <h4>Status</h4>
                        <span class="relay-status relay-status-small" class:connected={st.enabled} class:disabled={!st.enabled}>{st.enabled ? 'enabled' : 'disabled'}</span>
                    </div>
                </div>
                <div class="relay-metrics-section">
                    <div class="relay-metrics-header">
                        <h4>Capabilities</h4>
                    </div>
                    <p class="blossom-cap-blurb">
                        Servers don’t all accept the same files. Vector quietly tests them and remembers what each one handles, then routes every upload to the best-suited server automatically.
                    </p>
                    <div class="blossom-cap-slot"><BlossomCaps {h} /></div>
                </div>
                <div class="relay-dialog-buttons">
                    <button class="btn danger-btn" onclick={h.action}>{actionLabel}</button>
                    <button class="btn primary-btn" onclick={h.close}>Done</button>
                </div>
            </div>
        </div>
    </div>
{/if}
