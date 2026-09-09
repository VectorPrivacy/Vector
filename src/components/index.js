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
import Chatlist from './chatlist/Chatlist.svelte';
import RailShortcuts from './rail/RailShortcuts.svelte';
import MessageRow from './chat/MessageRow.svelte';
import MessageList from './chat/MessageList.svelte';
import ComposerChrome from './composer/ComposerChrome.svelte';
import ComposerPopups from './composer/ComposerPopups.svelte';
import CommandComposer from './composer/CommandComposer.svelte';
import CommandStrip from './composer/CommandStrip.svelte';
import ChatHeader from './chat/ChatHeader.svelte';
import ProfileScreen from './profile/ProfileScreen.svelte';
import CommunityOverview from './community/CommunityOverview.svelte';
import MiniProfile from './people/MiniProfile.svelte';
import MessageToolbar from './chat/MessageToolbar.svelte';
import ReactionPopups from './chat/ReactionPopups.svelte';
import { openReactionTip, closeReactionTip, openReactionDetails, closeReactionDetails } from './lib/reactionpopups.svelte.js';
import { setMessageToolbar } from './lib/toolbar.svelte.js';
import { uploadProgressed, downloadProgressed, transferDone, transferFailed } from './lib/attachments.svelte.js';
import { setMiniappStatus } from './lib/miniapps.svelte.js';
import FileBox from './chat/attachments/FileBox.svelte';
import PackSidebar from './picker/PackSidebar.svelte';
import RecentsGrid from './picker/RecentsGrid.svelte';
import AllGrid from './picker/AllGrid.svelte';
import SearchGrid from './picker/SearchGrid.svelte';
import PackSections from './picker/PackSections.svelte';
import PackCreator from './picker/PackCreator.svelte';
import PackPreviewCard from './picker/PackPreviewCard.svelte';
import PackDetailsModal from './picker/PackDetailsModal.svelte';
import GifGrid from './picker/GifGrid.svelte';
import PinsDrawer from './chat/PinsDrawer.svelte';
import ModList from './moderation/ModList.svelte';
import ModFilters from './moderation/ModFilters.svelte';
import ModStats from './moderation/ModStats.svelte';
import ModChrome from './moderation/ModChrome.svelte';
import PolicyDesigner from './moderation/PolicyDesigner.svelte';
import MiniAppsGrid from './miniapps/MiniAppsGrid.svelte';
import InviteLinks from './community/InviteLinks.svelte';
import BlossomCaps from './settings/BlossomCaps.svelte';
import RelayLogs from './settings/RelayLogs.svelte';
import AccountRows from './people/AccountRows.svelte';
import EditHistory from './chat/EditHistory.svelte';
import LoginChrome from './auth/LoginChrome.svelte';
import Popup from './ui/Popup.svelte';
import ProcessingOverlay from './ui/ProcessingOverlay.svelte';
import PermissionPrompt from './ui/PermissionPrompt.svelte';
import PublishDialog from './ui/PublishDialog.svelte';
export { publishState, openPublishDialog, activatePublishDialog, closePublishDialog, unmountPublishDialog, setPublishPerms, setPublishPermsError, setPublishHint, setPublishBusy } from './lib/publish.svelte.js';
export { showProcessing, hideProcessing, openPermissionPrompt, activatePermissionPrompt, closePermissionPrompt, unmountPermissionPrompt, permissionState } from './lib/overlays.svelte.js';
export { popupState, openPopupDialog, closePopupDialog } from './lib/popup.svelte.js';
export { loginState, bunkerState, loginScreen, loginShowForm, loginHide, loginShowBunker, loginHideBunker, bunkerStatus, bunkerLink, bunkerCopied, bunkerBusy, bunkerDeadline, bunkerTick } from './lib/login.svelte.js';
import CreateCommunity from './community/CreateCommunity.svelte';
export { ccState, ccOpen, ccSetAvatar, ccSetBusy, ccSetError, ccProfilesChanged } from './lib/createcommunity.svelte.js';
export { setEditHistory, setEditHistoryBelow, clearEditHistory } from './lib/edithistory.svelte.js';
export { setBlossomCaps, setRelayLogs } from './lib/settings.svelte.js';
export { ilSet, ilSetBusy, ilSetCreating, ilSetRevoking, ilReset } from './lib/invitelinks.svelte.js';
import Marketplace from './marketplace/Marketplace.svelte';
import MarketplaceFilters from './marketplace/Filters.svelte';
import AppDetails from './marketplace/AppDetails.svelte';
export { mktState, mktApps, mktActions, mktIcons, mktPerms, mktSetApps, mktPatchApp, mktSetQuery, mktAddFilter, mktRemoveFilter, mktClearFilters, mktSetLoading, mktSetError, mktSetAnimate, mktSetAction, mktSetIcon, mktOpenDetails, mktCloseDetails, mktSetPerms } from './lib/marketplace.svelte.js';
export { gridState, gridApps, gridSetApps, gridSetQuery, gridSetEditMode, gridPatch, gridRemove } from './lib/miniappsgrid.svelte.js';
export { polState, polPresets, polRuleKinds, polStored, polDraft, polSetCatalogue, polSetStored, polSetChannels, polResetChannels, polShowGallery, polOpenEditor, polSetBusy, polSetPreview, polSetPreviewError } from './lib/policy.svelte.js';
import { modState, modIntel, modKeep, modOpen, modSetIntel, modSetError, modSetQuery, modSetBusy, modSetProgress } from './lib/moderation.svelte.js';
import { pinsState, setPins, setPinsOpen } from './lib/pins.svelte.js';
import { gifLoading, gifResults, gifEmpty, gifLoadingMore } from './lib/gifs.svelte.js';
import { packDetails, openPackDetails, resolvePackDetails, closePackDetails } from './lib/packdetails.svelte.js';
import { setCreator, setCreatorBusy, clearCreatorBusy, markCreatorBroken } from './lib/packcreator.svelte.js';
import { pickerState, setPickerPacks, setPickerActive, setPickerQuery, bumpPickerRecents, bumpPickerChrome } from './lib/picker.svelte.js';
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
export { pickerState, setPickerPacks, setPickerActive, setPickerQuery, bumpPickerRecents, bumpPickerChrome };
export { setCreator, setCreatorBusy, clearCreatorBusy, markCreatorBroken };
export { packDetails, openPackDetails, resolvePackDetails, closePackDetails };
export { gifLoading, gifResults, gifEmpty, gifLoadingMore };
export { pinsState, setPins, setPinsOpen };
export { modState, modIntel, modKeep, modOpen, modSetIntel, modSetError, modSetQuery, modSetBusy, modSetProgress };
export { openReactionTip, closeReactionTip, openReactionDetails, closeReactionDetails };
// The chat window as a derivation (streaks, day breaks, merged system events).
export { deriveWindow } from './lib/chatwindow.js';
// The chat view's window state: the engine sets it, the list island derives from it.
export { setWindow, clearWindow, touchWindow, touchMessage, setDivider, clearDivider } from './lib/chatview.svelte.js';
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
 * Mount the chat-list island into `target` (#chat-list). The vanilla side supplies
 * every helper it still owns (list.js, row.js, channels.js, main.js globals) plus a
 * snapshot() of the raw state arrays — the bundle is an IIFE and cannot see the page's
 * global lexical bindings. The island owns #chat-list's children exclusively; its keyed
 * {#each} reuses row nodes so single-chat changes patch single rows.
 */
export function mountChatlist(target, { h, snapshot }) {
    target.replaceChildren();
    return mount(Chatlist, { target, props: { h, snapshot } });
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
 * Mount the composer's chrome reconciler over the existing elements (index.html keeps
 * the markup; the editor is never touched). Renderless: `target` only hosts the effects.
 */
export function mountComposerChrome({ els, h }) {
    const host = document.createElement('div');
    host.hidden = true;
    document.body.appendChild(host);
    return mount(ComposerChrome, { target: host, props: { els, h } });
}

/**
 * Mount the composer's autocomplete popups (mention, shortcode, command) at body
 * level. The controllers publish views through `openPopup`/`closePopup`.
 */
export function mountComposerPopups({ anchor, h }) {
    return mount(ComposerPopups, { target: document.body, props: { anchor, h } });
}

/**
 * Mount the structured command composer: its argument pills render in a host placed
 * before the editor (display: contents, so they are the row's flex children), and
 * the context strip fills `strip` (#chat-command-bar). `onCancel` is the strip's
 * cancel button.
 */
export function mountCommandComposer({ editor, strip, onCancel }) {
    const host = document.createElement('div');
    host.style.display = 'contents';
    editor.before(host);
    const pills = mount(CommandComposer, { target: host });
    strip.replaceChildren();
    const bar = mount(CommandStrip, { target: strip, props: { onCancel } });
    return { pills, bar };
}

/**
 * Mount the chat header reconciler over the existing header elements (renderless).
 * It derives name, avatar, subtext and menu visibility from the open chat's signals.
 */
export function mountChatHeader({ els, h }) {
    const host = document.createElement('div');
    host.hidden = true;
    document.body.appendChild(host);
    return mount(ChatHeader, { target: host, props: { els, h } });
}

/** Mount the community pane's head into `target` (#ws-community-head). */
export function mountCommunityHead(target, { h }) {
    target.replaceChildren();
    return mount(CommunityHead, { target, props: { h } });
}

/** Mount the send-file preview overlay at body level; it shows itself from `fpOpen`. */
export function mountFilePreview({ h }) {
    return mount(FilePreview, { target: document.body, props: { h } });
}

/** Mount the Settings screen into `container` (#settings); the sections read the settings stores. */
export function mountSettings(container, { h }) {
    container.replaceChildren();
    return mount(Settings, { target: container, props: { h } });
}

/** Mount the Profile screen into `container` (#profile); the nav keeps showing and hiding the container. */
export function mountProfileScreen(container, { h }) {
    container.replaceChildren();
    return mount(ProfileScreen, { target: container, props: { root: container, h } });
}

/** Mount the Community overview body into `target` (#group-overview-scroll); `els` is its chat header. */
export function mountCommunityOverview(target, { els, h }) {
    target.replaceChildren();
    return mount(CommunityOverview, { target, props: { els, h } });
}

/** Mount the mini profile popup at body level; it shows itself from `openMiniProfile`. */
export function mountMiniProfile({ h }) {
    return mount(MiniProfile, { target: document.body, props: { h } });
}

/** Mount the message toolbar's buttons into `host` (#dmsg-toolbar). */
export function mountMessageToolbar(host) {
    host.replaceChildren();
    return mount(MessageToolbar, { target: host });
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

/** Mount the emoji picker's rail and its three stock grids into their static hosts. */
export function mountEmojiPicker({ sidebar, recents, all, results, resultsSection, sections, h }) {
    sidebar.replaceChildren();
    recents.replaceChildren();
    all.replaceChildren();
    results.replaceChildren();
    mount(PackSidebar, { target: sidebar, props: { h } });
    mount(RecentsGrid, { target: recents, props: { h } });
    mount(AllGrid, { target: all, props: { grid: all, h } });
    mount(SearchGrid, { target: results, props: { section: resultsSection, h } });
    sections.replaceChildren();
    mount(PackSections, { target: sections, props: { h } });
}

/** Mount the pack creator's cells into `grid`, adopting its chrome elements. */
export function mountPackCreator(grid, { els, h }) {
    grid.replaceChildren();
    return mount(PackCreator, { target: grid, props: { els, h } });
}

/** Mount one in-chat pack preview card into `target`; returns the instance for teardown. */
export function mountPackPreviewCard(target, props) {
    return mount(PackPreviewCard, { target, props });
}

/** Mount the pack details modal's body into `body`, adopting `overlay` for visibility. */
export function mountPackDetails(body, { overlay, h }) {
    body.replaceChildren();
    return mount(PackDetailsModal, { target: body, props: { overlay, h } });
}

/** Mount the GIF grid into `grid` (#gif-grid). */
export function mountGifGrid(grid, { h }) {
    grid.replaceChildren();
    return mount(GifGrid, { target: grid, props: { grid, h } });
}

/** Mount the pins drawer's list into `list` (#pins-drawer-list). */
export function mountPinsDrawer(list, { h }) {
    list.replaceChildren();
    return mount(PinsDrawer, { target: list, props: { h } });
}

/** Mount the moderation console's islands: list, filters, stats, and the renderless chrome. */
export function mountModeration({ list, filters, stats, els, h }) {
    list.replaceChildren(); filters.replaceChildren(); stats.replaceChildren();
    mount(ModList, { target: list, props: { h } });
    mount(ModFilters, { target: filters, props: {} });
    mount(ModStats, { target: stats, props: {} });
    const host = document.createElement('div'); host.hidden = true; document.body.appendChild(host);
    mount(ModChrome, { target: host, props: { els, h } });
}

/** Mount the policy designer into the console's Policies pane (#mod-policies-pane). */
export function mountPolicyDesigner(pane, { h }) {
    pane.replaceChildren();
    return mount(PolicyDesigner, { target: pane, props: { h } });
}

/** Mount the Mini Apps panel grid into `grid` (#miniapps-grid); the Nexus tile is rendered by it. */
export function mountMiniAppsGrid(grid, { h }) {
    grid.replaceChildren();
    return mount(MiniAppsGrid, { target: grid, props: { h } });
}

/** Mount the Nexus: the scroll body (featured + catalogue), the filter tags, and the details panel body. */
export function mountMarketplace({ body, filters, details, h }) {
    body.replaceChildren(); filters.replaceChildren(); details.replaceChildren();
    mount(Marketplace, { target: body, props: { h } });
    mount(MarketplaceFilters, { target: filters, props: {} });
    mount(AppDetails, { target: details, props: { h } });
}

/** Mount the invite panel's link section into `host` (#cmt-links); the panel is built per open. */
export function mountInviteLinks(host, { h }) {
    host.replaceChildren();
    return mount(InviteLinks, { target: host, props: { h } });
}

/** Mount the media server capabilities into `slot` (#blossom-info-capabilities). */
export function mountBlossomCaps(slot, { h }) {
    slot.replaceChildren();
    return mount(BlossomCaps, { target: slot, props: { h } });
}

/** Mount the relay activity log into `list` (#relay-info-logs). */
export function mountRelayLogs(list) {
    list.replaceChildren();
    return mount(RelayLogs, { target: list, props: {} });
}

/** Mount account rows into `host`; mounted fresh per open, so props are a snapshot. */
export function mountAccountRows(host, props) {
    host.replaceChildren();
    return mount(AccountRows, { target: host, props });
}

/** Mount the edit-history popup's entries into `content` (#edit-history-content). */
export function mountEditHistory(content, { h }) {
    content.replaceChildren();
    return mount(EditHistory, { target: content, props: { h } });
}

/** Mount the Create Community panel into `host` (#create-group). */
export function mountCreateCommunity(host, { h }) {
    host.replaceChildren();
    return mount(CreateCommunity, { target: host, props: { h } });
}

/** Mount the renderless login chrome over the shell's elements. */
export function mountLoginChrome({ els }) {
    const host = document.createElement('div'); host.hidden = true; document.body.appendChild(host);
    return mount(LoginChrome, { target: host, props: { els } });
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
