<script>
    // Multi-account: the My Profile drop-down. From the Profile header it drops down over a
    // blurred backdrop; from the widescreen rail's chip it drops UP behind a plain
    // click-catcher, anchored by pixel to the chip.
    import { switcherState, switcherHandlers } from '../lib/switcher.svelte.js';
    import AccountRows from '../people/AccountRows.svelte';
    const sw = switcherState();
    const h = () => switcherHandlers();
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="profile-switcher-backdrop" class:visible={sw.open} class:ws-dropup={sw.dropup} onclick={() => h().close?.()}></div>
<div class="profile-switcher-panel" class:open={sw.open} class:ws-dropup={sw.dropup} role="dialog" aria-label="My Profile"
     style:bottom={sw.bottomPx == null ? null : `${sw.bottomPx}px`}>
    <div class="profile-switcher-list">
        {#if sw.open}
            <AccountRows accounts={sw.accounts} activeNpub={sw.activeNpub} h={h().rowHelpers}
                         onPick={(m) => h().onPick?.(m)} onDelete={(m) => h().onDelete?.(m)} />
        {/if}
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="profile-switcher-add btn" class:disabled={sw.addDisabled} onclick={() => { if (!sw.addDisabled) h().onAdd?.(); }}>
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2M19 8v6M22 11h-6M8.5 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
        <span class="profile-switcher-add-label">{sw.addLabel}</span>
    </div>
</div>
