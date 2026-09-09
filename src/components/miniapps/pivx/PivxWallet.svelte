<script>
    // The wallet card inside the attachment panel: balance with its fiat line, and the
    // Send / Deposit / Withdraw / Settings dock. The card's slide-in replays per `pulse`.
    import { pivxWalletState } from '../../lib/attachmentpanel.svelte.js';
    let { h, pulse } = $props();   // h: back(), send(), deposit(), withdraw(), settings()
    const w = pivxWalletState();
</script>

<div class="pivx-wallet-content">
    <button class="pivx-close-btn" id="attachment-panel-pivx-back" aria-label="Back" onclick={h.back}>
        <span class="icon icon-x"></span>
    </button>
    {#key pulse}
        <div class="pivx-balance-section pivx-panel-animate">
            <div class="pivx-balance-logo"><img src="./icons/pivx.svg" alt="PIVX" /></div>
            <div class="pivx-balance-info">
                {#key w.v}
                    <div class="pivx-balance-amount" id="pivx-balance-amount" class:pivx-fade-in={w.balance !== null}>
                        {#if w.balance === null}
                            <div class="pivx-balance-loading"><div class="pivx-spinner"></div></div>
                        {:else}
                            {w.balance.toFixed(2)} <span style="color: #642D8F;">PIV</span>
                        {/if}
                    </div>
                    {#if w.fiat}
                        <div class="pivx-balance-fiat pivx-fade-in" id="pivx-balance-fiat">{w.fiat}</div>
                    {/if}
                {/key}
            </div>
        </div>
        <div class="pivx-actions-dock">
            {#each [
                { id: 'pivx-send-btn', icon: 'icon-send', label: 'Send', primary: true, go: h.send },
                { id: 'pivx-deposit-btn', icon: 'icon-plus-circle', label: 'Deposit', deposit: true, go: h.deposit },
                { id: 'pivx-withdraw-btn', icon: 'icon-arrow-up', label: 'Withdraw', go: h.withdraw },
                { id: 'pivx-settings-btn', icon: 'icon-settings', label: 'Settings', go: h.settings },
            ] as b, i (b.id)}
                <button class="pivx-dock-btn pivx-panel-animate" class:pivx-dock-btn-primary={b.primary} id={b.id}
                        class:disabled={b.deposit && w.depositDisabled} class:loading={b.deposit && w.depositLoading}
                        disabled={b.deposit && w.depositLoading}
                        title={b.deposit && w.depositDisabled ? 'Balance too high - please withdraw first' : ''}
                        style:animation-delay="{(i + 1) * 0.06}s" onclick={b.go}>
                    <div class="pivx-dock-icon"><span class="icon {b.icon}"></span></div>
                    <span class="pivx-dock-label">{b.label}</span>
                </button>
            {/each}
        </div>
    {/key}
</div>
