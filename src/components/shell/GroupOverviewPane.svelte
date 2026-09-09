<script>
    // The community overview's frame: its header from overview state (the body island
    // mounts into the scroll host below). Ids stay where the stylesheets select them.
    import { shellPanes, shellReveals, reveal } from '../lib/shell.svelte.js';
    import { overviewState, overviewHeadHandlers } from '../lib/overview.svelte.js';
    import { communityVersion } from '../lib/signals.svelte.js';
    import Avatar from '../ui/Avatar.svelte';
    const panes = shellPanes();
    const ov = overviewState();
    const h = () => overviewHeadHandlers();
    const subtext = $derived.by(() => {
        if (!ov.communityId) return '';
        communityVersion(ov.communityId);
        return h().memberSubtext?.(ov.communityId) || '';
    });
    const reveals = shellReveals();
</script>

<div id="group-overview" class="chats" style:display={panes.groupOverview ? null : 'none'} use:reveal={['groupOverview', reveals.groupOverview]} data-group-id={ov.groupId || undefined}>
    <div class="chat-header" style="top: 0px;">
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div id="group-overview-back-btn" class="btn nav-back-btn" onclick={() => h().back?.()}>
            <span class="icon icon-chevron-double-left nav-icon"></span>
        </div>
        <div class="profile-header-info">
            <div class="profile-header-name-row">
                <div id="group-overview-header-avatar-container">
                    {#if ov.chatId}<Avatar src={ov.avatarSrc} size={22} group />{/if}
                </div>
                <h3 id="group-overview-name" class="cutoff chat-contact-with-status btn">{ov.name}</h3>
            </div>
            <span id="group-overview-status" class="cutoff chat-contact-status btn">{subtext}</span>
        </div>
    </div>
    <div id="group-overview-scroll" style="overflow-y: auto; height: 100%; position: relative;"></div>
</div>
