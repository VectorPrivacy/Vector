<script>
    // A pending community invite, shaped like the community row it becomes on accept.
    // The bundled icon is decrypted once per invite and swaps in over the placeholder.
    import Avatar from '../ui/Avatar.svelte';

    let { invite, h } = $props();   // h: cacheInviteLogo(icon) → Promise<src|null>, showGlobalTooltip, hideGlobalTooltip, acceptInvite, declineInvite

    let src = $state(null);
    $effect(() => {
        const icon = invite.icon;
        src = null;
        if (!icon) return;
        let live = true;
        h.cacheInviteLogo(icon).then((s) => { if (live && s) src = s; }).catch(() => {});
        return () => { live = false; };
    });
</script>

<div class="chatlist-contact chatlist-invite" id="community-invite-{invite.community_id}">
    <div style="position: relative;">
        <Avatar {src} size={50} group />
    </div>
    <div class="chatlist-contact-preview">
        <div class="chatlist-contact-header">
            <h4 class="cutoff">{invite.name || 'Community'}</h4>
            <span class="icon icon-users-multi chatlist-type-icon" role="img" aria-label="Group Chat"
                  onmouseenter={(e) => h.showGlobalTooltip('Group Chat', e.currentTarget)} onmouseleave={() => h.hideGlobalTooltip()}></span>
        </div>
        <p class="cutoff">Community invite</p>
    </div>
    <div class="invite-action-buttons">
        <button class="invite-action-btn invite-accept-btn" title="Accept Invite" onclick={(e) => { e.stopPropagation(); h.acceptInvite(invite.community_id); }}>
            <span class="icon icon-check"></span>
        </button>
        <button class="invite-action-btn invite-decline-btn" title="Decline Invite" onclick={(e) => { e.stopPropagation(); h.declineInvite(invite.community_id); }}>
            <span class="icon icon-x"></span>
        </button>
    </div>
</div>
