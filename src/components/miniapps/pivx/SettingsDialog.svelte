<script>
    // Wallet settings: the auto-withdraw address and the display currency.
    import { pivxSettings } from '../../lib/pivx.svelte.js';
    let { h } = $props();   // h: close(), save()
    const st = pivxSettings.state();
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="pivx-dialog-overlay" class:active={st.active} onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="pivx-dialog">
            <div class="pivx-dialog-header">
                <h3>PIVX Wallet Settings</h3>
                <button class="pivx-dialog-close" onclick={h.close}>&times;</button>
            </div>
            <div class="pivx-dialog-content">
                <div class="pivx-settings-section">
                    <label class="pivx-settings-label" for="pivx-wallet-address-input">Auto Withdraw Address</label>
                    <input type="text" id="pivx-wallet-address-input" placeholder="D..." class="pivx-settings-input" bind:value={st.address}>
                    <p class="pivx-settings-hint">If set, claimed PIVX will be sent directly to this address. Leave empty to keep funds in your Vector wallet.</p>
                </div>
                <div class="pivx-settings-section">
                    <label class="pivx-settings-label" for="pivx-currency-select">Display Currency</label>
                    <select id="pivx-currency-select" class="pivx-settings-select" disabled={st.currenciesLoading} bind:value={st.currency}>
                        {#if st.currenciesLoading}
                            <option value="">Loading...</option>
                        {:else}
                            {#each st.currencies as c (c)}<option value={c}>{c}</option>{/each}
                        {/if}
                    </select>
                    <p class="pivx-settings-hint">Choose your preferred fiat currency for balance display.</p>
                </div>
                <button class="pivx-settings-save-btn" onclick={h.save}>Save Settings</button>
            </div>
        </div>
    </div>
{/if}
