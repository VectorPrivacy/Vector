<script>
    // Play with Friends: pick a chat, and the game goes there as an invite while it opens
    // for you. Your chats newest first, a search over them, and the app it's sending.
    import { mktPicker, mktApps, mktHandlers } from '../lib/marketplace.svelte.js';
    import { popIn } from '../lib/popin.js';
    import AppIcon from './AppIcon.svelte';
    const st = mktPicker();
    const h = $derived(mktHandlers());
    const app = $derived(mktApps().find(a => a.id === st.appId) || null);

    let query = $state('');
    let chats = $state([]);
    $effect(() => { st.tick; query = ''; chats = st.open && h ? h.pickChats() : []; });
    const q = $derived(query.toLowerCase().trim());
    const shown = $derived(q ? chats.filter(c => c.name.toLowerCase().includes(q) || c.sub.toLowerCase().includes(q)) : chats);

    function focusOnOpen(node) {
        // A phone keyboard would cover the list it's meant to search.
        if (matchMedia('(pointer: fine)').matches) requestAnimationFrame(() => node.focus());
    }
</script>

<svelte:window onkeydown={(e) => { if (st.open && !st.closing && e.key === 'Escape') { e.preventDefault(); h?.closePicker(); } }} />

{#if st.open && app}
<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="vcard-overlay nx-pick-overlay" class:active={st.active || st.open} class:closing={st.closing}
     onclick={(e) => { if (e.target === e.currentTarget) h?.closePicker(); }}>
    <div class="vcard nx-pick" role="dialog" aria-modal="true" aria-labelledby="nx-pick-title" use:popIn={st.tick}>
        <div class="vcard-head">
            <AppIcon {app} h={h} size={40} />
            <div class="nx-pick-id">
                <h3 id="nx-pick-title" class="vcard-title">Play with Friends</h3>
                <span class="nx-pick-sub">Send {app.name} to a chat, and it opens for you</span>
            </div>
            <button type="button" class="vcard-x" aria-label="Close" onclick={() => h?.closePicker()}>&#x2715;</button>
        </div>
        <div class="nx-pick-search">
            <span class="nx-g g-search"></span>
            <input type="text" placeholder="Search chats" bind:value={query} use:focusOnOpen
                   autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false">
        </div>
        <div class="nx-pick-list">
            {#each shown as c (c.id)}
                <button type="button" class="nx-pick-row" onclick={() => h.sendTo(app, c.id)}>
                    <span class="nx-pick-avatar" class:square={c.group}>
                        {#if c.avatar}<img src={c.avatar} alt="" draggable="false">{:else}<span class="nx-g {c.group ? 'g-users' : 'g-user'}"></span>{/if}
                    </span>
                    <span class="nx-list-text">
                        <span class="nx-list-label">{c.name}</span>
                        <span class="nx-list-sub">{c.sub}</span>
                    </span>
                    <span class="nx-pick-send">Send</span>
                </button>
            {:else}
                <p class="nx-pick-none">{q ? 'No chats match' : 'No chats yet: start one to play together'}</p>
            {/each}
        </div>
    </div>
</div>
{/if}
