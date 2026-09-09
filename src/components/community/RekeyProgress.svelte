<script>
    // The readout counts up with the ring's animated value rather than snapping to the
    // target, so it reads the live custom property each frame while open.
    import { rekeyState } from '../lib/rekey.svelte.js';
    const r = rekeyState();
    let ring = $state(null);
    let shown = $state(0);
    $effect(() => {
        if (!r.open || !ring) return;
        let raf = requestAnimationFrame(function tick() {
            shown = Math.round(parseFloat(getComputedStyle(ring).getPropertyValue('--rekey-pct')) || 0);
            raf = requestAnimationFrame(tick);
        });
        return () => cancelAnimationFrame(raf);
    });
</script>

{#if r.open}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="modal-overlay rekey-progress-overlay" onclick={(e) => e.stopPropagation()}>
        <div class="modal-box rekey-progress-box">
            <div class="rekey-ring" bind:this={ring} style="--rekey-pct: {r.pct}%;"><span class="rekey-pct">{shown}%</span></div>
            <p class="rekey-title">{r.title}</p>
            <p class="rekey-step">{r.step}</p>
            <p class="rekey-warning"><span class="icon icon-info"></span>Do not close the app during this process</p>
        </div>
    </div>
{/if}
