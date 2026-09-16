<script>
    // The tab bar, which doubles as the widescreen rail: the shortcut strip (its own
    // island, mounted by rail.js), the tabs, the collapse toggle and the account slot
    // widescreen.js docks #account into. The rail-only children are display:none
    // outside `body.ws`.
    import { shellPanes, shellState, shellHandlers, shellReveals, reveal, bindShellEl } from '../lib/shell.svelte.js';
    import AccountRow from './AccountRow.svelte';
    import RailShortcuts from './RailShortcuts.svelte';
    import { shellScreens } from '../lib/shell.svelte.js';
    const panes = shellPanes();
    const st = shellState();
    const h = () => shellHandlers();
    const reveals = shellReveals();
    const bindNavbar = bindShellEl('navbar');
    const screens = shellScreens();
</script>

<div id="navbar" class="row navbar" style:display={panes.navbar ? null : 'none'} use:bindNavbar use:reveal={['navbar', reveals.navbar]}>
    <!-- Unread DMs over communities; the strip renders once rail.js registers its bag. -->
    <div id="ws-rail-shortcuts">
        {#if screens.rail}<RailShortcuts h={screens.rail.h} snapshot={screens.rail.snapshot} />{/if}
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="profile-btn" class="btn navbar-btn" class:navbar-btn-inactive={st.tab !== 'profile-btn'} onclick={() => h().openProfile?.()}>
        <span class="icon icon-user-circle navbar-icon"></span>
        <p class="navbar-text">Profile</p>
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="chat-btn" class="btn navbar-btn" class:navbar-btn-inactive={st.tab !== 'chat-btn'} onclick={() => h().openChatlist?.()}>
        <span class="icon icon-chats navbar-icon"></span>
        <p class="navbar-text">Chat</p>
        {#if st.ws && st.chatBadge}<span class="ws-rail-item-badge">{st.chatBadge}</span>{/if}
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="miniapps-btn" class="btn navbar-btn" class:navbar-btn-inactive={st.tab !== 'miniapps-btn'} style:display={st.ws ? null : 'none'} onclick={() => h().openMiniApps?.()}>
        <span class="icon icon-grid navbar-icon"></span>
        <p class="navbar-text">Mini Apps</p>
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="settings-btn" class="btn navbar-btn" class:navbar-btn-inactive={st.tab !== 'settings-btn'} style:display={st.settingsTab ? null : 'none'} onclick={() => h().openSettings?.()}>
        <span class="icon icon-settings navbar-icon"></span>
        <p class="navbar-text">Settings</p>
        <span class="update-notification-dot" style:display={st.updateDot ? null : 'none'}></span>
    </div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="invites-btn" class="btn navbar-btn" class:navbar-btn-inactive={st.tab !== 'invites-btn'} style:display={st.invitesTab ? null : 'none'} onclick={() => h().openInvites?.()}>
        <span class="icon icon-gift navbar-icon"></span>
        <p class="navbar-text">Invites</p>
    </div>
    <div id="ws-rail-spacer"></div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div id="ws-rail-collapse" class="btn" title={st.railLocked ? null : st.railCollapsed ? 'Expand the sidebar' : 'Collapse the sidebar'}
         onclick={() => { if (!st.railLocked) h().toggleRailCollapse?.(); }}>
        <span class="icon icon-chevron-double-left navbar-icon"></span>
        <p class="navbar-text">Collapse</p>
    </div>
    <!-- The rail's footer: your row, rendered here instead of in the list while widescreen. -->
    <div id="ws-rail-account">
        {#if st.ws}
            <AccountRow />
        {/if}
    </div>
</div>
