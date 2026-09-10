<script>
    // The Add Custom Relay form: a domain (wss:// is added by the opener) and a mode.
    import { addRelayDialog } from '../../lib/network.svelte.js';
    let { h } = $props();   // h: close(), confirm({ url, mode })
    const st = addRelayDialog.state();
    function focus(node) { requestAnimationFrame(() => node.focus()); }
    function confirm() { h.confirm({ url: st.url, mode: st.mode }); }
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="relay-dialog-overlay" id="add-relay-overlay" class:active={st.active}
         onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="relay-dialog">
            <div class="relay-dialog-header">
                <h3>Add Custom Relay</h3>
                <button class="relay-dialog-close" onclick={h.close}>&times;</button>
            </div>
            <div class="relay-dialog-content">
                <div class="relay-form-group">
                    <label class="relay-form-label" for="add-relay-url">Relay URL</label>
                    <input type="text" id="add-relay-url" placeholder="relay.example.com" class="relay-form-input"
                           bind:value={st.url} use:focus onkeydown={(e) => { if (e.key === 'Enter') confirm(); }}>
                    <p class="relay-form-hint">Enter the relay domain (wss:// is added automatically)</p>
                </div>
                <div class="relay-form-group">
                    <label class="relay-form-label" for="add-relay-mode">Mode</label>
                    <select id="add-relay-mode" class="relay-form-select" style="margin-bottom: 0;" bind:value={st.mode}>
                        <option value="both">Read & Write</option>
                        <option value="read">Read Only</option>
                        <option value="write">Write Only</option>
                    </select>
                    <p class="relay-form-hint">Choose how Vector uses this relay.</p>
                </div>
                <div class="relay-dialog-buttons">
                    <button class="btn cancel-btn" onclick={h.close}>Cancel</button>
                    <button class="btn primary-btn" onclick={confirm}>Add Relay</button>
                </div>
            </div>
        </div>
    </div>
{/if}
