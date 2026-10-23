<script>
    // One app as a row: icon, name, what it is, a line about it, and its action. `note`
    // replaces the line (a library row says when you last played).
    import AppIcon from './AppIcon.svelte';
    import AppAction from './AppAction.svelte';
    import { mktDevice } from '../lib/marketplace.svelte.js';
    import { fitsDevice, leadTag, needsLabel, tagLabel, isMultiplayer } from '../lib/nexus.js';
    let { app, h, note = '', index = 0 } = $props();
    const dev = mktDevice();
    const fits = $derived(fitsDevice(app, dev));
</script>

<!-- A div, not a button: it holds the action's own button. -->
<div class="nx-row" role="button" tabindex="0" style:--i={index} onclick={() => h.showDetails(app)}
     onkeydown={(e) => { if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); h.showDetails(app); } }}>
    <AppIcon {app} {h} size={56} />
    <div class="nx-row-text">
        <div class="nx-row-name">{app.name}</div>
        <div class="nx-row-sub">
            {#if isMultiplayer(app)}<span class="nx-g g-users nx-row-mp" aria-label="Multiplayer"></span>{/if}
            <span>{tagLabel(leadTag(app))}</span>
            {#if app.update_available}<span class="nx-dot"></span><span class="nx-row-new">Update ready</span>
            {:else if !fits}<span class="nx-dot"></span><span class="nx-row-warn">For {needsLabel(app)}</span>{/if}
        </div>
        <div class="nx-row-line">{note || app.description}</div>
    </div>
    <AppAction {app} {h} />
</div>
