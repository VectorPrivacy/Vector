<script>
    // One invite card: skeleton while the preview resolves, then the community with Join, or
    // Open when we are already a member. A member's card renders from the chat we sync, never
    // from the fetched preview, so it can't diverge.
    import { listVersion } from '../lib/signals.svelte.js';
    let { inviteKey, h } = $props();
    let res = $state(null);
    let joining = $state(false);
    // One resolve per card: the parent keys cards by invite key, so a new key is a new card.
    // Re-renders hit the settled cache: fill without the pop; only a fresh resolve animates.
    // svelte-ignore state_referenced_locally
    const animate = !h.invitePreviewSettled(inviteKey);
    // svelte-ignore state_referenced_locally
    h.resolveInvite(inviteKey).then((r) => { res = r; });

    const joined = $derived.by(() => { listVersion(); return res?.state === 'ok' ? h.joinedChat(res.info.community_id) : null; });
    const name = $derived((joined && joined.metadata?.custom_fields?.name) || res?.info?.name || 'Community');
    const desc = $derived(joined ? (joined.metadata?.custom_fields?.description || '') : (res?.info?.description || ''));
    const icon = $derived(joined?.metadata?.avatar_cached ? h.fileSrc(joined.metadata.avatar_cached) : (res?.iconSrc || 'icons/group-placeholder.svg'));

    async function join(e) {
        e.stopPropagation();
        if (joining) return;
        joining = true;
        try { await h.joinFromCard(inviteKey, res.info.community_id); }
        finally { joining = false; }
    }
</script>

<div class="community-invite-card" class:is-loading={!res} class:is-invalid={res && res.state !== 'ok'} class:cic-ready={res?.state === 'ok' && animate}>
    <div class="cic-eyebrow">Community Invite</div>
    <div class="cic-body">
        <img class="cic-icon" src={icon} alt="">
        <div class="cic-meta">
            {#if !res}
                <div class="cic-name"><span class="pack-skel cic-skel-name"></span></div>
                <div class="cic-desc"><span class="pack-skel cic-skel-desc"></span></div>
            {:else if res.state !== 'ok'}
                <div class="cic-name">Invite Unavailable</div>
                <div class="cic-desc">This invite could not be loaded — it may be revoked or expired.</div>
            {:else}
                <div class="cic-name">{name}</div>
                {#if desc}<div class="cic-desc">{desc}</div>{/if}
            {/if}
        </div>
        {#if !res}
            <button class="cic-btn" type="button" disabled>Join</button>
        {:else if res.state === 'ok'}
            {#if joined}
                <button class="cic-btn cic-btn-open" type="button" onclick={(e) => { e.stopPropagation(); h.openCommunity(res.info.community_id); }}>Open</button>
            {:else}
                <button class="cic-btn" class:is-joining={joining} type="button" disabled={joining} onclick={join}>{joining ? 'Joining…' : 'Join'}</button>
            {/if}
        {/if}
    </div>
</div>
