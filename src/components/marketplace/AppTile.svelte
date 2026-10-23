<script>
    // One app as a shelf tile: a large icon over its name and what it is.
    import AppIcon from './AppIcon.svelte';
    import { leadTag, tagLabel } from '../lib/nexus.js';
    let { app, h, sub = '' } = $props();
</script>

<button type="button" class="nx-tile" onclick={() => h.showDetails(app)}>
    <AppIcon {app} {h} size={84} />
    <span class="nx-tile-name">{app.name}</span>
    <span class="nx-tile-sub">{sub || tagLabel(leadTag(app))}</span>
</button>
