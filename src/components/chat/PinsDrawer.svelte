<script>
    // The pins drawer under the chat header: the sealed or empty notice, or one row per
    // verified pin. Stays in the layout through its slide-up so the close animates.
    import { pinsState, pinsList, pinsHandlers, pinsEls, pinsCloseSettled } from '../lib/pins.svelte.js';
    import PinRow from './PinRow.svelte';
    const st = pinsState();
    const pins = $derived(pinsList());
    const h = $derived(pinsHandlers());
    let drawer = $state(null);
    let list = $state(null);
    $effect(() => { pinsEls().drawer = drawer; pinsEls().list = list; });

    // Inline code in a pin copies itself: a pin is a reference document (relay URLs,
    // keys, commands). Fenced blocks keep their own copy button, so the fence body
    // stays neutral.
    function copyCode(e) {
        const code = e.target.closest?.('code');
        if (!code || code.closest('pre')) return;
        e.stopPropagation();
        navigator.clipboard.writeText(code.textContent).then(
            () => h?.toast('Copied to clipboard'),
            () => h?.toast('Copy failed'),
        );
    }
</script>

<div class="pins-drawer" class:pins-drawer-closing={st.closing} style:display={st.shown ? null : 'none'}
     bind:this={drawer} onanimationend={() => pinsCloseSettled()}>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="pins-drawer-list" bind:this={list} onclick={copyCode}>
        {#if st.sealed}
            <div class="pins-drawer-notice">This channel's pins are protected by a key you don't hold yet.</div>
        {:else if !pins.length}
            <div class="pins-drawer-notice">No pinned messages yet.</div>
        {:else if h}
            {#key st.v}
                {#each pins as pin (pin.rumor_id)}
                    <PinRow {pin} {h} />
                {/each}
            {/key}
        {/if}
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="pins-drawer-close btn" onclick={() => h?.close()}>
        <span>Click to Close</span>
        <svg class="pins-drawer-close-icon" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
            <path d="M18 15L12 9L6 15" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
    </div>
</div>
