// Navigation between the top-level screens.
// One global scope: loads after main.js and shares its globals.

async function openChatlist() {
    // Chatlist is the root — clearing the back stack means the next back
    // press exits to the home screen instead of replaying old open fns.
    clearBack();
    navbarSelect('chat-btn');
    domNavbar.style.display = '';
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';
    domGroupOverview.style.display = 'none';
    // Hide the chat view too. openChat shows domChat BEFORE it resolves the chat, so a bail-to-list
    // (e.g. a community torn down mid-open by a ban/removal → no chat found) would otherwise strand
    // the blank chat header over the list. The list is the root view: nothing else should overlay it.
    domChat.style.display = 'none';
    previousChatBeforeProfile = ""; // Clear when navigating away

    if (domChats.style.display !== '') {
        // Run a subtle fade-in animation
        domChats.classList.add('fadein-subtle-anim');
        domChats.addEventListener('animationend', () => domChats.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domChats.style.display = '';
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
    domNavbar.style.display = '';
    domSettings.style.display = '';

    // Hide the other tabs
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domChats.style.display = 'none';
    domInvites.style.display = 'none';
    domGroupOverview.style.display = 'none';
    previousChatBeforeProfile = ""; // Clear when navigating away

    // Update the Storage Breakdown
    initStorageSection();

    // Refresh blocked users list, logs cache, and Remote Signer card
    loadBlockedUsersList();
    invoke('get_logs').then((log) => { window._cachedLogs = log || ''; });
    refreshRemoteSignerCard();

    // If an update is available, scroll to the updates section
    const updateDot = document.getElementById('settings-update-dot');
    if (updateDot && updateDot.style.display !== 'none') {
        // Give the settings tab time to render
        setTimeout(() => {
            const updatesSection = document.getElementById('settings-updates');
            if (updatesSection) {
                updatesSection.scrollIntoView({ behavior: 'smooth', block: 'start' });
                // Hide the notification dot after scrolling
                updateDot.style.display = 'none';
            }
        }, 100);
    }
}

async function openInvites() {
    pushBack('invites', () => openChatlist());
    navbarSelect('invites-btn');
    domNavbar.style.display = '';
    domInvites.style.display = '';

    // Hide the other tabs
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domChats.style.display = 'none';
    domSettings.style.display = 'none';
    domGroupOverview.style.display = 'none';
    previousChatBeforeProfile = ""; // Clear when navigating away

    // Fetch and display the invite code
    const inviteCodeElement = document.getElementById('invite-code');
    inviteCodeElement.textContent = 'Loading';
    
    try {
        const inviteCode = await invoke('get_or_create_invite_code');
        inviteCodeElement.textContent = inviteCode;
        document.getElementById('invite-code-twitter').href = buildXIntentUrl(inviteCode);
        
        // Add invite code copy functionality
        const copyBtn = document.getElementById('invite-code-copy');
        if (copyBtn) {
            // Remove any existing listeners to prevent duplicates
            copyBtn.replaceWith(copyBtn.cloneNode(true));
            const newCopyBtn = document.getElementById('invite-code-copy');
            
            newCopyBtn.addEventListener('click', (e) => {
                if (inviteCode && inviteCode !== 'Loading...' && inviteCode !== 'Error loading code') {
                    navigator.clipboard.writeText(inviteCode).then(() => {
                        const btn = e.target.closest('.invite-code-copy-btn');
                        if (btn) {
                            btn.innerHTML = '<span class="icon icon-check"></span>';
                            setTimeout(() => {
                                btn.innerHTML = '<span class="icon icon-copy"></span>';
                            }, 2000);
                        }
                    });
                }
            });
        }
    } catch (error) {
        inviteCodeElement.textContent = 'Error loading code';
        console.error('Failed to get invite code:', error);
    }

    // Note: MLS invites are now shown in the Chat tab, not here
}

/**
 * A utility to "select" one Navbar item, deselecting the rest automatically.
 */
function navbarSelect(strSelectionID = '') {
    // Scoped to the tab buttons: the navbar also carries the widescreen rail's
    // head, collapse toggle and account chip, which must not be dimmed.
    for (const navItem of domNavbar.querySelectorAll('.navbar-btn')) {
        if (strSelectionID === navItem.id) navItem.classList.remove('navbar-btn-inactive');
        else navItem.classList.add('navbar-btn-inactive');
    }
}
