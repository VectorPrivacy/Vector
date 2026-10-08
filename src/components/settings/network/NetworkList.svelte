<script>
    // The Network section: relay rows and media server rows from network state.
    // Rows are keyed by url so a status change repaints one badge, not the list. A row the
    // network in use can't reach says so in place of its socket state.
    import { networkState } from '../../lib/settings.svelte.js';
    import { transportState, routeOf, shortI2pHost } from '../../lib/transport.svelte.js';
    // part: 'relays' | 'servers' renders one half under a heading its host draws, with the
    // add action as a row of its own; both halves by default, each titled.
    let { h, part = null } = $props();   // h: explain(kind), addRelay, addServer, openRelay(relay), openServer(server), toggleRelay(relay, enabled) → Promise<boolean>, routes(urls), routeTag(route), routeBadge(route)

    const n = networkState();
    const t = transportState();
    const host = (url) => shortI2pHost(url.replace(/^(wss?|https?):\/\//, ''));

    // The listed rows' routes, read when the rows change (not on every socket state);
    // js/transport.js re-reads them whenever the network changes.
    let routed = '';
    $effect(() => {
        const urls = [...(part !== 'servers' ? n.relays : []), ...(part !== 'relays' ? n.servers : [])].map((r) => r.url);
        const key = urls.join('\n');
        if (key && key !== routed) { routed = key; h.routes(urls); }
    });
    const tag = (row) => (row.enabled && t.view ? h.routeTag(routeOf(row.url)) : null);
    const badge = (row) => (row.enabled && t.view ? h.routeBadge(routeOf(row.url)) : null);
    // The paged layout words a status as a reader would; the phone list keeps the raw state.
    const said = (status) => status === 'disconnected' ? 'Not Connected' : status.charAt(0).toUpperCase() + status.slice(1);
    function info(kind) {
        return (e) => { e.preventDefault(); e.stopPropagation(); h.explain(kind); };
    }
    async function toggle(relay, e) {
        const enabled = e.target.checked;
        const applied = await h.toggleRelay(relay, enabled);
        // A refusal or failure leaves state as it was; the box follows it.
        if (!applied) e.target.checked = relay.enabled;
    }
</script>

{#snippet addRow(label, kind, button, add)}
    <div class="st-row">
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span class="st-row-label">{label}<span class="icon icon-info btn network-info" onclick={info(kind)}></span></span>
        <button class="st-btn" onclick={(e) => { e.preventDefault(); e.stopPropagation(); add(); }}>{button}</button>
    </div>
{/snippet}

{#if part !== 'servers'}
{#if part}
    {@render addRow('Add Custom Relay', 'relays', 'Add Relay', h.addRelay)}
{:else}
<div class="relay-section-header">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <h3 class="network-section-title" style="display: inline-flex; align-items: center;">Nostr Relays<span class="icon icon-info btn network-info" onclick={info('relays')}></span></h3>
    <button class="relay-add-btn" title="Add Custom Relay" onclick={(e) => { e.preventDefault(); e.stopPropagation(); h.addRelay(); }}>+</button>
</div>
{/if}
{#each n.relays as relay (relay.url)}
    {@const g = tag(relay)}
    <div class="relay-item" class:disabled={!relay.enabled} data-relay-url={relay.url} data-relay-is-default={relay.is_default} data-relay-is-custom={relay.is_custom}>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="relay-item-content" onclick={() => h.openRelay(relay)}>
            {#if relay.is_custom && relay.mode !== 'both'}
                <span class="relay-mode-badge">{relay.mode === 'read' ? 'R' : 'W'}</span>
            {/if}
            {#if relay.is_default}<span class="relay-default-badge">default</span>
            {:else if part}<span class="relay-default-badge relay-custom-badge">custom</span>{/if}
            <span class="relay-url" title={relay.url}>{host(relay.url)}</span>
        </div>
        <div class="relay-item-actions">
            {#if !g}{@const b = badge(relay)}{#if b}<span class="relay-route-badge {b.cls}" title={b.title}>{b.text}</span>{/if}{/if}
            {#if g}
                <span class="relay-status {g.cls}" title={g.title}>{#if g.short}<span class="tag-long">{g.text}</span><span class="tag-short">{g.short}</span>{:else}{g.text}{/if}</span>
            {:else}
                {@const st = relay.enabled ? relay.status : 'disabled'}
                <span class="relay-status {st}">{part ? said(st) : st}</span>
            {/if}
            <input type="checkbox" class="relay-toggle" checked={relay.enabled} onclick={(e) => e.stopPropagation()} onchange={(e) => toggle(relay, e)}>
        </div>
    </div>
{/each}
{/if}

{#if part !== 'relays'}
{#if part}
    {@render addRow('Add Custom Server', 'servers', 'Add Server', h.addServer)}
{:else}
<div class="relay-section-header" style="margin-top: 2rem;">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <h3 class="network-section-title" style="display: inline-flex; align-items: center;">Media Servers<span class="icon icon-info btn network-info" onclick={info('servers')}></span></h3>
    <button class="relay-add-btn" title="Add Custom Media Server" onclick={(e) => { e.preventDefault(); e.stopPropagation(); h.addServer(); }}>+</button>
</div>
{/if}
{#each n.servers as server (server.url)}
    {@const g = tag(server)}
    <div class="relay-item media-server-item" class:disabled={!server.enabled} data-server-url={server.url}>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="relay-item-content" onclick={() => h.openServer(server)}>
            {#if server.is_default}<span class="relay-default-badge">default</span>
            {:else if part}<span class="relay-default-badge relay-custom-badge">custom</span>{/if}
            <span class="relay-url" title={server.url}>{host(server.url)}</span>
        </div>
        {#if g}
            <span class="relay-status {g.cls}" title={g.title}>{#if g.short}<span class="tag-long">{g.text}</span><span class="tag-short">{g.short}</span>{:else}{g.text}{/if}</span>
        {:else}
            {@const b = badge(server)}
            {#if b}<span class="relay-route-badge {b.cls}" title={b.title}>{b.text}</span>{/if}
            {#if server.status?.latency_ms != null && server.enabled}<span class="relay-latency">{server.status.latency_ms} ms</span>{/if}
            <span class="relay-status {server.status?.tone || (server.enabled ? 'connected' : 'disabled')}">{server.status?.label || (server.enabled ? 'Online' : 'Disabled')}</span>
        {/if}
    </div>
{/each}
{/if}
