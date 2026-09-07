<script>
    // One pinned message: type icon or pin glyph, the one-line preview (the full message
    // once expanded), and the controls floating top-right. Expansion, the content
    // renderers and the media upgrade are the app's, working on this row's elements.
    import { tick } from 'svelte';
    import { pinsState } from '../lib/pins.svelte.js';

    let { pin, h } = $props();
    // h: typeIcon(pin), rowSvg, formatDate(ms), renderCollapsed(body, pin), lineClips(text), previewableMedia(pin),
    //    hasChips(pin), sourceOf(pin), isInteractive(e), jump(id), toggle(row, text), openFile(pin), unpin(id), isAndroid()

    const st = pinsState();
    let row = $state(null);
    let text = $state(null);
    let body = $state(null);
    let clips = $state(false);

    // Mount-time facts: a row is born for one pin.
    // svelte-ignore state_referenced_locally
    const typeIcon = h.typeIcon(pin);
    // svelte-ignore state_referenced_locally
    const media = h.previewableMedia(pin);
    // svelte-ignore state_referenced_locally
    const hasMoreLines = h.sourceOf(pin).split('\n').filter(l => l.trim()).length > 1;
    // svelte-ignore state_referenced_locally
    const canOpen = h.hasChips(pin) && !media;
    // The expander appears wherever expansion would SHOW more: a clipped first line,
    // further lines beyond the one-line preview, or previewable media.
    const expandable = $derived(clips || !!media || hasMoreLines);
    const expandOnly = $derived(!pin._jumpable && expandable);

    function collapsed(node) { h.renderCollapsed(node, pin); }
    // Clipping is measured in layout, so it waits for the drawer to be showing.
    $effect(() => {
        if (!st.open || !text) return;
        tick().then(() => { if (text?.isConnected) clips = h.lineClips(text); });
    });

    function click(e) {
        if (h.isInteractive(e)) return;
        if (pin._jumpable) h.jump(pin.rumor_id);
        else if (expandable) h.toggle(row, text);
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="pins-drawer-row" class:pins-no-jump={!pin._jumpable} class:pins-expandable={expandOnly} data-rumor-id={pin.rumor_id} bind:this={row} onclick={click}>
    {#if typeIcon}
        <span class="icon icon-{typeIcon} pins-drawer-row-type-icon"></span>
    {:else}
        {@html h.rowSvg}
    {/if}
    <!-- Controls float inside the text flow: an expanded pin's lines wrap around them. -->
    <div class="pins-drawer-row-text" bind:this={text}>
        <span class="pins-drawer-row-controls">
            <span class="pins-drawer-row-date">{h.formatDate(pin.ms)}</span>
            {#if expandable}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <span class="icon icon-chevron-down pins-drawer-row-expander btn" title="Show more" onclick={(e) => { e.stopPropagation(); h.toggle(row, text); }}></span>
            {/if}
            {#if canOpen}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <span class="icon icon-file-search pins-drawer-row-open btn" title={h.isAndroid() ? 'Open file' : 'Reveal in folder'} onclick={(e) => { e.stopPropagation(); h.openFile(pin); }}></span>
            {/if}
            {#if st.canPin}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <span class="icon icon-x pins-drawer-row-unpin btn" title="Unpin" onclick={(e) => { e.stopPropagation(); h.unpin(pin.rumor_id); }}></span>
            {/if}
        </span>
        <span class="pins-drawer-row-body" bind:this={body} use:collapsed></span>
    </div>
</div>
