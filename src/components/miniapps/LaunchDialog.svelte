<script>
    // The mini app launch choice: play and invite the open chat, play alone, or update
    // when the Nexus has a newer version. Owns its overlay element; a backdrop click cancels.
    import { launchDialog } from '../lib/miniapps.svelte.js';
    import { popIn } from '../lib/popin.js';
    let { h } = $props();   // h: cancel(), solo(), invite()
    const st = launchDialog.state();
    let shown = $state(false);
    $effect(() => { if (st.active) shown = true; else if (!st.open) shown = false; });
</script>

<svelte:window onkeydown={(e) => { if (st.open && st.active && e.key === 'Escape') { e.preventDefault(); h.cancel(); } }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="vcard-overlay" id="miniapp-launch-overlay" class:active={st.open} class:closing={shown && !st.active}
     onclick={(e) => { if (e.target === e.currentTarget) h.cancel(); }}>
{#if st.open}
    <div class="vcard ma-card ma-launch" role="dialog" aria-modal="true" aria-labelledby="ma-launch-title" use:popIn={shown}>
        <div class="vcard-head">
            <span class="ma-launch-icon">
                {#if st.icon}<img src={st.icon} alt="" draggable="false">{:else}<span class="ma-launch-initial">{(st.name.match(/[\p{L}\p{N}]/u)?.[0] || '?').toUpperCase()}</span>{/if}
            </span>
            <div class="ma-launch-id">
                <h3 id="ma-launch-title" class="vcard-title">{st.name}</h3>
                <span class="ma-launch-sub">{st.updateMode ? 'A newer version is in the Nexus' : 'Play here, or bring this chat along'}</span>
            </div>
            <button type="button" class="vcard-x" aria-label="Close" onclick={h.cancel}>&#x2715;</button>
        </div>
        <div class="vcard-actions">
            <button type="button" class="vcard-btn" onclick={h.invite}>{st.updateMode ? 'Update' : `${st.actionText} & Invite`}</button>
            <button type="button" class="vcard-btn ghost" onclick={h.solo}>{st.updateMode ? `${st.actionText} This Version` : `${st.actionText} Alone`}</button>
        </div>
    </div>
{/if}
</div>
