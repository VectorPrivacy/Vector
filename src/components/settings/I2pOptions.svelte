<script>
    // I2P's settings under its card: the router Vector uses (its port, a test, a SAM password),
    // the connection's steps and a new address while I2P is in use, I2P-Only, then the outproxies
    // clearnet servers are reached through.
    import { transportState } from '../lib/transport.svelte.js';
    import InfoIcon from './InfoIcon.svelte';
    import TransportSteps from './TransportSteps.svelte';
    import I2pOutproxies from './I2pOutproxies.svelte';

    let { h } = $props();   // h: TransportHandlers (js/transport.js)

    const PORT_RANGE = 'Enter a port from 1 to 65535.';

    const t = transportState();
    const cfg = $derived(t.config);
    const view = $derived(t.view);
    const inUse = $derived(view?.kind === 'i2p');
    const i2pOnly = $derived(cfg?.exit === 'off');
    // Your own relays see the Account address; other servers and outproxies see the Shared one.
    const addresses = $derived([
        { who: 'your relays', b32: view?.detail?.addresses?.account },
        { who: 'other servers', b32: view?.detail?.addresses?.shared },
    ].filter((a) => a.b32));
    const shortB32 = (b32) => `${b32.slice(0, 4)}…${b32.slice(48, 52)}.b32.i2p`;

    $effect(() => { if (!cfg) h.i2p.load(); });
    // Outproxy health and the Clearnet step move with every connection; resting ones count down.
    $effect(() => { if (inUse) return h.i2p.watch(); });

    // ── router ──
    // A typed port is only tried (Test) or kept (Save, Enter): leaving the field never saves it,
    // so testing another port can't move a running I2P.
    let port = $state('');
    let typed = $state(false);
    // The saved port shows until something else is typed.
    $effect(() => { const saved = cfg?.sam_port; if (saved && !typed) port = String(saved); });
    const dirty = $derived(!!cfg && typed && port !== String(cfg.sam_port));
    let note = $state(null);   // { text, tone: 'ok' | 'error' }: a test result or a refused save
    let testing = $state(false);
    let saving = $state(false);
    const portOk = (p) => Number.isInteger(p) && p >= 1 && p <= 65535;
    // A result describes the router as it was: once the network or its router state moves, it goes.
    let seen = '';
    $effect(() => {
        const router = view?.kind === 'i2p' ? (view.steps || []).find((s) => s.id === 'router')?.state : '';
        const now = `${view?.kind}:${router === 'ok'}`;
        if (seen && now !== seen) note = null;
        seen = now;
    });

    async function savePort() {
        const p = Number(port);
        if (!cfg || p === cfg.sam_port) { typed = false; return; }
        if (!portOk(p)) { note = { text: PORT_RANGE, tone: 'error' }; return; }
        saving = true;
        try {
            await h.i2p.setRouter(p);
            note = null;
            typed = false;
        } catch (e) {
            note = { text: String(e), tone: 'error' };
        } finally {
            saving = false;
        }
    }
    async function test() {
        const p = Number(port);
        if (!portOk(p)) { note = { text: PORT_RANGE, tone: 'error' }; return; }
        testing = true;
        note = null;
        try {
            const r = await h.i2p.testRouter(p);
            note = { text: r?.text || '', tone: r?.ok ? 'ok' : 'error' };
        } catch (e) {
            note = { text: String(e), tone: 'error' };
        } finally {
            testing = false;
        }
    }

    // ── SAM password ──
    let authOpen = $state(false);
    let user = $state('');
    let password = $state('');
    let authError = $state('');
    let authBusy = $state(false);
    async function saveAuth() {
        authBusy = true;
        authError = '';
        try {
            await h.i2p.setRouter(cfg.sam_port, user, password);
            authOpen = false;
            user = '';
            password = '';
        } catch (e) {
            authError = String(e);
        } finally {
            authBusy = false;
        }
    }
    async function removeAuth() {
        authBusy = true;
        authError = '';
        try { await h.i2p.setRouter(cfg.sam_port, '', ''); } catch (e) { authError = String(e); } finally { authBusy = false; }
    }

    // ── new address, I2P-Only ──
    let renewing = $state(false);
    async function renew() {
        renewing = true;
        try { await h.newIdentity(); } finally { renewing = false; }
    }
    let exitError = $state('');
    // The box shows what is in force once the backend answers (or the user backs out).
    async function toggleOnly(e) {
        const box = e.currentTarget;
        exitError = '';
        try { await h.i2p.setI2pOnly(box.checked); } catch (err) { exitError = String(err); }
        box.checked = t.config?.exit === 'off';
    }
    const onEnter = (fn) => (e) => { if (e.key === 'Enter') { e.preventDefault(); fn(); } };
</script>

{#if cfg}
    <div class="form-group st-line i2p-router">
        <InfoIcon side="right" flex onclick={() => h.help('i2pRouter')} />
        <span class="st-line-label">Router</span>
        <div class="i2p-addr">
            <span class="i2p-host">127.0.0.1</span>
            <span class="i2p-colon">:</span>
            <input class="i2p-port" type="text" inputmode="numeric" maxlength="5" aria-label="SAM port" spellcheck="false" autocomplete="off"
                   bind:value={port}
                   oninput={() => { port = port.replace(/\D/g, ''); typed = true; note = null; }}
                   onkeydown={(e) => {
                       if (e.key === 'Enter') { e.preventDefault(); savePort(); }
                       else if (e.key === 'Escape' && dirty) { e.preventDefault(); e.stopPropagation(); typed = false; note = null; }
                   }}>
            <button type="button" class="st-btn i2p-test" class:is-busy={testing} disabled={testing} onclick={test}>Test</button>
            {#if dirty}
                <button type="button" class="st-btn net-primary" disabled={saving} onclick={savePort}>Save</button>
            {/if}
        </div>
    </div>
    {#if note}<p class="i2p-note {note.tone}" role="status">{note.text}</p>{/if}

    <div class="i2p-auth">
        {#if cfg.sam_auth}
            <span class="i2p-muted">Signs in as {cfg.sam_user}.</span>
            <button type="button" class="net-link" disabled={authBusy} onclick={removeAuth}>Remove</button>
        {:else if authOpen}
            <div class="i2p-form">
                <div class="i2p-form-head">
                    <span>SAM Password</span>
                    <InfoIcon align="middle" onclick={() => h.help('i2pAuth')} />
                </div>
                <div class="i2p-fields">
                    <label class="i2p-field">
                        <span>Username</span>
                        <!-- svelte-ignore a11y_autofocus -->
                        <input type="text" autocomplete="off" autocapitalize="none" spellcheck="false" autofocus bind:value={user} onkeydown={onEnter(saveAuth)}>
                    </label>
                    <label class="i2p-field">
                        <span>Password</span>
                        <input type="password" autocomplete="off" bind:value={password} onkeydown={onEnter(saveAuth)}>
                    </label>
                </div>
                <div class="i2p-form-actions">
                    <button type="button" class="st-btn" onclick={() => { authOpen = false; authError = ''; user = ''; password = ''; }}>Cancel</button>
                    <button type="button" class="st-btn net-primary" disabled={authBusy || (!user && !password)} onclick={saveAuth}>Save</button>
                </div>
            </div>
        {:else}
            <span class="i2p-auth-add">
                <button type="button" class="net-link" onclick={() => { authOpen = true; authError = ''; }}>Add SAM Password</button>
                <InfoIcon align="middle" onclick={() => h.help('i2pAuth')} />
            </span>
        {/if}
        {#if authError}<p class="i2p-note error" role="alert">{authError}</p>{/if}
    </div>

    {#if inUse}
        <TransportSteps steps={view.steps || []} kind="i2p">
            {#if addresses.length}
                <li class="net-step net-step-address" data-state="ok">
                    <span class="net-step-mark"><span class="net-step-dot"></span></span>
                    <span class="net-step-label">Address</span>
                    <span class="net-step-text net-addresses">
                        {#each addresses as a (a.who)}
                            <button type="button" class="net-address" title="Copy {a.b32}" aria-label="Copy the address {a.who} see" onclick={() => h.i2p.copyAddress(a.b32)}>
                                <span class="net-address-b32">{shortB32(a.b32)}</span>
                                <span class="net-address-who">{a.who}</span>
                            </button>
                        {/each}
                    </span>
                </li>
            {/if}
        </TransportSteps>
        <div class="form-group st-line">
            <InfoIcon side="right" flex onclick={() => h.help('i2pIdentity')} />
            <span class="st-line-label">New Address</span>
            <button type="button" class="st-btn" disabled={renewing || !view.ready || t.locked} onclick={renew}>Renew</button>
        </div>
    {/if}

    <div class="form-group i2p-only-row">
        <label class="toggle-container">
            <span><InfoIcon side="right" align="middle" onclick={() => h.help('i2pOnly')} />I2P-Only</span>
            <input type="checkbox" checked={i2pOnly} disabled={t.locked} onchange={toggleOnly}>
            <span class="neon-toggle"></span>
        </label>
    </div>
    {#if exitError}<p class="i2p-note error" role="alert">{exitError}</p>{/if}
    {#if i2pOnly && t.stranded && inUse}
        <p class="i2p-only-line warn">None of your relays are on I2P. <button type="button" class="net-link" onclick={() => h.openNetwork()}>Open Network</button></p>
    {:else if i2pOnly}
        <p class="i2p-only-line">Only I2P servers and I2P addresses you added.</p>
    {:else}
        <I2pOutproxies {h} />
    {/if}
{/if}
<p class="i2p-foot">Works with i2pd or Java I2P with SAM turned on.</p>
