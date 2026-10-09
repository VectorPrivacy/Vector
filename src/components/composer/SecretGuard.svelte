<script>
    // Sending a secret: a private key, a seed phrase or a wallet key. The safe answer is the
    // easy one: the main button, Escape, the ✕ and a backdrop tap all keep it unsent.
    import { secretGuard } from '../lib/secretguard.svelte.js';
    import { popIn } from '../lib/popin.js';
    let { h } = $props();   // h: answer('keep' | 'send' | 'timer')
    const st = secretGuard.state();

    const key = $derived(st.kind === 'nostr_key');
    const title = $derived(key ? 'This is a private key' : st.kind === 'seed_phrase' ? 'This is a seed phrase' : 'This is a wallet key');
    let keep = $state(null);
    $effect(() => { st.tick; if (st.active) keep?.focus(); });

    // Send Anyway waits ten seconds from each opening: long enough to read, and no
    // reflexive tap or double press can reach it.
    const WAIT = 10;
    let wait = $state(WAIT);
    $effect(() => {
        st.tick;
        if (!st.active) return;
        wait = WAIT;
        const timer = setInterval(() => { if (--wait <= 0) clearInterval(timer); }, 1000);
        return () => clearInterval(timer);
    });
</script>

<svelte:window onkeydown={(e) => { if (st.active && !st.closing && e.key === 'Escape') h.answer('keep'); }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="vcard-overlay" class:active={st.active} class:closing={st.closing}
     onclick={(e) => { if (e.target === e.currentTarget) h.answer('keep'); }}>
    <div class="vcard secret-card" role="alertdialog" aria-modal="true" aria-labelledby="secret-title" use:popIn={st.tick}>
        <div class="vcard-head">
            <span class="vcard-badge secret-badge" aria-hidden="true"></span>
            <h3 id="secret-title" class="vcard-title secret-title">{title}</h3>
            <button type="button" class="vcard-x" aria-label="Close" onclick={() => h.answer('keep')}>&#x2715;</button>
        </div>
        <p class="vcard-lead">
            {#if key}
                Your message contains an <b>nsec</b>: the private key to a Nostr account. Whoever has it can
                read every private message, post as that account and take it over, for good. Once sent, it
                can't be taken back.
            {:else}
                Your message looks like a <b>{st.kind === 'seed_phrase' ? 'seed phrase' : 'wallet private key'}</b>.
                Whoever has it can take everything in that wallet, instantly and for good.
            {/if}
        </p>
        <p class="vcard-lead secret-loud">
            {#if st.community}
                This is a community. Everyone in <b>{st.where}</b> would see it, and so would anyone who joins later.
            {:else if key}
                No one ever needs your private key: not Vector, not support, not a friend. If <b>{st.where}</b>
                asked you for it, it's almost certainly a scam.
            {:else}
                Scammers pose as support staff, giveaways and friends to ask for these. If <b>{st.where}</b>
                asked, stop and check who they really are.
            {/if}
        </p>
        {#if st.timer === 'offer'}
            <p class="vcard-lead">If you're sure, turn on a <b>Self-Destruct Timer</b> first, so it doesn't stay on devices and relays forever.</p>
        {:else if st.timer === 'on'}
            <p class="vcard-lead">This chat's Self-Destruct Timer is on, so it won't stay on devices and relays.</p>
        {/if}
        <div class="vcard-actions">
            <button type="button" class="vcard-btn" bind:this={keep} onclick={() => h.answer('keep')}>Don't Send</button>
            {#if st.timer === 'offer'}
                <button type="button" class="vcard-btn ghost" onclick={() => h.answer('timer')}>Set a Self-Destruct Timer</button>
            {/if}
            <button type="button" class="vcard-btn ghost secret-send" disabled={wait > 0} onclick={() => { if (wait <= 0) h.answer('send'); }}>
                {wait > 0 ? `Send Anyway (${wait})` : 'Send Anyway'}
            </button>
        </div>
    </div>
</div>
