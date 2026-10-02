<script>
    // A Tor circuit's hops, guard to exit: the Tor settings' active circuit and each relay's
    // and media server's own.
    let { hops = [] } = $props();
</script>

{#each hops as hop, i (i)}
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
