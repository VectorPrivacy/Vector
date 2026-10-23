<script>
    // An app's one-tap action: Install, Update, or Play / Open once it's here, with the
    // download's ring while it comes down and what went wrong for a moment after.
    import { mktActions } from '../lib/marketplace.svelte.js';
    import { isInstalled } from '../lib/nexus.js';
    let { app, h, size = 'sm' } = $props();   // h: MarketplaceHelpers
    const action = $derived(mktActions().get(app.id) || null);
    const downloading = $derived(action?.kind === 'installing' || action?.kind === 'updating');
    const pct = $derived(Math.max(4, action?.pct || 0));
    const kind = $derived(action?.kind === 'failed' ? 'failed'
        : app.update_available ? 'update'
        : isInstalled(app) ? 'play' : 'install');
    const label = $derived(action && !downloading ? action.label
        : kind === 'update' ? 'Update' : kind === 'play' ? h.actionText(app) : 'Install');
</script>

<button type="button" class="nx-act nx-{size} {kind}" class:busy={!!action && action.kind !== 'failed'}
        disabled={!!action && action.kind !== 'failed'}
        aria-label={downloading ? `${action.label} ${app.name}` : `${label} ${app.name}`}
        onclick={(e) => { e.stopPropagation(); h.cardAction(app); }}>
    {#if downloading}
        <span class="nx-ring" style:--p={`${pct}%`}></span>
    {:else}
        {label}
    {/if}
</button>
