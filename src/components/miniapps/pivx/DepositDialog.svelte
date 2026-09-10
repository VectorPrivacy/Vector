<script>
    // Deposit: the fresh promo address, copy, and the polling status (spinner, then the tick).
    import { pivxDeposit } from '../../lib/pivx.svelte.js';
    let { h } = $props();   // h: close(), copy()
    const st = pivxDeposit.state();
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="pivx-dialog-overlay" class:active={st.active} onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="pivx-dialog">
            <div class="pivx-dialog-header">
                <h3>Deposit PIVX</h3>
                <button class="pivx-dialog-close" onclick={h.close}>&times;</button>
            </div>
            <div class="pivx-dialog-content">
                <p class="pivx-deposit-instructions">Send PIVX to this address to deposit:</p>
                <div class="pivx-address-display">{st.address}</div>
                <button class="pivx-copy-btn" onclick={h.copy}>
                    <span class="icon icon-copy"></span>
                    Copy Address
                </button>
                <div class="pivx-deposit-status" id="pivx-deposit-status">
                    {#if st.received > 0}
                        <div class="pivx-deposit-received">
                            <span class="icon icon-check"></span>
                            <span>Received {st.received.toFixed(8)} PIV!</span>
                        </div>
                    {:else}
                        <div class="pivx-awaiting-deposit">
                            <div class="pivx-spinner"></div>
                            <span>Awaiting Deposit...</span>
                        </div>
                    {/if}
                </div>
            </div>
        </div>
    </div>
{/if}
