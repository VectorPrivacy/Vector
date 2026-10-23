<script>
    // The slim layout's community rail: the widescreen strip at icon size, then a tile to
    // start a community and the fold that trades the rail for the list's own filters.
    import { shellScreens, shellHandlers } from '../lib/shell.svelte.js';
    import RailShortcuts from './RailShortcuts.svelte';
    const screens = shellScreens();
    const h = () => shellHandlers();
    function railMenu(node) {
        h().consoleRailMenu?.(node);
    }
</script>

<div id="cc-rail" class="cc-rail" use:railMenu>
    <div id="cc-rail-shortcuts">
        {#if screens.rail}
            <RailShortcuts h={screens.rail.h} snapshot={screens.rail.snapshot}>
                {#snippet tail()}
                    <!-- The last things in the list, so they scroll with the communities. -->
                    <button class="cc-rail-add" title="New Community" aria-label="New Community" onclick={() => h().openCreateGroup?.()}>
                        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14"/></svg>
                    </button>
                    <button class="cc-rail-fold" title="Hide communities" aria-label="Hide communities" onclick={() => h().consoleSetRail?.(false)}>
                        <span class="icon icon-chevron-double-left"></span>
                    </button>
                {/snippet}
            </RailShortcuts>
        {/if}
    </div>
</div>
