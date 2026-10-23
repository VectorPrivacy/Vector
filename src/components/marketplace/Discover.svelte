<script>
    // The Nexus front page: the spotlight, picks for this device's controls, what you played
    // last, games to play together, the newest arrivals, the apps that aren't games, and the
    // categories to browse. Only what fits this device, unless that filter is off.
    import { mktApps, mktPlayed, mktDevice, mktFits } from '../lib/marketplace.svelte.js';
    import { controlsOf, inCategory, isGame, isInstalled, isMultiplayer, newestFirst, tagCounts, tagLabel } from '../lib/nexus.js';
    import Spotlight from './Spotlight.svelte';
    import Shelf from './Shelf.svelte';
    import AppTile from './AppTile.svelte';
    import AppRow from './AppRow.svelte';
    import AppIcon from './AppIcon.svelte';
    let { h } = $props();   // h: MarketplaceHelpers

    const dev = mktDevice();
    const fit = $derived(mktApps().filter(mktFits));
    const newest = $derived(newestFirst(fit));
    const games = $derived(newest.filter(isGame));
    const together = $derived(games.filter(isMultiplayer));
    const tools = $derived(newest.filter(a => !isGame(a)));
    // What this device plays best: its controller, its touchscreen, or its keyboard.
    const picks = $derived.by(() => {
        const out = [];
        if (dev.pad) out.push({ key: 'pad', title: 'Grab Your Controller', glyph: 'g-pad', tag: 'controller', apps: games.filter(a => controlsOf(a).pad) });
        if (dev.touch && !dev.keys) out.push({ key: 'touch', title: 'Play on the Go', glyph: 'g-touch', tag: 'mobile', apps: newest.filter(a => controlsOf(a).touch) });
        else if (dev.keys) out.push({ key: 'keys', title: 'Made for Keyboard & Mouse', glyph: 'g-keys', tag: 'keyboard', apps: newest.filter(a => controlsOf(a).keys) });
        return out.filter(p => p.apps.length);
    });
    // Five to lead with: the newest to play together, topped up with the newest games; what
    // this device plays best goes first.
    const spotlight = $derived.by(() => {
        const pick = together.slice(0, 5);
        for (const g of games) if (pick.length < 5 && !pick.includes(g)) pick.push(g);
        const best = (a) => (dev.pad && controlsOf(a).pad) || (dev.touch && !dev.keys && controlsOf(a).touch);
        return [...pick.filter(best), ...pick.filter(a => !best(a))];
    });
    const recent = $derived.by(() => {
        const byId = new Map(mktApps().map(a => [a.id, a]));
        return mktPlayed().map(p => ({ app: byId.get(p.id), at: p.at })).filter(r => r.app && isInstalled(r.app)).slice(0, 12);
    });
    const categories = $derived(tagCounts(fit).filter(c => c.tag !== 'multiplayer' && c.n > 1).slice(0, 8)
        .map(c => ({ ...c, faces: newest.filter(a => inCategory(a, c.tag)).slice(0, 3) })));
</script>

<Spotlight apps={spotlight} {h} />

{#each picks as p (p.key)}
    <Shelf title={p.title} glyph={p.glyph} more={() => h.show('category', p.tag)}>
        {#each p.apps as app (app.id)}<AppTile {app} {h} />{/each}
    </Shelf>
{/each}

{#if recent.length}
    <Shelf title="Jump Back In" more={() => h.show('library')}>
        {#each recent as r (r.app.id)}<AppTile app={r.app} {h} sub={h.playedAgo(r.at)} />{/each}
    </Shelf>
{/if}

{#if together.length}
    <Shelf title="Play Together" more={() => h.show('category', 'multiplayer')}>
        {#each together as app (app.id)}<AppTile {app} {h} />{/each}
    </Shelf>
{/if}

<section class="nx-section">
    <div class="nx-shelf-head">
        <h3 class="nx-h3">New Arrivals</h3>
        <button type="button" class="nx-link" onclick={() => h.show('all')}>See All</button>
    </div>
    <div class="nx-rows">
        {#each newest.slice(0, 6) as app, i (app.id)}<AppRow {app} {h} index={i} />{/each}
    </div>
</section>

{#if tools.length}
    <Shelf title="Apps & Tools" more={tools.length > 6 ? () => h.show('category', 'app') : null}>
        {#each tools as app (app.id)}<AppTile {app} {h} />{/each}
    </Shelf>
{/if}

{#if categories.length}
    <section class="nx-section">
        <div class="nx-shelf-head"><h3 class="nx-h3">Browse by Category</h3></div>
        <div class="nx-cats">
            {#each categories as c (c.tag)}
                <button type="button" class="nx-cat" onclick={() => h.show('category', c.tag)}>
                    <span class="nx-cat-text">
                        <span class="nx-cat-name">{tagLabel(c.tag)}</span>
                        <span class="nx-cat-n">{c.n} {c.n === 1 ? 'app' : 'apps'}</span>
                    </span>
                    <span class="nx-cat-faces">
                        {#each c.faces as app (app.id)}<AppIcon {app} {h} size={34} />{/each}
                    </span>
                </button>
            {/each}
        </div>
    </section>
{/if}
