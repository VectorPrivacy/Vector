<script>
    // The active circuit's hops, mounted into the Advanced panel's list.
    import { torState } from '../lib/settings.svelte.js';
    import CircuitHops from './CircuitHops.svelte';
    const tor = torState();
    const c = $derived(tor.circuits);
</script>

{#if c.phase === 'loading'}
    <li class="tor-circuits-empty is-loading">Building circuit…</li>
{:else if c.phase === 'error'}
    <li class="tor-circuits-error">Failed: {c.error}</li>
{:else if c.phase === 'ok' && c.hops.length === 0}
    <li class="tor-circuits-empty">No active circuit.</li>
{:else if c.phase === 'ok'}
    <CircuitHops hops={c.hops} />
{/if}
