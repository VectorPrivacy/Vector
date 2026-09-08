<script>
    // Publish to Nexus: the metadata form over the store, the permission picker, and the
    // Cancel / Publish buttons. `active` drives the fade; the element outlives the fade-out.
    import { publishState, publishHandlers } from '../lib/publish.svelte.js';
    const st = publishState();
    const h = () => publishHandlers();
    const f = $derived(st.form);
</script>

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="publish-app-overlay" class="publish-app-overlay" class:active={st.active} style="display: flex;"
         onclick={(e) => { if (e.target === e.currentTarget) h()?.cancel(); }}>
        <div class="publish-app-container">
            <div class="publish-app-header">
                <h2>Publish to Nexus</h2>
            </div>
            <div class="publish-app-content">
                <div class="publish-app-icon-container">
                    {#if st.icon}
                        <img src={st.icon} alt="App Icon" class="publish-app-icon">
                    {:else}
                        <span class="icon icon-play publish-app-icon-placeholder"></span>
                    {/if}
                </div>
                <div class="publish-app-form">
                    <div class="publish-app-field">
                        <label for="publish-app-id">App ID</label>
                        <input type="text" id="publish-app-id" placeholder="my-awesome-game" bind:value={f.id} oninput={() => h()?.idInput()} onblur={() => h()?.idBlur()}>
                        <span class="publish-app-hint" style:color={st.hint.accent ? 'var(--accent-color)' : ''}>{st.hint.text}</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-name">Name</label>
                        <input type="text" id="publish-app-name" placeholder="My Awesome Game" bind:value={f.name}>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-description">Description</label>
                        <textarea id="publish-app-description" placeholder="A brief description of your app..." bind:value={f.description}></textarea>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-version">Version</label>
                        <input type="text" id="publish-app-version" placeholder="1.0.0" bind:value={f.version}>
                    </div>
                    <div class="publish-app-field publish-app-toggle-field">
                        <label class="toggle-container">
                            <span>Is this app a Game?</span>
                            <input type="checkbox" id="publish-app-is-game" bind:checked={f.isGame}>
                            <span class="neon-toggle"></span>
                        </label>
                        <span class="publish-app-hint">This allows Vector to present your app correctly the users.</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-categories">Categories</label>
                        <input type="text" id="publish-app-categories" placeholder="shooter, art, multiplayer, arcade" bind:value={f.categories}>
                        <span class="publish-app-hint">Comma-separated tags</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-developer">Developer (optional)</label>
                        <input type="text" id="publish-app-developer" placeholder="Developer or studio name" bind:value={f.developer}>
                        <span class="publish-app-hint">The original creator of this app</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-source">Source / Website (optional)</label>
                        <input type="text" id="publish-app-source" placeholder="https://github.com/..." bind:value={f.source}>
                        <span class="publish-app-hint">Link to source code or website</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-changelog">Changelog (optional)</label>
                        <textarea id="publish-app-changelog" placeholder="What's new in this version..." bind:value={f.changelog}></textarea>
                    </div>
                    <div class="publish-app-field">
                        <!-- svelte-ignore a11y_label_has_associated_control -->
                        <label>Requested Permissions (optional)</label>
                        <span class="publish-app-hint">Users must explicitly grant these permissions. Leave all unchecked if your app doesn't need special access.</span>
                        <div class="publish-app-permissions" id="publish-app-permissions">
                            {#if st.permsError}
                                <span class="publish-app-hint">{st.permsError}</span>
                            {/if}
                            {#each st.perms as perm (perm.id)}
                                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                                <div class="publish-app-permission-item" class:checked={perm.checked} data-permission={perm.id} onclick={() => { perm.checked = !perm.checked; }}>
                                    <input type="checkbox" name="permissions" value={perm.id} checked={perm.checked} tabindex="-1" onclick={(e) => e.stopPropagation()} onchange={(e) => { perm.checked = e.currentTarget.checked; }}>
                                    <span class="publish-app-permission-check"></span>
                                    <div class="publish-app-permission-text">
                                        <span class="publish-app-permission-label">{perm.label}</span>
                                        <span class="publish-app-permission-desc">{perm.description}</span>
                                    </div>
                                </div>
                            {/each}
                        </div>
                    </div>
                </div>
            </div>
            <div class="publish-app-buttons">
                <button class="file-preview-btn file-preview-btn-cancel" id="publish-app-cancel" onclick={() => h()?.cancel()}>Cancel</button>
                <button class="file-preview-btn file-preview-btn-send" id="publish-app-submit" disabled={st.busy} onclick={() => h()?.submit()}>
                    <span class="icon {st.busy ? 'icon-loading' : 'icon-star'}"></span> {st.busy ? 'Publishing...' : 'Publish'}
                </button>
            </div>
        </div>
    </div>
{/if}
