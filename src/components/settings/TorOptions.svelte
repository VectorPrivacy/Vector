<script>
    // Tor's settings under its card: bridges, which must be reachable before Tor is on
    // (a network that blocks Tor only connects through them), then the active circuit
    // once there is one to show.
    import { torState, settingsScreen } from '../lib/settings.svelte.js';
    import TorCircuits from './TorCircuits.svelte';
    import InfoIcon from './InfoIcon.svelte';

    let { h } = $props();   // h: help(key), loadCircuits(), refreshCircuitCount(), newCircuit(), setBridgesEnabled(on), setMultiCircuit(on), bridgesInput(), applyBridges(), openLink(key)

    const tor = torState();
    const sc = settingsScreen();
    const b = $derived(sc.bridges);
    const connected = $derived(!!tor.state?.running);
    const multi = $derived(tor.state?.multi_circuit !== false);
    const dirty = $derived(b.lines !== b.saved);
    const lineCount = $derived(b.lines.split(/\r?\n/).map(l => l.trim()).filter(Boolean).length);
    const status = $derived(b.status || (lineCount === 0 ? 'No bridges configured.' : `${lineCount} bridge${lineCount === 1 ? '' : 's'} configured`));

    // The circuit is read once per connection; New builds another on request.
    $effect(() => { if (connected) h.loadCircuits(); });
    // The count follows hosts being contacted, so it re-reads while this is on screen.
    $effect(() => {
        if (!connected || !multi) return;
        const t = setInterval(() => h.refreshCircuitCount(), 5000);
        return () => clearInterval(t);
    });
    const c = $derived(tor.circuits);
</script>

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

<div class="form-group">
    <label class="toggle-container">
        <span><InfoIcon onclick={() => h.help('torMultiCircuit')} />Multi-Circuit</span>
        <input type="checkbox" checked={multi} disabled={tor.locked} onchange={(e) => h.setMultiCircuit(e.currentTarget.checked)}>
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
        {#if multi && c.phase === 'ok'}
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
