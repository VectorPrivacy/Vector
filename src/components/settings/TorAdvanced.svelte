<script>
    // The Advanced disclosure under the Tor card: the active circuit and the bridges
    // editor. Only offered once connected; the card fuses with it when shown.
    import { torState, settingsScreen } from '../lib/settings.svelte.js';
    import TorCircuits from './TorCircuits.svelte';

    let { h } = $props();   // h: toggleAdvanced(), newCircuit(), setBridgesEnabled(on), bridgesInput(), applyBridges(), openLink(key)

    const tor = torState();
    const sc = settingsScreen();
    const b = $derived(sc.bridges);
    const connected = $derived(!!tor.state?.running);
    const open = $derived(connected && tor.advancedOpen);
    const dirty = $derived(b.lines !== b.saved);
    const lineCount = $derived(b.lines.split(/\r?\n/).map(l => l.trim()).filter(Boolean).length);
    const status = $derived(b.status || (lineCount === 0 ? 'No bridges configured.' : `${lineCount} bridge${lineCount === 1 ? '' : 's'} configured`));
</script>

<div id="settings-tor-advanced" class="tor-advanced" class:expanded={open} style:display={connected ? '' : 'none'}>
    <button type="button" id="tor-advanced-toggle" class="tor-advanced-toggle" onclick={() => h.toggleAdvanced()}>
        <span class="icon icon-chevron-down tor-advanced-chevron"></span>
        <span>Advanced</span>
    </button>
    <div id="tor-advanced-panel" class="tor-advanced-panel" style:display={open ? '' : 'none'}>
        <header class="tor-circuits-head">
            <span class="tor-circuits-head-label">Active circuit</span>
            <button type="button" id="tor-circuits-refresh" class="tor-circuits-refresh" title="Build a new circuit"
                    disabled={tor.circuits.phase === 'loading'} onclick={() => h.newCircuit()}>
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="M21 12a9 9 0 1 1-3-6.7L21 8"/>
                    <path d="M21 3v5h-5"/>
                </svg>
                <span>New</span>
            </button>
        </header>
        <ol id="tor-circuits-list" class="tor-circuits"><TorCircuits /></ol>

        <section class="tor-bridges">
            <header class="tor-bridges-head">
                <div class="tor-bridges-head-text">
                    <span class="tor-bridges-head-label">Use bridges</span>
                    <span class="tor-bridges-head-sub">Connect via private relays. Use this if Tor is blocked on your network.</span>
                </div>
                <label class="toggle-container tor-bridges-toggle-wrap">
                    <input type="checkbox" id="tor-bridges-toggle" checked={b.enabled} disabled={b.busy} onchange={(e) => h.setBridgesEnabled(e.currentTarget.checked)}>
                    <span class="neon-toggle"></span>
                </label>
            </header>
            <!-- The body stays open through a failed toggle so the error and the obfs4 hint are readable. -->
            <div id="tor-bridges-body" class="tor-bridges-body" style:display={b.enabled || b.statusClass === 'is-error' ? '' : 'none'}>
                <textarea id="tor-bridges-textarea" class="tor-bridges-textarea" rows="4" placeholder="obfs4 1.2.3.4:443 ABCD... cert=... iat-mode=0"
                          spellcheck="false" autocomplete="off" wrap="off" disabled={b.busy}
                          bind:value={sc.bridges.lines} oninput={() => h.bridgesInput()}></textarea>
                {#if b.obfs4Hint}
                    <div id="tor-obfs4-banner" class="tor-obfs4-banner">
                        <span class="tor-obfs4-banner-icon">⚠</span>
                        <span id="tor-obfs4-banner-msg">obfs4 bridges need <code>obfs4proxy</code> installed: {@html b.obfs4Hint}. Apply will fail until it's available.</span>
                    </div>
                {/if}
                <div class="tor-bridges-help">
                    One bridge per line. <b>obfs4</b> recommended (bypasses DPI). Vanilla <code>IP:port fingerprint</code> also works for private bridges.
                    <!-- svelte-ignore a11y_invalid_attribute -->
                    Get bridges at <a href="#" id="tor-bridges-link" onclick={(e) => { e.preventDefault(); e.stopPropagation(); h.openLink('bridges'); }}>bridges.torproject.org</a>.
                </div>
                <div class="tor-bridges-foot">
                    <span id="tor-bridges-status" class="tor-bridges-status {b.statusClass}">{status}</span>
                    <button type="button" id="tor-bridges-apply" class="tor-bridges-apply" disabled={!dirty || b.busy} onclick={() => h.applyBridges()}>Apply &amp; Reconnect</button>
                </div>
            </div>
        </section>
    </div>
</div>
