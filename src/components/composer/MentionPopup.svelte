<script>
    // The @mention autocomplete. Rows are keyed on the npub, so moving the
    // keyboard highlight repaints one class, not the list.
    import AnchoredPanel from './AnchoredPanel.svelte';
    import { composerPopup } from '../lib/composer.svelte.js';

    let { anchor } = $props();

    const open = $derived(composerPopup().kind === 'mention');
    // The last view outlives the close: the panel fades out over its content.
    let view = $state.raw({ items: [], active: 0, pick: () => {} });
    $effect(() => { if (open) view = composerPopup(); });
</script>

<AnchoredPanel cls="mention-selector" {open} {anchor} {view}>
    <div class="mention-selector-header">Members</div>
    {#each view.items as item, i (item.npub)}
        <!-- svelte-ignore a11y_no_static_element_interactions (byte-identical to the vanilla row) -->
        <div
            class="mention-item"
            class:active={i === view.active}
            onmousedown={(e) => { e.preventDefault(); view.pick(i); }}
        >
            <img src={item.avatarSrc || 'icons/user-placeholder.svg'} alt="" />
            <span class="mention-item-name">{item.name}</span>
        </div>
    {/each}
</AnchoredPanel>
