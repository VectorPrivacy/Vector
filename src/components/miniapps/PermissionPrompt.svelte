<script>
    // A mini app's first launch: which of its requested permissions to grant. `active`
    // drives the fade; the element stays mounted through the fade-out.
    import { permissionState, permissionAnswer } from '../lib/overlays.svelte.js';
    const st = permissionState();
    const answer = () => permissionAnswer();
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="permission-prompt-overlay" class="permission-prompt-overlay" class:active={st.active} style="display: flex;"
         onclick={(e) => { if (e.target === e.currentTarget) answer()?.deny(); }}>
        <div class="permission-prompt-container">
            <div class="permission-prompt-header">
                <h2>Permission Request</h2>
                <p class="permission-prompt-subtitle">{st.appName} is requesting the following permissions:</p>
            </div>
            <div class="permission-prompt-content">
                <p class="permission-prompt-hint">Select which permissions you want to grant. The app may have reduced functionality without certain permissions.</p>
                <div class="permission-prompt-list">
                    {#each st.items as item (item.id)}
                        <div class="permission-prompt-item">
                            <div class="permission-prompt-info">
                                <span class="permission-prompt-label">{item.label}</span>
                                <span class="permission-prompt-desc">{item.description}</span>
                            </div>
                            <label class="toggle-container permission-prompt-toggle">
                                <input type="checkbox" name="permission" value={item.id} bind:checked={item.granted}>
                                <span class="neon-toggle"></span>
                            </label>
                        </div>
                    {/each}
                </div>
            </div>
            <div class="permission-prompt-buttons">
                <button class="file-preview-btn file-preview-btn-cancel" onclick={() => answer()?.deny()}>Cancel</button>
                <button class="file-preview-btn file-preview-btn-send" onclick={() => answer()?.allow(st.items.map(i => [i.id, i.granted]))}>Continue</button>
            </div>
        </div>
    </div>
{/if}
