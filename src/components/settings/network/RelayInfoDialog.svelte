<script>
    // One relay's settings: status, ping and last check (refreshed by the opener every
    // second), the mode for a custom relay, the activity log, and disable or remove.
    import { relayInfoDialog } from '../../lib/network.svelte.js';
    import { popIn } from '../../lib/popin.js';
    import RelayLogs from './RelayLogs.svelte';
    let { h } = $props();   // h: close(), disable(), setMode(mode), copy()
    const st = relayInfoDialog.state();
    // A custom relay's button removes it but has always read "Disable".
    const disableLabel = $derived(st.isDefault && !st.enabled ? 'Enable' : 'Disable');
</script>

<svelte:window onkeydown={(e) => { if (st.active && e.key === 'Escape') h.close(); }} />

{#if st.active}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="pop-dialog-overlay" class:closing={st.closing}
         onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="relay-dialog relay-info-dialog pop-dialog" use:popIn={st.tick}>
            <div class="relay-dialog-content">
                <div class="relay-info-header-row">
                    <p id="relay-info-url" class="pop-dialog-host">{st.url}</p>
                    <span class="relay-status relay-status-small {st.status}">{st.status}</span>
                </div>
                <div class="relay-metrics-inline">
                    <span class="relay-metric-inline-label">Ping</span>
                    <span class="relay-metric-inline-value" style:color={st.pingColor}>{st.ping}</span>
                    <span class="relay-metric-inline-label relay-metric-inline-right">Last Check</span>
                    <span class="relay-metric-inline-value">{st.lastCheck}</span>
                </div>
                <div class="relay-form-group">
                    <label class="relay-form-label" for="relay-info-mode">Mode</label>
                    <select id="relay-info-mode" class="relay-form-select" value={st.mode} disabled={st.isDefault}
                            onchange={(e) => h.setMode(e.currentTarget.value)}>
                        <option value="both">Read & Write</option>
                        <option value="read">Read Only</option>
                        <option value="write">Write Only</option>
                    </select>
                </div>
                <div class="relay-logs-section">
                    <div class="relay-logs-header">
                        <h4>Recent Activity</h4>
                        <button class="relay-logs-copy-btn" title="Copy logs to clipboard" onclick={h.copy}>
                            <span class="icon" class:icon-copy={!st.copied} class:icon-check={st.copied}></span>
                        </button>
                    </div>
                    <ul class="relay-logs-list"><RelayLogs /></ul>
                </div>
                <div class="relay-dialog-buttons">
                    <button class="btn danger-btn" onclick={h.disable}>
                        <span class="icon icon-disable"></span>
                        {disableLabel}
                    </button>
                    <button class="btn cancel-btn" id="relay-info-done" onclick={h.close}>Close</button>
                </div>
            </div>
        </div>
    </div>
{/if}
