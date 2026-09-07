<script>
    // The Tor card as ONE reconciler over the last TorState. Renderless: the card's
    // markup (glyph, attribution, toggle, Advanced disclosure) stays in index.html;
    // this adopts those elements and derives their state, the way five painters
    // used to be called in lockstep from every handler and poll.
    import { torState } from '../lib/settings.svelte.js';

    let { els, h } = $props();   // els: card, toggle, status, advanced, panel, refresh; h: stateClass, formatStatus, isTransitional

    const tor = torState();
    const state = $derived(tor.state);
    const cls = $derived(h.stateClass(state));
    const connected = $derived(!!state?.running);

    // ── the glyph's state class ──
    // CSS transitions the colours, but the keyframe animations (comet sweep, orbital
    // dots) snap at a state change. Fade the glyph out, swap the class while it is
    // invisible, fade back in; no-transition is held across the whole cycle so the
    // 0.5s inner tweens cannot crossfade the old colours during the fade-in.
    const STATE_CLASSES = ['tor-state-disabled', 'tor-state-bootstrapping', 'tor-state-connected', 'tor-state-failed'];
    let applied = null;
    $effect(() => {
        const next = cls;
        if (next === applied) return;
        const first = applied === null;
        applied = next;
        const card = els.card;
        const glyph = card.querySelector('.tor-glyph');
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

    // ── the rings as a radial progress bar while Arti bootstraps ──
    $effect(() => {
        const p = state?.bootstrap_progress;
        if (typeof p === 'number' && p >= 0 && p <= 100) els.card.style.setProperty('--tor-bootstrap-progress', String(p));
        else els.card.style.removeProperty('--tor-bootstrap-progress');
    });

    // ── the toggle: on when enabled or running; locked while transitional or mid-operation ──
    $effect(() => {
        els.toggle.checked = !!state?.running || !!state?.enabled;
        els.toggle.disabled = !state || !state.supported || tor.locked || h.isTransitional(state);
    });

    // ── the status line ──
    $effect(() => {
        els.status.textContent = tor.statusOverride || h.formatStatus(state);
    });

    // ── the Advanced disclosure: only once connected; the card fuses with it when shown ──
    $effect(() => {
        const open = connected && tor.advancedOpen;
        els.advanced.style.display = connected ? '' : 'none';
        els.card.classList.toggle('has-advanced', connected);
        els.advanced.classList.toggle('expanded', open);
        els.panel.style.display = open ? '' : 'none';
    });
    $effect(() => {
        if (els.refresh) els.refresh.disabled = tor.circuits.phase === 'loading';
    });
</script>
