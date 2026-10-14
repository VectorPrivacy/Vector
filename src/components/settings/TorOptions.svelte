<script>
    // Tor's settings under its card: bridges, which must be reachable before Tor is in use
    // (a network that blocks Tor only connects through them), then the active circuit
    // once there is one to show.
    import { torState, settingsScreen } from '../lib/settings.svelte.js';
    import { transportState } from '../lib/transport.svelte.js';
    import TorCircuits from './TorCircuits.svelte';
    import InfoIcon from './InfoIcon.svelte';

    let { h } = $props();   // h: help(key), loadCircuits(), refreshCircuitCount(), newCircuit(), setBridgesEnabled(on), setMultiCircuit(on), bridgesInput(), applyBridges(), openLink(key), setEngine(external, port), findTor()

    const tor = torState();
    const net = transportState();
    const sc = settingsScreen();
    const b = $derived(sc.bridges);
    const connected = $derived(net.view?.kind === 'tor' && !!net.view.ready);
    const eng = $derived(tor.engine);
    // Typed but not saved; empty means find it.
    let portText = $state(null);
    const portShown = $derived(portText ?? (eng.port ? String(eng.port) : ''));
    const portDirty = $derived(portText !== null && portText !== (eng.port ? String(eng.port) : ''));
    const found = $derived(eng.found ? `Found ${eng.found === 9150 ? 'Tor Browser' : 'Tor'} on port ${eng.found}.` : null);
    async function savePort() {
        const p = Number(portText || 0);
        if (p > 65535) return;
        await h.setEngine(true, p || null);
        portText = null;
    }
    const multi = $derived(tor.multi);
    const dirty = $derived(b.lines !== b.saved);
    const lineCount = $derived(b.lines.split(/\r?\n/).map(l => l.trim()).filter(Boolean).length);
    const status = $derived(b.status || (lineCount === 0 ? 'No bridges configured.' : `${lineCount} bridge${lineCount === 1 ? '' : 's'} configured`));

    // The circuit is read once per connection; New builds another on request. A running Tor
    // keeps its circuits to itself.
    $effect(() => { if (connected && !eng.external) h.loadCircuits(); });
    // Where it answers can change while this is open: read it again on each connect.
    $effect(() => { if (connected && eng.external) h.findTor(); });
    // The count follows hosts being contacted, so it re-reads while this is on screen.
    $effect(() => {
        if (!connected || !multi || eng.external) return;
        const t = setInterval(() => h.refreshCircuitCount(), 5000);
        return () => clearInterval(t);
    });
    const c = $derived(tor.circuits);
</script>

<div class="form-group">
    <label class="toggle-container">
        <span><InfoIcon onclick={() => h.help('torExternal')} />Use Running Tor</span>
        <input type="checkbox" checked={eng.external} disabled={eng.busy || net.locked} onchange={(e) => h.setEngine(e.currentTarget.checked, eng.port)}>
        <span class="neon-toggle"></span>
    </label>
</div>
{#if eng.external}
    <div class="form-group st-line i2p-router">
        <span class="st-line-label">Port</span>
        <div class="i2p-addr">
            <span class="i2p-host">127.0.0.1</span>
            <span class="i2p-colon">:</span>
            <input class="i2p-port" type="text" inputmode="numeric" maxlength="5" aria-label="Tor SOCKS port" placeholder="Auto" spellcheck="false" autocomplete="off"
                   value={portShown}
                   oninput={(e) => { portText = e.currentTarget.value.replace(/\D/g, ''); e.currentTarget.value = portText; }}
                   onkeydown={(e) => {
                       if (e.key === 'Enter') { e.preventDefault(); if (portDirty) savePort(); }
                       else if (e.key === 'Escape' && portDirty) { e.preventDefault(); e.stopPropagation(); portText = null; }
                   }}>
            {#if portDirty}
                <button type="button" class="st-btn net-primary" disabled={eng.busy} onclick={savePort}>Save</button>
            {:else}
                <button type="button" class="st-btn i2p-test" disabled={eng.busy} onclick={() => h.findTor()}>Find</button>
            {/if}
        </div>
    </div>
    <p class="i2p-note {found ? 'ok' : 'error'}" role="status">{found ?? 'No Tor found. Open Tor Browser or start tor.'}</p>
{:else}
<div class="form-group">
    <label class="toggle-container">
        <span><InfoIcon onclick={() => h.help('torBridges')} />Use Bridges</span>
        <input type="checkbox" checked={b.enabled} disabled={b.busy} onchange={(e) => h.setBridgesEnabled(e.currentTarget.checked)}>
        <span class="neon-toggle"></span>
    </label>
</div>
<!-- Stays open through a failed toggle so the error and the obfs4 hint are readable. -->
<div id="tor-bridges-body" class="tor-bridges-body" style:display={b.enabled || b.statusClass === 'is-error' ? '' : 'none'}>
    <textarea id="tor-bridges-textarea" class="tor-bridges-textarea" rows="4" placeholder="obfs4 1.2.3.4:443 ABCD... cert=... iat-mode=0"
              spellcheck="false" autocomplete="off" wrap="off" disabled={b.busy}
              bind:value={sc.bridges.lines} oninput={() => h.bridgesInput()}></textarea>
    {#if b.obfs4Hint}
        <div id="tor-obfs4-banner" class="tor-obfs4-banner">
            <span class="tor-obfs4-banner-icon">⚠</span>
            <span>obfs4 bridges need <code>obfs4proxy</code> installed: {@html b.obfs4Hint}. Saving will fail until it's available.</span>
        </div>
    {/if}
    <div class="tor-bridges-help">
        One bridge per line. <b>obfs4</b> recommended (bypasses DPI). Vanilla <code>IP:port fingerprint</code> also works for private bridges.
        <!-- svelte-ignore a11y_invalid_attribute -->
        Get bridges at <a href="#" onclick={(e) => { e.preventDefault(); e.stopPropagation(); h.openLink('bridges'); }}>bridges.torproject.org</a>.
    </div>
    <div class="tor-bridges-foot">
        <span id="tor-bridges-status" class="tor-bridges-status {b.statusClass}">{status}</span>
        <button type="button" id="tor-bridges-apply" class="st-btn" disabled={!dirty || b.busy} onclick={() => h.applyBridges()}>
            {connected ? 'Apply & Reconnect' : 'Save'}
        </button>
    </div>
</div>
{/if}

<div class="form-group">
    <label class="toggle-container">
        <span><InfoIcon onclick={() => h.help('torMultiCircuit')} />Multi-Circuit</span>
        <input type="checkbox" checked={multi} disabled={net.locked} onchange={(e) => h.setMultiCircuit(e.currentTarget.checked)}>
        <span class="neon-toggle"></span>
    </label>
</div>

{#if connected}
    <div class="tor-circuit">
        <header class="tor-circuits-head">
            <span class="tor-circuits-head-label">{multi ? 'Circuits' : 'Active Circuit'}</span>
            <button type="button" id="tor-circuits-refresh" class="tor-circuits-refresh" title="Build a new circuit"
                    disabled={tor.circuits.phase === 'loading'} onclick={() => h.newCircuit()}>
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="M21 12a9 9 0 1 1-3-6.7L21 8"/>
                    <path d="M21 3v5h-5"/>
                </svg>
                <span>New</span>
            </button>
        </header>
        {#if eng.external}
            <p class="tor-circuit-count-sub">That Tor builds the circuits. New gives every server a fresh one.</p>
        {:else if multi && c.phase === 'ok'}
            <div class="tor-circuit-count">
                <span class="tor-circuit-count-num">{c.count}</span>
                <span class="tor-circuit-count-text">
                    active circuit{c.count === 1 ? '' : 's'}
                    <span class="tor-circuit-count-sub">across {c.hosts} {c.hosts === 1 ? 'host' : 'relays and servers'}. Each one's circuit is in its info.</span>
                </span>
            </div>
        {:else}
            <ol class="tor-circuits"><TorCircuits /></ol>
        {/if}
    </div>
{/if}
