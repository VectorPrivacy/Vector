<script>
    // Settings' Tor / I2P explainer: the login screen's card, without its router setup.
    import { netInfoState, closeNetInfo } from '../lib/transport.svelte.js';
    import { popIn } from '../lib/popin.js';
    import NetInfo from '../auth/NetInfo.svelte';

    const info = netInfoState();
</script>

<svelte:window onkeydown={(ev) => { if (info.kind && ev.key === 'Escape') { ev.preventDefault(); closeNetInfo(); } }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="lg-net-modal" class:active={!!info.kind} class:closing={info.closing}
     onclick={(ev) => { if (ev.target === ev.currentTarget) closeNetInfo(); }}>
    <div class="lg-net-card" class:is-i2p={info.kind === 'i2p'} role="dialog" aria-modal="true" aria-labelledby="lg-net-title" use:popIn={info.tick}>
        <button type="button" class="lg-net-close" aria-label="Close" onclick={closeNetInfo}>&#x2715;</button>
        {#if info.kind}<NetInfo kind={info.kind} openLink={info.openLink} inSettings />{/if}
    </div>
</div>
