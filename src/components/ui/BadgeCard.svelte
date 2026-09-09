<script>
    // The 3D badge card. Tilt and sheen are CSS custom properties so the transform and
    // gradients stay composited; the pointer and gyro math lives in badge-card.js.
    import { badgeCardState, badgeCardEls, badgeCardHandlers } from '../lib/badgecard.svelte.js';
    const b = badgeCardState();
    const els = badgeCardEls();
    const h = () => badgeCardHandlers();
    const badge = $derived(b.badge);
    const tiers = $derived.by(() => {
        const tp = badge.tiers;
        if (!tp || !tp.total) return [];
        return Array.from({ length: tp.total }, (_, i) => {
            const ic = tp.icons && tp.icons[i];
            return { n: i + 1, filled: i + 1 <= tp.current, src: ic ? (/:\/\/|^data:|^blob:/.test(ic) ? ic : './icons/' + ic) : '', connFilled: tp.current >= i + 2 };
        });
    });
    function bindCard(node) { els.card = node; return { destroy() { els.card = null; } }; }
</script>

{#if b.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="badge-card-overlay" id="badge-card-overlay" class:is-visible={b.visible} style="display: flex;"
         onclick={(e) => { if (e.target === e.currentTarget) h().close?.(); }}>
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="badge-card-scene" onpointermove={(e) => h().pointerMove?.(e)} onpointerleave={() => h().pointerEnd?.()} onpointercancel={() => h().pointerEnd?.()}>
            <div class="badge-card" class:badge-card--idle={b.tilt.idle} use:bindCard
                 style="--rx: {b.tilt.rx}; --ry: {b.tilt.ry}; --mx: {b.tilt.mx}; --my: {b.tilt.my}; --holo: {b.tilt.holo};">
                <div class="badge-card-holo"></div>
                <div class="badge-card-glare"></div>
                <img class="badge-card-badge" alt="" draggable="false" src={badge.src}>
                <div class="badge-card-title">{badge.title}</div>
                {#if badge.subtitle}<div class="badge-card-subtitle">{badge.subtitle}</div>{/if}
                <!-- Trusted copy: the badge texts are hardcoded in the app. -->
                <div class="badge-card-desc">{@html badge.html}</div>
                {#if tiers.length}
                    <div class="badge-card-tiers">
                        {#each tiers as t (t.n)}
                            <div class="badge-card-tier-node" class:is-filled={t.filled}><img alt="" draggable="false" src={t.src || undefined}></div>
                            {#if t.n < tiers.length}<div class="badge-card-tier-conn" class:is-filled={t.connFilled}></div>{/if}
                        {/each}
                    </div>
                {/if}
                {#if badge.access}
                    <div class="badge-card-access">
                        <svg class="badge-card-access-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="8" r="6"></circle><path d="M15.477 12.89 17 22l-5-3-5 3 1.523-9.11"></path></svg>
                        <span class="badge-card-access-text">{badge.access}</span>
                    </div>
                {/if}
                {#if badge.perks?.length}
                    <div class="badge-card-perks">
                        <div class="badge-card-perks-label">{badge.perks.length === 1 ? 'Perk' : 'Perks'}</div>
                        {#each badge.perks as p}
                            <div class="badge-card-perk">
                                <span class="badge-card-perk-text">{p.text}</span>
                                {#if p.sub}<span class="badge-card-perk-sub">{p.sub}</span>{/if}
                            </div>
                        {/each}
                    </div>
                {/if}
            </div>
        </div>
    </div>
{/if}
