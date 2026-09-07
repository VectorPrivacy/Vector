<script>
    // One app in full: hero with the actions its state allows, description, changelog,
    // developer, permission toggles for an installed app, publisher, and the file facts.
    import { mktState, mktApps, mktActions, mktPerms, mktAddFilter } from '../lib/marketplace.svelte.js';
    import AppIcon from './AppIcon.svelte';
    let { h } = $props();
    const st = mktState();
    const app = $derived(mktApps().find(a => a.id === st.detailsId) || null);
    const action = $derived(app ? mktActions().get(app.id) || null : null);
    const installed = $derived(!!app && (app.installed || app.local_path));
    const perms = $derived(mktPerms());
    const publisher = $derived(app ? h.publisher(app.publisher) : null);
    const npubShort = $derived(!app ? '' : app.publisher.length > 20
        ? app.publisher.substring(0, 12) + '...' + app.publisher.substring(app.publisher.length - 8) : app.publisher);
    const wantsPerms = $derived(!!app && app.requested_permissions && app.requested_permissions.length > 0 && installed);
    const busy = $derived(!!action && action.kind !== 'failed');
    const spinning = $derived(action?.kind === 'installing' || action?.kind === 'updating');

    function pickCategory(c) { h.closeDetails(); mktAddFilter(c); }
</script>

{#if app}
    <div class="app-details-hero">
        <div class="app-details-icon-container">
            <AppIcon {app} {h} cls="app-details-icon" ph="app-details-icon-placeholder" />
        </div>
        <h1 class="app-details-name">{app.name}</h1>
        {#if app.update_available && app.installed_version}
            <span class="app-details-version">
                <span class="app-details-version-installed">Installed: v{app.installed_version}</span>
                <span class="app-details-version-arrow">→</span>
                <span class="app-details-version-new">v{app.version}</span>
            </span>
        {:else}
            <span class="app-details-version">Version {app.version}</span>
        {/if}
        {#if app.categories.length}
            <div class="app-details-categories">
                {#each app.categories as c}
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                    <span class="app-details-category" onclick={() => pickCategory(c)}>{c}</span>
                {/each}
            </div>
        {/if}
        {#if app.update_available}
            <div class="app-details-actions">
                <button class="app-details-action-btn app-details-update-btn" class:updating={action?.kind === 'updating'} disabled={busy} onclick={() => h.update(app)}>
                    {#if spinning}<span class="marketplace-progress-spinner" data-app-id={app.id}></span>{:else}{action?.label || 'Update'}{/if}
                </button>
                <button class="app-details-action-btn app-details-uninstall-btn" disabled={busy} onclick={() => h.uninstall(app)}>
                    {action?.kind === 'uninstalling' ? action.label : 'Uninstall'}
                </button>
            </div>
        {:else if installed}
            <div class="app-details-actions">
                <button class="app-details-action-btn app-details-play-btn" disabled={busy} onclick={() => h.play(app)}>
                    {action?.kind === 'launching' ? action.label : h.actionText(app)}
                </button>
                <button class="app-details-action-btn app-details-uninstall-btn" disabled={busy} onclick={() => h.uninstall(app)}>
                    {action?.kind === 'uninstalling' || action?.kind === 'failed' ? action.label : 'Uninstall'}
                </button>
            </div>
        {:else}
            <button class="app-details-action-btn" class:installing={action?.kind === 'installing'} disabled={busy} onclick={() => h.install(app)}>
                {#if spinning}<span class="marketplace-progress-spinner" data-app-id={app.id}></span>{:else}{action?.label || 'Install'}{/if}
            </button>
        {/if}
    </div>

    {#if app.description}
        <div class="app-details-section">
            <h3 class="app-details-section-title">Description</h3>
            <p class="app-details-description">{app.description}</p>
        </div>
    {/if}
    {#if app.changelog}
        <div class="app-details-section">
            <h3 class="app-details-section-title">What's New</h3>
            <p class="app-details-changelog">{app.changelog}</p>
        </div>
    {/if}
    {#if app.developer}
        <div class="app-details-section">
            <h3 class="app-details-section-title">Developer</h3>
            <div class="app-details-developer"><span class="app-details-developer-name">{app.developer}</span></div>
        </div>
    {/if}
    {#if wantsPerms}
        <div class="app-details-section">
            <h3 class="app-details-section-title">Permissions</h3>
            <p class="app-details-permissions-hint">This app requests the following permissions. Toggle to grant or revoke access.</p>
            <div class="app-details-permissions" data-app-id={app.id}>
                {#if !perms}
                    <div class="app-details-permissions-loading">Loading permissions...</div>
                {:else if perms.error}
                    <p class="app-details-permissions-hint">{perms.error}</p>
                {:else}
                    {#each perms.items as p (p.id)}
                        <div class="app-details-permission-item">
                            <div class="app-details-permission-info">
                                <span class="app-details-permission-label">{p.label}</span>
                                <span class="app-details-permission-desc">{p.description}</span>
                            </div>
                            <label class="toggle-container app-details-permission-toggle">
                                <input type="checkbox" checked={p.granted} onchange={(e) => h.setPermission(app, p.id, e.currentTarget.checked)}>
                                <span class="neon-toggle"></span>
                            </label>
                        </div>
                    {:else}
                        <p class="app-details-permissions-hint">No permissions available</p>
                    {/each}
                    <button class="app-details-reset-permissions-btn" onclick={() => h.resetPermissions(app)}>Reset Permissions</button>
                {/if}
            </div>
        </div>
    {/if}

    <div class="app-details-section">
        <h3 class="app-details-section-title">Publisher</h3>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="app-details-publisher" onclick={() => h.openPublisher(app.publisher)}>
            <div class="app-details-publisher-avatar">
                {#if publisher?.avatar}<img src={publisher.avatar} alt="Avatar">{:else}<span class="icon icon-user-circle"></span>{/if}
            </div>
            <div class="app-details-publisher-info">
                <p class="app-details-publisher-name">{publisher?.name || app.publisher.substring(0, 12) + '...'}</p>
                <span class="app-details-publisher-npub">{npubShort}</span>
            </div>
            <div class="app-details-publisher-arrow-container">
                <span class="icon icon-chevron-double-left app-details-publisher-arrow"></span>
            </div>
        </div>
    </div>

    <div class="app-details-section">
        <h3 class="app-details-section-title">Information</h3>
        <div class="app-details-meta">
            <div class="app-details-meta-row"><span class="app-details-meta-label">Size</span><span class="app-details-meta-value">{h.fileSize(app.size)}</span></div>
            <div class="app-details-meta-row"><span class="app-details-meta-label">Published</span><span class="app-details-meta-value">{h.publishDate(app.published_at)}</span></div>
            {#if app.source_url}
                <div class="app-details-meta-row"><span class="app-details-meta-label">Source</span><a href="#top" class="app-details-meta-link" onclick={(e) => { e.preventDefault(); h.openUrl(app.source_url); }}>{h.sourceUrl(app.source_url)}</a></div>
            {/if}
            <div class="app-details-meta-row"><span class="app-details-meta-label">App ID</span><span class="app-details-meta-value" style="font-family: monospace; font-size: 12px;">{app.id}</span></div>
        </div>
    </div>
{/if}
