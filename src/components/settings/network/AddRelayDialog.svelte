<script>
    // The Add Custom Relay form: a domain (the opener adds wss://, or ws:// for an .i2p or .onion host) and a mode.
    import { addRelayDialog } from '../../lib/network.svelte.js';
    let { h } = $props();   // h: close(), confirm({ url, mode })
    const st = addRelayDialog.state();
    const host = $derived(st.url.trim().replace(/^wss?:\/\//i, '').split(/[/:?#]/)[0]);
    // A relay inside one network: only people on it can reach the relay.
    const inside = $derived(/\.i2p$/i.test(host) ? 'I2P' : /\.onion$/i.test(host) ? 'Tor' : '');
    function focus(node) { requestAnimationFrame(() => node.focus()); }
    function confirm() { h.confirm({ url: st.url, mode: st.mode }); }
    const MODES = [{ value: 'both', label: 'Read & Write' }, { value: 'read', label: 'Read' }, { value: 'write', label: 'Write' }];
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="relay-dialog-overlay" class:active={st.active}
         onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="relay-dialog add-relay">
            <div class="relay-dialog-header">
                <h3>Add Relay</h3>
                <button class="relay-dialog-close" aria-label="Close" onclick={h.close}>&times;</button>
            </div>
            <div class="add-relay-body">
                <input type="text" class="relay-form-input add-relay-url" placeholder="relay.example.com" aria-label="Relay address"
                       inputmode="url" autocapitalize="none" autocorrect="off" spellcheck="false"
                       bind:value={st.url} use:focus onkeydown={(e) => { if (e.key === 'Enter') confirm(); }}>
                {#if inside}
                    <p class="relay-form-hint" class:i2p-hint={inside === 'I2P'} class:tor-hint={inside === 'Tor'}>Only people using {inside} can send to you on this relay.</p>
                {/if}
                <div class="add-relay-modes" role="radiogroup" aria-label="Mode">
                    {#each MODES as m (m.value)}
                        <button type="button" role="radio" aria-checked={st.mode === m.value} class:on={st.mode === m.value}
                                onclick={() => { st.mode = m.value; }}>{m.label}</button>
                    {/each}
                </div>
                <div class="relay-dialog-buttons">
                    <button class="btn cancel-btn" onclick={h.close}>Cancel</button>
                    <button class="btn primary-btn" onclick={confirm}>Add</button>
                </div>
            </div>
        </div>
    </div>
{/if}
