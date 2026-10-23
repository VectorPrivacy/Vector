<script>
    // Publish to Nexus, or edit a listing you published: its fields over the store, the
    // permissions it asks for, and the submit. An edit keeps the App ID and version (they
    // name the package). `active` drives the card in and out; the element outlives the fade-out.
    import { publishState, publishHandlers } from '../lib/publish.svelte.js';
    import { popIn } from '../lib/popin.js';
    const st = publishState();
    const h = () => publishHandlers();
    const f = $derived(st.form);
    const editing = $derived(st.mode === 'edit');
    let shown = $state(false);
    $effect(() => { if (st.active) shown = true; else if (!st.open) shown = false; });
</script>

<svelte:window onkeydown={(e) => { if (st.open && st.active && !st.busy && e.key === 'Escape') { e.preventDefault(); h()?.cancel(); } }} />

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="vcard-overlay" class:active={st.open} class:closing={shown && !st.active}
         onclick={(e) => { if (e.target === e.currentTarget && !st.busy) h()?.cancel(); }}>
        <div class="vcard ma-card ma-publish" role="dialog" aria-modal="true" aria-labelledby="ma-publish-title" use:popIn={shown}>
            <div class="vcard-head">
                <span class="ma-launch-icon">
                    {#if st.icon}<img src={st.icon} alt="" draggable="false">{:else}<span class="ma-launch-initial">{(f.name.match(/[\p{L}\p{N}]/u)?.[0] || '?').toUpperCase()}</span>{/if}
                </span>
                <div class="ma-launch-id">
                    <h3 id="ma-publish-title" class="vcard-title">{editing ? 'Edit Listing' : 'Publish to Nexus'}</h3>
                    <span class="ma-launch-sub">{editing ? 'Changes go live without uploading the app again' : 'Everyone on Vector can find and install it'}</span>
                </div>
                <button type="button" class="vcard-x" aria-label="Close" disabled={st.busy} onclick={() => h()?.cancel()}>&#x2715;</button>
            </div>

            <div class="ma-form">
                <label class="ma-field">
                    <span class="vcard-label">App ID</span>
                    <input class="vcard-input" type="text" placeholder="my-awesome-game" bind:value={f.id} readonly={editing}
                           oninput={() => h()?.idInput?.()} onblur={() => h()?.idBlur?.()}
                           autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false">
                    {#if !editing}<span class="ma-hint" class:accent={st.hint.accent}>{st.hint.text}</span>{/if}
                </label>
                <label class="ma-field">
                    <span class="vcard-label">Name</span>
                    <input class="vcard-input" type="text" placeholder="My Awesome Game" bind:value={f.name}>
                </label>
                <label class="ma-field">
                    <span class="vcard-label">Description</span>
                    <textarea class="vcard-input" rows="3" placeholder="A line or two about what it is" bind:value={f.description}></textarea>
                </label>
                <div class="ma-pair">
                    <label class="ma-field">
                        <span class="vcard-label">Version</span>
                        <input class="vcard-input" type="text" placeholder="1.0.0" bind:value={f.version} readonly={editing}
                               title={editing ? 'A new version is a new upload' : undefined}>
                    </label>
                    <div class="ma-field">
                        <span class="vcard-label">Kind</span>
                        <span class="ma-kind-pick" role="radiogroup" aria-label="Kind">
                            <button type="button" role="radio" aria-checked={f.isGame} class:on={f.isGame} onclick={() => { f.isGame = true; }}>Game</button>
                            <button type="button" role="radio" aria-checked={!f.isGame} class:on={!f.isGame} onclick={() => { f.isGame = false; }}>App</button>
                        </span>
                    </div>
                </div>
                <div class="ma-field">
                    <span class="vcard-label">Plays With</span>
                    <span class="ma-controls" role="group" aria-label="Plays with">
                        <button type="button" aria-pressed={f.touch} class:on={f.touch} onclick={() => { f.touch = !f.touch; }}><span class="nx-g g-touch"></span>Touch</button>
                        <button type="button" aria-pressed={f.pad} class:on={f.pad} onclick={() => { f.pad = !f.pad; }}><span class="nx-g g-pad"></span>Controller</button>
                        <button type="button" aria-pressed={f.keys} class:on={f.keys} onclick={() => { f.keys = !f.keys; }}><span class="nx-g g-keys"></span>Keyboard</button>
                    </span>
                    <span class="ma-hint">Devices with these get it recommended, and phones need Touch to list it.</span>
                </div>
                <label class="ma-field">
                    <span class="vcard-label">Categories</span>
                    <input class="vcard-input" type="text" placeholder="multiplayer, arcade, 2d" bind:value={f.categories}>
                    <span class="ma-hint">Comma-separated</span>
                </label>
                <label class="ma-field">
                    <span class="vcard-label">Developer <i>(optional)</i></span>
                    <input class="vcard-input" type="text" placeholder="Who made it" bind:value={f.developer}>
                </label>
                <label class="ma-field">
                    <span class="vcard-label">Source or Website <i>(optional)</i></span>
                    <input class="vcard-input" type="text" placeholder="https://github.com/..." bind:value={f.source}
                           autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false">
                </label>
                <label class="ma-field">
                    <span class="vcard-label">What's New <i>(optional)</i></span>
                    <textarea class="vcard-input" rows="3" placeholder="What changed in this version" bind:value={f.changelog}></textarea>
                </label>
                <div class="ma-field">
                    <span class="vcard-label">Permissions <i>(optional)</i></span>
                    <span class="ma-hint">Each one is asked for on first launch. Leave them all off unless the app needs one.</span>
                    {#if st.permsError}<span class="ma-hint">{st.permsError}</span>{/if}
                    {#if st.perms.length}
                        <div class="ma-list">
                            {#each st.perms as perm (perm.id)}
                                <label class="ma-list-row">
                                    <span class="ma-list-text">
                                        <span class="ma-list-label">{perm.label}</span>
                                        <span class="ma-list-sub">{perm.description}</span>
                                    </span>
                                    <span class="toggle-container ma-toggle">
                                        <input type="checkbox" name="permissions" value={perm.id} bind:checked={perm.checked}>
                                        <span class="neon-toggle"></span>
                                    </span>
                                </label>
                            {/each}
                        </div>
                    {/if}
                </div>
            </div>

            <div class="vcard-actions">
                <button type="button" class="vcard-btn" disabled={st.busy} onclick={() => h()?.submit()}>
                    {st.busy ? (editing ? 'Saving…' : 'Publishing…') : (editing ? 'Save Changes' : 'Publish')}
                </button>
            </div>
        </div>
    </div>
{/if}
