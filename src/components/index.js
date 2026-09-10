// Entry for the Svelte bundle. esbuild compiles this (+ every .svelte it pulls in) into
// src/components.bundle.js as an IIFE exposing the `VectorSvelte` global, which the vanilla
// one-global-scope scripts call.
//
// How UI gets on screen, in one sentence: shell/App.svelte renders everything. A component
// whose helpers live in a vanilla script is registered from that script with
// `setScreen(name, { h })` (lib/shell.svelte.js) and renders once the entry lands; one that
// needs only its store renders unconditionally. Register on DOMContentLoaded when the bag
// names helpers by value: the scripts share one scope but load in order, and a name from a
// later file is a ReferenceError that silently registers nothing. Do not add a `mount*` export: the four below
// exist for targets App cannot own (the message list's container, a card inside a rendered
// row, a host placed beside the composer's editor).
//
// Layout:
//   lib/       stores and shared logic, one file per concern
//   ui/        leaf atoms (Avatar, Toast, ...)
//   shell/     the root App and its panes
//   people/ chat/ composer/ settings/ ...   one directory per screen or feature
import { mount, unmount, flushSync } from 'svelte';

import MessageList from './chat/MessageList.svelte';
import CommandComposer from './composer/CommandComposer.svelte';
import { openReactionTip, closeReactionTip, openReactionDetails, closeReactionDetails } from './lib/reactionpopups.svelte.js';
import { setMessageToolbar } from './lib/toolbar.svelte.js';
import { uploadProgressed, downloadProgressed, transferDone } from './lib/attachments.svelte.js';
import { setMiniappStatus } from './lib/miniapps.svelte.js';
import FileBox from './chat/attachments/FileBox.svelte';
import PackPreviewCard from './picker/PackPreviewCard.svelte';
export { attachmentEls, setAttachmentHandlers, onAttachmentVisibility, attachmentVisible, setAttachmentVisible, attachmentState, attachmentSetView, attachmentPatch, attachmentPulse, pivxWalletLoading, pivxWalletSet, pivxWalletPatch } from './lib/attachmentpanel.svelte.js';
export { pivxDeposit, pivxSend, pivxWithdraw, pivxSettings } from './lib/pivx.svelte.js';
export { pivxBubble, setPivxBubble } from './lib/pivxbubble.svelte.js';
export { setLaunchDialogHandlers, launchDialog } from './lib/miniapps.svelte.js';
export { addRelayDialog, relayInfoDialog, blossomInfoDialog } from './lib/network.svelte.js';
export { qrOverlay, setQrScanner } from './lib/qr.svelte.js';
export { statusDialog } from './lib/statusdialog.svelte.js';
export { modOverlay } from './lib/moderation.svelte.js';
export { showDowngradeBlock } from './lib/overlays.svelte.js';
export { setInvites } from './lib/invitescreen.svelte.js';
import App from './shell/App.svelte';
export { setOverviewGroup, setOverviewHeadHandlers } from './lib/overview.svelte.js';
export { switcherState, setSwitcherHandlers, setSwitcherRows, setSwitcherAdd, openSwitcher, closeSwitcher } from './lib/switcher.svelte.js';
export { showTooltip, hideTooltip } from './lib/tooltip.svelte.js';
export { setAccount, revealAccount, setAccountHandlers } from './lib/account.svelte.js';
export { setMailBadge, mergeShellHandlers, setScreen, revealPane, revealPending, setSyncLine, onPaneChange, shellElements, shellState, showPane, paneShown, panesSnapshot, restorePanes, setTab, setShellFlag } from './lib/shell.svelte.js';
export { publishState, openPublishDialog, activatePublishDialog, closePublishDialog, unmountPublishDialog, setPublishPerms, setPublishPermsError, setPublishHint, setPublishBusy } from './lib/publish.svelte.js';
export { showProcessing, hideProcessing, openPermissionPrompt, activatePermissionPrompt, closePermissionPrompt, unmountPermissionPrompt } from './lib/overlays.svelte.js';
export { popupState, openPopupDialog, closePopupDialog } from './lib/popup.svelte.js';
export { loginState, bunkerState, pickerState as loginPickerState, encryptState, patchLogin, patchBunker, patchPicker, patchEncrypt, loginScreen, loginShowForm, loginHide, loginShowBunker, loginHideBunker, bunkerStatus, bunkerLink, bunkerCopied, bunkerBusy, bunkerDeadline, bunkerTick, resetLoginPin, focusLoginInput } from './lib/login.svelte.js';
export { credentialState, openCredentialDialog, closeCredentialDialog, showMigration, hideMigration, setMigrationProgress } from './lib/credential.svelte.js';
export { ccState, ccOpen, ccSetAvatar, ccSetBusy, ccSetError, ccProfilesChanged } from './lib/createcommunity.svelte.js';
export { editHistoryState, setEditHistory, openEditHistory, clearEditHistory } from './lib/edithistory.svelte.js';
export { setBlossomCaps, setRelayLogs } from './lib/network.svelte.js';
export { patchRelayStatus } from './lib/settings.svelte.js';
export { ilSet, ilSetBusy, ilSetCreating, ilSetRevoking, ilReset } from './lib/invitelinks.svelte.js';
export { mktState, mktActions, mktIcons, mktPerms, mktSetApps, mktPatchApp, mktAddFilter, mktClearFilters, mktSetLoading, mktSetError, mktSetAnimate, mktSetAction, mktSetIcon, mktOpenDetails, mktCloseDetails, mktSetPerms, setMarketplaceHandlers, mktOpenPanel, mktOpenDetailsPanel, mktClosePanel } from './lib/marketplace.svelte.js';
export { gridState, gridSetApps, gridSetQuery, gridSetEditMode, gridPatch } from './lib/miniappsgrid.svelte.js';
export { polState, polPresets, polRuleKinds, polStored, polDraft, polSetCatalogue, polSetStored, polSetChannels, polResetChannels, polShowGallery, polOpenEditor, polSetBusy, polSetPreview, polSetPreviewError } from './lib/policy.svelte.js';
import { modState, modIntel, modKeep, modOpen, modSetIntel, modSetError, modSetQuery, modSetBusy, modSetProgress, modSetTab } from './lib/moderation.svelte.js';
import { pinsState, setPins, setPinsOpen, setPinsButtonVisible, setPinsHandlers, pinsEls } from './lib/pins.svelte.js';
import { gifLoading, gifResults, gifEmpty, gifLoadingMore } from './lib/gifs.svelte.js';
import { packDetails, openPackDetails, resolvePackDetails, closePackDetails } from './lib/packdetails.svelte.js';
import { setCreator, setCreatorBusy, clearCreatorBusy, markCreatorBroken, setCreatorSaving, focusCreatorName } from './lib/packcreator.svelte.js';
import { pickerState, setPickerPacks, setPickerActive, setPickerQuery, bumpPickerRecents, bumpPickerChrome, panelState, setPanelMode, setPickerReady, setCreatorOpen, setPickerError, setPickerProgress, setPickerProgressDetail, setPickerConfirm, setPickerNaming, setPickerNamingError, setPickerCropperOpen } from './lib/picker.svelte.js';
import { miniProfile, openMiniProfile, closeMiniProfile } from './lib/miniprofile.svelte.js';
import { overviewRoster, overviewState, setOverview } from './lib/overview.svelte.js';
import { profileEdit, startProfileEdit, endProfileEdit, setProfileEditPicture, profileEditDirty } from './lib/profileedit.svelte.js';

// Shared store layer (SVELTE_MIGRATION_PLAN.md): the clock, and per-entity signals.
// Nothing here says "render": the vanilla side names WHAT changed and the islands
// re-derive exactly the DOM that depends on it.
export { bumpClockTick } from './lib/signals.svelte.js';
export {
    ensureSignals,
    touchChat,
    touchProfile,
    touchCommunity,
    touchInvites,
    reorderChatlist,
    setOpenChat,
    setPane,
    profileViewState,
    setOpenProfile,
    setProfileEditing,
} from './lib/signals.svelte.js';
/** Apply pending updates synchronously (for the rare caller that reads the DOM right after). */
export { flushSync };
export { profileEdit, startProfileEdit, endProfileEdit, setProfileEditPicture, profileEditDirty };
export { setProfileSwitcherOpen, profileEls } from './lib/profilescreen.svelte.js';
export { overviewRoster, overviewState, setOverview };
export { miniProfile, openMiniProfile, closeMiniProfile };
export { setMessageToolbar };
export { uploadProgressed, downloadProgressed, transferDone, setMiniappStatus };
export { pickerState, setPickerPacks, setPickerActive, setPickerQuery, bumpPickerRecents, bumpPickerChrome, setPanelMode, setPickerReady, setCreatorOpen, setPickerError, setPickerProgress, setPickerProgressDetail, setPickerConfirm, setPickerNaming, setPickerNamingError, setPickerCropperOpen };
export { setCreator, setCreatorBusy, clearCreatorBusy, markCreatorBroken, setCreatorSaving, focusCreatorName };
export { openPackDetails, resolvePackDetails, closePackDetails };
export { gifLoading, gifResults, gifEmpty, gifLoadingMore };
export { setPins, setPinsOpen, setPinsButtonVisible, setPinsHandlers, pinsEls };
export { setChatPaneHandlers, setChatHeaderHandlers } from './lib/chatpane.svelte.js';
export { wallpaperState, setWallpaperLayer, setWallpaperSliders, setWallpaperBusy, setWallpaperLabel, setWallpaperPreviewing } from './lib/wallpaper.svelte.js';
export { modState, modIntel, modKeep, modOpen, modSetIntel, modSetError, modSetBusy, modSetProgress, modSetTab };
export { openReactionTip, closeReactionTip, openReactionDetails, closeReactionDetails };
// The chat window as a derivation (streaks, day breaks, merged system events).
export { deriveWindow } from './lib/chatwindow.js';
// The chat view's window state: the engine sets it, the list island derives from it.
export { setWindow, clearWindow, touchWindow, touchMessage, setDivider, clearDivider, setNotice, setArrival } from './lib/chatview.svelte.js';
export { setSelfDestructSecs } from './lib/composer.svelte.js';
export { toolbarHost, toolbarEls, setToolbarHandlers, setToolbarHost, setToolbarSwipe } from './lib/toolbar.svelte.js';
export { showToast, hideToast } from './lib/toast.svelte.js';
export { contextMenuEls, setContextMenuHandlers, setContextMenu } from './lib/contextmenu.svelte.js';
export { setRekey } from './lib/rekey.svelte.js';
export { setInviteModalHandlers, setInviteModal, setInviteModalStatus, inviteModalPicker } from './lib/invitemodal.svelte.js';
export { badgeCardState, badgeCardEls, setBadgeCardHandlers, setBadgeCard, setBadgeTiltVars } from './lib/badgecard.svelte.js';
export { imageViewerState, imageViewerEls, setImageViewerHandlers, setImageViewer, setImageViewerZoom, setImageViewerTip } from './lib/imageviewer.svelte.js';
export { setModelDownload } from './lib/audio.svelte.js';
export { setVoiceState, setVoiceStatusText, setVoiceTimer, setVoiceDrag, setVoiceDot, setVoiceLockFading, setVoiceTooltip, setVoicePreview, voiceFadeIn, setVoiceHandlers, voiceEls } from './lib/voicerecorder.svelte.js';
export { setPickerHandlers, onPickerVisibility, pickerVisible, setPickerVisible, setPickerBottom, setPickerAnchor, pickerRoot, pickerEls, showPickerTip, hidePickerTip } from './lib/picker.svelte.js';
// The send-file preview overlay's state.
export {
    filePreview,
    openFilePreview as fpOpen,
    closeFilePreview as fpClose,
    setFilePreviewContent as fpContent,
    patchFilePreview as fpPatch,
} from './lib/filepreview.svelte.js';
// Settings: the Tor card's state and the blocked-users list's version.
export { torState, setTorState, setTorLocked, setTorAdvancedOpen, setTorCircuits, reloadBlockedUsers, setStorageDistribution, setNotifSettings, setSecurity, setSigner, setSignerDot, setDisplaySettings, setUpdates, setNetwork, voiceState, setVoice, setVoiceDownloadProgress, settingsScreen, setSettingsScreen, requestSettingsScroll, setSettingsHandlers } from './lib/settings.svelte.js';
// The composer's state: mode (reply/edit), draft emptiness, lock, command bar.
export {
    startReply,
    cancelReply,
    startEdit,
    cancelEdit,
    setDraftEmpty,
    setLock,
    setComposerStatus,
    openPopup,
    closePopup,
    setCommand,
    clearCommand,
    setCommandHint,
    setCommandInvalid,
    setCommandValue,
    openChoiceMenu,
    closeChoiceMenu,
    composerMode,
    setAttachmentOpen,
    setEmojiIcon,
    setScrollBadge,
    setComposerHandlers,
    composerEls,
} from './lib/composer.svelte.js';



/** Tear down a mounted island (call on dialog close / element removal). */
export function unmountComponent(instance) {
    return unmount(instance);
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


// The shell mounts as the bundle evaluates: it loads before main.js, whose load-time
// getElementById handles need the screens' containers in the document already.
mount(App, { target: document.body, props: {} });
flushSync();
export { reactionEls } from './lib/reactionpopups.svelte.js';
export { miniProfileEls } from './lib/miniprofile.svelte.js';
export { railEls } from './lib/rail.svelte.js';
