<script>
    // A mini app's first launch: which of its requested permissions to grant. `active`
    // drives the card in and out; the element stays mounted through the fade-out.
    import { permissionState, permissionAnswer } from '../lib/overlays.svelte.js';
    import { popIn } from '../lib/popin.js';
    const st = permissionState();
    const answer = () => permissionAnswer();
    // Shown once it has been active: open but not active after that is the way out.
    let shown = $state(false);
    $effect(() => { if (st.active) shown = true; else if (!st.open) shown = false; });
</script>

<svelte:window onkeydown={(e) => { if (st.open && st.active && e.key === 'Escape') { e.preventDefault(); answer()?.deny(); } }} />

{#if st.open}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="vcard-overlay" class:active={st.open} class:closing={shown && !st.active}
         onclick={(e) => { if (e.target === e.currentTarget) answer()?.deny(); }}>
        <div class="vcard ma-card" role="dialog" aria-modal="true" aria-labelledby="ma-perm-title" use:popIn={shown}>
            <div class="vcard-head">
                <span class="vcard-badge ma-perm-badge" aria-hidden="true"></span>
                <h3 id="ma-perm-title" class="vcard-title">{st.appName} asks for access</h3>
            </div>
            <p class="vcard-lead">Turn on what you're happy to share. It may work with less, and you can change these later on its Nexus page.</p>
            <div class="ma-list">
                {#each st.items as item (item.id)}
                    <label class="ma-list-row">
                        <span class="ma-list-text">
                            <span class="ma-list-label">{item.label}</span>
                            <span class="ma-list-sub">{item.description}</span>
                        </span>
                        <span class="toggle-container ma-toggle">
                            <input type="checkbox" name="permission" value={item.id} bind:checked={item.granted}>
                            <span class="neon-toggle"></span>
                        </span>
                    </label>
                {/each}
            </div>
            <div class="vcard-actions">
                <button type="button" class="vcard-btn" onclick={() => answer()?.allow(st.items.map(i => [i.id, i.granted]))}>Continue</button>
                <button type="button" class="vcard-btn ghost" onclick={() => answer()?.deny()}>Cancel</button>
            </div>
        </div>
    </div>
{/if}
