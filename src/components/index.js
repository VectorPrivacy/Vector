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
import ProfileView from './profile/ProfileView.svelte';
import ProfileEditFields from './profile/ProfileEditFields.svelte';
import CommunityOverview from './community/CommunityOverview.svelte';
import MiniProfile from './people/MiniProfile.svelte';
import MessageToolbar from './chat/MessageToolbar.svelte';
import { setMessageToolbar } from './lib/toolbar.svelte.js';
import { miniProfile, openMiniProfile, closeMiniProfile } from './lib/miniprofile.svelte.js';
import { overviewState, setOverview } from './lib/overview.svelte.js';
import { profileEdit, startProfileEdit, endProfileEdit, setProfileEditPicture, profileEditDirty } from './lib/profileedit.svelte.js';
import CommunityHead from './chatlist/CommunityHead.svelte';
import FilePreview from './files/FilePreview.svelte';
import TorCard from './settings/TorCard.svelte';
import TorCircuits from './settings/TorCircuits.svelte';
import BlockedUsers from './settings/BlockedUsers.svelte';
import StorageDonut from './settings/StorageDonut.svelte';
import Notifications from './settings/Notifications.svelte';
import SecurityCard from './settings/SecurityCard.svelte';
import Display from './settings/Display.svelte';
import Updates from './settings/Updates.svelte';
import NetworkList from './settings/NetworkList.svelte';
import Voice from './settings/Voice.svelte';

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
export { overviewState, setOverview };
export { miniProfile, openMiniProfile, closeMiniProfile };
export { setMessageToolbar };
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
} from './lib/settings.svelte.js';
// The composer's state: mode (reply/edit), draft emptiness, lock, command bar.
export {
    startReply, cancelReply, startEdit, cancelEdit, setDraftEmpty, setLock,
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

/** Mount the Tor card reconciler over the Privacy section's elements, and the circuit list into `els.list`. */
export function mountTorCard({ els, h }) {
    const host = document.createElement('div');
    host.hidden = true;
    document.body.appendChild(host);
    const card = mount(TorCard, { target: host, props: { els, h } });
    els.list.replaceChildren();
    const circuits = mount(TorCircuits, { target: els.list });
    return { card, circuits };
}

/** Mount the blocked-users list into `target` (#settings-blocked-list). */
export function mountBlockedUsers(target, { h, emptyEl }) {
    target.replaceChildren();
    return mount(BlockedUsers, { target, props: { h, emptyEl } });
}

/** Mount the Storage breakdown (summary, donut, legend) into `target` (#storage-breakdown). */
export function mountStorageDonut(target, { h }) {
    target.replaceChildren();
    return mount(StorageDonut, { target, props: { h } });
}

/** Mount the Notifications section body into `target` (#settings-notifications-body). */
export function mountNotifications(target, { h }) {
    target.replaceChildren();
    return mount(Notifications, { target, props: { h } });
}

/** Mount the Security reconciler over the section's elements. */
export function mountSecurityCard({ els }) {
    const host = document.createElement('div');
    host.hidden = true;
    document.body.appendChild(host);
    return mount(SecurityCard, { target: host, props: { els } });
}

/** Mount the Display section body into `target` (#settings-display-body). */
export function mountDisplay(target, { h }) {
    target.replaceChildren();
    return mount(Display, { target, props: { h } });
}

/** Mount the Updates section body into `target` (#settings-updates-body). */
export function mountUpdates(target, { h }) {
    target.replaceChildren();
    return mount(Updates, { target, props: { h } });
}

/** Mount the Network lists into `target` (#network-list). */
export function mountNetworkList(target, { h }) {
    target.replaceChildren();
    return mount(NetworkList, { target, props: { h } });
}

/** Mount the Voice section body into `target` (#settings-voice-body). */
export function mountVoice(target, { h }) {
    target.replaceChildren();
    return mount(Voice, { target, props: { h } });
}

/** Mount the profile view reconciler over #profile's elements. */
export function mountProfileView({ els, h }) {
    const host = document.createElement('div');
    host.hidden = true;
    document.body.appendChild(host);
    els.editFields.replaceChildren();
    mount(ProfileEditFields, { target: els.editFields });
    return mount(ProfileView, { target: host, props: { els, h } });
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
