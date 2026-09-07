<script>
    // The active circuit's hops, mounted into the Advanced panel's list.
    import { torState } from '../lib/settings.svelte.js';
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
    {#each c.hops as hop, i (i)}
        <li class="tor-hop" data-position={hop.position || ''} data-bridge={hop.is_bridge ? 'true' : undefined}>
            <span class="tor-hop-mark"><span class="tor-hop-dot"></span></span>
            <span class="tor-hop-pos">{hop.position || ''}</span>
            <span class="tor-hop-addr">{hop.address || '—'}</span>
            {#if hop.fingerprint}
                <!-- 8 chars disambiguate at a glance; the tooltip carries the full id. -->
                <span class="tor-hop-fp" title={hop.fingerprint}>{hop.fingerprint.slice(0, 8)}…</span>
            {/if}
        </li>
    {/each}
{/if}
