<script>
    // The pins drawer's list: the sealed or empty notice, or one row per verified pin.
    import { pinsState, pinsList } from '../lib/pins.svelte.js';
    import PinRow from './PinRow.svelte';
    let { h } = $props();
    const st = pinsState();
    const pins = $derived(pinsList());
</script>

{#if st.sealed}
    <div class="pins-drawer-notice">This channel's pins are protected by a key you don't hold yet.</div>
{:else if !pins.length}
    <div class="pins-drawer-notice">No pinned messages yet.</div>
{:else}
    {#key st.v}
        {#each pins as pin (pin.rumor_id)}
            <PinRow {pin} {h} />
        {/each}
    {/key}
{/if}
