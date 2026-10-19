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
    <div class="mention-selector-header">{view.items[0]?.channel ? 'Channels' : view.items.some((i) => !i.special) ? 'Members' : 'Time'}</div>
    {#each view.items as item, i (item.npub)}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
            class="mention-item"
            class:active={i === view.active}
            onmousedown={(e) => { e.preventDefault(); view.pick(i); }}
        >
            {#if item.channel}
                <span class="mention-item-channel"><span class="icon icon-{item.channel.voice ? 'volume-max' : 'channel-hash'}"></span></span>
                <span class="mention-item-name">{item.name}</span>
                <span class="mention-item-hint">{item.channel.voice ? 'Voice' : 'Text'}</span>
            {:else if item.special === 'time'}
                <span class="mention-item-time"><span class="icon icon-clock"></span></span>
                <span class="mention-item-name">time</span>
                <span class="mention-item-hint">A time everyone sees in their own zone</span>
            {:else if item.role}
                <span class="mention-item-role" style:--role-color={item.role.color ? '#' + (item.role.color >>> 0).toString(16).padStart(6, '0').slice(-6) : null}>@</span>
                <span class="mention-item-name">{item.name}</span>
                <span class="mention-item-hint">Role</span>
            {:else}
                <img src={item.avatarSrc || 'icons/user-placeholder.svg'} alt="" />
                <span class="mention-item-name">{item.name}</span>
            {/if}
        </div>
    {/each}
</AnchoredPanel>
