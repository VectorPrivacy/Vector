<script>
    // The root: everything under <body>. The popups and overlays keep their containers
    // (their openers mount into them by id); the screens migrated to islands are empty
    // mounts nav shows and hides through the shell store.
    import { shellPanes, shellScreens, shellReveals, shellHandlers, reveal, bindShellEl } from '../lib/shell.svelte.js';
    import { profileScreen } from '../lib/profilescreen.svelte.js';
    import { profileEdit } from '../lib/profileedit.svelte.js';
    import { loginState } from '../lib/login.svelte.js';
    import ProfileScreen from '../people/ProfileScreen.svelte';
    import Settings from '../settings/Settings.svelte';
    import InvitesScreen from '../people/InvitesScreen.svelte';
    import NewChat from '../people/NewChat.svelte';
    import CreateCommunity from '../community/CreateCommunity.svelte';
    import LoginScreen from '../auth/LoginScreen.svelte';
    import Navbar from './Navbar.svelte';
    import WindowChrome from './WindowChrome.svelte';
    import ChatListPane from './ChatListPane.svelte';
    import ChatPane from './ChatPane.svelte';
    import GroupOverviewPane from './GroupOverviewPane.svelte';
    import ProfileSwitcher from './ProfileSwitcher.svelte';
    import PickerRoot from '../picker/PickerRoot.svelte';
    import PickerTooltip from '../picker/PickerTooltip.svelte';
    import ImageViewer from '../ui/ImageViewer.svelte';
    import Toast from '../ui/Toast.svelte';
    import BadgeCard from '../ui/BadgeCard.svelte';
    import ContextMenu from '../ui/ContextMenu.svelte';
    import RekeyProgress from '../community/RekeyProgress.svelte';
    import InviteModal from '../community/InviteModal.svelte';
    import Tooltip from './Tooltip.svelte';
    import ComposerPopups from '../composer/ComposerPopups.svelte';
import CallOverlay from '../calls/CallOverlay.svelte';
    import FilePreview from '../composer/FilePreview.svelte';
    import MiniProfile from '../people/MiniProfile.svelte';
    import ReactionPopups from '../chat/ReactionPopups.svelte';
    import EditHistoryPopup from '../chat/EditHistoryPopup.svelte';
    import QrOverlay from '../ui/QrOverlay.svelte';
    import QrScanner from '../ui/QrScanner.svelte';
    import StatusDialog from '../ui/StatusDialog.svelte';
    import DowngradeBlock from '../ui/DowngradeBlock.svelte';
    import ModConsole from '../moderation/ModConsole.svelte';
    import CommunitySettings from '../community/CommunitySettings.svelte';
    import AddRelayDialog from '../settings/network/AddRelayDialog.svelte';
    import RelayInfoDialog from '../settings/network/RelayInfoDialog.svelte';
    import BlossomInfoDialog from '../settings/network/BlossomInfoDialog.svelte';
    import DepositDialog from '../miniapps/pivx/DepositDialog.svelte';
    import SendDialog from '../miniapps/pivx/SendDialog.svelte';
    import WithdrawDialog from '../miniapps/pivx/WithdrawDialog.svelte';
    import PivxSettingsDialog from '../miniapps/pivx/SettingsDialog.svelte';
    import CredentialModal from '../auth/CredentialModal.svelte';
    import MigrationOverlay from '../auth/MigrationOverlay.svelte';
    import ProcessingOverlay from '../ui/ProcessingOverlay.svelte';
    import PermissionPrompt from '../miniapps/PermissionPrompt.svelte';
    import PublishDialog from '../marketplace/PublishDialog.svelte';
    import Popup from '../ui/Popup.svelte';
    import PackDetailsOverlay from '../picker/PackDetailsOverlay.svelte';
    import AttachmentPanelRoot from '../miniapps/AttachmentPanelRoot.svelte';
    import MarketplaceRoot from '../marketplace/MarketplaceRoot.svelte';
    import LaunchDialogRoot from '../miniapps/LaunchDialogRoot.svelte';
    const panes = shellPanes();
    const screens = shellScreens();
    const reveals = shellReveals();
    const profileView = profileScreen();
    const edit = profileEdit();
    const login = loginState();
    let profileEl = $state(null);
    let loginEl = $state(null);
    const bindProfile = bindShellEl('profile');
</script>

<Popup />
<!-- Pack details: opened by deep link (vector://emojis/pack/<naddr>) and the share-pack flow. -->
{#if screens.packDetails}<PackDetailsOverlay h={screens.packDetails.h} />{/if}

<WindowChrome />
<main class="container">
    <PickerRoot />
    <AttachmentPanelRoot />
    <MarketplaceRoot />
    <LaunchDialogRoot />

    <div id="profile" class="chats" style:display={panes.profile ? null : 'none'}
         class:is-own-profile={profileView.ownProfile} class:profile-edit-active={edit.active}
         bind:this={profileEl} use:bindProfile use:reveal={['profile', reveals.profile]}>
        {#if screens.profile}<ProfileScreen h={screens.profile.h} />{/if}
    </div>
    <GroupOverviewPane />
    <ChatListPane />
    <ChatPane />
    <!-- Widescreen: fills the conversation column while no chat is open, and the drag
         handle on the list pane's trailing edge. Both are inert elsewhere. -->
    <div id="ws-conv-empty">
        <img src="./icons/vector-logo.svg" alt="">
        <p>Pick a conversation to get started</p>
    </div>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div id="ws-list-resize" title="Drag to resize (double-click to reset)"
         onpointerdown={(e) => shellHandlers().listResizeStart?.(e)} ondblclick={() => shellHandlers().listResizeReset?.()}></div>
    <div id="voice-progress-container" class="voice-progress-container" style="display: none;">
        <div class="voice-progress-bar">
            <div class="voice-progress-fill"></div>
            <div class="voice-progress-text">Setting up voice transcription...</div>
        </div>
    </div>

    <div id="chat-new" style:display={panes.chatNew ? null : 'none'}>
        {#if screens.chatNew}<NewChat h={screens.chatNew.h} />{/if}
    </div>
    <div id="create-group" class="create-group-container" style:display={panes.createGroup ? null : 'none'}>
        {#if screens.createGroup}<CreateCommunity h={screens.createGroup.h} />{/if}
    </div>
    <div id="settings" style:display={panes.settings ? null : 'none'}>
        {#if screens.settings}<Settings h={screens.settings.h} />{/if}
    </div>
    <div id="invites" style:display={panes.invites ? null : 'none'}>
        {#if screens.invites}<InvitesScreen />{/if}
    </div>
    <Navbar />
    <!-- Boots with the fade-in; the class drops once it has played. -->
    <div id="login-form" class="fadein-anim" style:display={login.shown ? null : 'none'}
         class:has-back-bar={login.backBar} class:bunker-active={login.bunker}
         bind:this={loginEl} use:reveal={['login', reveals.login]}
         onanimationend={(e) => { if (e.target === e.currentTarget) e.currentTarget.classList.remove('fadein-anim'); }}>
        {#if screens.login}<LoginScreen h={screens.login.h} />{/if}
    </div>
</main>

<Tooltip />
<ProfileSwitcher />
<PickerTooltip />
<ImageViewer />
<Toast />
<BadgeCard />
<ContextMenu />
<RekeyProgress />
<InviteModal />

<!-- Body-level singletons. The ones whose helpers come from the vanilla side render once
     their script registers them with setScreen; the rest render from their stores alone. -->
{#if screens.composerPopups}<ComposerPopups h={screens.composerPopups.h} />{/if}
{#if screens.filePreview}<FilePreview h={screens.filePreview.h} />{/if}
{#if screens.miniProfile}<MiniProfile h={screens.miniProfile.h} />{/if}
{#if screens.reactionPopups}<ReactionPopups h={screens.reactionPopups.h} />{/if}
{#if screens.editHistory}<EditHistoryPopup h={screens.editHistory.h} />{/if}
{#if screens.qrOverlay}<QrOverlay h={screens.qrOverlay.h} />{/if}
{#if screens.qrScanner}<QrScanner h={screens.qrScanner.h} />{/if}
{#if screens.statusDialog}<StatusDialog h={screens.statusDialog.h} />{/if}
{#if screens.call}<CallOverlay h={screens.call.h} />{/if}
{#if screens.downgradeBlock}<DowngradeBlock h={screens.downgradeBlock.h} />{/if}
{#if screens.modConsole}<ModConsole h={screens.modConsole.h} />{/if}
{#if screens.communitySettings}<CommunitySettings h={screens.communitySettings.h} />{/if}
{#if screens.network}
    <AddRelayDialog h={screens.network.h.addRelay} />
    <RelayInfoDialog h={screens.network.h.relayInfo} />
    <BlossomInfoDialog h={screens.network.h.blossom} />
{/if}
{#if screens.pivx}
    <DepositDialog h={screens.pivx.h.deposit} />
    <SendDialog h={screens.pivx.h.send} />
    <WithdrawDialog h={screens.pivx.h.withdraw} />
    <PivxSettingsDialog h={screens.pivx.h.settings} />
{/if}
<CredentialModal />
<MigrationOverlay />
<ProcessingOverlay />
<PermissionPrompt />
<PublishDialog />
