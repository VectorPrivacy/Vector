<script>
    // One media server: whether it is enabled, what it will do for this account, and
    // remove (custom) or enable / disable (default). A server that publishes its own
    // document speaks for itself; otherwise the view is what uploads have taught us.
    import { blossomInfoDialog, blossomInfoState } from '../../lib/network.svelte.js';
    import BlossomCaps from './BlossomCaps.svelte';
    import BlossomAccount from './BlossomAccount.svelte';
    let { h } = $props();   // h: close(), action(), formatBytes
    const st = blossomInfoDialog.state();
    const doc = blossomInfoState();
    const actionLabel = $derived(st.isCustom ? 'Remove Server' : (st.enabled ? 'Disable Server' : 'Enable Server'));
    const personalised = $derived(doc.status === 'ok' && doc.info && doc.info.caller);
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
                {#if doc.status === 'loading'}
                    <div class="relay-metrics-section">
                        <span style="opacity: 0.6;">Asking the server…</span>
                    </div>
                {:else if personalised}
                    <BlossomAccount info={doc.info} {h} />
                {:else}
                    {#if doc.status === 'ok' && doc.info}
                        <BlossomAccount info={doc.info} {h} />
                    {/if}
                    <div class="relay-metrics-section">
                        <div class="relay-metrics-header">
                            <h4>What we’ve learned</h4>
                        </div>
                        <p class="blossom-cap-blurb">
                            This server doesn’t say what it accepts, so Vector learns as you send: the largest file it has taken of each type, and the types it has refused. Uploads are routed to the best-suited server automatically.
                        </p>
                        <div class="blossom-cap-slot"><BlossomCaps {h} /></div>
                    </div>
                {/if}
                <div class="relay-dialog-buttons">
                    <button class="btn danger-btn" onclick={h.action}>{actionLabel}</button>
                    <button class="btn primary-btn" onclick={h.close}>Done</button>
                </div>
            </div>
        </div>
    </div>
{/if}
