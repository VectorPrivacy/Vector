<script>
    // The root: everything under <body>. The popups and overlays keep their containers
    // (their openers mount into them by id); the screens migrated to islands are empty
    // mounts nav shows and hides through the shell store.
    import { shellPanes } from '../lib/shell.svelte.js';
    import Navbar from './Navbar.svelte';
    import ChatListPane from './ChatListPane.svelte';
    import ChatPane from './ChatPane.svelte';
    import GroupOverviewPane from './GroupOverviewPane.svelte';
    import ProfileSwitcher from './ProfileSwitcher.svelte';
    import PickerRoot from '../picker/PickerRoot.svelte';
    import PickerTooltip from '../picker/PickerTooltip.svelte';
    import ImageViewer from '../ui/ImageViewer.svelte';
    import Tooltip from './Tooltip.svelte';
    const panes = shellPanes();
</script>

<div id="popup-container" class="popup-container"></div>
<!-- Pack details: opened by deep link (vector://emojis/pack/<naddr>) and the share-pack flow. -->
<div id="pack-details-overlay" class="pack-details-overlay" hidden></div>

<main class="container">
    <PickerRoot />
    <div class="attachment-panel" id="attachment-panel" tabindex="-1"></div>
    <div class="marketplace-panel" id="marketplace-panel" style="display: none;"></div>
    <div class="app-details-panel" id="app-details-panel" style="display: none;"></div>
    <div class="miniapp-launch-overlay" id="miniapp-launch-overlay"></div>

    <div id="profile" class="chats" style:display={panes.profile ? null : 'none'}></div>
    <GroupOverviewPane />
    <ChatListPane />
    <ChatPane />
    <!-- Widescreen: fills the conversation column while no chat is open, and the drag
         handle on the list pane's trailing edge. Both are inert elsewhere. -->
    <div id="ws-conv-empty">
        <img src="./icons/vector-logo.svg" alt="">
        <p>Pick a conversation to get started</p>
    </div>
    <div id="ws-list-resize" title="Drag to resize (double-click to reset)"></div>
    <div id="voice-progress-container" class="voice-progress-container" style="display: none;">
        <div class="voice-progress-bar">
            <div class="voice-progress-fill"></div>
            <div class="voice-progress-text">Setting up voice transcription...</div>
        </div>
    </div>

    <div id="chat-new" style:display={panes.chatNew ? null : 'none'}></div>
    <div id="create-group" class="create-group-container" style:display={panes.createGroup ? null : 'none'}></div>
    <div id="settings" style:display={panes.settings ? null : 'none'}></div>
    <div id="invites" style:display={panes.invites ? null : 'none'}></div>
    <Navbar />
    <div id="login-form" class="fadein-anim"></div>
</main>

<Tooltip />
<ProfileSwitcher />
<PickerTooltip />
<ImageViewer />
