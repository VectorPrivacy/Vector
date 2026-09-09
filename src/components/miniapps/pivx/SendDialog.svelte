<script>
    // Send to the open chat: quick send picks a whole promo, custom types an amount.
    import { pivxSend } from '../../lib/pivx.svelte.js';
    let { h } = $props();   // h: close(), confirm(), custom(), quick(), max()
    const st = pivxSend.state();
    const step = $derived(st.promos.length > 1 ? Math.min(0.06, 0.3 / (st.promos.length - 1)) : 0);
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="pivx-dialog-overlay" id="pivx-send-overlay" class:active={st.active} onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="pivx-dialog">
            <div class="pivx-dialog-header">
                <h3>Send PIVX</h3>
                <button class="pivx-dialog-close" id="pivx-send-close" onclick={h.close}>&times;</button>
            </div>
            <div class="pivx-dialog-content">
                {#if st.mode === 'quick'}
                    <div class="pivx-send-promo-section" id="pivx-send-promo-section">
                        <div class="pivx-send-promo-label">Select amount to send:</div>
                        <div class="pivx-send-promo-list" id="pivx-send-promo-list">
                            {#if st.loading}
                                <div class="pivx-send-promo-loading"><div class="pivx-spinner"></div><span>Loading...</span></div>
                            {:else if st.error}
                                <div class="pivx-send-promo-empty">{st.error}</div>
                            {:else if st.promos.length === 0}
                                <div class="pivx-send-promo-empty">No funds available to send.<br>Deposit PIVX first.</div>
                            {:else}
                                {#each st.promos as promo, i (promo.gift_code)}
                                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                                    <div class="pivx-send-promo-item animate-in" class:selected={st.selectedCode === promo.gift_code}
                                         style:animation-delay="{i * step}s" onclick={() => { st.selectedCode = promo.gift_code; }}>
                                        <span class="pivx-send-promo-item-amount">{promo.balance_piv.toFixed(2)} PIV</span>
                                        <span class="pivx-send-promo-item-code">{promo.gift_code}</span>
                                    </div>
                                {/each}
                            {/if}
                        </div>
                        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                        <div class="pivx-send-custom-toggle" id="pivx-send-custom-toggle" onclick={h.custom}>Send Custom Amount</div>
                    </div>
                {:else}
                    <div class="pivx-send-custom-section" id="pivx-send-custom-section">
                        <div class="pivx-send-custom-warning">
                            <span class="icon icon-info"></span>
                            Custom amounts require on-chain confirmation and may take longer to send.
                        </div>
                        <div class="pivx-amount-input-container">
                            <input type="number" id="pivx-send-amount" placeholder="0.00" step="0.01" min="0" bind:value={st.amount}>
                            <span class="pivx-amount-label">PIV</span>
                        </div>
                        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                        <div class="pivx-send-available" id="pivx-send-available" onclick={h.max}>
                            Available: <span id="pivx-send-available-amount">{st.available.toFixed(2)}</span> PIV
                            <span class="pivx-withdraw-max-btn" id="pivx-send-max">MAX</span>
                        </div>
                        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                        <div class="pivx-send-back-toggle" id="pivx-send-back-toggle" onclick={h.quick}>Back to Quick Send</div>
                    </div>
                {/if}
                <div class="pivx-send-info">Sending to: <span id="pivx-send-recipient">{st.recipient}</span></div>
                <button class="pivx-send-confirm-btn" id="pivx-send-confirm" class:loading={st.loading} disabled={st.loading || st.busy} onclick={h.confirm}>
                    {st.busy ? 'Sending...' : 'Send to Chat'}
                </button>
            </div>
        </div>
    </div>
{/if}
