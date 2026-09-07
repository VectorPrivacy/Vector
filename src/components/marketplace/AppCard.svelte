<script>
    // One catalogue row: icon, name and version, description, category tags that truncate to the
    // row's width, size and date, and the install / update / play button with its action state.
    import { mktState, mktActions, mktAddFilter } from '../lib/marketplace.svelte.js';
    import AppIcon from './AppIcon.svelte';
    let { app, h, index } = $props();
    const st = mktState();
    const action = $derived(mktActions().get(app.id) || null);
    const spinning = $derived(action?.kind === 'installing' || action?.kind === 'updating');
    const label = $derived(action ? action.label : app.update_available ? 'Update' : app.installed ? h.actionText(app) : 'Install');

    // Fit the tags to the row: hide what overflows behind a "+N" pill with the hidden names as its tooltip.
    function fitTags(container, categories) {
        const gap = 6, overflowWidth = 35;
        let overflow = null;
        const fit = () => {
            const tags = [...container.querySelectorAll('.marketplace-app-category')];
            if (!tags.length) return;
            tags.forEach(t => { t.style.display = ''; });
            overflow?.remove(); overflow = null;
            const width = container.offsetWidth;
            if (!width) return;
            let used = 0, visible = 0;
            for (let i = 0; i < tags.length; i++) {
                const reserved = tags.length - i - 1 > 0 ? overflowWidth + gap : 0;
                const w = tags[i].offsetWidth + (i > 0 ? gap : 0);
                if (used + w + reserved <= width) { used += w; visible++; } else break;
            }
            visible = Math.max(1, visible);
            const hidden = categories.slice(visible);
            if (!hidden.length) return;
            for (let i = visible; i < tags.length; i++) tags[i].style.display = 'none';
            overflow = document.createElement('span');
            overflow.className = 'marketplace-app-category-overflow';
            overflow.textContent = `+${hidden.length}`;
            overflow.onmouseenter = (e) => { e.stopPropagation(); h.showTip(hidden.join(', '), overflow); };
            overflow.onmouseleave = (e) => { e.stopPropagation(); h.hideTip(); };
            container.appendChild(overflow);
        };
        const ro = new ResizeObserver(fit);
        ro.observe(container);
        requestAnimationFrame(fit);
        return { destroy() { ro.disconnect(); } };
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="marketplace-app-card" class:marketplace-animate-in={st.animate} data-app-id={app.id}
     style:animation-delay={st.animate ? `${(index + 1) * 0.03}s` : undefined} onclick={() => h.showDetails(app)}>
    <div class="marketplace-app-icon-container">
        <AppIcon {app} {h} cls="marketplace-app-icon" ph="marketplace-app-icon-placeholder" />
    </div>
    <div class="marketplace-app-info">
        <div class="marketplace-app-header">
            <span class="marketplace-app-name">{app.name}</span>
            {#if app.update_available && app.installed_version}
                <span class="marketplace-app-version update-available" title="Update available: {app.installed_version} → {app.version}">v{app.installed_version} → v{app.version}</span>
            {:else}
                <span class="marketplace-app-version">v{app.version}</span>
            {/if}
        </div>
        <div class="marketplace-app-description">{app.description}</div>
        {#if app.categories.length}
            <div class="marketplace-app-categories" use:fitTags={app.categories}>
                {#each app.categories as c}
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                    <span class="marketplace-app-category" onclick={(e) => { e.stopPropagation(); mktAddFilter(c); }}>{c}</span>
                {/each}
            </div>
        {/if}
        <div class="marketplace-app-meta">
            <span class="marketplace-app-size">{h.fileSize(app.size)}</span>
            <span class="marketplace-app-date">{h.publishDate(app.published_at)}</span>
        </div>
    </div>
    <button class="marketplace-install-btn" class:update-available={!action && app.update_available}
            class:installed={!action && app.installed && !app.update_available}
            class:installing={action?.kind === 'installing'} class:updating={action?.kind === 'updating'}
            class:failed={action?.kind === 'failed'} disabled={!!action && action.kind !== 'failed'}
            onclick={(e) => { e.stopPropagation(); h.cardAction(app); }}>
        {#if spinning}<span class="marketplace-progress-spinner" data-app-id={app.id}></span>{:else}{label}{/if}
    </button>
</div>
