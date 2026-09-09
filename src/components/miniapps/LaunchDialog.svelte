<script>
    // The mini app launch choice: play alone, play and invite the open chat, or update
    // when the Nexus has a newer version. Renders inside the overlay container, which
    // keeps the `active` class so the panel's outside-click and Escape checks still see it.
    import { launchDialog } from '../lib/dialogs.svelte.js';
    let { container, h } = $props();   // h: cancel(), solo(), invite()
    const st = launchDialog.state();
    $effect(() => { container.classList.toggle('active', st.active); });
    $effect(() => {
        const onClick = (e) => { if (e.target === container) h.cancel(); };
        container.addEventListener('click', onClick);
        return () => container.removeEventListener('click', onClick);
    });
</script>

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
