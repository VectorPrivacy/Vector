<script>
    // A phone's call plays to the ear, the speaker or a headset; each tap moves it to the next.
    import { callAudio } from '../lib/calls.svelte.js';
    /** @type {{ set: (route: string) => void }} */
    let { set } = $props();
    const a = callAudio();
    const NAMES = { earpiece: 'Phone', headset: 'Headset', speaker: 'Speaker', bluetooth: 'Bluetooth' };
    function next() {
        const r = a.route;
        set(r.available[(r.available.indexOf(r.current) + 1) % r.available.length]);
    }
</script>

{#if a.route?.available.length > 1}
    <button class="call-btn" class:call-btn-on={a.route.current === 'speaker'} title="Sound: {NAMES[a.route.current] || a.route.current}" onclick={next}>
        {#if a.route.current === 'speaker'}
            <span class="icon icon-volume-max"></span>
        {:else if a.route.current === 'bluetooth'}
            <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><path d="m7 7 10 10-5 5V2l5 5L7 17" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
        {:else if a.route.current === 'headset'}
            <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><path d="M3 18v-6a9 9 0 0 1 18 0v6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M21 19a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3zM3 19a2 2 0 0 0 2 2h1a2 2 0 0 0 2-2v-3a2 2 0 0 0-2-2H3z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
        {:else}
            <svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true"><path d="M5 4h3l2 5-2.5 1.5a11 11 0 0 0 6 6L15 14l5 2v3a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
        {/if}
    </button>
{/if}
