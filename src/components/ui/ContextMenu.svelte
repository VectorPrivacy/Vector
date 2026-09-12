<script>
    // The menu is always in the tree so an open can measure it before placing it. A
    // mousedown inside must not reach the document's dismiss before the item's click.
    import { contextMenuState, contextMenuEls, contextMenuHandlers } from '../lib/contextmenu.svelte.js';
    const m = contextMenuState();
    const els = contextMenuEls();
    const h = () => contextMenuHandlers();
    function bindRoot(node) { els.root = node; return { destroy() { els.root = null; } }; }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="context-menu" role="menu" tabindex="-1" class:is-visible={m.open} style:left="{m.x}px" style:top="{m.y}px"
     use:bindRoot onmousedown={(e) => e.stopPropagation()}>
    {#each m.items as item, i (i)}
        {#if item.divider}
            <div class="context-menu-divider"></div>
        {:else}
            <div class="context-menu-item" class:is-danger={item.danger} role="menuitem" tabindex="-1"
                 onclick={(e) => { e.stopPropagation(); h().activate?.(item); }}>
                <span>{item.label}{#if item.hint}<span class="context-menu-item-hint">{item.hint}</span>{/if}</span>
                {#if item.icon}<span class="icon icon-{item.icon}"></span>{/if}
            </div>
        {/if}
    {/each}
</div>
