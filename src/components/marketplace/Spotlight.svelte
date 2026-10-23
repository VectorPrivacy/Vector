<script>
    // The front page's lead: a few apps in turn, each over a wash of its own icon. Moves on
    // by itself unless a pointer rests on it or motion is reduced; dots and swipes pick one,
    // and a tap anywhere but its buttons opens the app.
    import { mktIcons } from '../lib/marketplace.svelte.js';
    import AppIcon from './AppIcon.svelte';
    import AppAction from './AppAction.svelte';
    import { isMultiplayer, isInstalled, tagLabel, tagsOf } from '../lib/nexus.js';
    let { apps, h } = $props();
    const DWELL_MS = 7000;

    let at = $state(0);
    let held = $state(false);
    const shown = $derived(apps[Math.min(at, apps.length - 1)]);
    const still = matchMedia('(prefers-reduced-motion: reduce)').matches;

    $effect(() => {
        if (still || held || apps.length < 2) return;
        at;
        const t = setTimeout(() => { at = (at + 1) % apps.length; }, DWELL_MS);
        return () => clearTimeout(t);
    });

    let x0 = null, y0 = 0;
    let dragged = false;
    function down(e) { x0 = e.clientX; y0 = e.clientY; dragged = false; }
    function up(e) {
        if (x0 == null) return;
        const dx = e.clientX - x0;
        x0 = null;
        // A drag is a swipe, or nothing: never also a tap on what it ended over.
        dragged = Math.hypot(dx, e.clientY - y0) > 8;
        if (Math.abs(dx) < 40 || apps.length < 2) return;
        at = (at + (dx < 0 ? 1 : -1) + apps.length) % apps.length;
    }
    function open(e) {
        if (dragged || e.target.closest('button')) return;
        h.showDetails(shown);
    }
    const wash = (app) => { const src = mktIcons().get(h.iconKey(app)); return src ? `url("${src}")` : 'none'; };
</script>

{#if shown}
<!-- The card's own Details button is the keyboard's way in. -->
<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<section class="nx-spot" onpointerenter={(e) => { if (e.pointerType === 'mouse') held = true; }} onpointerleave={() => { held = false; }}
         onpointerdown={down} onpointerup={up} onpointercancel={() => { x0 = null; }} onclick={open}>
    {#each apps as app, i (app.id)}
        <div class="nx-spot-wash" class:on={i === at} style:--wash={wash(app)}></div>
    {/each}
    <div class="nx-spot-shade"></div>
    {#key shown.id}
        <div class="nx-spot-body">
            <div class="nx-spot-text">
                <span class="nx-eyebrow">{isMultiplayer(shown) ? 'Play Together' : 'New in the Nexus'}</span>
                <h2 class="nx-spot-name">{shown.name}</h2>
                <p class="nx-spot-desc">{shown.description}</p>
                <div class="nx-spot-tags">
                    {#each tagsOf(shown).slice(0, 3) as t}<span class="nx-chip quiet">{tagLabel(t)}</span>{/each}
                </div>
                <div class="nx-spot-actions">
                    <AppAction app={shown} {h} size="lg" />
                    {#if isMultiplayer(shown) && isInstalled(shown)}
                        <button type="button" class="nx-btn soft" onclick={() => h.playWith(shown)}><span class="nx-g g-users"></span>Play with Friends</button>
                    {:else}
                        <button type="button" class="nx-btn soft" onclick={() => h.showDetails(shown)}>Details</button>
                    {/if}
                </div>
            </div>
            <div class="nx-spot-art"><AppIcon app={shown} {h} size={132} /></div>
        </div>
    {/key}
    {#if apps.length > 1}
        <div class="nx-spot-dots">
            {#each apps as app, i (app.id)}
                <button type="button" class="nx-spot-dot" class:on={i === at} aria-label={app.name} onclick={() => { at = i; }}></button>
            {/each}
        </div>
    {/if}
</section>
{/if}
