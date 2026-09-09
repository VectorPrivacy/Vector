<script>
    // A PIVX payment inside a message: amount, the fiat line when a price is cached, and
    // the claim state from the store (the balance check and a claim write it).
    import { pivxBubble } from '../lib/pivxbubble.svelte.js';
    let { msg, h } = $props();   // h: fiat(amountPiv) → string | null, ensure(payment, mine), claim(giftCode)

    const pay = $derived(msg.pivx_payment);
    const st = $derived(pivxBubble(pay.gift_code));
    const phase = $derived(st?.phase || 'claimable');
    const hint = $derived(st?.hint || (msg.mine ? 'Click to reclaim' : 'Click to claim'));
    const fiat = $derived(h.fiat(pay.amount_piv));

    // The balance check runs once per gift code; the store remembers across re-renders.
    $effect(() => { h.ensure(pay, msg.mine); });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="msg-pivx-payment" data-gift-code={pay.gift_code} data-address={pay.address || undefined}
     class:syncing={phase === 'syncing'} class:claiming={phase === 'claiming'} class:claimed={phase === 'claimed'}
     onclick={() => { if (phase !== 'claimed' && phase !== 'syncing' && phase !== 'claiming') h.claim(pay.gift_code); }}>
    <img src="./icons/pivx.svg" alt="">
    <div class="msg-pivx-payment-info">
        <div class="msg-pivx-payment-amount">{pay.amount_piv.toFixed(2)} PIV</div>
        {#if fiat}<div class="msg-pivx-payment-fiat">{fiat}</div>{/if}
        <div class="msg-pivx-payment-hint">{hint}</div>
    </div>
</div>
