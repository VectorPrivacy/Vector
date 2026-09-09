<script>
    // Withdraw to an on-chain address.
    import { pivxWithdraw } from '../../lib/pivx.svelte.js';
    let { h } = $props();   // h: close(), confirm(), max()
    const st = pivxWithdraw.state();
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="pivx-dialog-overlay" id="pivx-withdraw-overlay" class:active={st.active} onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="pivx-dialog">
            <div class="pivx-dialog-header">
                <h3>Withdraw PIVX</h3>
                <button class="pivx-dialog-close" id="pivx-withdraw-close" onclick={h.close}>&times;</button>
            </div>
            <div class="pivx-dialog-content">
                <div class="pivx-withdraw-section">
                    <label class="pivx-settings-label" for="pivx-withdraw-address">Destination Address</label>
                    <input type="text" id="pivx-withdraw-address" placeholder="D..." class="pivx-settings-input" bind:value={st.address}>
                </div>
                <div class="pivx-withdraw-section">
                    <label class="pivx-settings-label" for="pivx-withdraw-amount">Amount</label>
                    <div class="pivx-amount-input-container">
                        <input type="number" id="pivx-withdraw-amount" placeholder="0.00" step="0.01" min="0" bind:value={st.amount}>
                        <span class="pivx-amount-label">PIV</span>
                    </div>
                    <div class="pivx-withdraw-available">
                        Available: <span id="pivx-withdraw-available-amount">{st.available.toFixed(2)}</span> PIV
                        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                        <span class="pivx-withdraw-max-btn" id="pivx-withdraw-max" onclick={h.max}>MAX</span>
                    </div>
                </div>
                <div class="pivx-withdraw-fee-info" id="pivx-withdraw-fee-info">Network fee: ~0.0001 PIV</div>
                <button class="pivx-settings-save-btn" id="pivx-withdraw-confirm" disabled={st.busy} onclick={h.confirm}>
                    {st.busy ? 'Withdrawing...' : 'Withdraw'}
                </button>
            </div>
        </div>
    </div>
{/if}
