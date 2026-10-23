<script>
    // One app in full: its identity and actions, the facts at a glance, its tags, what it
    // is, what's new, the permissions it holds once installed, who published it, and the
    // finer print.
    import { mktState, mktApps, mktActions, mktPerms } from '../lib/marketplace.svelte.js';
    import { controlsOf, fitsDevice, isGame, isInstalled, isMultiplayer, needsLabel, tagLabel, tagsOf } from '../lib/nexus.js';
    import { mktDevice } from '../lib/marketplace.svelte.js';
    import AppIcon from './AppIcon.svelte';
    import AppAction from './AppAction.svelte';
    import { profileVersion } from '../lib/signals.svelte.js';
    let { h } = $props();   // h: MarketplaceHelpers
    const st = mktState();
    const app = $derived(mktApps().find(a => a.id === st.detailsId) || null);
    const action = $derived(app ? mktActions().get(app.id) || null : null);
    const installed = $derived(!!app && isInstalled(app));
    const perms = $derived(mktPerms());
    const publisher = $derived.by(() => {
        if (!app) return null;
        profileVersion(app.publisher);
        return h.publisher(app.publisher);
    });
    const npubShort = $derived(!app ? '' : app.publisher.length > 20
        ? `${app.publisher.slice(0, 12)}…${app.publisher.slice(-8)}` : app.publisher);
    const wantsPerms = $derived(!!app?.requested_permissions && installed);
    const busy = $derived(!!action && action.kind !== 'failed');
    const dev = mktDevice();
    const controls = $derived(app ? controlsOf(app) : null);
    const fits = $derived(!app || fitsDevice(app, dev));
    // Installed by any path (here, a sync, another window): its toggles load then.
    $effect(() => { if (wantsPerms && !perms) h.loadPerms(app); });
    async function toggle(e, p) {
        const box = e.currentTarget;
        if (!(await h.setPermission(app, p.id, box.checked))) box.checked = p.granted;
    }
</script>

{#if app}
<article class="nx-detail">
    <header class="nx-d-head">
        <AppIcon {app} {h} size={112} />
        <div class="nx-d-id">
            <h1 class="nx-d-name">{app.name}</h1>
            <div class="nx-d-maker">{app.developer || publisher?.name || 'Unknown maker'}</div>
        </div>
        <div class="nx-d-actions">
            <AppAction {app} {h} size="lg" />
            {#if isMultiplayer(app)}
                <button type="button" class="nx-btn soft" disabled={busy} onclick={() => h.playWith(app)}>
                    <span class="nx-g g-users"></span>Play with Friends
                </button>
            {/if}
            {#if h.isMine(app)}
                <button type="button" class="nx-btn soft nx-edit" onclick={() => h.editListing(app)}>
                    <span class="nx-g g-edit"></span>Edit Listing
                </button>
            {/if}
        </div>
    </header>

    <div class="nx-facts">
        <div class="nx-fact">
            <span class="nx-fact-k">Version</span>
            <span class="nx-fact-v">{app.version}</span>
            {#if app.update_available && app.installed_version}<span class="nx-fact-s">You have {app.installed_version}</span>{/if}
        </div>
        <div class="nx-fact">
            <span class="nx-fact-k">Size</span>
            <span class="nx-fact-v">{h.fileSize(app.size)}</span>
        </div>
        <div class="nx-fact">
            <span class="nx-fact-k">Released</span>
            <span class="nx-fact-v">{h.publishDate(app.published_at)}</span>
        </div>
        <div class="nx-fact">
            <span class="nx-fact-k">{isGame(app) ? 'Players' : 'Kind'}</span>
            <span class="nx-fact-v">{isGame(app) ? (isMultiplayer(app) ? 'Multiplayer' : 'Solo') : 'App'}</span>
        </div>
    </div>

    {#if controls.touch || controls.pad || controls.keys}
        <div class="nx-plays">
            <span class="nx-fact-k">Plays With</span>
            {#if controls.touch}<span class="nx-play" class:have={dev.touch}><span class="nx-g g-touch"></span>Touch</span>{/if}
            {#if controls.pad}<span class="nx-play" class:have={dev.pad}><span class="nx-g g-pad"></span>Controller</span>{/if}
            {#if controls.keys}<span class="nx-play" class:have={dev.keys}><span class="nx-g g-keys"></span>Keyboard & Mouse</span>{/if}
        </div>
    {/if}
    {#if !fits}
        <div class="nx-warn"><span class="nx-g g-warn"></span>Made for {needsLabel(app)}, which this device doesn't seem to have</div>
    {/if}

    {#if tagsOf(app).length}
        <div class="nx-chips">
            {#each tagsOf(app) as t}<button type="button" class="nx-chip" onclick={() => h.show('category', t)}>{tagLabel(t)}</button>{/each}
        </div>
    {/if}

    {#if app.description}
        <section class="nx-d-sec">
            <h3 class="nx-h3">About</h3>
            <p class="nx-d-prose">{app.description}</p>
        </section>
    {/if}

    {#if app.changelog}
        <section class="nx-d-sec">
            <div class="nx-d-sec-head"><h3 class="nx-h3">What's New</h3><span class="nx-d-ver">Version {app.version}</span></div>
            <p class="nx-d-prose">{app.changelog}</p>
        </section>
    {/if}

    {#if wantsPerms}
        <section class="nx-d-sec">
            <h3 class="nx-h3">Permissions</h3>
            <p class="nx-d-hint">What {app.name} may use. Turn any off at any time.</p>
            <div class="nx-list">
                {#if !perms}
                    <div class="nx-list-row"><span class="nx-d-hint">Loading permissions…</span></div>
                {:else if perms.error}
                    <div class="nx-list-row"><span class="nx-d-hint">{perms.error}</span></div>
                {:else}
                    {#each perms.items as p (p.id)}
                        <div class="nx-list-row">
                            <div class="nx-list-text">
                                <span class="nx-list-label">{p.label}</span>
                                <span class="nx-list-sub">{p.description}</span>
                            </div>
                            <label class="toggle-container nx-toggle">
                                <input type="checkbox" checked={p.granted} onchange={(e) => toggle(e, p)}>
                                <span class="neon-toggle"></span>
                            </label>
                        </div>
                    {/each}
                {/if}
            </div>
            {#if perms?.items?.length}
                <button type="button" class="nx-link danger" onclick={() => h.resetPermissions(app)}>Reset Permissions</button>
            {/if}
        </section>
    {/if}

    <section class="nx-d-sec">
        <h3 class="nx-h3">Publisher</h3>
        <button type="button" class="nx-list nx-pub" onclick={() => h.openPublisher(app.publisher)}>
            <span class="nx-pub-avatar">
                {#if publisher?.avatar}<img src={publisher.avatar} alt="" draggable="false">{:else}<span class="nx-g g-user"></span>{/if}
            </span>
            <span class="nx-list-text">
                <span class="nx-list-label">{publisher?.name || npubShort}</span>
                {#if publisher && !publisher.veiled}<span class="nx-list-sub">{npubShort}</span>{/if}
            </span>
            <span class="nx-g g-right nx-pub-chev"></span>
        </button>
    </section>

    <section class="nx-d-sec">
        <h3 class="nx-h3">Information</h3>
        <div class="nx-list">
            {#if app.developer}<div class="nx-list-row"><span class="nx-info-k">Developer</span><span class="nx-info-v">{app.developer}</span></div>{/if}
            <div class="nx-list-row"><span class="nx-info-k">Category</span><span class="nx-info-v">{isGame(app) ? 'Game' : 'App'}</span></div>
            {#if installed && app.installed_version}<div class="nx-list-row"><span class="nx-info-k">Installed</span><span class="nx-info-v">Version {app.installed_version}</span></div>{/if}
            {#if app.source_url}
                <div class="nx-list-row">
                    <span class="nx-info-k">Source</span>
                    <button type="button" class="nx-info-v nx-info-link" onclick={() => h.openUrl(app.source_url)}>{h.sourceUrl(app.source_url)}<span class="nx-g g-out"></span></button>
                </div>
            {/if}
            <div class="nx-list-row"><span class="nx-info-k">App ID</span><span class="nx-info-v mono">{app.id}</span></div>
        </div>
        {#if installed}
            <button type="button" class="nx-btn ghost danger nx-d-uninstall" disabled={busy} onclick={() => h.uninstall(app)}>
                {action?.kind === 'uninstalling' ? action.label : `Uninstall ${app.name}`}
            </button>
        {/if}
    </section>
</article>
{/if}
