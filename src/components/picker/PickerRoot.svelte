<script>
    // The picker's root element, painted from the store; the panel inside renders once
    // picker.js has handed over its bag.
    import { pickerRoot, pickerHandlers, setPickerRootEl } from '../lib/picker.svelte.js';
    import PickerPanel from './PickerPanel.svelte';
    const r = pickerRoot();
    const h = $derived(pickerHandlers());
    function rootEl(node) { setPickerRootEl(node); return { destroy() { setPickerRootEl(null); } }; }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="emoji-picker" tabindex="-1" use:rootEl
     onclickcapture={(e) => h?.rootClickCapture(e)} onclick={(e) => h?.rootClick(e)}
     class:visible={r.visible}
     class:emoji-picker-status-mode={r.statusMode}
     class:emoji-picker-no-gifs={r.noGifs}
     class:emoji-picker-message-type={r.messageType}
     style:bottom={r.bottom || null}
     style:transition={r.teleporting ? 'none' : null}>
    {#if h}<PickerPanel {h} />{/if}
</div>
