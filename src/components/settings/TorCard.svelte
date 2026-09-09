<script>
    // The Tor card: glyph, title, status line, attribution and the toggle, all derived
    // from the last TorState. The glyph's state class is swapped while it is faded out
    // so its keyframe animations cannot snap mid-frame.
    import { torState } from '../lib/settings.svelte.js';
    import InfoIcon from './InfoIcon.svelte';

    let { h } = $props();   // h: stateClass, formatStatus, isTransitional, setTorEnabled(on), injectGlyph(svg), help(key), openLink(key)

    const tor = torState();
    const state = $derived(tor.state);
    const cls = $derived(h.stateClass(state));
    const connected = $derived(!!state?.running);
    const checked = $derived(!!state?.running || !!state?.enabled);
    const disabled = $derived(!state || !state.supported || tor.locked || h.isTransitional(state));
    const status = $derived(tor.statusOverride || h.formatStatus(state));

    let card = $state(null);
    let glyph = $state(null);

    // CSS transitions the colours, but the keyframe animations (comet sweep, orbital
    // dots) snap at a state change. Fade the glyph out, swap the class while it is
    // invisible, fade back in; no-transition is held across the whole cycle.
    const STATE_CLASSES = ['tor-state-disabled', 'tor-state-bootstrapping', 'tor-state-connected', 'tor-state-failed'];
    let applied = null;
    $effect(() => {
        const next = cls;
        if (!card || next === applied) return;
        const first = applied === null;
        applied = next;
        if (first || !glyph) {
            card.classList.remove(...STATE_CLASSES);
            card.classList.add(next);
            return;
        }
        card.classList.add('tor-no-transition');
        glyph.style.opacity = '0';
        const swap = setTimeout(() => {
            card.classList.remove(...STATE_CLASSES);
            card.classList.add(next);
            void glyph.offsetWidth;   // commit the new state while invisible
            glyph.style.opacity = '';
        }, 220);
        const release = setTimeout(() => card.classList.remove('tor-no-transition'), 500);
        return () => { clearTimeout(swap); clearTimeout(release); };
    });

    // The rings double as a radial progress bar while Arti bootstraps.
    $effect(() => {
        if (!card) return;
        const p = state?.bootstrap_progress;
        if (typeof p === 'number' && p >= 0 && p <= 100) card.style.setProperty('--tor-bootstrap-progress', String(p));
        else card.style.removeProperty('--tor-bootstrap-progress');
    });

    function glyphInto(svg) { h.injectGlyph(svg); }
</script>

<div class="form-group tor-card" id="settings-tor-card" class:has-advanced={connected} bind:this={card}>
    <div class="tor-glyph-wrap">
        <svg class="tor-glyph" viewBox="0 0 120 120" aria-hidden="true" bind:this={glyph} use:glyphInto></svg>
    </div>
    <div class="tor-card-body">
        <div class="tor-card-title">
            <InfoIcon side="lead" onclick={() => h.help('tor')} />
            Route traffic through Tor<sup class="tor-tm">™</sup>
        </div>
        <div id="privacy-tor-status" class="tor-card-status">{status}</div>
        <!-- Per the Tor Project's trademark guidelines: a link home, the logo unaltered. -->
        <!-- svelte-ignore a11y_invalid_attribute -->
        <a href="#" id="tor-attribution-link" class="tor-attribution" title="Visit torproject.org"
           onclick={(e) => { e.preventDefault(); e.stopPropagation(); h.openLink('torAttribution'); }}>
            <img src="/icons/tor-logo.svg" class="tor-attribution-logo" alt="Tor">
        </a>
    </div>
    <label class="toggle-container tor-card-toggle">
        <input type="checkbox" id="privacy-tor-toggle" {checked} {disabled} onchange={(e) => h.setTorEnabled(e.currentTarget.checked)}>
        <span class="neon-toggle"></span>
    </label>
</div>
