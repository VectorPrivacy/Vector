// Authentication: the login screens, the remote signer (bunker) session, the
// invite and welcome steps, and the encryption (PIN / password) flow. One global
// scope: this loads before main.js and shares its globals.

const domLogin = document.getElementById('login-form');

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
    VectorSvelte.bunkerDeadline(0);
}

/** One tick of the live countdown; at expiry the link is single-use, so a fresh one rolls. */
function tickBunkerSession() {
    if (bunkerSessionDeadline - Date.now() > 0) {
        VectorSvelte.bunkerTick(Date.now());
        return;
    }
    stopBunkerSessionTimer();
    VectorSvelte.bunkerStatus('Refreshing connection link…', 'connecting');
    startBunkerSession();
}

function armBunkerSessionTimer() {
    stopBunkerSessionTimer();
    bunkerSessionDeadline = Date.now() + BUNKER_SESSION_TIMEOUT_MS;
    VectorSvelte.bunkerDeadline(bunkerSessionDeadline);
    bunkerSessionTimerHandle = setInterval(tickBunkerSession, 1000);
}

/**
 * Kick off a NIP-46 client-initiated session — either fresh
 * (`start_nostrconnect_session`) or re-pairing an existing committed account
 * (`reauthorize_bunker`). Backend returns a `nostrconnect://` URL that we
 * render as a QR + Copy button.
 */
async function startBunkerSession() {
    strBunkerNostrConnectUrl = '';
    VectorSvelte.bunkerLink('', false);
    VectorSvelte.bunkerCopied(false);
    VectorSvelte.bunkerStatus('Waiting for signer…', 'connecting');
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
                        VectorSvelte.loginScreen('none', false);
                        VectorSvelte.loginShowForm(false);
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
        VectorSvelte.bunkerLink(url);
        // Start the live countdown — auto-rerolls a fresh QR when the
        // 120s backend timeout expires so the user isn't stranded.
        armBunkerSessionTimer();
    } catch (e) {
        stopBunkerSessionTimer();
        VectorSvelte.bunkerStatus(String(e), 'error');
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
        const settingsVisible = domSettings.style.display !== 'none';
        bunkerReauthOrigin = settingsVisible ? 'settings' : 'chats';
        domNavbar.style.display = 'none';
        domSettings.style.display = 'none';
        domChats.style.display = 'none';
        domProfile.style.display = 'none';
        domInvites.style.display = 'none';
        domGroupOverview.style.display = 'none';
    } else {
        bunkerReauthOrigin = null;
    }
    // The overlay replaces every screen and shows the form + back bar, so the reauth
    // path can enter from anywhere in the main app.
    VectorSvelte.loginShowBunker(mode);
    // Fresh URL per open — single-use, can't be cached.
    startBunkerSession();
}
window.showBunkerForm = showBunkerForm;

/** Hide the bunker form and clear its in-memory state. */
function hideBunkerForm() {
    stopBunkerSessionTimer();
    VectorSvelte.loginHideBunker();
    strBunkerNostrConnectUrl = '';
}
window.hideBunkerForm = hideBunkerForm;

/**
 * Login to the Nostr network
 * @param {boolean} skipAnimations - Skip intro animations (for instant login without PIN)
 */
async function login(skipAnimations = false) {
    if (strPubkey) {
        if (addAccountFlow.committed) {
            // A new account boots the way a switch does: marker, then reload. Booting it
            // in-process leaves the previous account's open chat, pane and window state on
            // screen, since only the chat arrays are replaced by init_finished.
            addAccountFlow.finish();
            try { await invoke('set_active_account', { npub: strPubkey }); }
            catch (e) { console.error('[add-account] marker write failed:', e); }
            window.location.reload();
            return;
        }
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
                    VectorSvelte.patchEncrypt({ title: message, typing: true, error: false });
                    break;
                case 'progress':
                    VectorSvelte.patchEncrypt({ title: current && total ? `${message} (${Math.round((current / total) * 100)}%)` : message });
                    break;
                case 'complete':
                    VectorSvelte.patchEncrypt({ title: message, typing: false });
                    break;
                case 'error':
                    VectorSvelte.patchEncrypt({ title: message, typing: false, error: true });
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
                VectorSvelte.patchLogin({ importKey: '' });
                VectorSvelte.loginHide();

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
                domGroupOverview.style.display = 'none';
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
        VectorSvelte.patchEncrypt({ title: 'Decrypting Database...' });

        // Note: this also begins the Rust backend's iterative sync, thus, init should ONLY be called once, to initiate it
        init(true);
    }
}

/**
 * Display the Invite code input flow.
 */
function openInviteFlow() {
    VectorSvelte.patchLogin({ inviteCode: '' });
    VectorSvelte.loginScreen('invite');
}

async function submitInvite() {
    const inviteCode = VectorSvelte.loginState().inviteCode.trim();
    if (!inviteCode) {
        return popupConfirm('Please enter an invite code', '', true, '', 'vector_warning.svg');
    }
    try {
        await invoke('accept_invite_code', { inviteCode });
        showWelcomeScreen();
    } catch (e) {
        const errorMessage = e.toString() || 'Please check your invite code and try again.';
        popupConfirm('Invalid invite code', errorMessage, true, '', 'vector_warning.svg');
    }
}

/**
 * Display the welcome screen after successful invite code acceptance
 */
function showWelcomeScreen() {
    // The chrome hides the logo and tagline while the welcome screen is up.
    VectorSvelte.loginScreen('welcome');
    // After 5 seconds, transition to the encryption flow
    setTimeout(() => openEncryptionFlow(false), 5000);
}

/** The open encrypt screen's handlers; the component's controls route here. */
let encryptFlow = null;

/**
 * Display the Encryption/Decryption flow.
 * @param {boolean} fUnlock - Whether we're unlocking an existing key, or encrypting a new one.
 * @param {string} securityType - "pin" or "password" (determines which UI to show)
 */
function openEncryptionFlow(fUnlock = false, securityType = 'pin') {
    VectorSvelte.loginScreen('encrypt');
    // Hide the picker only for the NEW-account PIN-setup path (fUnlock=false).
    // The unlock path keeps it visible so the user can switch between
    // existing accounts before entering their PIN/password.
    if (!fUnlock) loginPicker.hide();
    VectorSvelte.patchEncrypt({
        gradient: false, typing: false, error: false, headerShown: true, lockShown: true,
        typeSelectShown: false, pinShown: false, passwordShown: false, password: '',
        bioOptionShown: false, bioOptionLabel: 'Use Biometrics', recommended: '*Recommended Option',
        bioBtnShown: false, bioBtnLabel: 'Unlock with Biometrics',
    });

    // Track chosen security type
    let chosenSecurityType = securityType;
    const setTitle = (title, patch = {}) => VectorSvelte.patchEncrypt({ title, ...patch });
    const currentTitle = () => VectorSvelte.encryptState().title;

    // Android biometric fast-path. Auto-fires AT MOST once per unlock-screen
    // mount; a cancel lands the user on the PIN/password pad and only the
    // button re-triggers — never a render loop.
    let biometricBusy = false;
    // True once the unlock button was offered this mount; processing states
    // hide it and settled states bring it back (account switching included).
    let biometricOffered = false;
    // One login dispatch per screen, whoever gets there first. Deliberately a
    // plain latch and nothing more: the credential path must NEVER be gated on
    // biometric state, or a stuck flag leaves the user with a dead PIN pad.
    let loginDispatched = false;
    function setBiometricBtnVisible(visible) {
        if (biometricOffered) VectorSvelte.patchEncrypt({ bioBtnShown: visible });
    }

    // A later open replaces this object, so a stale screen's controls go nowhere.
    const flow = {
        choose: () => {}, pinFull: () => {}, pinBackspace: () => {}, submitPassword: () => {},
        biometric: () => { if (biometricOffered) attemptBiometricUnlock(); },
    };
    encryptFlow = flow;

    // If unlocking, go straight to the appropriate input
    if (fUnlock) {
        startCredentialEntry(chosenSecurityType);
    } else {
        // New account setup — show security type selection first
        showSecurityTypeSelector();
    }

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
            VectorSvelte.patchEncrypt({ bioBtnLabel: status.label });
            if (chosenSecurityType === 'biometric') setTitle(status.label + ' to unlock Vector');
        }
        biometricOffered = true;
        VectorSvelte.patchEncrypt({ bioBtnShown: true });
        // Auto-fire on mount: an enrolled account is biometric-ONLY (the modes
        // are mutually exclusive), so there is no credential pad for the prompt
        // to race. Once per mount — a cancel leaves the button for a manual
        // retry and never re-fires on its own.
        if (chosenSecurityType === 'biometric') attemptBiometricUnlock();
    }

    async function showBiometricRecovery() {
        setTitle('Device security changed', { gradient: false, bioBtnShown: false });
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
                setTitle('Decrypting your keys...', { gradient: true });
                setBiometricBtnVisible(false);
                loginPicker.hide();
            });
            const npub = await runWithTorBootstrapStatus(() => invoke('biometric_login'));
            // A typed credential already drove the login — don't start a second.
            if (loginDispatched) return;
            loginDispatched = true;
            strPubkey = npub;
            login();
        } catch (e) {
            const msg = String(e);
            VectorSvelte.patchEncrypt({ gradient: false });
            if (msg.includes('BIOMETRIC_INVALIDATED')) {
                biometricOffered = false;
                VectorSvelte.patchEncrypt({ bioBtnShown: false });
                if (chosenSecurityType === 'biometric') {
                    showBiometricRecovery();
                } else {
                    setTitle('Biometrics changed. Enter your '
                        + (chosenSecurityType === 'password' ? 'password' : 'PIN') + '.');
                }
            } else if (msg.includes('BIOMETRIC_UNAVAILABLE')) {
                if (chosenSecurityType === 'biometric') {
                    setTitle('Unlock unavailable right now. Restart Vector and try again.');
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
        // The type selector uses the login logo above instead of the lock header.
        VectorSvelte.patchEncrypt({ headerShown: false, pinShown: false, passwordShown: false, typeSelectShown: true });

        // Biometric-only mode (Android 11+ with capable hardware): a generated
        // 256-bit credential nobody ever knows, unlocked solely by the OS.
        if (platformFeatures.os === 'android') {
            invoke('biometric_status')
                .then(s => {
                    if (!s.supported) return;
                    // The OS's own wording for what the user will actually see on the
                    // sheet; two highlighted choices now, so the label above them agrees.
                    VectorSvelte.patchEncrypt({
                        bioOptionShown: true, bioOptionLabel: s.label || 'Use Biometrics',
                        recommended: '*Recommended Options',
                    });
                })
                .catch(() => {});
        }

        flow.choose = async (type) => {
            if (type === 'pin' || type === 'password') {
                chosenSecurityType = type;
                VectorSvelte.patchEncrypt({ typeSelectShown: false });
                startCredentialEntry(type);
                return;
            }
            if (type === 'biometric') {
                if (!(await confirmBiometricOnlyWarning())) return;
                setTitle('Setting up your account...', { typeSelectShown: false, headerShown: true, lockShown: false, gradient: true });
                try {
                    await invoke('setup_encryption_biometric');
                    login();
                } catch (e) {
                    VectorSvelte.patchEncrypt({ gradient: false });
                    const msg = String(e);
                    if (!msg.includes('BIOMETRIC_CANCELLED')) {
                        await popupConfirm('Could not enable biometrics', escapeHtml(msg), true);
                    }
                    VectorSvelte.patchEncrypt({ headerShown: false, typeSelectShown: true });
                }
                return;
            }
            // Skip encryption — backend stores the key in plaintext (key never crosses IPC)
            setTitle('Setting up your account...', { typeSelectShown: false, headerShown: true, lockShown: false, gradient: true });
            try {
                await invoke('skip_encryption');
                login();
            } catch (e) {
                // Backend rejected (disk full, DB locked by AV, migration
                // in flight, etc.) — PENDING_NSEC is preserved server-side
                // so a retry is possible. Surface the error and bring the
                // user back to the type-selector so they can try again.
                VectorSvelte.patchEncrypt({ gradient: false });
                await popupConfirm('Could not finish setup', String(e), true);
                VectorSvelte.patchEncrypt({ typeSelectShown: true });
            }
        };
    }

    /** Start the credential entry phase for the chosen type */
    function startCredentialEntry(type) {
        // Re-show lock icon header (hidden during type selector phase)
        VectorSvelte.patchEncrypt({ headerShown: true });
        if (type === 'biometric') {
            // Biometric-only account: no credential to type. The auto-fire
            // below owns the unlock; a dead enrollment routes to recovery.
            setTitle('Unlock with Biometrics', { pinShown: false, passwordShown: false });
        } else if (type === 'password') {
            startPasswordFlow();
        } else {
            startPinFlow();
        }
    }

    // ========================================================================
    // PIN Flow (6-digit input; the boxes live in the component)
    // ========================================================================
    function startPinFlow() {
        let strPinLast = [];

        const DECRYPTION_PROMPT = `Enter your Decryption Pin`;
        const INITIAL_ENCRYPTION_PROMPT = `Enter your Pin`;
        const RE_ENTER_PROMPT = `Re-enter your Pin`;
        const DECRYPTING_MSG = `Decrypting your keys...`;
        const ENCRYPTING_MSG = `Encrypting your keys...`;
        const INCORRECT_PIN_MSG = `Incorrect pin, try again`;
        const MISMATCH_PIN_MSG = `Pin doesn't match, re-try`;

        function updateStatusMessage(message, isProcessing = false) {
            if (isProcessing) {
                setTitle(message, { gradient: true, pinShown: false });
                setBiometricBtnVisible(false);
                // Past the point of no return — backend is decrypting or
                // encrypting against THIS account. Mid-flight account swap
                // would race the in-progress crypto and bind the wrong
                // session to the result.
                loginPicker.hide();
            } else {
                setTitle(message, { gradient: false, pinShown: true });
                setBiometricBtnVisible(true);
                // Back to input state. On the unlock path, re-show the
                // picker so a wrong-PIN retry can swap accounts. On the
                // new-account setup path (fUnlock=false) the picker was
                // intentionally hidden by openEncryptionFlow and stays so.
                if (fUnlock && loginPicker.accounts && loginPicker.accounts.length >= 2) {
                    loginPicker.show(loginPicker.activeNpub);
                }
            }
            VectorSvelte.patchEncrypt({ passwordShown: false });
        }

        function revertErrorTitle() {
            const title = currentTitle();
            if (title === INCORRECT_PIN_MSG || title === MISMATCH_PIN_MSG) {
                updateStatusMessage(fUnlock ? DECRYPTION_PROMPT : (strPinLast.length > 0 ? RE_ENTER_PROMPT : INITIAL_ENCRYPTION_PROMPT));
            }
        }

        function resetPinDisplay(focusFirst = true, revertTitleFromErrorState = true) {
            if (revertTitleFromErrorState) revertErrorTitle();
            VectorSvelte.resetLoginPin(focusFirst);
        }

        let pinProcessing = false;

        async function handleFullPinEntered(currentPinString) {
            if (pinProcessing) return;
            pinProcessing = true;
            const strPinCurrent = currentPinString.split('');

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

        flow.pinFull = handleFullPinEntered;
        flow.pinBackspace = revertErrorTitle;
        flow.submitPassword = () => {};

        updateStatusMessage(fUnlock ? DECRYPTION_PROMPT : INITIAL_ENCRYPTION_PROMPT);
        VectorSvelte.resetLoginPin(true);
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
            if (isProcessing) {
                setTitle(message, { gradient: true, passwordShown: false });
                setBiometricBtnVisible(false);
                // Past the point of no return — see PIN flow.
                loginPicker.hide();
            } else {
                setTitle(message, { gradient: false, passwordShown: true });
                setBiometricBtnVisible(true);
                // See PIN flow for the rationale.
                if (fUnlock && loginPicker.accounts && loginPicker.accounts.length >= 2) {
                    loginPicker.show(loginPicker.activeNpub);
                }
            }
            VectorSvelte.patchEncrypt({ pinShown: false });
        }

        function clearAndFocus() {
            VectorSvelte.patchEncrypt({ password: '' });
            VectorSvelte.focusLoginInput();
        }

        updateStatusMessage(fUnlock ? DECRYPTION_PROMPT : INITIAL_ENCRYPTION_PROMPT);
        clearAndFocus();

        async function handlePasswordSubmit() {
            if (passwordProcessing) return;

            const password = VectorSvelte.encryptState().password;

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
                    clearAndFocus();
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
                clearAndFocus();
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
                        clearAndFocus();
                        passwordProcessing = false;
                    }
                } else {
                    updateStatusMessage(MISMATCH_MSG);
                    lastPassword = '';
                    clearAndFocus();
                }
            }
        }

        flow.submitPassword = handlePasswordSubmit;
        flow.pinFull = () => {};
        flow.pinBackspace = () => {};
    }
}

// ============================================================================
// Login screen actions (the component's controls land here through LOGIN_HELPERS)
// ============================================================================

async function createAccount() {
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
}

function openImportScreen() {
    VectorSvelte.loginScreen('import', true);
    // Hide the picker pill — once the user is entering an nsec / seed
    // phrase, the active-account-from-marker context no longer applies.
    loginPicker.hide();
}

async function importKey() {
    // Import and derive our keys
    try {
        // Add Profile commit point: tear down the existing session
        // before importing the new key.
        if (addAccountFlow.active) await addAccountFlow.commit();

        const { public: pubKey, existing } = await invoke("login", { importKey: VectorSvelte.loginState().importKey.trim() });
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

/** NIP-55 offline signer (Amber): the button only shows once the signer app is known to be installed. */
async function loginWithNip55() {
    VectorSvelte.patchLogin({ nip55Busy: true });
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
        VectorSvelte.patchLogin({ nip55Busy: false });
    }
}

async function copyBunkerLink() {
    if (!strBunkerNostrConnectUrl) return;
    try {
        await navigator.clipboard.writeText(strBunkerNostrConnectUrl);
        VectorSvelte.bunkerCopied(true);
        setTimeout(() => VectorSvelte.bunkerCopied(false), 2500);
    } catch (err) {
        VectorSvelte.bunkerStatus('Could not copy to clipboard', 'error');
    }
}

async function connectBunkerUrl() {
    const url = (VectorSvelte.bunkerState().urlInput || '').trim();
    if (!url.toLowerCase().startsWith('bunker://')) {
        VectorSvelte.bunkerStatus('Must start with bunker://', 'error');
        return;
    }
    // Inputs lock while the bunker handshake runs (5–10s typical while the
    // user taps "approve" on their signer); a failure unlocks them to retry.
    VectorSvelte.bunkerBusy(true);
    VectorSvelte.bunkerStatus('Connecting to signer…', 'connecting');
    try {
        if (addAccountFlow.active) await addAccountFlow.commit();
        const { public: pubKey, existing } = await invoke('connect_bunker', {
            bunkerUrl: url,
        });
        strPubkey = pubKey;
        VectorSvelte.patchBunker({ urlInput: '' });
        if (existing) {
            // Bunker identity matches an existing account; backend has
            // armed `session_reload`. Just hide the form — the document
            // reload will switch into the stored account.
            VectorSvelte.bunkerStatus('Account already added — switching…', 'online');
            hideBunkerForm();
            return;
        }
        VectorSvelte.bunkerStatus('Connected. Choosing security…', 'online');
        // UI advances first; relay connect runs in the background so a
        // hang there doesn't strand the user on the bunker screen.
        hideBunkerForm();
        openEncryptionFlow(false);
        invoke('connect').catch((err) => {
            console.warn('[connect_bunker] connect() failed:', err);
        });
    } catch (e) {
        VectorSvelte.bunkerStatus(String(e), 'error');
        VectorSvelte.bunkerBusy(false);
    }
}

async function loginBack() {
    // Add Profile flow back has two cases — independent of which sub-
    // screen the user happens to be on (start / import / encryption /
    // welcome).
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
    const wasOnBunkerForm = VectorSvelte.loginState().bunker;
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
        VectorSvelte.loginScreen('none', false);
        VectorSvelte.loginShowForm(false);
        bunkerReauthOrigin = null;
        if (origin === 'settings' && typeof openSettings === 'function') {
            openSettings();
        } else if (typeof closeChat === 'function') {
            closeChat();
        }
        return;
    }
    // Regular login back: collapse every sub-screen back to the start picker.
    hideBunkerForm();
    VectorSvelte.loginScreen('start', false);
    VectorSvelte.patchLogin({ importKey: '' });
    // Re-reveal the picker pill if we have ≥2 accounts on disk: the Login
    // button hid it, and without this restore the user could no longer
    // switch accounts from the start screen without restarting the app.
    if (loginPicker.accounts && loginPicker.accounts.length >= 2) {
        loginPicker.show(loginPicker.activeNpub);
    }
}

// Handlers resolve at call time: main.js, accounts.js and the renderers hold the
// functions, and the encrypt flow's object changes per open.
const LOGIN_HELPERS = {
    back: () => loginBack(),
    createAccount: () => createAccount(),
    openImport: () => openImportScreen(),
    importKey: () => importKey(),
    invite: () => submitInvite(),
    nip55: () => loginWithNip55(),
    picker: {
        toggle: () => loginPicker.toggle(),
        close: () => loginPicker.close(),
        pick: (meta) => loginPicker.onPick(meta),
        rowHelpers: () => accountRowHelpers,
        avatarImg: (src) => createAvatarImg(src, 36, false),
    },
    bunker: {
        // Bunker is a login flow (the signer is the identity), so it lives under Login.
        open: () => showBunkerForm('new'),
        copy: () => copyBunkerLink(),
        // Blow the QR up fullscreen; openQrOverlay no-ops while the link is still generating.
        openQr: () => openQrOverlay(strBunkerNostrConnectUrl),
        renderQr: (node, url) => renderQrInto(node, url),
        connect: () => connectBunkerUrl(),
    },
    encrypt: {
        choose: (type) => encryptFlow?.choose(type),
        pinFull: (pin) => encryptFlow?.pinFull(pin),
        pinBackspace: () => encryptFlow?.pinBackspace(),
        submitPassword: () => encryptFlow?.submitPassword(),
        biometric: () => encryptFlow?.biometric(),
    },
};
VectorSvelte.mountLoginScreen(domLogin, { h: LOGIN_HELPERS });

/** The NIP-55 button (Android) shows only when a signer app is installed, so it is never a dead end. */
async function wireLoginUi() {
    if (platformFeatures.os !== 'android') return;
    try {
        if (await invoke('is_external_signer_installed')) VectorSvelte.patchLogin({ nip55Shown: true });
    } catch (_) { /* leave hidden */ }
}
