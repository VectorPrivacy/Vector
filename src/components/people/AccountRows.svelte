<script>
    // Rows of the My Profile switcher and the pre-login picker: avatar, name, full npub
    // (CSS ellipsises it), the active dot, and a trash button where deleting is allowed.
    let { accounts, activeNpub = '', onPick = null, onDelete = null, h } = $props();
    let failed = $state(new Set());
    function src(meta) { return meta.avatar_cached ? h.fileSrc(meta.avatar_cached) : (meta.avatar_url || null); }
    function fail(npub) { const next = new Set(failed); next.add(npub); failed = next; }
    function placeholder(node) { node.replaceChildren(h.placeholder()); }
</script>

{#each accounts as meta (meta.npub)}
    {@const active = meta.npub === activeNpub}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="profile-switcher-row" class:active data-npub={meta.npub} onclick={() => { if (onPick && !active) onPick(meta); }}>
        <span class="profile-switcher-active-dot"></span>
        {#if src(meta) && !failed.has(meta.npub)}
            <img class="profile-switcher-avatar" src={src(meta)} alt="" style="width: 28px; height: 28px; object-fit: cover; border-radius: 50%;" onerror={() => fail(meta.npub)}>
        {:else}
            <span style="display: contents;" use:placeholder></span>
        {/if}
        <div class="profile-switcher-meta">
            <span class="profile-switcher-name">{meta.display_name || 'Unnamed'}</span>
            <span class="profile-switcher-npub">{meta.npub}</span>
        </div>
        {#if onDelete}
            <button class="profile-switcher-row-trash btn" aria-label="Delete account" onclick={(e) => { e.stopPropagation(); onDelete(meta); }}>
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none"><path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m3 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6h14ZM10 11v6M14 11v6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
            </button>
        {/if}
    </div>
{/each}
