// Navigation between the top-level screens.
// One global scope: loads after main.js and shares its globals.

async function openChatlist() {
    // Chatlist is the root — clearing the back stack means the next back
    // press exits to the home screen instead of replaying old open fns.
    clearBack();
    navbarSelect('chat-btn');
    VectorSvelte.showPane('navbar', true);
    if (fProfileEditMode) exitProfileEditMode(true);
    VectorSvelte.showPane('profile', false);
    VectorSvelte.showPane('settings', false);
    VectorSvelte.showPane('invites', false);
    VectorSvelte.showPane('groupOverview', false);
    // Hide the chat view too. openChat shows domChat BEFORE it resolves the chat, so a bail-to-list
    // (e.g. a community torn down mid-open by a ban/removal → no chat found) would otherwise strand
    // the blank chat header over the list. The list is the root view: nothing else should overlay it.
    VectorSvelte.showPane('chat', false);
    previousChatBeforeProfile = ""; // Clear when navigating away

    if (!VectorSvelte.paneShown('chats')) {
        VectorSvelte.revealPane('chats', 'fadein-subtle-anim');
        VectorSvelte.showPane('chats', true);
    }
    
    // Load and display pending Community invites (adjust layout before/after for consistency)
    adjustSize();
    await loadCommunityInvites();
    adjustSize();

    // Refresh timestamps immediately so they're not stale after viewing a chat
    updateChatlistTimestamps();
}

/** Apply the current bunker connection state to the Security panel's
 *  status dot. State strings match the backend's `bunker_state` event:
 *  'idle' | 'connecting' | 'online' | 'offline'. Idle clears the dot. */
function applyRemoteSignerDot(state) {
    VectorSvelte.setSignerDot(state === 'online' || state === 'offline' || state === 'connecting' ? state : '');
}

/** Resolve the external-signer card's content: a NIP-46 bunker, an on-device
 *  NIP-55 signer (Amber), or nothing for a local-key account. */
async function refreshRemoteSignerCard() {
    try {
        const status = await invoke('get_bunker_status');
        if (status) {
            // A remote signer reached over a relay: the bunker_state listener drives the dot.
            VectorSvelte.setSigner({
                label: 'Remote Signer',
                hint: 'Your identity key lives on your signer app. Vector only holds a device pairing key.',
                npub: status.remote_npub || '',
            });
            return;
        }
        const nip55 = await invoke('get_nip55_status').catch(() => null);
        if (nip55) {
            VectorSvelte.setSigner({
                label: 'Offline Signer',
                hint: 'Your identity key stays in your signer app. Vector holds nothing on this device.',
                npub: nip55.user_npub || '',
            });
            // A local IPC signer has no connection to drop: the dot reflects install
            // health. A transient needs-auth is a toast plus Re-authorize, not a red dot.
            applyRemoteSignerDot(nip55.installed ? 'online' : 'offline');
            return;
        }
        VectorSvelte.setSigner(null);
    } catch (e) {
        console.warn('[settings] remote signer status failed:', e);
        VectorSvelte.setSigner(null);
    }
}

function openSettings() {
    pushBack('settings', () => openChatlist());
    navbarSelect('settings-btn');
    VectorSvelte.showPane('navbar', true);
    VectorSvelte.showPane('settings', true);

    // Hide the other tabs
    if (fProfileEditMode) exitProfileEditMode(true);
    VectorSvelte.showPane('profile', false);
    VectorSvelte.showPane('chats', false);
    VectorSvelte.showPane('invites', false);
    VectorSvelte.showPane('groupOverview', false);
    previousChatBeforeProfile = ""; // Clear when navigating away

    // Update the Storage Breakdown
    initStorageSection();

    // Refresh blocked users list, logs cache, and Remote Signer card
    loadBlockedUsersList();
    invoke('get_logs').then((log) => { window._cachedLogs = log || ''; });
    refreshRemoteSignerCard();

    // An update is waiting: bring the Updates section into view and clear the dot.
    if (VectorSvelte.shellState().updateDot) {
        VectorSvelte.requestSettingsScroll('updates');
        VectorSvelte.setShellFlag('updateDot', false);
    }
}

let invitesMounted = false;
function invitesEnsureMounted() {
    if (invitesMounted) return;
    invitesMounted = true;
    VectorSvelte.setScreen('invites', {});
}

async function openInvites() {
    pushBack('invites', () => openChatlist());
    navbarSelect('invites-btn');
    VectorSvelte.showPane('navbar', true);
    VectorSvelte.showPane('invites', true);

    // Hide the other tabs
    if (fProfileEditMode) exitProfileEditMode(true);
    VectorSvelte.showPane('profile', false);
    VectorSvelte.showPane('chats', false);
    VectorSvelte.showPane('settings', false);
    VectorSvelte.showPane('groupOverview', false);
    previousChatBeforeProfile = ""; // Clear when navigating away

    // Fetch and display the invite code
    invitesEnsureMounted();
    VectorSvelte.setInvites({ phase: 'loading', code: '', xUrl: '' });
    try {
        const inviteCode = await invoke('get_or_create_invite_code');
        VectorSvelte.setInvites({ phase: 'ok', code: inviteCode, xUrl: buildXIntentUrl(inviteCode) });
    } catch (error) {
        VectorSvelte.setInvites({ phase: 'error' });
        console.error('Failed to get invite code:', error);
    }

    // Note: MLS invites are now shown in the Chat tab, not here
}

/** Light one navbar tab; the Navbar component dims the rest. */
function navbarSelect(strSelectionID = '') {
    VectorSvelte.setTab(strSelectionID);
}
