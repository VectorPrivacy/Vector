// Authentication: the login screens, the remote signer (bunker) session, the
// invite and welcome steps, and the encryption (PIN / password) flow. One global
// scope: this loads before main.js and shares its globals.

const domLoginStart = document.getElementById('login-start');
const domLoginAccountCreationBtn = document.getElementById('start-account-creation-btn');
const domLoginAccountBtn = document.getElementById('start-login-btn');
const domLoginBunkerStartBtn = document.getElementById('start-bunker-btn');
const domLoginNip55StartBtn = document.getElementById('start-nip55-btn');
const domLogin = document.getElementById('login-form');
const domLoginImport = document.getElementById('login-import');
const domLoginInput = document.getElementById('login-input');
const domLoginBtn = document.getElementById('login-btn');
const domLoginBunker = document.getElementById('login-bunker');
const domLoginBunkerUrlInput = document.getElementById('bunker-url-input');
const domLoginBunkerConnectBtn = document.getElementById('bunker-connect-btn');
const domLoginBunkerStatus = document.getElementById('bunker-status-text');
const domLoginBunkerQrWrap = document.querySelector('.login-bunker-qr-wrap');
const domLoginBunkerQr = document.getElementById('bunker-qr');
const domLoginBunkerCopyBtn = document.getElementById('bunker-copy-url-btn');

// Active nostrconnect:// URL — captured when start_nostrconnect_session
// returns so the Copy button can place it on the clipboard.
let strBunkerNostrConnectUrl = '';

// Bunker form mode — 'new' for fresh logins / Add Profile, 'reauth' for
// re-pairing an already-committed account whose signer wiped its
// permissions. Module scope so the boot-time login catch can route into
// reauth mode before DOMContentLoaded finishes wiring click handlers.
let bunkerFormMode = 'new';

// Bunker connection URL is single-use and the backend's NostrConnect uses
// a 120s timeout — pair this client-side so the user sees a live countdown
// and we auto-reroll a fresh QR + URL when it expires.
const BUNKER_SESSION_TIMEOUT_MS = 120 * 1000;
let bunkerSessionDeadline = 0;
let bunkerSessionTimerHandle = null;

function stopBunkerSessionTimer() {
    if (bunkerSessionTimerHandle) {
        clearInterval(bunkerSessionTimerHandle);
        bunkerSessionTimerHandle = null;
    }
    bunkerSessionDeadline = 0;
}

function renderBunkerCountdown() {
    const status = document.getElementById('bunker-status-text');
    if (!status) return;
    const remaining = Math.max(0, bunkerSessionDeadline - Date.now());
    if (remaining === 0) {
        stopBunkerSessionTimer();
        status.textContent = 'Refreshing connection link…';
        status.className = 'login-bunker-status connecting';
        startBunkerSession();
        return;
    }
    const secs = Math.ceil(remaining / 1000);
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    status.textContent = `Waiting for signer… (${m}:${s.toString().padStart(2, '0')})`;
    status.className = 'login-bunker-status connecting';
}

function armBunkerSessionTimer() {
    stopBunkerSessionTimer();
    bunkerSessionDeadline = Date.now() + BUNKER_SESSION_TIMEOUT_MS;
    bunkerSessionTimerHandle = setInterval(renderBunkerCountdown, 1000);
    renderBunkerCountdown();
}

/**
 * Kick off a NIP-46 client-initiated session — either fresh
 * (`start_nostrconnect_session`) or re-pairing an existing committed account
 * (`reauthorize_bunker`). Backend returns a `nostrconnect://` URL that we
 * render as a QR + Copy button.
 */
async function startBunkerSession() {
    strBunkerNostrConnectUrl = '';
    if (domLoginBunkerQrWrap) domLoginBunkerQrWrap.classList.remove('ready');
    if (domLoginBunkerCopyBtn) {
        domLoginBunkerCopyBtn.disabled = true;
        domLoginBunkerCopyBtn.classList.remove('copied');
        domLoginBunkerCopyBtn.textContent = 'Copy connection link';
    }
    if (domLoginBunkerStatus) {
        domLoginBunkerStatus.textContent = 'Waiting for signer…';
        domLoginBunkerStatus.className = 'login-bunker-status connecting';
    }
    try {
        // Reauth re-uses the existing client keypair from MY_SECRET_KEY and
        // is for already-committed accounts — no Add Profile commit step.
        const cmd = bunkerFormMode === 'reauth' ? 'reauthorize_bunker' : 'start_nostrconnect_session';
        if (bunkerFormMode !== 'reauth' && typeof addAccountFlow !== 'undefined' && addAccountFlow.active) {
            await addAccountFlow.commit();
        }
        // Recover from a missed `bunker_reauthorize_succeeded` — if the
        // frontend reloaded between the event firing and the listener
        // registering, the backend stashes the npub in a one-shot slot we
        // can poll here. (No-op when nothing was stashed.)
        if (bunkerFormMode === 'reauth') {
            try {
                const recoveredNpub = await invoke('get_pending_reauth_result');
                if (recoveredNpub) {
                    // The new pairing was already installed by the bg task;
                    // we just need to put the UI back where the user came
                    // from. Mirror the success-listener restore logic.
                    strPubkey = recoveredNpub;
                    stopBunkerSessionTimer();
                    const origin = bunkerReauthOrigin;
                    hideBunkerForm();
                    if (origin) {
                        if (domLoginBackBar) domLoginBackBar.style.display = 'none';
                        const lf = document.getElementById('login-form');
                        if (lf) lf.classList.remove('has-back-bar', 'bunker-active');
                        if (domLogin) domLogin.style.display = 'none';
                        bunkerReauthOrigin = null;
                        if (origin === 'settings' && typeof openSettings === 'function') {
                            openSettings();
                        } else if (typeof closeChat === 'function') {
                            closeChat();
                        }
                    } else {
                        // No origin = came from the boot-time popup; full boot.
                        invoke('connect').catch(() => {});
                        login(true);
                    }
                    return;
                }
            } catch (_) { /* missing-command-fail-open */ }
        }
        const url = await invoke(cmd);
        strBunkerNostrConnectUrl = url;
        const rendered = renderQrInto(domLoginBunkerQr, url);
        if (rendered && domLoginBunkerQrWrap) {
            domLoginBunkerQrWrap.classList.add('ready');
        }
        if (domLoginBunkerCopyBtn) domLoginBunkerCopyBtn.disabled = false;
        // Start the live countdown — auto-rerolls a fresh QR when the
        // 120s backend timeout expires so the user isn't stranded.
        armBunkerSessionTimer();
    } catch (e) {
        stopBunkerSessionTimer();
        if (domLoginBunkerStatus) {
            domLoginBunkerStatus.textContent = String(e);
            domLoginBunkerStatus.className = 'login-bunker-status error';
        }
    }
}

/** Tracks which main-app panel was visible when the bunker form took over
 *  (reauth from Settings, etc.) so the back button can restore it. Null
 *  when entering from the login screen. */
let bunkerReauthOrigin = null;

/**
 * Show the bunker form (QR + Copy + paste fallback). `mode` is 'new' for
 * regular login / Add Profile entry, 'reauth' for the recovery flow when a
 * signer has wiped its permissions.
 */
function showBunkerForm(mode = 'new') {
    bunkerFormMode = mode;
    // Reauth enters from inside the main app, where Settings (or another
    // panel) is rendered behind #login-form and shows through. Snapshot the
    // visible panel and hide every major view so the bunker form gets the
    // full viewport with no see-through layout.
    if (mode === 'reauth') {
        const settingsVisible = typeof domSettings !== 'undefined' && domSettings
            && domSettings.style.display !== 'none';
        bunkerReauthOrigin = settingsVisible ? 'settings' : 'chats';
        if (typeof domNavbar !== 'undefined' && domNavbar) domNavbar.style.display = 'none';
        if (typeof domSettings !== 'undefined' && domSettings) domSettings.style.display = 'none';
        if (typeof domChats !== 'undefined' && domChats) domChats.style.display = 'none';
        if (typeof domProfile !== 'undefined' && domProfile) domProfile.style.display = 'none';
        if (typeof domInvites !== 'undefined' && domInvites) domInvites.style.display = 'none';
        if (typeof domGroupOverview !== 'undefined' && domGroupOverview) domGroupOverview.style.display = 'none';
    } else {
        bunkerReauthOrigin = null;
    }
    if (domLoginImport) domLoginImport.style.display = 'none';
    if (domLoginStart) domLoginStart.style.display = 'none';
    if (domLoginInvite) domLoginInvite.style.display = 'none';
    if (typeof domLoginEncrypt !== 'undefined' && domLoginEncrypt) domLoginEncrypt.style.display = 'none';
    // Also show the parent login form + back-bar in case we're entering
    // from the main app (reauth path can fire from anywhere).
    const loginForm = document.getElementById('login-form');
    if (loginForm) {
        loginForm.classList.add('bunker-active');
        loginForm.classList.add('has-back-bar');
    }
    if (typeof domLogin !== 'undefined' && domLogin) domLogin.style.display = '';
    if (typeof domLoginBackBar !== 'undefined' && domLoginBackBar) domLoginBackBar.style.display = '';
    domLoginBunker.classList.remove('is-hidden');
    domLoginBunker.style.display = '';
    if (domLoginBunkerStatus) {
        domLoginBunkerStatus.textContent = '';
        domLoginBunkerStatus.className = 'login-bunker-status';
    }
    // Fresh URL per open — single-use, can't be cached.
    startBunkerSession();
}
window.showBunkerForm = showBunkerForm;

/** Hide the bunker form and clear its in-memory state. */
function hideBunkerForm() {
    stopBunkerSessionTimer();
    domLoginBunker.classList.add('is-hidden');
    domLoginBunker.style.display = 'none';
    const loginForm = document.getElementById('login-form');
    if (loginForm) loginForm.classList.remove('bunker-active');
    if (domLoginBunkerUrlInput) domLoginBunkerUrlInput.value = '';
    strBunkerNostrConnectUrl = '';
    if (domLoginBunkerQr) domLoginBunkerQr.innerHTML = '';
    if (domLoginBunkerQrWrap) domLoginBunkerQrWrap.classList.remove('ready');
}
window.hideBunkerForm = hideBunkerForm;

const domLoginImportError = document.getElementById('login-import-error');

const domLoginBackBar = document.getElementById('login-back-bar');
const domLoginBackBtn = document.getElementById('login-back-btn');

const domLoginInvite = document.getElementById('login-invite');
const domInviteInput = document.getElementById('invite-input');
const domInviteBtn = document.getElementById('invite-btn');

const domLoginWelcome = document.getElementById('login-welcome');

const domLoginEncrypt = document.getElementById('login-encrypt');
const domLoginEncryptTitle = document.getElementById('login-encrypt-title');
const domLoginEncryptPinRow = document.getElementById('login-encrypt-pins');
const domLoginEncryptPassword = document.getElementById('login-encrypt-password');
const domLoginPasswordInput = document.getElementById('login-password-input');
const domLoginEncryptTypeSelect = document.getElementById('login-encrypt-type-select');

/**
 * Login to the Nostr network
 * @param {boolean} skipAnimations - Skip intro animations (for instant login without PIN)
 */
async function login(skipAnimations = false) {
    if (strPubkey) {
        // Successful end of the Add Profile flow — drop the back-target
        // and reset flags so the next session starts clean.
        addAccountFlow.finish();
        // Fire connect + all listener registrations in parallel (no sequential IPC waits)
        console.time('[Boot] connect + listeners');
        const _connectP = invoke("connect");
        const _listenersP = setupRustListeners();

        // Setup unified progress operation event listener
        const _progressP = listen('progress_operation', (evt) => {
            const { type, current, total, message } = evt.payload;
            
            switch (type) {
                case 'start':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.add('typing-indicator-text');
                    domLoginEncryptTitle.style.color = '';
                    break;
                    
                case 'progress':
                    if (current && total) {
                        const progress = Math.round((current / total) * 100);
                        domLoginEncryptTitle.textContent = `${message} (${progress}%)`;
                    } else {
                        domLoginEncryptTitle.textContent = message;
                    }
                    break;
                    
                case 'complete':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.remove('typing-indicator-text');
                    break;
                    
                case 'error':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.remove('typing-indicator-text');
                    domLoginEncryptTitle.style.color = 'red';
                    break;
            }
        });


        // Setup a Rust Listener for the backend's init finish
        // (helper hoisted above this block — see runWithTorBootstrapStatus)
        const _initFinishedP = listen('init_finished', async (evt) => {
            console.timeEnd('[Boot] login() total');
            console.time('[Boot] init_finished handler');
            // The backend now sends both profiles (without messages) and chats (with messages)
            arrProfiles = evt.payload.profiles || [];
            arrChats = evt.payload.chats || [];

            // Pinned favourites, from the LOCAL mirror so the first paint is
            // already in pin order; the self-sync subscription streams any
            // sibling-device edit in afterwards.
            _pinnedLoaded = false; // a fresh account's pins are not the last one's
            await ensurePinnedLoaded();

            // Seed unread badges from the DB — boot loads only the last message per chat into RAM,
            // so the in-memory walk can't see a backlog received in a prior session. Fire-and-render
            // (re-renders the chatlist when it lands; the first paint uses the RAM-walk fallback).
            refreshUnreadCounts();

            // Resolve Community logos in the background (Community metadata rides the
            // chat payload; only the encrypted logo needs a lazy cache step).
            resolveCommunityAvatars();

            // Warm the full emoji set in the background (subscribed packs + the
            // active theme's pinned pack). Without this `arrEmojiPacks` stays empty
            // until the picker is first opened, so `:shortcode:` autocomplete shows
            // nothing and optimistic custom-emoji renders can't resolve. Also
            // registers the theme with the send resolver and makes the first picker
            // open cheap (data + DOM already composed). Read-only/local + guarded.
            loadEmojiPacks();
            // Warm frecency too, so `:` autocomplete + the picker reflect ranked/recent use from the
            // first interaction, not only after the panel's first open (which is where it loaded before).
            loadEmojiUsage();

            // Helper to show the main UI after login
            const showMainUI = async () => {
                console.time('[Boot] showMainUI:dom');
                domLoginInput.value = "";
                domLogin.style.display = 'none';
                domLoginEncrypt.style.display = 'none';

                // Show navbar and bookmarks
                domNavbar.style.display = '';
                domChatBookmarksBtn.style.display = 'flex';

                // Land on the Chat tab. Without an explicit reset here, the
                // visibility of the main panels is whatever they were before
                // login fired — which is fine for fresh boots (chats panel
                // is visible by default) but not for the Add Profile flow,
                // which hid every panel and never re-showed them. Always
                // resetting to Chat gives one consistent landing point for
                // new accounts, imported accounts, and normal logins alike.
                domChats.style.display = '';
                domChat.style.display = 'none';
                domProfile.style.display = 'none';
                domSettings.style.display = 'none';
                domInvites.style.display = 'none';
                if (typeof domGroupOverview !== 'undefined') {
                    domGroupOverview.style.display = 'none';
                }
                navbarSelect('chat-btn');

                // Render our profile
                const cProfile = arrProfiles.find(p => p.mine);
                renderCurrentProfile(cProfile);
                domAccount.style.display = '';
                console.timeEnd('[Boot] showMainUI:dom');

                // Refresh our own profile from the network
                if (cProfile?.id) {
                    invoke("queue_profile_sync", {
                        npub: cProfile.id,
                        priority: "critical",
                        forceRefresh: true
                    });
                }

                // Finished boot!
                fInit = false;
                // Widescreen waits for a live session (it docks the account chip).
                wsUpdate();
                // Catch a share that landed between the cold-start poll and now (the live listener
                // skips events while fInit was still true).
                consumePendingShare();
                // Same window exists for deep links: drain any action stored while
                // fInit gated the live listener.
                invoke('get_pending_deep_link').then(a => { if (a) executeDeepLinkAction(a); }).catch(() => {});

                // Mount the chatlist island; from here every change flows through the signals.
                console.time('[Boot] showMainUI:mountChatlist');
                mountChatlist();
                console.timeEnd('[Boot] showMainUI:mountChatlist');

                // Show the New Chat buttons
                if (domChatNewDM) {
                    domChatNewDM.style.display = '';
                    domChatNewDM.onclick = openNewChat;
                }
                if (domChatNewGroup) {
                    domChatNewGroup.style.display = '';
                    domChatNewGroup.onclick = openCreateGroup;
                }

                // Adjust the Chat List sizes (deferred — layout reflows don't block first paint)
                requestAnimationFrame(() => adjustSize());

                // Prompt for background service / battery optimization (mobile only, once)
                // Deferred so login animations finish first
                if (platformFeatures.is_mobile) {
                    setTimeout(async () => {
                        try {
                            const prompted = await invoke('get_background_service_prompted');
                            console.log('[Battery] prompted =', prompted);
                            if (!prompted) {
                                await invoke('set_background_service_prompted');
                                await showBackgroundServicePrompt();
                            }
                        } catch (e) {
                            console.error('[Battery] prompt error:', e);
                        }
                    }, 1500);
                }
            };

            if (skipAnimations) {
                console.time('[Boot] showMainUI');
                await showMainUI();
                console.timeEnd('[Boot] showMainUI');
                console.timeEnd('[Boot] init_finished handler');
                console.log('[Boot] UI visible - instant login complete');

                // Apply the same intro animations as the encryption flow
                domChatBookmarksBtn.style.opacity = '0';
                domNavbar.classList.add('fadein-anim');
                domNavbar.addEventListener('animationend', () => {
                    domNavbar.classList.remove('fadein-anim');
                    domChatBookmarksBtn.style.opacity = '';
                    domChatBookmarksBtn.classList.add('fadein-anim');
                    domChatBookmarksBtn.addEventListener('animationend', () => domChatBookmarksBtn.classList.remove('fadein-anim'), { once: true });
                }, { once: true });

                domAccount.classList.add('fadein-anim');
                domAccount.addEventListener('animationend', () => domAccount.classList.remove('fadein-anim'), { once: true });

                domChatList.classList.add('intro-anim');
                domChatList.addEventListener('animationend', () => domChatList.classList.remove('intro-anim'), { once: true });

                if (domChatNewDM) {
                    domChatNewDM.classList.add('intro-anim');
                    domChatNewDM.addEventListener('animationend', () => domChatNewDM.classList.remove('intro-anim'), { once: true });
                }
                if (domChatNewGroup) {
                    domChatNewGroup.classList.add('intro-anim');
                    domChatNewGroup.addEventListener('animationend', () => domChatNewGroup.classList.remove('intro-anim'), { once: true });
                }
            } else {
                // Fadeout the login and encryption UI with animation
                domLogin.classList.add('fadeout-anim');
                domLogin.addEventListener('animationend', async () => {
                    domLogin.classList.remove('fadeout-anim');
                    await showMainUI();

                    // Add fade-in animations
                    domChatBookmarksBtn.style.opacity = '0';
                    domNavbar.classList.add('fadein-anim');
                    domNavbar.addEventListener('animationend', () => {
                        domNavbar.classList.remove('fadein-anim');
                        domChatBookmarksBtn.style.opacity = '';
                        domChatBookmarksBtn.classList.add('fadein-anim');
                        domChatBookmarksBtn.addEventListener('animationend', () => domChatBookmarksBtn.classList.remove('fadein-anim'), { once: true });
                    }, { once: true });

                    domAccount.classList.add('fadein-anim');
                    domAccount.addEventListener('animationend', () => domAccount.classList.remove('fadein-anim'), { once: true });

                    domChatList.classList.add('intro-anim');
                    domChatList.addEventListener('animationend', () => domChatList.classList.remove('intro-anim'), { once: true });

                    if (domChatNewDM) {
                        domChatNewDM.classList.add('intro-anim');
                        domChatNewDM.addEventListener('animationend', () => domChatNewDM.classList.remove('intro-anim'), { once: true });
                    }
                    if (domChatNewGroup) {
                        domChatNewGroup.classList.add('intro-anim');
                        domChatNewGroup.addEventListener('animationend', () => domChatNewGroup.classList.remove('intro-anim'), { once: true });
                    }
                }, { once: true });
            }

            // Setup a subscription for new websocket messages (runs in both animation modes)
            invoke("notifs");

            // Apply badge-gated limits from the cached flag (a prior session's
            // result), so perks are live before this session's post-sync refresh.
            invoke("get_my_badges").then(b => {
                _myBadges = b;
                applyTierLimits(b?.tier | 0);
            }).catch(() => {});
            invoke('get_max_account_tier').then(t => { _maxAccountTier = t | 0; }).catch(() => {});

            // Setup our Unread Counters
            await invoke("update_unread_counter");

            // Monitor relay connections
            invoke("monitor_relay_connections");

            // Render the initial relay list
            renderRelayList();

            // Initialize the updater
            initializeUpdater();

            // Re-initialize encryption settings now that login is complete,
            // so the toggle reflects the actual backend state.
            initEncryptionSettings();

            // Execute any pending deep link action that was received before login
            try {
                const pendingAction = await invoke('get_pending_deep_link');
                if (pendingAction) {
                    console.log('Executing pending deep link action:', pendingAction);
                    await executeDeepLinkAction(pendingAction);
                }
            } catch (e) {
                console.error('Failed to check for pending deep link:', e);
            }

            // Handle a share (file/text from another app) that arrived on a cold
            // start before the live listener was attached.
            await consumePendingShare();
        });

        // Wait for connect + all listener registrations to complete
        await Promise.all([_connectP, _listenersP, _progressP, _initFinishedP]);
        console.timeEnd('[Boot] connect + listeners');

        // Load and Decrypt our database; fetching the full chat state from disk for immediate bootup
        domLoginEncryptTitle.textContent = `Decrypting Database...`;

        // Note: this also begins the Rust backend's iterative sync, thus, init should ONLY be called once, to initiate it
        init(true);
    }
}

/**
 * Display the Invite code input flow.
 */
function openInviteFlow() {
    domLoginStart.style.display = 'none';
    domLoginImport.style.display = 'none';
    domLoginInvite.style.display = '';
    
    // Focus on the invite input
    domInviteInput.focus();
    
    // Handle invite code submission
    domInviteBtn.onclick = async () => {
        const inviteCode = domInviteInput.value.trim();
        if (!inviteCode) {
            return popupConfirm('Please enter an invite code', '', true, '', 'vector_warning.svg');
        }
        
        try {
            // Accept the invite code
            await invoke('accept_invite_code', { inviteCode });
            
            // Hide invite screen and show welcome screen
            domLoginInvite.style.display = 'none';
            showWelcomeScreen();
        } catch (e) {
            // Display the specific error from the backend
            const errorMessage = e.toString() || 'Please check your invite code and try again.';
            popupConfirm('Invalid invite code', errorMessage, true, '', 'vector_warning.svg');
        }
    };
    
    // Handle enter key on invite input
    domInviteInput.onkeydown = async (evt) => {
        if (evt.code === 'Enter' || evt.code === 'NumpadEnter') {
            evt.preventDefault();
            domInviteBtn.click();
        }
    };
    
    // Handle enter key on invite input
    domInviteInput.onkeydown = async (evt) => {
        if (evt.code === 'Enter' || evt.code === 'NumpadEnter') {
            evt.preventDefault();
            domInviteBtn.click();
        }
    };
}

/**
 * Display the welcome screen after successful invite code acceptance
 */
function showWelcomeScreen() {
    // Hide the logo and subtext
    const domLogo = document.querySelector('.login-logo');
    const domSubtext = document.querySelector('.login-subtext');
    domLogo.style.display = 'none';
    domSubtext.style.display = 'none';

    // Show the welcome screen
    domLoginWelcome.style.display = '';

    // After 5 seconds, transition to the encryption flow
    setTimeout(() => {
        domLoginWelcome.style.display = 'none';
        // Restore the logo and subtext
        domLogo.style.display = '';
        domSubtext.style.display = '';
        openEncryptionFlow(false);
    }, 5000);
}

/**
 * Display the Encryption/Decryption flow.
 * @param {boolean} fUnlock - Whether we're unlocking an existing key, or encrypting a new one.
 * @param {string} securityType - "pin" or "password" (determines which UI to show)
 */
function openEncryptionFlow(fUnlock = false, securityType = 'pin') {
    domLoginStart.style.display = 'none';
    domLoginImport.style.display = 'none';
    domLoginInvite.style.display = 'none';
    domLoginEncrypt.style.display = '';
    // Hide the picker only for the NEW-account PIN-setup path (fUnlock=false).
    // The unlock path keeps it visible so the user can switch between
    // existing accounts before entering their PIN/password.
    if (!fUnlock) loginPicker.hide();

    // Hide all input variants initially
    domLoginEncryptPinRow.style.display = 'none';
    domLoginEncryptPassword.style.display = 'none';
    domLoginEncryptTypeSelect.style.display = 'none';

    // Track chosen security type
    let chosenSecurityType = securityType;

    // AbortControllers for listener cleanup (avoids cloning DOM — mobile WebViews
    // don't reliably handle cloned inputs)
    let pinAbortController = null;
    let passwordAbortController = null;

    // Android biometric fast-path. Auto-fires AT MOST once per unlock-screen
    // mount; a cancel lands the user on the PIN/password pad and only the
    // button re-triggers — never a render loop.
    const domBiometricBtn = document.getElementById('login-biometric-btn');
    let biometricBusy = false;
    // True once the unlock button was offered this mount; processing states
    // hide it and settled states bring it back (account switching included).
    let biometricOffered = false;
    // One login dispatch per screen, whoever gets there first. Deliberately a
    // plain latch and nothing more: the credential path must NEVER be gated on
    // biometric state, or a stuck flag leaves the user with a dead PIN pad.
    let loginDispatched = false;
    function setBiometricBtnVisible(visible) {
        if (domBiometricBtn && biometricOffered) {
            domBiometricBtn.style.display = visible ? '' : 'none';
        }
    }

    // If unlocking, go straight to the appropriate input
    if (fUnlock) {
        startCredentialEntry(chosenSecurityType);
    } else {
        // New account setup — show security type selection first
        showSecurityTypeSelector();
    }

    if (domBiometricBtn) domBiometricBtn.style.display = 'none';
    if (fUnlock && platformFeatures.os === 'android') {
        offerBiometricUnlock();
    }
    async function offerBiometricUnlock() {
        let status;
        try {
            status = await invoke('biometric_status');
        } catch (e) {
            return;
        }
        if (!status.enrolled) {
            // A biometric-only account with no enrollment means the hardware
            // key died (or the DB was restored to a new device): recovery.
            if (chosenSecurityType === 'biometric') showBiometricRecovery();
            return;
        }
        // The OS tells us its own words for what the sheet will ask for
        // ("Use fingerprint", "Use screen lock", ...) — Android never exposes
        // the credential type itself, but this is the honest next best.
        if (status.label) {
            const lbl = document.getElementById('login-biometric-label');
            if (lbl) lbl.textContent = status.label;
            if (chosenSecurityType === 'biometric') {
                domLoginEncryptTitle.textContent = status.label + ' to unlock Vector';
            }
        }
        if (domBiometricBtn) {
            biometricOffered = true;
            domBiometricBtn.style.display = '';
            domBiometricBtn.onclick = () => attemptBiometricUnlock();
        }
        // Auto-fire on mount: an enrolled account is biometric-ONLY (the modes
        // are mutually exclusive), so there is no credential pad for the prompt
        // to race. Once per mount — a cancel leaves the button for a manual
        // retry and never re-fires on its own.
        if (chosenSecurityType === 'biometric') attemptBiometricUnlock();
    }

    async function showBiometricRecovery() {
        domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
        domLoginEncryptTitle.textContent = 'Device security changed';
        if (domBiometricBtn) domBiometricBtn.style.display = 'none';
        const yes = await popupConfirm(
            'Device Security Changed',
            'This device\'s encrypted data can no longer be unlocked: its hardware key was invalidated (screen lock removed, or the data was moved to a new device).<br><br><b>Reset this device and sign in with your keys to restore from the network.</b>',
            false, '', 'vector_warning.svg'
        );
        if (yes) {
            try { await invoke('logout'); } catch (e) { console.error('[Biometric] recovery logout failed:', e); }
            window.location.reload();
        }
    }

    async function attemptBiometricUnlock() {
        if (biometricBusy || loginDispatched) return;
        biometricBusy = true;
        // The prompt sheet overlays the pad; once it passes, the backend emits
        // biometric_unlocked and the title flips to the processing state while
        // the real login (relays, sync) runs.
        let unlisten = null;
        try {
            unlisten = await window.__TAURI__.event.once('biometric_unlocked', () => {
                if (loginDispatched) return;
                domLoginEncryptTitle.textContent = 'Decrypting your keys...';
                domLoginEncryptTitle.classList.add('startup-subtext-gradient');
                setBiometricBtnVisible(false);
                if (typeof loginPicker !== 'undefined') loginPicker.hide();
            });
            const npub = await runWithTorBootstrapStatus(() => invoke('biometric_login'));
            // A typed credential already drove the login — don't start a second.
            if (loginDispatched) return;
            loginDispatched = true;
            strPubkey = npub;
            login();
        } catch (e) {
            const msg = String(e);
            domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
            if (msg.includes('BIOMETRIC_INVALIDATED')) {
                biometricOffered = false;
                if (domBiometricBtn) domBiometricBtn.style.display = 'none';
                if (chosenSecurityType === 'biometric') {
                    showBiometricRecovery();
                } else {
                    domLoginEncryptTitle.textContent = 'Biometrics changed. Enter your '
                        + (chosenSecurityType === 'password' ? 'password' : 'PIN') + '.';
                }
            } else if (msg.includes('BIOMETRIC_UNAVAILABLE')) {
                if (chosenSecurityType === 'biometric') {
                    domLoginEncryptTitle.textContent = 'Unlock unavailable right now. Restart Vector and try again.';
                }
            } else if (!msg.includes('BIOMETRIC_CANCELLED')
                && !msg.includes('BIOMETRIC_NOT_ENROLLED')) {
                console.error('[Biometric] unlock failed:', msg);
            }
            setBiometricBtnVisible(true);
        } finally {
            // Unconditional: delegate paths resolve without the event ever
            // firing, which would otherwise leak the listener for the webview
            // lifetime. Unlistening an already-fired once() is a no-op.
            if (unlisten) { try { unlisten(); } catch (_) {} }
            biometricBusy = false;
        }
    }

    /** Show the security type selection phase */
    function showSecurityTypeSelector() {
        // Hide lock icon header — the type selector uses the login logo above instead
        document.querySelector('.login-encrypt-header').style.display = 'none';
        domLoginEncryptPinRow.style.display = 'none';
        domLoginEncryptPassword.style.display = 'none';
        domLoginEncryptTypeSelect.style.display = '';

        const btnPin = document.getElementById('security-type-pin');
        const btnPassword = document.getElementById('security-type-password');
        const btnSkip = document.getElementById('security-type-skip');

        btnPin.onclick = () => {
            chosenSecurityType = 'pin';
            domLoginEncryptTypeSelect.style.display = 'none';
            startCredentialEntry('pin');
        };

        btnPassword.onclick = () => {
            chosenSecurityType = 'password';
            domLoginEncryptTypeSelect.style.display = 'none';
            startCredentialEntry('password');
        };

        // Biometric-only mode (Android 11+ with capable hardware): a generated
        // 256-bit credential nobody ever knows, unlocked solely by the OS.
        const btnBiometric = document.getElementById('security-type-biometric');
        if (btnBiometric && platformFeatures.os === 'android') {
            invoke('biometric_status')
                .then(s => {
                    if (!s.supported) return;
                    btnBiometric.style.display = '';
                    // The OS's own wording for what the user will actually see
                    // on the sheet ("Use fingerprint" / "Use screen lock").
                    if (s.label) btnBiometric.textContent = s.label;
                    // Two highlighted choices now — the label above them agrees.
                    const rec = document.querySelector('.security-type-recommended');
                    if (rec) rec.textContent = '*Recommended Options';
                })
                .catch(() => {});
            btnBiometric.onclick = async () => {
                if (!(await confirmBiometricOnlyWarning())) return;
                domLoginEncryptTypeSelect.style.display = 'none';
                document.querySelector('.login-encrypt-header').style.display = '';
                document.querySelector('.login-lock-icon').style.display = 'none';
                domLoginEncryptTitle.textContent = 'Setting up your account...';
                domLoginEncryptTitle.classList.add('startup-subtext-gradient');
                try {
                    await invoke('setup_encryption_biometric');
                    login();
                } catch (e) {
                    domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
                    const msg = String(e);
                    if (!msg.includes('BIOMETRIC_CANCELLED')) {
                        await popupConfirm('Could not enable biometrics', escapeHtml(msg), true);
                    }
                    document.querySelector('.login-encrypt-header').style.display = 'none';
                    domLoginEncryptTypeSelect.style.display = '';
                }
            };
        }

        btnSkip.onclick = async () => {
            // Skip encryption — backend stores the key in plaintext (key never crosses IPC)
            domLoginEncryptTypeSelect.style.display = 'none';
            document.querySelector('.login-encrypt-header').style.display = '';
            document.querySelector('.login-lock-icon').style.display = 'none';
            domLoginEncryptTitle.textContent = 'Setting up your account...';
            domLoginEncryptTitle.classList.add('startup-subtext-gradient');
            try {
                await invoke('skip_encryption');
                login();
            } catch (e) {
                // Backend rejected (disk full, DB locked by AV, migration
                // in flight, etc.) — PENDING_NSEC is preserved server-side
                // so a retry is possible. Surface the error and bring the
                // user back to the type-selector so they can try again.
                domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
                await popupConfirm('Could not finish setup', String(e), true);
                domLoginEncryptTypeSelect.style.display = '';
            }
        };
    }

    /** Start the credential entry phase for the chosen type */
    function startCredentialEntry(type) {
        // Re-show lock icon header (hidden during type selector phase)
        document.querySelector('.login-encrypt-header').style.display = '';
        if (type === 'biometric') {
            // Biometric-only account: no credential to type. The auto-fire
            // below owns the unlock; a dead enrollment routes to recovery.
            domLoginEncryptPinRow.style.display = 'none';
            domLoginEncryptPassword.style.display = 'none';
            domLoginEncryptTitle.textContent = 'Unlock with Biometrics';
        } else if (type === 'password') {
            startPasswordFlow();
        } else {
            startPinFlow();
        }
    }

    // ========================================================================
    // PIN Flow (existing 6-digit input logic)
    // ========================================================================
    function startPinFlow() {
        // Abort previous listeners if startPinFlow is called again
        if (pinAbortController) pinAbortController.abort();
        pinAbortController = new AbortController();
        const signal = pinAbortController.signal;

        let strPinLast = [];
        let strPinCurrent = Array(6).fill('-');

        const DECRYPTION_PROMPT = `Enter your Decryption Pin`;
        const INITIAL_ENCRYPTION_PROMPT = `Enter your Pin`;
        const RE_ENTER_PROMPT = `Re-enter your Pin`;
        const DECRYPTING_MSG = `Decrypting your keys...`;
        const ENCRYPTING_MSG = `Encrypting your keys...`;
        const INCORRECT_PIN_MSG = `Incorrect pin, try again`;
        const MISMATCH_PIN_MSG = `Pin doesn't match, re-try`;

        // Always query fresh from the live DOM
        const pinRow = document.getElementById('login-encrypt-pins');
        const arrPinDOMs = pinRow.querySelectorAll('input');

        function updateStatusMessage(message, isProcessing = false) {
            domLoginEncryptTitle.textContent = message;
            if (isProcessing) {
                domLoginEncryptTitle.classList.add('startup-subtext-gradient');
                pinRow.style.display = 'none';
                setBiometricBtnVisible(false);
                // Past the point of no return — backend is decrypting or
                // encrypting against THIS account. Mid-flight account swap
                // would race the in-progress crypto and bind the wrong
                // session to the result.
                if (typeof loginPicker !== 'undefined') loginPicker.hide();
            } else {
                domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
                pinRow.style.display = '';
                setBiometricBtnVisible(true);
                // Back to input state. On the unlock path, re-show the
                // picker so a wrong-PIN retry can swap accounts. On the
                // new-account setup path (fUnlock=false) the picker was
                // intentionally hidden by openEncryptionFlow and stays so.
                if (fUnlock && typeof loginPicker !== 'undefined'
                    && loginPicker.accounts && loginPicker.accounts.length >= 2) {
                    loginPicker.show(loginPicker.activeNpub);
                }
            }
            domLoginEncryptPassword.style.display = 'none';
        }

        function resetPinDisplay(focusFirst = true, revertTitleFromErrorState = true) {
            strPinCurrent = Array(6).fill('-');
            arrPinDOMs.forEach(input => input.value = '');
            if (revertTitleFromErrorState) {
                const currentTitle = domLoginEncryptTitle.textContent;
                if (currentTitle === INCORRECT_PIN_MSG || currentTitle === MISMATCH_PIN_MSG) {
                    const newTitle = fUnlock ? DECRYPTION_PROMPT : (strPinLast.length > 0 ? RE_ENTER_PROMPT : INITIAL_ENCRYPTION_PROMPT);
                    updateStatusMessage(newTitle);
                }
            }
            if (focusFirst && arrPinDOMs.length > 0) {
                arrPinDOMs[0].focus();
            }
        }

        let pinProcessing = false;

        async function handleFullPinEntered() {
            if (pinProcessing) return;
            pinProcessing = true;
            const currentPinString = strPinCurrent.join('');

            if (strPinLast.length === 0) {
                if (fUnlock) {
                    // For bunker accounts the decrypt is sub-second but the
                    // bunker bootstrap RPC takes most of the wait — surface
                    // that instead of leaving "Decrypting…" up the whole time.
                    const loadingMsg = window.__activeSignerType === 'bunker'
                        ? 'Connecting to Signer…'
                        : window.__activeSignerType === 'nip55'
                        ? 'Unlocking…'
                        : DECRYPTING_MSG;
                    updateStatusMessage(loadingMsg, true);
                    try {
                        // Decrypt and login entirely in backend (key never crosses IPC).
                        // The wrapper polls Tor's bootstrap state so the title flips
                        // to "Bootstrapping Tor…" while Arti is fetching consensus,
                        // instead of leaving "Decrypting…" up for 5-15s.
                        const npub = await runWithTorBootstrapStatus(() =>
                            invoke("login_from_stored_key", { password: currentPinString })
                        );
                        loginDispatched = true;
                        strPubkey = npub;
                        login();
                    } catch (e) {
                        // Distinguish bunker-unreachable from wrong-PIN: the
                        // PIN was already validated by internal_decrypt, so a
                        // post-decrypt failure (signer unreachable) shouldn't
                        // be presented as "Incorrect PIN".
                        const handled = typeof window.handleBunkerLoginError === 'function'
                            ? await window.handleBunkerLoginError(e)
                            : false;
                        if (handled) { pinProcessing = false; return; }
                        updateStatusMessage(INCORRECT_PIN_MSG);
                        resetPinDisplay(true, false);
                        pinProcessing = false;
                    }
                } else {
                    strPinLast = [...strPinCurrent];
                    updateStatusMessage(RE_ENTER_PROMPT);
                    resetPinDisplay(true, false);
                    pinProcessing = false;
                }
            } else {
                const isMatching = strPinLast.every((char, idx) => char === strPinCurrent[idx]);
                if (isMatching) {
                    updateStatusMessage(ENCRYPTING_MSG, true);
                    // Encrypt and store key entirely in backend (key never crosses IPC).
                    // Wrap in try/catch — `setup_encryption` can reject (disk
                    // full, DB locked, migration mid-flight). Backend preserves
                    // PENDING_NSEC on failure so a retry is possible.
                    try {
                        await invoke('setup_encryption', { password: strPinLast.join(''), securityType: chosenSecurityType });
                        login();
                    } catch (e) {
                        await popupConfirm('Could not save your PIN', String(e), true);
                        strPinLast = [];
                        resetPinDisplay(true, true);
                        pinProcessing = false;
                    }
                } else {
                    updateStatusMessage(MISMATCH_PIN_MSG);
                    strPinLast = [];
                    resetPinDisplay(true, true);
                    pinProcessing = false;
                }
            }
        }

        // Attach listeners directly to each original input with AbortController signal
        arrPinDOMs.forEach((input, nIndex) => {
            input.addEventListener('keydown', (event) => {
                if (event.key === 'Backspace') {
                    event.preventDefault();
                    const currentTitle = domLoginEncryptTitle.textContent;
                    if (currentTitle === INCORRECT_PIN_MSG || currentTitle === MISMATCH_PIN_MSG) {
                        const newTitle = fUnlock ? DECRYPTION_PROMPT : (strPinLast.length > 0 ? RE_ENTER_PROMPT : INITIAL_ENCRYPTION_PROMPT);
                        updateStatusMessage(newTitle);
                    }
                    if (input.value !== '') {
                        input.value = '';
                        strPinCurrent[nIndex] = '-';
                    } else if (nIndex > 0) {
                        const prev = arrPinDOMs[nIndex - 1];
                        prev.value = '';
                        strPinCurrent[nIndex - 1] = '-';
                        prev.focus();
                    }
                } else if (event.key === 'ArrowLeft') {
                    event.preventDefault();
                    if (nIndex > 0) arrPinDOMs[nIndex - 1].focus();
                } else if (event.key === 'ArrowRight') {
                    event.preventDefault();
                    if (nIndex + 1 < arrPinDOMs.length) arrPinDOMs[nIndex + 1].focus();
                } else if (event.key.length === 1 && !event.key.match(/^[0-9]$/)) {
                    event.preventDefault();
                }
            }, { signal });

            input.addEventListener('input', async () => {
                let sanitizedValue = input.value.replace(/[^0-9]/g, '');
                if (sanitizedValue.length > 1) sanitizedValue = sanitizedValue.charAt(0);
                input.value = sanitizedValue;

                if (sanitizedValue) {
                    strPinCurrent[nIndex] = sanitizedValue;
                    if (nIndex + 1 < arrPinDOMs.length) arrPinDOMs[nIndex + 1].focus();
                } else {
                    strPinCurrent[nIndex] = '-';
                }

                if (!strPinCurrent.includes('-')) {
                    await handleFullPinEntered();
                }
            }, { signal });

            input.value = '';
        });

        updateStatusMessage(fUnlock ? DECRYPTION_PROMPT : INITIAL_ENCRYPTION_PROMPT);
        if (arrPinDOMs.length > 0) arrPinDOMs[0].focus();
    }

    // ========================================================================
    // Password Flow (text input)
    // ========================================================================
    function startPasswordFlow() {
        let lastPassword = '';
        let passwordProcessing = false;

        const DECRYPTION_PROMPT = `Enter your Password`;
        const INITIAL_ENCRYPTION_PROMPT = `Choose a Password`;
        const RE_ENTER_PROMPT = `Re-enter your Password`;
        const DECRYPTING_MSG = `Decrypting your keys...`;
        const ENCRYPTING_MSG = `Encrypting your keys...`;
        const INCORRECT_MSG = `Incorrect password, try again`;
        const MISMATCH_MSG = `Passwords don't match, re-try`;
        const TOO_SHORT_MSG = `Password must be at least 4 characters`;

        function updateStatusMessage(message, isProcessing = false) {
            domLoginEncryptTitle.textContent = message;
            if (isProcessing) {
                domLoginEncryptTitle.classList.add('startup-subtext-gradient');
                domLoginEncryptPassword.style.display = 'none';
                setBiometricBtnVisible(false);
                // Past the point of no return — see PIN flow.
                if (typeof loginPicker !== 'undefined') loginPicker.hide();
            } else {
                domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
                domLoginEncryptPassword.style.display = '';
                setBiometricBtnVisible(true);
                // See PIN flow for the rationale.
                if (fUnlock && typeof loginPicker !== 'undefined'
                    && loginPicker.accounts && loginPicker.accounts.length >= 2) {
                    loginPicker.show(loginPicker.activeNpub);
                }
            }
            domLoginEncryptPinRow.style.display = 'none';
        }

        updateStatusMessage(fUnlock ? DECRYPTION_PROMPT : INITIAL_ENCRYPTION_PROMPT);

        // Abort previous password listeners if startPasswordFlow is called again
        if (passwordAbortController) passwordAbortController.abort();
        passwordAbortController = new AbortController();
        const signal = passwordAbortController.signal;

        const newInput = document.getElementById('login-password-input');
        newInput.value = '';
        newInput.focus();

        // Login button
        const loginBtn = document.getElementById('login-password-btn');

        async function handlePasswordSubmit() {
            if (passwordProcessing) return;

            const password = newInput.value;

            if (fUnlock) {
                // Unlock flow — single password entry
                if (!password) return;
                passwordProcessing = true;
                // Bunker accounts spend the bulk of the wait on the signer
                // RPC, not decryption — show the more accurate message.
                const loadingMsg = window.__activeSignerType === 'bunker'
                    ? 'Connecting to Signer…'
                    : window.__activeSignerType === 'nip55'
                    ? 'Unlocking…'
                    : DECRYPTING_MSG;
                updateStatusMessage(loadingMsg, true);
                try {
                    // Decrypt and login entirely in backend (key never crosses IPC).
                    // Wrapper flips the title to "Bootstrapping Tor…" if Arti is
                    // mid-bootstrap during the call.
                    const npub = await runWithTorBootstrapStatus(() =>
                        invoke("login_from_stored_key", { password })
                    );
                    loginDispatched = true;
                    strPubkey = npub;
                    login();
                } catch (e) {
                    // Bunker-unreachable case: pass through to re-auth flow
                    // instead of telling the user their password is wrong.
                    const handled = typeof window.handleBunkerLoginError === 'function'
                        ? await window.handleBunkerLoginError(e)
                        : false;
                    if (handled) { passwordProcessing = false; return; }
                    updateStatusMessage(INCORRECT_MSG);
                    newInput.value = '';
                    newInput.focus();
                    passwordProcessing = false;
                }
            } else if (!lastPassword) {
                // First entry — set password
                if (password.length < 4) {
                    updateStatusMessage(TOO_SHORT_MSG);
                    return;
                }
                lastPassword = password;
                updateStatusMessage(RE_ENTER_PROMPT);
                newInput.value = '';
                newInput.focus();
            } else {
                // Confirmation entry
                if (password === lastPassword) {
                    passwordProcessing = true;
                    updateStatusMessage(ENCRYPTING_MSG, true);
                    // Encrypt and store key entirely in backend (key never crosses IPC).
                    // Wrap in try/catch — `setup_encryption` can reject (disk
                    // full, DB locked, migration mid-flight). Backend preserves
                    // PENDING_NSEC on failure so a retry is possible.
                    try {
                        await invoke('setup_encryption', { password: lastPassword, securityType: chosenSecurityType });
                        login();
                    } catch (e) {
                        await popupConfirm('Could not save your password', String(e), true);
                        lastPassword = '';
                        newInput.value = '';
                        newInput.focus();
                        passwordProcessing = false;
                    }
                } else {
                    updateStatusMessage(MISMATCH_MSG);
                    lastPassword = '';
                    newInput.value = '';
                    newInput.focus();
                }
            }
        }

        newInput.addEventListener('keydown', (event) => {
            if (event.key === 'Enter') {
                event.preventDefault();
                handlePasswordSubmit();
            }
        }, { signal });

        if (loginBtn) loginBtn.addEventListener('click', handlePasswordSubmit, { signal });

        newInput.focus();
    }
}

/** Wire the login screens: account creation, import, bunker and NIP-55 signers, back. */
async function wireLoginUi() {
    domLoginAccountCreationBtn.onclick = async () => {
        try {
            // Add Profile commit point: tear down the existing session
            // before generating a new keypair, otherwise create_account's
            // lock-and-check guard would silently reuse the old client.
            if (addAccountFlow.active) await addAccountFlow.commit();

            const { public: pubKey } = await invoke("create_account");
            strPubkey = pubKey;

            // Connect to Nostr network
            await invoke("connect");

            // Skip invite flow - go directly to encryption (key stays backend-only)
            openEncryptionFlow(false);
        } catch (e) {
            // Display the backend error
            popupConfirm(e, '', true, '', 'vector_warning.svg');
        }
    };
    domLoginAccountBtn.onclick = () => {
        domLoginImport.style.display = '';
        domLoginStart.style.display = 'none';
        domLoginBackBar.style.display = '';
        document.getElementById('login-form').classList.add('has-back-bar');
        // Hide the picker pill — once the user is entering an nsec / seed
        // phrase, the active-account-from-marker context no longer applies.
        loginPicker.hide();
    };
    // Bunker form helpers (startBunkerSession, showBunkerForm,
    // hideBunkerForm) are now defined at module scope, near the DOM-ref
    // block — they need to be accessible to the boot-time login catch which
    // runs before this DOMContentLoaded handler reaches button wiring.
    // hideBunkerForm hoisted to module scope; window.hideBunkerForm assigned there.
    if (domLoginBunkerStartBtn) {
        // Lives inside the Login screen (not the entry screen). Bunker is a
        // login flow — your signer *is* the identity, so there's no "create"
        // path. Surfacing it as a secondary action under the nsec/seed input
        // keeps the entry screen clean for the 98% of users who don't run a
        // remote signer.
        domLoginBunkerStartBtn.onclick = showBunkerForm;
    }

    // NIP-55 offline signer (Amber): Android-only, and only when a signer app
    // is actually installed — otherwise the button is a dead end. The reveal
    // is async so the entry screen never flickers a button it can't honour.
    if (domLoginNip55StartBtn && platformFeatures.os === 'android') {
        invoke('is_external_signer_installed').then((installed) => {
            if (installed) domLoginNip55StartBtn.style.display = '';
        }).catch(() => { /* leave hidden */ });

        domLoginNip55StartBtn.onclick = async () => {
            domLoginNip55StartBtn.disabled = true;
            try {
                if (addAccountFlow.active) await addAccountFlow.commit();
                // Blocks while Amber is foregrounded and the user approves; the
                // Activity-result bridge resolves this once they return.
                const { public: pubKey, existing } = await invoke('login_with_nip55');
                strPubkey = pubKey;
                if (existing) {
                    // Identity already on disk; backend armed session_reload.
                    return;
                }
                // Reuse the shared post-login flow: pick a security mode, then
                // connect in the background so a relay hang doesn't strand us.
                openEncryptionFlow(false);
                invoke('connect').catch((err) => {
                    console.warn('[login_with_nip55] connect() failed:', err);
                });
            } catch (e) {
                popupConfirm(String(e), '', true, '', 'vector_warning.svg');
            } finally {
                domLoginNip55StartBtn.disabled = false;
            }
        };
    }
    if (domLoginBunkerCopyBtn) {
        domLoginBunkerCopyBtn.onclick = async () => {
            if (!strBunkerNostrConnectUrl) return;
            try {
                await navigator.clipboard.writeText(strBunkerNostrConnectUrl);
                domLoginBunkerCopyBtn.classList.add('copied');
                domLoginBunkerCopyBtn.textContent = 'Copied — paste in your signer';
                setTimeout(() => {
                    domLoginBunkerCopyBtn.classList.remove('copied');
                    domLoginBunkerCopyBtn.textContent = 'Copy connection link';
                }, 2500);
            } catch (err) {
                if (domLoginBunkerStatus) {
                    domLoginBunkerStatus.textContent = 'Could not copy to clipboard';
                    domLoginBunkerStatus.className = 'login-bunker-status error';
                }
            }
        };
    }
    // Tap the bunker QR to blow it up fullscreen — easier for a phone camera.
    // openQrOverlay no-ops while the connection link is still generating.
    if (domLoginBunkerQrWrap) {
        domLoginBunkerQrWrap.onclick = () => openQrOverlay(strBunkerNostrConnectUrl);
    }
    domLoginBackBtn.onclick = async () => {
        // Add Profile flow back has two cases — independent of which sub-
        // screen the user happens to be on (start / import / encryption /
        // welcome). Without this, backing out from the encryption screen
        // after Create Account left both `domLoginStart` and
        // `domLoginEncrypt` visible at the same time.
        //
        //   - Browsing (not committed): the original session is still alive
        //     in memory. Soft-restore the main UI; no backend touch, no
        //     reload, the user keeps their decrypted keys + listeners.
        //
        //   - Committed: enter_add_account_mode already tore the session
        //     down. We have to write the previous-account marker back and
        //     reload so the next boot lands on the original account.
        if (addAccountFlow.active) {
            if (!addAccountFlow.committed) {
                addAccountFlow.restore();
                return;
            }
            const target = addAccountFlow.backTarget();
            try {
                if (target) {
                    await invoke('set_active_account', { npub: target });
                }
            } catch (e) {
                console.error('[add-account] restore marker failed:', e);
                popupConfirm('Could not return to your account', String(e), true);
                return;
            }
            addAccountFlow.finish();
            window.location.reload();
            return;
        }
        // If the bunker form was visible, the user is bailing out of a
        // staged-but-not-committed session — drain it on the backend so the
        // next attempt doesn't see a leaked NOSTR_CLIENT. No-op when no
        // staged session exists.
        const wasOnBunkerForm = domLoginBunker
            && !domLoginBunker.classList.contains('is-hidden')
            && domLoginBunker.style.display !== 'none';
        if (wasOnBunkerForm) {
            invoke('cancel_bunker_session').catch((err) => {
                console.warn('[back] cancel_bunker_session failed:', err);
            });
        }
        // Reauth path: we're inside an active session, came from Settings /
        // Chats. Restore the panel the user was on; don't fall through to
        // the login-start picker.
        if (wasOnBunkerForm && bunkerReauthOrigin) {
            const origin = bunkerReauthOrigin;
            hideBunkerForm();
            if (domLoginBackBar) domLoginBackBar.style.display = 'none';
            const loginForm = document.getElementById('login-form');
            if (loginForm) loginForm.classList.remove('has-back-bar');
            if (domLogin) domLogin.style.display = 'none';
            bunkerReauthOrigin = null;
            if (origin === 'settings' && typeof openSettings === 'function') {
                openSettings();
            } else if (typeof closeChat === 'function') {
                closeChat();
            }
            return;
        }
        // Regular login back: collapse every sub-screen back to the start
        // picker. Encrypt + welcome were missing here, which is what made
        // the post-commit Add Profile case render two panels at once.
        domLoginImport.style.display = 'none';
        domLoginInvite.style.display = 'none';
        domLoginEncrypt.style.display = 'none';
        domLoginWelcome.style.display = 'none';
        hideBunkerForm();
        domLoginBackBar.style.display = 'none';
        domLoginStart.style.display = '';
        domLoginInput.value = '';
        document.getElementById('login-form').classList.remove('has-back-bar');
        // Re-reveal the picker pill if we have ≥2 accounts on disk. The
        // Login button's onclick hides the picker (the user is about to
        // import a key, so it'd be confusing to show), and without this
        // restore the picker stays hidden after the user backs out —
        // effectively removing their ability to switch accounts from the
        // start screen without restarting the app.
        if (typeof loginPicker !== 'undefined'
            && loginPicker.accounts && loginPicker.accounts.length >= 2) {
            loginPicker.show(loginPicker.activeNpub);
        }
    };
    domLoginBtn.onclick = async () => {
        // Import and derive our keys
        try {
            // Add Profile commit point: tear down the existing session
            // before importing the new key.
            if (addAccountFlow.active) await addAccountFlow.commit();

            const { public: pubKey, existing } = await invoke("login", { importKey: domLoginInput.value.trim() });
            strPubkey = pubKey;

            // Pasted key matches an account already on disk; the backend has
            // armed `session_reload` to swap into it. Skip the encryption-
            // setup flow — the boot path will load the stored credentials.
            if (existing) return;

            // Connect to Nostr
            await invoke("connect");

            // Skip invite flow - go directly to encryption (key stays backend-only)
            openEncryptionFlow(false);
        } catch (e) {
            // Display the backend error
            popupConfirm(e, '', true, '', 'vector_warning.svg');
        }
    }
    if (domLoginBunkerConnectBtn) {
        domLoginBunkerConnectBtn.onclick = async () => {
            const url = (domLoginBunkerUrlInput?.value || '').trim();
            if (!url.toLowerCase().startsWith('bunker://')) {
                domLoginBunkerStatus.textContent = 'Must start with bunker://';
                domLoginBunkerStatus.className = 'login-bunker-status error';
                return;
            }
            // Disable inputs while the bunker handshake runs (5–10s typical
            // while the user taps "approve" on their signer). Re-enable on
            // failure so they can retry without leaving the screen.
            const _disable = (v) => {
                domLoginBunkerConnectBtn.disabled = v;
                domLoginBunkerUrlInput.disabled = v;
                domLoginBunkerStartBtn && (domLoginBunkerStartBtn.disabled = v);
                if (domLoginBunkerCopyBtn) domLoginBunkerCopyBtn.disabled = v;
            };
            _disable(true);
            domLoginBunkerStatus.textContent = 'Connecting to signer…';
            domLoginBunkerStatus.className = 'login-bunker-status connecting';
            try {
                if (addAccountFlow.active) await addAccountFlow.commit();
                const { public: pubKey, existing } = await invoke('connect_bunker', {
                    bunkerUrl: url,
                });
                strPubkey = pubKey;
                domLoginBunkerUrlInput.value = '';
                if (existing) {
                    // Bunker identity matches an existing account; backend has
                    // armed `session_reload`. Just hide the form — the document
                    // reload will switch into the stored account.
                    domLoginBunkerStatus.textContent = 'Account already added — switching…';
                    domLoginBunkerStatus.className = 'login-bunker-status online';
                    hideBunkerForm();
                    return;
                }
                domLoginBunkerStatus.textContent = 'Connected. Choosing security…';
                domLoginBunkerStatus.className = 'login-bunker-status online';
                // UI advances first; relay connect runs in the background so a
                // hang there doesn't strand the user on the bunker screen.
                hideBunkerForm();
                openEncryptionFlow(false);
                invoke('connect').catch((err) => {
                    console.warn('[connect_bunker] connect() failed:', err);
                });
            } catch (e) {
                domLoginBunkerStatus.textContent = String(e);
                domLoginBunkerStatus.className = 'login-bunker-status error';
                _disable(false);
            }
        };
    }
}
