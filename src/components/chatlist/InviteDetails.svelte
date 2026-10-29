<script>
    // A parked Community invite: the community, who sent it, when, and until when.
    import { inviteDetails } from '../lib/invite-details.svelte.js';
    import { popIn } from '../lib/popin.js';
    import Avatar from '../ui/Avatar.svelte';
    import { advancedState } from '../lib/advanced.svelte.js';
    let { h } = $props();   // h: close(), accept(communityId), decline(communityId)
    const st = inviteDetails.state();
    const v = $derived(st.view);
    const advanced = advancedState();
</script>

<svelte:window onkeydown={(e) => { if (st.active && e.key === 'Escape') h.close(); }} />

{#if st.active && v}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="pop-dialog-overlay" class:closing={st.closing}
         onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
        <div class="relay-dialog invite-details pop-dialog" use:popIn={st.tick}>
            <div class="relay-dialog-content">
                <div class="invite-details-head">
                    <Avatar src={v.iconSrc} size={72} group />
                    <h3 class="invite-details-name cutoff">{v.name}</h3>
                    <p class="invite-details-sub">Community invite</p>
                </div>
                <div class="invite-details-rows">
                    <div class="invite-details-row">
                        <span class="invite-details-label">From</span>
                        <span class="invite-details-person" title={v.inviter.npub}>
                            <Avatar src={v.inviter.avatar} size={22} />
                            <span class="cutoff">{v.inviter.name}</span>
                        </span>
                    </div>
                    {#if v.owner}
                        <div class="invite-details-row">
                            <span class="invite-details-label">Owner</span>
                            <span class="invite-details-person" title={v.owner.npub}>
                                <Avatar src={v.owner.avatar} size={22} />
                                <span class="cutoff">{v.owner.name}</span>
                            </span>
                        </div>
                    {/if}
                    <div class="invite-details-row">
                        <span class="invite-details-label">{v.sent.label}</span>
                        <span title={v.sent.title}>{v.sent.text}</span>
                    </div>
                    <div class="invite-details-row">
                        <span class="invite-details-label">Expires</span>
                        <span title={v.ends.title}>{v.ends.text}</span>
                    </div>
                    {#if advanced.on && v.relays.length}
                        <div class="invite-details-row invite-details-relays">
                            <span class="invite-details-label">Relays</span>
                            <span class="invite-details-list">{#each v.relays as r (r)}<span class="cutoff">{r}</span>{/each}</span>
                        </div>
                    {/if}
                </div>
                <div class="relay-dialog-buttons">
                    <button class="btn danger-btn" onclick={() => h.decline(v.communityId)}>Decline</button>
                    <button class="btn primary-btn" onclick={() => h.accept(v.communityId)}>Accept</button>
                </div>
            </div>
        </div>
    </div>
{/if}
