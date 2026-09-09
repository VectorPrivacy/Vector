<script>
    // The (i) explainer beside a settings row. It often sits inside a checkbox label,
    // so the click must not also flip the switch.
    let { onclick, side = 'left', align = 'text-top', flex = false } = $props();
    const margin = $derived(side === 'left' ? 'margin-left: 5px;' : side === 'lead' ? 'margin-right: 6px;' : 'margin-right: 8px;');
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<span class="icon icon-info btn"
      style="width: 18px; height: 18px; position: relative; display: inline-block; {flex ? 'flex-shrink: 0;' : `vertical-align: ${align};`} {margin}"
      onclick={(e) => { e.preventDefault(); e.stopPropagation(); onclick?.(); }}></span>
