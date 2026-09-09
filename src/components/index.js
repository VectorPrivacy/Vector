// Entry for the Svelte island bundle. esbuild compiles this (+ every .svelte it pulls in)
// into src/components.bundle.js as an IIFE exposing the `VectorSvelte` global, so the vanilla
// one-global-scope frontend can call VectorSvelte.mountX(target, props) directly.
//
// Layout (SVELTE_MIGRATION_PLAN.md §3):
//   lib/       non-visual: stores, shared logic
//   ui/        leaf atoms (Avatar, ...)
//   people/    everything that lists persons: the row, the picker, the roster
//   chatlist/  the chat list island
//   chat/ composer/ settings/ ...   later phases, one directory per screen
import { mount, unmount, flushSync } from 'svelte';

import ContactPicker from './people/ContactPicker.svelte';
import MemberRoster from './people/MemberRoster.svelte';
import RailShortcuts from './rail/RailShortcuts.svelte';
import MessageRow from './chat/MessageRow.svelte';
import MessageList from './chat/MessageList.svelte';
import ComposerPopups from './composer/ComposerPopups.svelte';
import CommandComposer from './composer/CommandComposer.svelte';
import CommunityOverview from './community/CommunityOverview.svelte';
import MiniProfile from './people/MiniProfile.svelte';
import ReactionPopups from './chat/ReactionPopups.svelte';
import { openReactionTip, closeReactionTip, openReactionDetails, closeReactionDetails } from './lib/reactionpopups.svelte.js';
import { setMessageToolbar } from './lib/toolbar.svelte.js';
import { uploadProgressed, downloadProgressed, transferDone, transferFailed } from './lib/attachments.svelte.js';
import { setMiniappStatus } from './lib/miniapps.svelte.js';
import FileBox from './chat/attachments/FileBox.svelte';
import PackPreviewCard from './picker/PackPreviewCard.svelte';
import PackDetailsOverlay from './picker/PackDetailsOverlay.svelte';
import ModList from './moderation/ModList.svelte';
import ModFilters from './moderation/ModFilters.svelte';
import ModStats from './moderation/ModStats.svelte';
import ModConsole from './moderation/ModConsole.svelte';
import PolicyDesigner from './moderation/PolicyDesigner.svelte';
import AddRelayDialog from './settings/AddRelayDialog.svelte';
import RelayInfoDialog from './settings/RelayInfoDialog.svelte';
import BlossomInfoDialog from './settings/BlossomInfoDialog.svelte';
import LaunchDialog from './miniapps/LaunchDialog.svelte';
import AttachmentPanel from './miniapps/AttachmentPanel.svelte';
import DepositDialog from './miniapps/pivx/DepositDialog.svelte';
import SendDialog from './miniapps/pivx/SendDialog.svelte';
import WithdrawDialog from './miniapps/pivx/WithdrawDialog.svelte';
import PivxSettingsDialog from './miniapps/pivx/SettingsDialog.svelte';
export { attachmentState, attachmentSetView, attachmentPatch, attachmentPulse, pivxWalletState, pivxWalletLoading, pivxWalletSet, pivxWalletPatch } from './lib/attachmentpanel.svelte.js';
export { pivxDeposit, pivxSend, pivxWithdraw, pivxSettings } from './lib/pivx.svelte.js';
export { pivxBubble, setPivxBubble } from './lib/pivxbubble.svelte.js';
export { addRelayDialog, relayInfoDialog, blossomInfoDialog, launchDialog, qrOverlay, statusDialog, modOverlay, qrScanner, setQrScanner, showDowngradeBlock, setInvites } from './lib/dialogs.svelte.js';
import EditHistoryPopup from './chat/EditHistoryPopup.svelte';
import QrOverlay from './ui/QrOverlay.svelte';
import QrScanner from './ui/QrScanner.svelte';
import App from './shell/App.svelte';
export { setOverviewGroup, setOverviewHeadHandlers } from './lib/overview.svelte.js';
export { switcherState, setSwitcherHandlers, setSwitcherRows, setSwitcherAdd, openSwitcher, closeSwitcher } from './lib/switcher.svelte.js';
export { showTooltip, hideTooltip } from './lib/tooltip.svelte.js';
export { accountState, setAccount, revealAccount, setAccountHandlers } from './lib/account.svelte.js';
export { setMailBadge, shellScreens, setScreen, revealPane, revealPending, syncLineState, setSyncLine, onPaneChange, shellElements, shellState, showPane, paneShown, panesSnapshot, restorePanes, setTab, setShellFlag, setShellHandlers } from './lib/shell.svelte.js';
import StatusDialog from './ui/StatusDialog.svelte';
import DowngradeBlock from './ui/DowngradeBlock.svelte';
import CredentialModal from './ui/CredentialModal.svelte';
import MigrationOverlay from './ui/MigrationOverlay.svelte';
import Popup from './ui/Popup.svelte';
import ProcessingOverlay from './ui/ProcessingOverlay.svelte';
import PermissionPrompt from './ui/PermissionPrompt.svelte';
import PublishDialog from './ui/PublishDialog.svelte';
export { publishState, openPublishDialog, activatePublishDialog, closePublishDialog, unmountPublishDialog, setPublishPerms, setPublishPermsError, setPublishHint, setPublishBusy } from './lib/publish.svelte.js';
export { showProcessing, hideProcessing, openPermissionPrompt, activatePermissionPrompt, closePermissionPrompt, unmountPermissionPrompt, permissionState } from './lib/overlays.svelte.js';
export { popupState, openPopupDialog, closePopupDialog } from './lib/popup.svelte.js';
export { loginState, bunkerState, pickerState as loginPickerState, encryptState, patchLogin, patchBunker, patchPicker, patchEncrypt, loginScreen, loginShowForm, loginHide, loginShowBunker, loginHideBunker, bunkerStatus, bunkerLink, bunkerCopied, bunkerBusy, bunkerDeadline, bunkerTick, resetLoginPin, focusLoginInput } from './lib/login.svelte.js';
export { credentialState, openCredentialDialog, closeCredentialDialog, migrationState, showMigration, hideMigration, setMigrationProgress } from './lib/credential.svelte.js';
export { ccState, ccOpen, ccSetAvatar, ccSetBusy, ccSetError, ccProfilesChanged } from './lib/createcommunity.svelte.js';
export { editHistoryState, setEditHistory, setEditHistoryBelow, openEditHistory, clearEditHistory } from './lib/edithistory.svelte.js';
export { setBlossomCaps, setRelayLogs } from './lib/settings.svelte.js';
export { ilSet, ilSetBusy, ilSetCreating, ilSetRevoking, ilReset } from './lib/invitelinks.svelte.js';
import MarketplacePanel from './marketplace/MarketplacePanel.svelte';
import AppDetailsPanel from './marketplace/AppDetailsPanel.svelte';
export { mktState, mktApps, mktActions, mktIcons, mktPerms, mktSetApps, mktPatchApp, mktSetQuery, mktAddFilter, mktRemoveFilter, mktClearFilters, mktSetLoading, mktSetError, mktSetAnimate, mktSetAction, mktSetIcon, mktOpenDetails, mktCloseDetails, mktSetPerms } from './lib/marketplace.svelte.js';
export { gridState, gridApps, gridSetApps, gridSetQuery, gridSetEditMode, gridPatch, gridRemove } from './lib/miniappsgrid.svelte.js';
export { polState, polPresets, polRuleKinds, polStored, polDraft, polSetCatalogue, polSetStored, polSetChannels, polResetChannels, polShowGallery, polOpenEditor, polSetBusy, polSetPreview, polSetPreviewError } from './lib/policy.svelte.js';
import { modState, modIntel, modKeep, modOpen, modSetIntel, modSetError, modSetQuery, modSetBusy, modSetProgress, modSetTab } from './lib/moderation.svelte.js';
import { pinsState, setPins, setPinsOpen, setPinsButtonVisible, setPinsHandlers, pinsEls } from './lib/pins.svelte.js';
import { gifLoading, gifResults, gifEmpty, gifLoadingMore } from './lib/gifs.svelte.js';
import { packDetails, openPackDetails, resolvePackDetails, closePackDetails } from './lib/packdetails.svelte.js';
import { setCreator, setCreatorBusy, clearCreatorBusy, markCreatorBroken, setCreatorSaving, focusCreatorName } from './lib/packcreator.svelte.js';
import { pickerState, setPickerPacks, setPickerActive, setPickerQuery, bumpPickerRecents, bumpPickerChrome, panelState, setPanelMode, setPickerReady, setCreatorOpen, setPickerError, setPickerProgress, setPickerProgressDetail, setPickerConfirm, setPickerNaming, setPickerNamingError, setPickerCropperOpen } from './lib/picker.svelte.js';
import { miniProfile, openMiniProfile, closeMiniProfile } from './lib/miniprofile.svelte.js';
import { overviewState, setOverview } from './lib/overview.svelte.js';
import { profileEdit, startProfileEdit, endProfileEdit, setProfileEditPicture, profileEditDirty } from './lib/profileedit.svelte.js';
import CommunityHead from './chatlist/CommunityHead.svelte';
import FilePreview from './files/FilePreview.svelte';
import Settings from './settings/Settings.svelte';

// Shared store layer (SVELTE_MIGRATION_PLAN.md): the clock, and per-entity signals.
// Nothing here says "render": the vanilla side names WHAT changed and the islands
// re-derive exactly the DOM that depends on it.
export { timeTickVersion, bumpTimeTick } from './lib/stores.js';
export {
    ensureSignals, touchChat, touchProfile, touchCommunity, touchInvites,
    reorderChatlist, setOpenChat, setPane,
    profileViewState, setOpenProfile, setProfileEditing,
} from './lib/signals.svelte.js';
/** Apply pending updates synchronously (for the rare caller that reads the DOM right after). */
export { flushSync };
export { profileEdit, startProfileEdit, endProfileEdit, setProfileEditPicture, profileEditDirty };
export { profileScreen, setProfileSwitcherOpen } from './lib/profilescreen.svelte.js';
export { overviewState, setOverview };
export { miniProfile, openMiniProfile, closeMiniProfile };
export { setMessageToolbar };
export { uploadProgressed, downloadProgressed, transferDone, transferFailed, setMiniappStatus };
export { pickerState, setPickerPacks, setPickerActive, setPickerQuery, bumpPickerRecents, bumpPickerChrome, panelState, setPanelMode, setPickerReady, setCreatorOpen, setPickerError, setPickerProgress, setPickerProgressDetail, setPickerConfirm, setPickerNaming, setPickerNamingError, setPickerCropperOpen };
export { setCreator, setCreatorBusy, clearCreatorBusy, markCreatorBroken, setCreatorSaving, focusCreatorName };
export { packDetails, openPackDetails, resolvePackDetails, closePackDetails };
export { gifLoading, gifResults, gifEmpty, gifLoadingMore };
export { pinsState, setPins, setPinsOpen, setPinsButtonVisible, setPinsHandlers, pinsEls };
export { setChatPaneHandlers, setChatHeaderHandlers } from './lib/chatpane.svelte.js';
export { wallpaperState, setWallpaperLayer, setWallpaperSliders, setWallpaperBusy, setWallpaperLabel, setWallpaperPreviewing } from './lib/wallpaper.svelte.js';
export { modState, modIntel, modKeep, modOpen, modSetIntel, modSetError, modSetQuery, modSetBusy, modSetProgress, modSetTab };
export { openReactionTip, closeReactionTip, openReactionDetails, closeReactionDetails };
// The chat window as a derivation (streaks, day breaks, merged system events).
export { deriveWindow } from './lib/chatwindow.js';
// The chat view's window state: the engine sets it, the list island derives from it.
export { setWindow, clearWindow, touchWindow, touchMessage, setDivider, clearDivider, noticeState, setNotice, clearNotices, setArrival } from './lib/chatview.svelte.js';
export { setSelfDestructSecs } from './lib/composer.svelte.js';
export { toolbarHost, toolbarEls, setToolbarHandlers, setToolbarHost, setToolbarSwipe } from './lib/toolbar.svelte.js';
export { showToast, hideToast } from './lib/toast.svelte.js';
export { contextMenuState, contextMenuEls, setContextMenuHandlers, setContextMenu } from './lib/contextmenu.svelte.js';
export { rekeyState, setRekey } from './lib/rekey.svelte.js';
export { inviteModalState, setInviteModalHandlers, setInviteModal, setInviteModalStatus, inviteModalPicker } from './lib/invitemodal.svelte.js';
export { badgeCardState, badgeCardEls, setBadgeCardHandlers, setBadgeCard, setBadgeTiltVars } from './lib/badgecard.svelte.js';
export { imageViewerState, imageViewerEls, setImageViewerHandlers, setImageViewer, setImageViewerZoom, setImageViewerTip } from './lib/imageviewer.svelte.js';
export { setModelDownload, modelDownloadState } from './lib/audio.svelte.js';
export { recorderState, setVoiceState, setVoiceStatusText, setVoiceTimer, setVoiceDrag, setVoiceDot, setVoiceLockFading, setVoiceTooltip, setVoicePreview, voiceFadeIn, setVoiceHandlers, voiceEls } from './lib/voicerecorder.svelte.js';
export { setPickerHandlers, onPickerVisibility, pickerVisible, setPickerVisible, setPickerBottom, setPickerAnchor, pickerRoot, pickerEls, showPickerTip, hidePickerTip } from './lib/picker.svelte.js';
// The send-file preview overlay's state.
export {
    filePreview, filePreviewContent, openFilePreview as fpOpen, closeFilePreview as fpClose,
    setFilePreviewContent as fpContent, patchFilePreview as fpPatch,
} from './lib/filepreview.svelte.js';
// Settings: the Tor card's state and the blocked-users list's version.
export {
    torState, setTorState, setTorLocked, setTorAdvancedOpen, setTorCircuits, reloadBlockedUsers, setStorageDistribution, setNotifSettings, securityState, setSecurity, setSigner, setSignerDot, setDisplaySettings, updatesState, setUpdates, setNetwork, voiceState, setVoice, setVoiceDownloadProgress,
    settingsScreen, setSettingsScreen, requestSettingsScroll, setSettingsHandlers,
} from './lib/settings.svelte.js';
// The composer's state: mode (reply/edit), draft emptiness, lock, command bar.
export {
    startReply, cancelReply, startEdit, cancelEdit, setDraftEmpty, setLock, setComposerStatus,
    openPopup, closePopup,
    setCommand, clearCommand, setCommandHint, setCommandInvalid, setCommandValue, openChoiceMenu, closeChoiceMenu,
    composerMode, setAttachmentOpen, setEmojiIcon, setScrollBadge, setComposerHandlers, composerEls,
} from './lib/composer.svelte.js';

/**
 * Mount the contact picker into `target`. The component owns its dialog-local state;
 * the vanilla side drives it through the methods the returned instance exports
 * (setFilter / addStranger / select / setProfiles / reset / getSelection) and receives
 * selection changes through the `onSelectionChange` callback prop. Pass the return
 * value to `unmountComponent(instance)` on dialog close.
 */
export function mountContactList(target, props = {}) {
    target.replaceChildren();
    return mount(ContactPicker, { target, props });
}

/**
 * Mount the community member roster into `target` (#group-overview-members). The
 * vanilla side seeds it with the cached lists, then feeds authoritative fetches through
 * `setRoster` and profile loads through `setProfiles`; member-driven changes (kick,
 * ban, promote, unban) come back through the `onChange` callback prop.
 */
export function mountMemberRoster(target, props = {}) {
    target.replaceChildren();
    return mount(MemberRoster, { target, props });
}

/** Tear down a mounted island (call on dialog close / element removal). */
export function unmountComponent(instance) {
    return unmount(instance);
}


/**
 * Mount the widescreen rail shortcuts into `target` (#ws-rail-shortcuts). Derives from
 * the chat list's order and each chat's own signal; the open chat comes through
 * `setOpenChat`. `h.onUnreadDms(n)` reports the count the mail badge shows.
 */
export function mountRailShortcuts(target, { h, snapshot }) {
    target.replaceChildren();
    return mount(RailShortcuts, { target, props: { h, snapshot } });
}

/**
 * Mount the message-list island into `target` (#chat-messages). Rows, separators, the
 * unread divider and system events derive from the window state; the vanilla engine
 * sets that state and flushes synchronously before it measures.
 */
export function mountMessageList(target, { h }) {
    target.replaceChildren();
    return mount(MessageList, { target, props: { h } });
}

/**
 * Mount the composer's autocomplete popups (mention, shortcode, command) at body
 * level. The controllers publish views through `openPopup`/`closePopup`.
 */
export function mountComposerPopups({ anchor, h }) {
    return mount(ComposerPopups, { target: document.body, props: { anchor, h } });
}

/**
 * Mount the structured command composer's argument pills in a host placed before the
 * editor (display: contents, so they are the row's flex children). The context strip
 * is the composer box's own.
 */
export function mountCommandComposer({ editor }) {
    const host = document.createElement('div');
    host.style.display = 'contents';
    editor.before(host);
    return mount(CommandComposer, { target: host });
}

/**
 * Mount the chat header reconciler over the existing header elements (renderless).
 * It derives name, avatar, subtext and menu visibility from the open chat's signals.
 */
/** Mount the community pane's head into `target` (#ws-community-head). */
export function mountCommunityHead(target, { h }) {
    target.replaceChildren();
    return mount(CommunityHead, { target, props: { h } });
}

/** Mount the send-file preview overlay at body level; it shows itself from `fpOpen`. */
export function mountFilePreview({ h }) {
    return mount(FilePreview, { target: document.body, props: { h } });
}



/** Mount the Community overview body into `target` (#group-overview-scroll). */
export function mountCommunityOverview(target, { h }) {
    target.replaceChildren();
    return mount(CommunityOverview, { target, props: { h } });
}

/** Mount the mini profile popup at body level; it shows itself from `openMiniProfile`. */
export function mountMiniProfile({ h }) {
    return mount(MiniProfile, { target: document.body, props: { h } });
}


/** Mount the reaction hover tip + details popups at body level. */
export function mountReactionPopups({ h }) {
    return mount(ReactionPopups, { target: document.body, props: { h } });
}

/** Mount one file box into `target`, replacing whatever box it held (URL-shared Mini App cards). */
const fileBoxHosts = new WeakMap();
export function mountFileBox(target, props) {
    const prev = fileBoxHosts.get(target);
    if (prev) unmount(prev);
    target.replaceChildren();
    const inst = mount(FileBox, { target, props });
    fileBoxHosts.set(target, inst);
    return inst;
}


/** Mount one in-chat pack preview card into `target`; returns the instance for teardown. */
export function mountPackPreviewCard(target, props) {
    return mount(PackPreviewCard, { target, props });
}

/** Mount the pack details modal into its overlay (#pack-details-overlay), which it shows and hides. */
export function mountPackDetails(overlay, { h }) {
    overlay.replaceChildren();
    return mount(PackDetailsOverlay, { target: overlay, props: { overlay, h } });
}

/** Mount the moderation console onto the body (once); it renders its own overlay. */
export function mountModConsole({ h }) {
    return mount(ModConsole, { target: document.body, props: { h } });
}

/** Mount the policy designer into the console's Policies pane. */
export function mountPolicyDesigner(pane, { h }) {
    pane.replaceChildren();
    return mount(PolicyDesigner, { target: pane, props: { h } });
}

/** Mount the attachment panel into its fixed container (#attachment-panel); the grid rides inside. */
export function mountAttachmentPanel(container, { h }) {
    container.replaceChildren();
    return mount(AttachmentPanel, { target: container, props: { container, h } });
}

/** Mount the four PIVX wallet dialogs on the body; each opens through its store. */
export function mountPivxDialogs({ h }) {
    mount(DepositDialog, { target: document.body, props: { h: h.deposit } });
    mount(SendDialog, { target: document.body, props: { h: h.send } });
    mount(WithdrawDialog, { target: document.body, props: { h: h.withdraw } });
    mount(PivxSettingsDialog, { target: document.body, props: { h: h.settings } });
}

/** Mount the Nexus into its panel container (#marketplace-panel) and the details panel (#app-details-panel). */
export function mountMarketplace({ panel, details, h }) {
    panel.replaceChildren(); details.replaceChildren();
    mount(MarketplacePanel, { target: panel, props: { h } });
    mount(AppDetailsPanel, { target: details, props: { h } });
}

/** Mount the mini app launch dialog into its overlay container (#miniapp-launch-overlay). */
export function mountLaunchDialog(container, { h }) {
    container.replaceChildren();
    return mount(LaunchDialog, { target: container, props: { container, h } });
}

/** Mount the invite panel's link section into `host` (#cmt-links); the panel is built per open. */

/** Mount the Network section's dialogs (add relay, relay info, media server info) at body level. */
export function mountNetworkDialogs({ h }) {
    mount(AddRelayDialog, { target: document.body, props: { h: h.addRelay } });
    mount(RelayInfoDialog, { target: document.body, props: { h: h.relayInfo } });
    mount(BlossomInfoDialog, { target: document.body, props: { h: h.blossom } });
}

/** Mount account rows into `host`; mounted fresh per open, so props are a snapshot. */

/** Mount the edit-history popup onto the body (once); it renders when opened. */
export function mountEditHistory({ h }) {
    return mount(EditHistoryPopup, { target: document.body, props: { h } });
}


/** Mount the fullscreen QR overlay onto the body (once). */
export function mountQrOverlay({ h }) {
    return mount(QrOverlay, { target: document.body, props: { h } });
}

/** Mount the QR scanner onto the body (once); the video element is handed to `h.video`. */
export function mountQrScanner({ h }) {
    return mount(QrScanner, { target: document.body, props: { h } });
}

/** Mount the Status dialog onto the body (once); the composer host is handed to `h.composerHost`. */
export function mountStatusDialog({ h }) {
    return mount(StatusDialog, { target: document.body, props: { h } });
}

/** Mount the downgrade block onto the body (once); it renders when shown. */
export function mountDowngradeBlock({ h }) {
    return mount(DowngradeBlock, { target: document.body, props: { h } });
}




/** Mount the credential modal and the migration overlay at body level. */
export function mountCredentialModals() {
    mount(CredentialModal, { target: document.body, props: {} });
    mount(MigrationOverlay, { target: document.body, props: {} });
}

/** Mount the confirm/notice popup into its container (#popup-container). */
export function mountPopup(container) {
    container.replaceChildren();
    return mount(Popup, { target: container, props: { container } });
}

/** Mount the processing card onto the body (once). */
export function mountProcessingOverlay() {
    return mount(ProcessingOverlay, { target: document.body, props: {} });
}

/** Mount the mini app permission prompt onto the body (once). */
export function mountPermissionPrompt() {
    return mount(PermissionPrompt, { target: document.body, props: {} });
}

/** Mount the Nexus publish dialog onto the body (once). */
export function mountPublishDialog() {
    return mount(PublishDialog, { target: document.body, props: {} });
}

// The shell mounts as the bundle evaluates: it loads before main.js, whose load-time
// getElementById handles need the screens' containers in the document already.
mount(App, { target: document.body, props: {} });
flushSync();
