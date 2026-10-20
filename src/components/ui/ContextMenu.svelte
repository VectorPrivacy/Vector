<script>
    // The menu is always in the tree so an open can measure it before placing it. A
    // mousedown inside must not reach the document's dismiss before the item's click.
    //
    // A submenu drills down in place behind a back row rather than flying out beside
    // the panel: one interaction model for right-click and long-press, and a panel
    // that always fits because it never grows sideways.
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
        {:else if item.header}
            <div class="context-menu-header">{item.header}</div>
        {:else if item.slider}
            <!-- A live value: it applies as it moves, and the menu stays open. -->
            <div class="context-menu-slider">
                <div class="context-menu-slider-head">
                    <span>{item.label}</span>
                    <span class="context-menu-slider-value">{item.slider.format ? item.slider.format(item.slider.value) : item.slider.value}</span>
                </div>
                <input type="range" class="st-slider" min={item.slider.min} max={item.slider.max} step={item.slider.step} value={item.slider.value}
                       aria-label={item.label}
                       style:--slider-pct="{((item.slider.value - item.slider.min) / (item.slider.max - item.slider.min)) * 100}%"
                       oninput={(e) => { const v = Number(e.currentTarget.value); item.slider.value = v; item.slider.onInput(v); }}>
            </div>
        {:else}
            <div class="context-menu-item" class:is-danger={item.danger} class:is-back={item.back}
                 class:is-disabled={item.disabled} role="menuitem" tabindex="-1"
                 onclick={(e) => { e.stopPropagation(); h().activate?.(item); }}>
                {#if item.back}<span class="icon icon-chevron-left context-menu-lead"></span>{/if}
                {#if item.swatch}<span class="context-menu-swatch" style:background={item.swatch}></span>{/if}
                <span class="context-menu-label">{item.label}{#if item.hint}<span class="context-menu-item-hint">{item.hint}</span>{/if}</span>
                {#if item.submenu}
                    <span class="icon icon-chevron-right"></span>
                {:else if item.busy}
                    <span class="icon icon-loading spin"></span>
                {:else if item.checked}
                    <span class="icon icon-check"></span>
                {:else if item.icon}
                    <span class="icon icon-{item.icon}"></span>
                {/if}
            </div>
        {/if}
    {/each}
</div>
