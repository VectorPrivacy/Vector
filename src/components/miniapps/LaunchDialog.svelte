<script>
    // The mini app launch choice: play alone, play and invite the open chat, or update
    // when the Nexus has a newer version. Owns its overlay element; a backdrop click cancels.
    import { launchDialog } from '../lib/dialogs.svelte.js';
    let { h } = $props();   // h: cancel(), solo(), invite()
    const st = launchDialog.state();
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="miniapp-launch-overlay" id="miniapp-launch-overlay" class:active={st.active} onclick={(e) => { if (e.target === e.currentTarget) h.cancel(); }}>

{#if st.open}
    <div class="miniapp-launch-container">
        <div class="miniapp-launch-inner">
            <div class="miniapp-launch-icon-container">
                {#if st.icon}<img src={st.icon} alt={st.name}>{:else}<span class="icon icon-play"></span>{/if}
            </div>
            <div class="miniapp-launch-info">
                <div class="miniapp-launch-name">{st.name}</div>
            </div>
        </div>
        <div class="file-preview-buttons">
            <button class="file-preview-btn file-preview-btn-cancel" id="miniapp-launch-cancel" onclick={h.cancel}>Cancel</button>
            <button class="file-preview-btn file-preview-btn-cancel" onclick={h.solo}>{st.actionText}</button>
            <button class="file-preview-btn file-preview-btn-send" onclick={h.invite}>{st.updateMode ? 'Update' : `${st.actionText} & Invite`}</button>
        </div>
    </div>
{/if}
</div>
