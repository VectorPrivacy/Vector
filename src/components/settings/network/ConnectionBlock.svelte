<script>
    // How Vector reaches this relay or server right now: the route's own line, a recent failure,
    // and over Tor the circuit's hops.
    import { routeOf, transportState } from '../../lib/transport.svelte.js';
    import CircuitHops from '../CircuitHops.svelte';
    let { url = '', circuit = null } = $props();
    const t = transportState();
    const route = $derived(routeOf(url));
    // Held while the network is still starting is the normal course, not a failure.
    const starting = $derived(route?.class === 'blocked' && t.view?.phase === 'starting');
</script>

{#if route || circuit}
    <div class="relay-connection">
        <h4>Connection</h4>
        {#if starting}
            <p class="relay-route connecting via-{route.kind}">{t.view.label ? `Waiting for ${t.view.label} to connect` : 'Waiting for the network to connect'}</p>
        {:else if route}<p class="relay-route {route.class} via-{route.kind}">{route.text}</p>{/if}
        {#if route?.last_error && route.class !== 'blocked' && route.class !== 'refused'}
            <p class="relay-route-error">{route.last_error}</p>
        {/if}
        {#if circuit}<ol class="tor-circuits"><CircuitHops hops={circuit} /></ol>{/if}
    </div>
{/if}
