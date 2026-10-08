<script>
    // Settings > Privacy > Routing: a pill per network this build offers, then the card of the one
    // on view. A pill only changes the view, so a network can be set up before it is used; the
    // card's button switches. The glyph swaps its network and state class while faded out, so
    // keyframe animations never snap mid-frame.
    import { untrack } from 'svelte';
    import { transportState, viewedKind, startStuck } from '../lib/transport.svelte.js';
    import InfoIcon from './InfoIcon.svelte';
    import NetGlyph from '../ui/NetGlyph.svelte';

    let { h } = $props();   // h: TransportHandlers (js/transport.js)

    const TITLES = { clearnet: 'Direct Connection', i2p: 'Route traffic through I2P' };

    const t = transportState();
    const view = $derived(t.view);
    const inUse = $derived(view && view.kind !== 'unknown' ? view.kind : '');
    const viewed = $derived(viewedKind());
    // A network the account chose that this build lacks keeps its pill, so it can be left.
    const kinds = $derived(!view ? [] : inUse && !view.supported.includes(inUse) ? [...view.supported, inUse] : view.supported);
    const using = $derived(!!inUse && viewed === inUse);
    const missing = $derived(!!view && !view.supported.includes(viewed));
    const cls = $derived(h.stateClass(view, viewed));
    const status = $derived(using ? h.formatStatus(view)
        : missing ? `This build doesn't include ${h.label(viewed)}.`
        : view?.kind === 'unknown' ? 'Choose how Vector connects.'
        : 'Not in use.');
    // Up but with no outproxy answering, or I2P-Only with no relay inside I2P: only half works.
    const partial = $derived(!!view?.ready && ((view.steps || []).some((s) => s.id === 'exit' && s.state === 'fail') || t.stranded));
    // A lost I2P session rebuilds itself: it reads as connecting, like the glyph.
    const dot = $derived(!view ? '' : partial ? 'warn' : view.ready ? 'ok'
        : view.phase === 'starting' || view.reason?.code === 'session_lost' ? 'busy' : 'bad');
    // Tor's bootstrap holds the switch, as its toggle always did; nothing else that is merely
    // waiting does, so a network that can't connect never traps the user.
    const torStarting = $derived(inUse === 'tor' && view.detail?.status === 'bootstrapping');
    const held = $derived(t.locked || torStarting);
    const inCall = $derived(!!view?.call_active);

    // `retry_in` counts from when the view arrived; a start is timed from when its phase began.
    let now = $state(Date.now());
    $effect(() => {
        if (!using || (view.retry_in == null && view.phase !== 'starting')) return;
        now = Date.now();
        const timer = setInterval(() => { now = Date.now(); }, 1000);
        return () => clearInterval(timer);
    });
    const retryIn = $derived(view?.retry_in == null ? null : Math.max(1, Math.ceil(view.retry_in - Math.max(0, now - t.at) / 1000)));
    // A start nothing drives, or one past its budget, gets a way to try again.
    const stuck = $derived(using && startStuck(view, now));
    // Why the Use button can't be pressed, where a touch screen can read it.
    const holdWhy = $derived(inCall ? 'End the call to change networks.' : torStarting ? 'Wait for Tor to finish connecting.' : null);
    const sub = $derived.by(() => {
        if (!using && !missing && holdWhy) return { text: holdWhy, action: null };
        if (!using || view.ready) return null;
        if (view.phase === 'waiting') return { text: retryIn != null ? `Retrying in ${retryIn}s.` : '', action: 'Retry now' };
        if (view.phase === 'failed') return { text: view.reason?.text || '', action: 'Retry' };
        if (stuck) return { text: '', action: 'Retry' };
        return null;
    });

    // The network in use, by phase only: what a screen reader announces.
    const phaseWord = $derived(!view || !inUse ? ''
        : view.ready ? `${h.label(inUse)} connected.`
        : view.phase === 'failed' ? `${h.label(inUse)} failed to start.`
        : view.phase === 'starting' ? `${h.label(inUse)} connecting.`
        : view.phase === 'waiting' ? (view.reason?.text || '') : '');

    // Tor's rings double as a radial progress bar while Arti bootstraps.
    const progress = $derived(using && viewed === 'tor' && view.detail?.status === 'bootstrapping' && Number.isFinite(view.detail.bootstrap_progress)
        ? String(view.detail.bootstrap_progress) : null);

    let shownKind = $state('');
    let shown = $state('');
    let fading = $state(false);
    let still = $state(false);
    let swapTimer = 0;
    let releaseTimer = 0;
    $effect.pre(() => {
        const nextKind = viewed;
        const nextCls = cls;
        untrack(() => {
            if (!shownKind) { shownKind = nextKind; shown = nextCls; return; }
            if (nextKind === shownKind && nextCls === shown) return;
            still = true;
            fading = true;
            clearTimeout(swapTimer);
            clearTimeout(releaseTimer);
            swapTimer = setTimeout(() => { shownKind = nextKind; shown = nextCls; fading = false; }, 220);
            releaseTimer = setTimeout(() => { still = false; }, 500);
        });
    });
    $effect(() => () => { clearTimeout(swapTimer); clearTimeout(releaseTimer); });

    function glyphInto(svg) { h.injectGlyph(svg); }

    /** Arrow keys move between the pills (roving focus); only the viewed one is a tab stop. */
    function pickerKey(e) {
        const i = kinds.indexOf(viewed);
        const last = kinds.length - 1;
        const next = { ArrowRight: i + 1, ArrowDown: i + 1, ArrowLeft: i - 1, ArrowUp: i - 1, Home: 0, End: last }[e.key];
        if (next === undefined || !kinds.length) return;
        e.preventDefault();
        const j = (next + kinds.length) % kinds.length;
        h.selectView(kinds[j]);
        e.currentTarget.querySelectorAll('[role="tab"]')[j]?.focus();
    }
</script>

{#if view}
    <div class="net-picker" role="tablist" tabindex="-1" aria-label="Network" style:--n={kinds.length} style:--i={Math.max(0, kinds.indexOf(viewed))}
         onkeydown={pickerKey}>
        {#each kinds as k (k)}
            <button type="button" role="tab" class="net-pill net-pill-{k}" class:active={k === viewed} class:missing={!view.supported.includes(k)}
                    tabindex={k === viewed ? 0 : -1}
                    aria-selected={k === viewed} aria-label={k === inUse ? `${h.label(k)}, in use` : null} onclick={() => h.selectView(k)}>
                {#if k === inUse}<span class="net-dot {dot}" title="In use"></span>{/if}
                <span class="net-pill-label">{h.label(k)}</span>
            </button>
        {/each}
    </div>

    <div class="form-group tor-card net-card net-{shownKind || viewed} {shown || cls}" class:tor-no-transition={still}
         role="tabpanel" aria-label={h.label(viewed)} style:--tor-bootstrap-progress={progress}>
        <div class="tor-glyph-wrap net-glyph-wrap" style:opacity={fading ? '0' : null}>
            {#if (shownKind || viewed) === 'tor'}
                <svg class="tor-glyph" viewBox="0 0 120 120" aria-hidden="true" use:glyphInto></svg>
            {:else}
                <NetGlyph kind={shownKind || viewed} />
            {/if}
        </div>
        <div class="tor-card-body">
            <div class="tor-card-title">
                <InfoIcon side="lead" onclick={() => h.help(viewed === 'tor' || viewed === 'i2p' ? viewed : 'transport')} />
                {#if view.kind === 'unknown'}Choose a Network{:else if viewed === 'tor'}Route traffic through Tor<sup class="tor-tm">™</sup>{:else}{TITLES[viewed] || `Route traffic through ${h.label(viewed)}`}{/if}
            </div>
            <div class="tor-card-status">{status}</div>
            <!-- Read out once per phase: the bootstrap's percentages would crowd out other speech. -->
            <span class="sr-only" aria-live="polite">{phaseWord}</span>
            {#if sub}
                <div class="net-card-sub">
                    {#if sub.text}<span>{sub.text}</span>{/if}
                    {#if sub.action}<button type="button" class="net-link" disabled={t.locked} onclick={() => h.retry()}>{sub.action}</button>{/if}
                </div>
            {/if}
            {#if viewed === 'tor'}
                <!-- Per the Tor Project's trademark guidelines: a link home, the logo unaltered. -->
                <!-- svelte-ignore a11y_invalid_attribute -->
                <a href="#" class="tor-attribution" title="Visit torproject.org"
                   onclick={(e) => { e.preventDefault(); e.stopPropagation(); h.openLink('torAttribution'); }}>
                    <img src="/icons/tor-logo.svg" class="tor-attribution-logo" alt="Tor">
                </a>
            {/if}
        </div>
        {#if !using && !missing}
            <button type="button" class="net-use" class:is-busy={t.locked} disabled={held || inCall}
                    title={holdWhy} onclick={() => h.useKind(viewed)}>Use {h.label(viewed)}</button>
        {/if}
    </div>
{/if}
