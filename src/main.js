const { invoke, convertFileSrc } = window.__TAURI__.core;
const { getCurrentWebview } = window.__TAURI__.webview;
const { getCurrentWindow } = window.__TAURI__.window;
const { getCurrentWebviewWindow } = window.__TAURI__.webviewWindow;
const { listen } = window.__TAURI__.event;
const { openUrl, revealItemInDir } = window.__TAURI__.opener;

// System event types (matches Rust SystemEventType enum)
const SystemEventType = {
    MemberLeft: 0,
    MemberJoined: 1,
    MemberRemoved: 2,
};

const domTheme = document.getElementById('theme');

const domLoginStart = document.getElementById('login-start');
const domLoginAccountCreationBtn = document.getElementById('start-account-creation-btn');
const domLoginAccountBtn = document.getElementById('start-login-btn');
const domLogin = document.getElementById('login-form');
const domLoginImport = document.getElementById('login-import');
const domLoginInput = document.getElementById('login-input');
const domLoginBtn = document.getElementById('login-btn');

const domLoginInvite = document.getElementById('login-invite');
const domInviteInput = document.getElementById('invite-input');
const domInviteBtn = document.getElementById('invite-btn');

const domLoginWelcome = document.getElementById('login-welcome');

const domLoginEncrypt = document.getElementById('login-encrypt');
const domLoginEncryptTitle = document.getElementById('login-encrypt-title');
const domLoginEncryptPinRow = document.getElementById('login-encrypt-pins');

const domProfile = document.getElementById('profile');
const domProfileBackBtn = document.getElementById('profile-back-btn');
const domProfileHeaderAvatarContainer = document.getElementById('profile-header-avatar-container');
const domProfileName = document.getElementById('profile-name');
const domProfileStatus = document.getElementById('profile-status');
// Note: these are 'let' due to needing to use `.replaceWith` when hot-swapping profile elements
let domProfileBanner = document.getElementById('profile-banner');
let domProfileAvatar = document.getElementById('profile-avatar');
const domProfileNameSecondary = document.getElementById('profile-secondary-name');
const domProfileStatusSecondary = document.getElementById('profile-secondary-status');
const domProfileBadgeInvite = document.getElementById('profile-badge-invites');
const domProfileBadgeFawkes = document.getElementById('profile-badge-fawkes');
const domProfileDescription = document.getElementById('profile-description');
const domProfileDescriptionEditor = document.getElementById('profile-description-editor');
const domProfileOptions = document.getElementById('profile-option-list');
const domProfileOptionMute = document.getElementById('profile-option-mute');
const domProfileOptionMessage = document.getElementById('profile-option-message');
const domProfileOptionNickname = document.getElementById('profile-option-nickname');
const domProfileId = document.getElementById('profile-id');

const domGroupOverview = document.getElementById('group-overview');
const domGroupOverviewBackBtn = document.getElementById('group-overview-back-btn');
const domGroupOverviewName = document.getElementById('group-overview-name');
const domGroupOverviewStatus = document.getElementById('group-overview-status');
let domGroupOverviewAvatar = document.getElementById('group-overview-avatar');
const domGroupOverviewNameSecondary = document.getElementById('group-overview-secondary-name');
const domGroupOverviewDescription = document.getElementById('group-overview-description');
const domGroupOverviewMembers = document.getElementById('group-overview-members');
const domGroupMemberSearchInput = document.getElementById('group-member-search-input');
const domGroupInviteMemberBtn = document.getElementById('group-invite-member-btn');
const domGroupLeaveBtn = document.getElementById('group-leave-btn');

const domChats = document.getElementById('chats');
const domChatBookmarksBtn = document.getElementById('chat-bookmarks-btn');
const domAccount = document.getElementById('account');
const domAccountAvatarContainer = document.getElementById('account-avatar-container');
const domAccountName = document.getElementById('account-name');
const domAccountStatus = document.getElementById('account-status');
const domSyncLine = document.getElementById('sync-line');
const domChatList = document.getElementById('chat-list');
const domChatNewDM = document.getElementById('new-chat-btn');
const domChatNewGroup = document.getElementById('create-group-btn');
const domNavbar = document.getElementById('navbar');
const domInvites = document.getElementById('invites');
const domInvitesBtn = document.getElementById('invites-btn');
const domProfileBtn = document.getElementById('profile-btn');
const domChatlistBtn = document.getElementById('chat-btn');
const domSettingsBtn = document.getElementById('settings-btn');

const domChat = document.getElementById('chat');
const domChatBackBtn = document.getElementById('chat-back-btn');
const domChatBackNotificationDot = document.getElementById('chat-back-notification-dot');
const domChatHeaderAvatarContainer = document.getElementById('chat-header-avatar-container');
const domChatContact = document.getElementById('chat-contact');
const domChatContactStatus = document.getElementById('chat-contact-status');
const domChatMessages = document.getElementById('chat-messages');
const domChatMessageBox = document.getElementById('chat-box');
const domChatMessagesScrollReturnBtn = document.getElementById('chat-scroll-return');
const domChatMessageInput = document.getElementById('chat-input');
const domChatMessageInputFile = document.getElementById('chat-input-file');
const domChatMessageInputCancel = document.getElementById('chat-input-cancel');
const domChatMessageInputEmoji = document.getElementById('chat-input-emoji');
const domAttachmentPanel = document.getElementById('attachment-panel');
const domAttachmentPanelMain = document.getElementById('attachment-panel-main');
const domAttachmentPanelFile = document.getElementById('attachment-panel-file');
const domAttachmentPanelMiniApps = document.getElementById('attachment-panel-miniapps');
const domAttachmentPanelMiniAppsView = document.getElementById('attachment-panel-miniapps-view');
const domMiniAppsGrid = document.getElementById('miniapps-grid');
const domMiniAppsSearch = document.getElementById('miniapps-search');
const domAttachmentPanelBack = document.getElementById('attachment-panel-back');
const domAttachmentPanelMarketplace = document.getElementById('attachment-panel-marketplace');
const domAttachmentPanelPivx = document.getElementById('attachment-panel-pivx');
const domAttachmentPanelPivxView = document.getElementById('attachment-panel-pivx-view');
const domAttachmentPanelPivxBack = document.getElementById('attachment-panel-pivx-back');
const domPivxBalanceAmount = document.getElementById('pivx-balance-amount');
const domPivxDepositBtn = document.getElementById('pivx-deposit-btn');
const domPivxSendBtn = document.getElementById('pivx-send-btn');
const domPivxSettingsBtn = document.getElementById('pivx-settings-btn');
const domPivxDepositOverlay = document.getElementById('pivx-deposit-overlay');
const domPivxSendOverlay = document.getElementById('pivx-send-overlay');
const domPivxSettingsOverlay = document.getElementById('pivx-settings-overlay');
const domMarketplacePanel = document.getElementById('marketplace-panel');
const domMarketplaceBackBtn = document.getElementById('marketplace-back-btn');
const domMarketplaceContent = document.getElementById('marketplace-content');
const domMiniAppLaunchOverlay = document.getElementById('miniapp-launch-overlay');
const domMiniAppLaunchIconContainer = document.getElementById('miniapp-launch-icon-container');
const domMiniAppLaunchName = document.getElementById('miniapp-launch-name');
const domMiniAppLaunchCancel = document.getElementById('miniapp-launch-cancel');
const domMiniAppLaunchSolo = document.getElementById('miniapp-launch-solo');
const domMiniAppLaunchInvite = document.getElementById('miniapp-launch-invite');
const domChatMessageInputVoice = document.getElementById('chat-input-voice');
const domChatMessageInputSend = document.getElementById('chat-input-send');
const domChatInputContainer = document.querySelector('.chat-input-container');

const domChatNew = document.getElementById('chat-new');
const domChatNewBackBtn = document.getElementById('chat-new-back-text-btn');
const domShareNpub = document.getElementById('share-npub');
const domChatNewInput = document.getElementById('chat-new-input');
const domChatNewStartBtn = document.getElementById('chat-new-btn');

// Create Group UI refs
const domCreateGroup = document.getElementById('create-group');
const domCreateGroupBackBtn = document.getElementById('create-group-back-text-btn');
const domCreateGroupName = document.getElementById('create-group-name');
const domCreateGroupFilter = document.getElementById('create-group-filter');
const domCreateGroupList = document.getElementById('create-group-list');
const domCreateGroupCreateBtn = document.getElementById('create-group-create-btn');
const domCreateGroupCancelBtn = document.getElementById('create-group-cancel-btn');
const domCreateGroupStatus = document.getElementById('create-group-status');
const domSettings = document.getElementById('settings');
const domSettingsThemeSelect = document.getElementById('theme-select');
const domSettingsWhisperModelInfo = document.getElementById('whisper-model-info');
const domSettingsWhisperAutoTranslateInfo = document.getElementById('whisper-auto-translate-info');
const domSettingsWhisperAutoTranscribeInfo = document.getElementById('whisper-auto-transcribe-info');
const domSettingsPrivacyWebPreviewsInfo = document.getElementById('privacy-web-previews-info');
const domSettingsPrivacyStripTrackingInfo = document.getElementById('privacy-strip-tracking-info');
const domSettingsPrivacySendTypingInfo = document.getElementById('privacy-send-typing-info');
const domSettingsDisplayImageTypesInfo = document.getElementById('display-image-types-info');
const domSettingsNotifMuteInfo = document.getElementById('notif-mute-info');
const domSettingsDeepRescanInfo = document.getElementById('deep-rescan-info');
const domSettingsExportAccountInfo = document.getElementById('export-account-info');
const domSettingsLogoutInfo = document.getElementById('logout-info');
const domSettingsDonorsInfo = document.getElementById('donors-info');
const domDonorPivx = document.getElementById('donor-pivx');
const domSettingsLogout = document.getElementById('logout-btn');
const domSettingsExport = document.getElementById('export-account-btn');
const domRestorePivxGroup = document.getElementById('restore-pivx-group');
const domRestorePivxBtn = document.getElementById('restore-pivx-btn');

const domApp = document.getElementById('popup-container');
const domPopup = document.getElementById('popup');
const domPopupIcon = document.getElementById('popupIcon');
const domPopupTitle = document.getElementById('popupTitle');
const domPopupSubtext = document.getElementById('popupSubtext');
const domPopupConfirmBtn = document.getElementById('popupConfirm');
const domPopupCancelBtn = document.getElementById('popupCancel');
const domPopupInput = document.getElementById('popupInput');

const picker = document.querySelector('.emoji-picker');
/** @type {HTMLInputElement} */
const emojiSearch = document.getElementById('emoji-search-input');
const emojiSearchIcon = document.querySelector('.emoji-search-icon');
/**
 * The current reaction reference - i.e: a message being reacted to.
 * 
 * When empty, emojis are simply injected to the current chat input.
 */
let strCurrentReactionReference = "";

/**
 * Opens the Emoji Input Panel
 * 
 * The panel always appears in a fixed position at the bottom, regardless of whether
 * it's opened from the message input or a reaction button.
 * @param {MouseEvent?} e - An associated click event
 */
function openEmojiPanel(e) {
    const isDefaultPanel = e.target === domChatMessageInputEmoji || domChatMessageInputEmoji.contains(e.target);

    // Open or Close the panel depending on it's state
    const strReaction = e.target.classList.contains('add-reaction') ? e.target.parentElement.parentElement.id : '';
    const fClickedInputOrReaction = isDefaultPanel || strReaction;
    if (fClickedInputOrReaction && !picker.classList.contains('visible')) {
        // Close attachment panel if open
        if (domAttachmentPanel.classList.contains('visible')) {
            closeAttachmentPanel();
        }

        // Reset the emoji picker state first
        resetEmojiPicker();

        // Load emoji sections with optimized rendering
        renderEmojiPanel();

        // Display the picker - use class instead of inline style
        picker.classList.add('visible');

        // Always use the same fixed position (bottom-up) for both message input and reactions
        picker.classList.add('emoji-picker-message-type');

        // Position emoji picker dynamically above the chat-box (for both input and reactions)
        const chatBox = document.getElementById('chat-box');
        if (chatBox) {
            const chatBoxHeight = chatBox.getBoundingClientRect().height;
            picker.style.bottom = (chatBoxHeight + 10) + 'px'; // 10px gap above chat-box
        }

        // Clear any other positioning styles to ensure CSS fixed positioning takes effect
        picker.style.top = '';
        picker.style.left = '';
        picker.style.right = '';

        // Change the emoji button to a wink while the panel is open (only for message input)
        if (isDefaultPanel) {
            domChatMessageInputEmoji.innerHTML = `<span class="icon icon-wink-face"></span>`;
        }

        // If this is a Reaction, let's cache the Reference ID
        if (strReaction) {
            strCurrentReactionReference = strReaction;
        } else {
            strCurrentReactionReference = '';
        }

        // Focus on the emoji search box for easy searching (desktop only - mobile keyboards are disruptive)
        if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
            emojiSearch.focus();
        }

        // Prefetch GIF data in background (non-blocking)
        prefetchTrendingGifs();
    } else {
        // Hide and reset the UI - use class instead of inline style
        emojiSearch.value = '';
        picker.classList.remove('visible');
        picker.style.bottom = ''; // Reset to CSS default
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;
    }
}

/**
 * Opens or closes the Attachment Panel
 *
 * The panel slides up from behind the chat box, similar to the emoji panel.
 */
function toggleAttachmentPanel() {
    if (!domAttachmentPanel.classList.contains('visible')) {
        // Close emoji panel if open
        if (picker.classList.contains('visible')) {
            picker.classList.remove('visible');
            picker.style.bottom = '';
            domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;
        }

        // Display the attachment panel
        domAttachmentPanel.classList.add('visible');
        domChatMessageInputFile.classList.add('open');

        // Position attachment panel dynamically above the chat-box
        const chatBox = document.getElementById('chat-box');
        if (chatBox) {
            const chatBoxHeight = chatBox.getBoundingClientRect().height;
            domAttachmentPanel.style.bottom = (chatBoxHeight + 10) + 'px';
        }
        
        // Animate items when panel opens
        animateAttachmentPanelItems(domAttachmentPanelMain);
    } else {
        // Hide the attachment panel
        closeAttachmentPanel();
    }
}

/**
 * Closes the Attachment Panel
 */
function closeAttachmentPanel() {
    domAttachmentPanel.classList.remove('visible');
    domAttachmentPanel.style.bottom = '';
    domChatMessageInputFile.classList.remove('open');
    // Deactivate edit mode if active
    deactivateMiniAppsEditMode();
    // Reset to main view when closing
    showAttachmentPanelMain();
}

/**
 * Shows a global tooltip above the target element
 * @param {string} text - The tooltip text
 * @param {HTMLElement} targetElement - The element to position the tooltip above
 */
function showGlobalTooltip(text, targetElement) {
    const tooltip = document.getElementById('global-tooltip');
    if (!tooltip) return;
    
    tooltip.textContent = text;
    
    // Get the target element's position
    const rect = targetElement.getBoundingClientRect();
    
    // Position tooltip above the element, centered horizontally
    tooltip.style.left = `${rect.left + rect.width / 2}px`;
    tooltip.style.top = `${rect.top - 8}px`;
    tooltip.style.transform = 'translate(-50%, -100%)';
    
    // Show the tooltip
    tooltip.classList.add('visible');
}

/**
 * Hides the global tooltip
 */
function hideGlobalTooltip() {
    const tooltip = document.getElementById('global-tooltip');
    if (!tooltip) return;
    tooltip.classList.remove('visible');
}

/**
 * Shows the main attachment panel view (File, Mini Apps buttons)
 */
function showAttachmentPanelMain() {
    domAttachmentPanelMain.style.display = 'flex';
    domAttachmentPanelMiniAppsView.style.display = 'none';
    // Also hide PIVX wallet view if open
    if (domAttachmentPanelPivxView) {
        domAttachmentPanelPivxView.style.display = 'none';
    }
    // Remove PIVX-active border styling
    if (domAttachmentPanel) {
        domAttachmentPanel.classList.remove('pivx-active');
    }

    // Animate items with staggered delay
    animateAttachmentPanelItems(domAttachmentPanelMain);
}

/**
 * Shows the Mini Apps list view
 */
async function showAttachmentPanelMiniApps() {
    domAttachmentPanelMain.style.display = 'none';
    domAttachmentPanelMiniAppsView.style.display = 'flex';
    // Also hide PIVX wallet view if open
    if (domAttachmentPanelPivxView) {
        domAttachmentPanelPivxView.style.display = 'none';
    }

    // Clear search input and reset filter
    if (domMiniAppsSearch) {
        domMiniAppsSearch.value = '';
        filterMiniApps('');
    }

    // Load Mini Apps history from backend
    await loadMiniAppsHistory();

    // Animate items with staggered delay
    animateAttachmentPanelItems(domMiniAppsGrid);
}

/**
 * Shows the marketplace panel
 */
function showMarketplacePanel() {
    if (domMarketplacePanel) {
        domMarketplacePanel.style.display = 'flex';
        // Initialize marketplace on first show
        initMarketplace(domMarketplaceContent);
    }
}

/**
 * Hides the marketplace panel with fade out animation
 * @returns {Promise<void>} Resolves when the animation completes
 */
function hideMarketplacePanel() {
    return new Promise((resolve) => {
        if (domMarketplacePanel && domMarketplacePanel.style.display !== 'none') {
            domMarketplacePanel.classList.add('closing');
            domMarketplacePanel.addEventListener('animationend', function handler() {
                domMarketplacePanel.removeEventListener('animationend', handler);
                domMarketplacePanel.style.display = 'none';
                domMarketplacePanel.classList.remove('closing');
                resolve();
            });
        } else {
            resolve();
        }
    });
}

// ========== PIVX Wallet Functions ==========

/**
 * Shows a simple toast notification
 * @param {string} message - The message to display
 */
function showToast(message) {
    // Create toast element if it doesn't exist
    let toast = document.getElementById('pivx-toast');
    if (!toast) {
        toast = document.createElement('div');
        toast.id = 'pivx-toast';
        toast.style.cssText = `
            position: fixed;
            bottom: 80px;
            left: 50%;
            transform: translateX(-50%);
            background: rgba(0, 0, 0, 0.8);
            color: white;
            padding: 12px 24px;
            border-radius: 8px;
            z-index: 10000;
            font-size: 14px;
            opacity: 0;
            transition: opacity 0.3s ease;
            pointer-events: none;
        `;
        document.body.appendChild(toast);
    }

    toast.textContent = message;
    toast.style.opacity = '1';

    // Hide after 3 seconds
    clearTimeout(toast._timeout);
    toast._timeout = setTimeout(() => {
        toast.style.opacity = '0';
    }, 3000);
}

/**
 * Shows the PIVX wallet panel and hides the main Mini Apps view
 */
function showPivxWalletPanel() {
    // Track PIVX usage for history-based positioning
    localStorage.setItem('pivx_last_used', Date.now().toString());

    if (domAttachmentPanelMiniAppsView) {
        domAttachmentPanelMiniAppsView.style.display = 'none';
    }
    if (domAttachmentPanelPivxView) {
        domAttachmentPanelPivxView.style.display = 'flex';
        // Animate PIVX panel elements
        animatePivxPanelOpen(domAttachmentPanelPivxView);
    }
    if (domAttachmentPanel) {
        domAttachmentPanel.classList.add('pivx-active');
    }
    refreshPivxWallet();
}

/**
 * Animates PIVX panel elements when opened
 */
function animatePivxPanelOpen(container) {
    const balanceSection = container.querySelector('.pivx-balance-section');
    const dockButtons = container.querySelectorAll('.pivx-dock-btn');
    const staggerDelay = 0.06; // 60ms delay between each button

    // Reset animations (elements are opacity:0 by default in CSS)
    if (balanceSection) {
        balanceSection.classList.remove('pivx-panel-animate');
        void balanceSection.offsetWidth; // Force reflow
        balanceSection.classList.add('pivx-panel-animate');
    }

    dockButtons.forEach((btn, index) => {
        btn.classList.remove('pivx-panel-animate');
        btn.style.animationDelay = '';
        void btn.offsetWidth; // Force reflow
        btn.style.animationDelay = `${(index + 1) * staggerDelay}s`;
        btn.classList.add('pivx-panel-animate');
    });
}

/**
 * Hides the PIVX wallet panel and shows the main Mini Apps view
 */
function hidePivxWalletPanel() {
    if (domAttachmentPanelPivxView) {
        // Remove animation classes so elements revert to CSS default (opacity: 0)
        const balanceSection = domAttachmentPanelPivxView.querySelector('.pivx-balance-section');
        const dockButtons = domAttachmentPanelPivxView.querySelectorAll('.pivx-dock-btn');

        if (balanceSection) {
            balanceSection.classList.remove('pivx-panel-animate');
        }
        dockButtons.forEach((btn) => {
            btn.classList.remove('pivx-panel-animate');
            btn.style.animationDelay = '';
        });

        domAttachmentPanelPivxView.style.display = 'none';
    }
    if (domAttachmentPanelMiniAppsView) {
        domAttachmentPanelMiniAppsView.style.display = 'flex';
        animateAttachmentPanelItems(domMiniAppsGrid);
    }
    if (domAttachmentPanel) {
        domAttachmentPanel.classList.remove('pivx-active');
    }
}

/**
 * Refreshes the PIVX wallet balance by fetching from backend
 */
async function refreshPivxWallet() {
    const domPivxBalanceFiat = document.getElementById('pivx-balance-fiat');

    // Show loading spinner and hide fiat during load
    if (domPivxBalanceAmount) {
        domPivxBalanceAmount.classList.remove('pivx-fade-in');
        domPivxBalanceAmount.innerHTML = '<div class="pivx-balance-loading"><div class="pivx-spinner"></div></div>';
    }
    if (domPivxBalanceFiat) {
        domPivxBalanceFiat.classList.remove('pivx-fade-in');
        domPivxBalanceFiat.style.display = 'none';
    }

    try {
        // Fetch balance and price in parallel
        const [balance, priceInfo] = await Promise.all([
            invoke('pivx_get_wallet_balance'),
            fetchPivxPrice()
        ]);

        // Store current balance for deposit limit check
        pivxCurrentWalletBalance = balance;

        if (domPivxBalanceAmount) {
            domPivxBalanceAmount.innerHTML = `${balance.toFixed(2)} <span style="color: #642D8F;">PIV</span>`;
            // Trigger fade-in animation
            void domPivxBalanceAmount.offsetWidth; // Force reflow
            domPivxBalanceAmount.classList.add('pivx-fade-in');
        }

        // Update deposit button state based on balance
        if (domPivxDepositBtn) {
            if (balance >= PIVX_MAX_BALANCE_WARNING) {
                domPivxDepositBtn.classList.add('disabled');
                domPivxDepositBtn.title = 'Balance too high - please withdraw first';
            } else {
                domPivxDepositBtn.classList.remove('disabled');
                domPivxDepositBtn.title = '';
            }
        }

        // Show fiat value if we have price data
        if (domPivxBalanceFiat && priceInfo && priceInfo.value > 0) {
            const fiatValue = balance * priceInfo.value;
            domPivxBalanceFiat.textContent = formatFiatValue(fiatValue, priceInfo.currency.toUpperCase());
            domPivxBalanceFiat.style.display = '';
            // Trigger fade-in animation with slight delay
            void domPivxBalanceFiat.offsetWidth; // Force reflow
            domPivxBalanceFiat.classList.add('pivx-fade-in');
        } else if (domPivxBalanceFiat) {
            domPivxBalanceFiat.style.display = 'none';
        }
    } catch (err) {
        console.error('Failed to refresh PIVX wallet:', err);
        if (domPivxBalanceAmount) {
            domPivxBalanceAmount.innerHTML = `0.00 <span style="color: #642D8F;">PIV</span>`;
            void domPivxBalanceAmount.offsetWidth;
            domPivxBalanceAmount.classList.add('pivx-fade-in');
        }
        if (domPivxBalanceFiat) {
            domPivxBalanceFiat.style.display = 'none';
        }
    }
}

// Track deposit polling state
let pivxDepositPollingInterval = null;
let pivxCurrentDepositAddress = null;
let pivxCurrentWalletBalance = 0;
const PIVX_MAX_BALANCE_WARNING = 1000;

/**
 * Shows the deposit dialog with a new promo code and address
 */
async function showPivxDepositDialog() {
    // Security check: prevent deposits if balance is too high
    if (pivxCurrentWalletBalance >= PIVX_MAX_BALANCE_WARNING) {
        await popupConfirm(
            'Balance Too High',
            `Your wallet balance is <b>${pivxCurrentWalletBalance.toFixed(2)} PIV</b>, which exceeds the recommended limit of ${PIVX_MAX_BALANCE_WARNING} PIV.<br><br>Vector is not intended to replace a proper cryptocurrency wallet. Please <b>withdraw your funds</b> to a secure wallet before depositing more.`,
            true,
            '',
            'vector_warning.svg'
        );
        return;
    }

    // Show loading state on deposit button
    if (domPivxDepositBtn) {
        domPivxDepositBtn.classList.add('loading');
        domPivxDepositBtn.disabled = true;
    }

    try {
        // Create a new promo code for deposit
        const promo = await invoke('pivx_create_promo');
        pivxCurrentDepositAddress = promo.address;

        const addressEl = document.getElementById('pivx-deposit-address');
        const statusEl = document.getElementById('pivx-deposit-status');

        if (addressEl) addressEl.textContent = promo.address;
        if (statusEl) {
            statusEl.innerHTML = `
                <div class="pivx-awaiting-deposit">
                    <div class="pivx-spinner"></div>
                    <span>Awaiting Deposit...</span>
                </div>
            `;
        }

        if (domPivxDepositOverlay) {
            domPivxDepositOverlay.classList.add('active');
        }

        // Start polling for incoming deposit
        startDepositPolling(promo.address);
    } catch (err) {
        console.error('Failed to create deposit promo:', err);
        showToast('Failed to create deposit address');
    } finally {
        // Remove loading state from deposit button
        if (domPivxDepositBtn) {
            domPivxDepositBtn.classList.remove('loading');
            domPivxDepositBtn.disabled = false;
        }
    }
}

/**
 * Start polling for deposits on the given address
 */
function startDepositPolling(address) {
    // Clear any existing polling
    stopDepositPolling();
    pivxCurrentDepositAddress = address;

    // Poll every 5 seconds
    pivxDepositPollingInterval = setInterval(() => {
        checkForDeposit(address);
    }, 5000);
}

/**
 * Check if a deposit has arrived at the address
 */
async function checkForDeposit(address) {
    if (!pivxCurrentDepositAddress || address !== pivxCurrentDepositAddress) return;

    try {
        // We need to check balance by address - use the wallet balance refresh
        const promos = await invoke('pivx_refresh_balances');
        const thisPromo = promos.find(p => p.address === address);

        if (thisPromo && thisPromo.balance_piv > 0) {
            // Deposit detected!
            stopDepositPolling();

            const statusEl = document.getElementById('pivx-deposit-status');
            if (statusEl) {
                statusEl.innerHTML = `
                    <div class="pivx-deposit-received">
                        <span class="icon icon-check"></span>
                        <span>Received ${thisPromo.balance_piv.toFixed(8)} PIV!</span>
                    </div>
                `;
            }

            showToast(`Received ${thisPromo.balance_piv.toFixed(8)} PIV!`);

            // Close dialog after a short delay and refresh wallet
            setTimeout(() => {
                closePivxDepositDialog();
                refreshPivxWallet();
            }, 1500);
        }
    } catch (err) {
        console.error('Check deposit error:', err);
    }
}

/**
 * Stop polling for deposits
 */
function stopDepositPolling() {
    if (pivxDepositPollingInterval) {
        clearInterval(pivxDepositPollingInterval);
        pivxDepositPollingInterval = null;
    }
    pivxCurrentDepositAddress = null;
}

/**
 * Closes the deposit dialog
 */
function closePivxDepositDialog() {
    stopDepositPolling();
    if (domPivxDepositOverlay) {
        domPivxDepositOverlay.classList.remove('active');
    }
}

// Track send dialog state
let pivxSendAvailableBalance = 0;
let pivxSendSelectedPromo = null;
let pivxSendPromos = [];
let pivxSendMode = 'quick'; // 'quick' or 'custom'

// Currency/price tracking (session-cached)
let pivxCurrencyList = null; // Cached currency list (fetched once per session)
let pivxCurrentPrice = null; // Current price in preferred currency
let pivxPreferredCurrency = null; // User's preferred currency code

/**
 * Detects the user's default currency based on their locale
 * @returns {string} Currency code (e.g., 'USD', 'EUR', 'GBP')
 */
function detectDefaultCurrency() {
    try {
        // Get locale from browser
        const locale = navigator.language || navigator.languages?.[0] || 'en-US';

        // Map common locale regions to currencies
        const localeCurrencyMap = {
            'US': 'USD', 'CA': 'CAD', 'AU': 'AUD', 'NZ': 'NZD', 'GB': 'GBP', 'UK': 'GBP',
            'IE': 'EUR', 'DE': 'EUR', 'FR': 'EUR', 'ES': 'EUR', 'IT': 'EUR', 'NL': 'EUR',
            'BE': 'EUR', 'AT': 'EUR', 'PT': 'EUR', 'FI': 'EUR', 'GR': 'EUR', 'SK': 'EUR',
            'SI': 'EUR', 'EE': 'EUR', 'LV': 'EUR', 'LT': 'EUR', 'MT': 'EUR', 'CY': 'EUR',
            'LU': 'EUR', 'MC': 'EUR', 'SM': 'EUR', 'VA': 'EUR', 'AD': 'EUR', 'ME': 'EUR',
            'XK': 'EUR', 'JP': 'JPY', 'CN': 'CNY', 'HK': 'HKD', 'TW': 'TWD', 'KR': 'KRW',
            'IN': 'INR', 'SG': 'SGD', 'MY': 'MYR', 'TH': 'THB', 'ID': 'IDR', 'PH': 'PHP',
            'VN': 'VND', 'PK': 'PKR', 'BD': 'BDT', 'RU': 'RUB', 'UA': 'UAH', 'PL': 'PLN',
            'CZ': 'CZK', 'HU': 'HUF', 'RO': 'RON', 'BG': 'BGN', 'HR': 'HRK', 'RS': 'RSD',
            'CH': 'CHF', 'SE': 'SEK', 'NO': 'NOK', 'DK': 'DKK', 'IS': 'ISK', 'TR': 'TRY',
            'IL': 'ILS', 'AE': 'AED', 'SA': 'SAR', 'QA': 'QAR', 'KW': 'KWD', 'BH': 'BHD',
            'OM': 'OMR', 'EG': 'EGP', 'ZA': 'ZAR', 'NG': 'NGN', 'KE': 'KES', 'GH': 'GHS',
            'MX': 'MXN', 'BR': 'BRL', 'AR': 'ARS', 'CL': 'CLP', 'CO': 'COP', 'PE': 'PEN',
            'VE': 'VES', 'NI': 'NIO', 'CR': 'CRC', 'PA': 'PAB', 'DO': 'DOP', 'GT': 'GTQ',
        };

        // Extract region code from locale (e.g., 'en-US' -> 'US', 'de-DE' -> 'DE')
        const parts = locale.split('-');
        const region = parts.length > 1 ? parts[1].toUpperCase() : parts[0].toUpperCase();

        return localeCurrencyMap[region] || 'USD';
    } catch (err) {
        console.error('Failed to detect default currency:', err);
        return 'USD';
    }
}

/**
 * Fetches the currency list from the PIVX Oracle API (cached per session)
 * @returns {Promise<Array>} Array of currency info objects
 */
async function fetchPivxCurrencies() {
    // Return cached list if available
    if (pivxCurrencyList) {
        return pivxCurrencyList;
    }

    try {
        const currencies = await invoke('pivx_get_currencies');
        // Filter to common fiat currencies for the dropdown
        const fiatCurrencies = ['USD', 'EUR', 'GBP', 'CAD', 'AUD', 'JPY', 'CHF', 'CNY',
            'INR', 'RUB', 'BRL', 'MXN', 'KRW', 'SGD', 'HKD', 'SEK', 'NOK', 'DKK',
            'PLN', 'CZK', 'HUF', 'TRY', 'ZAR', 'AED', 'SAR', 'THB', 'MYR', 'IDR',
            'PHP', 'VND', 'NZD', 'ILS', 'ARS', 'CLP', 'COP', 'PEN', 'NGN', 'KES',
            'EGP', 'PKR', 'BDT', 'TWD', 'RON', 'BGN', 'HRK', 'ISK', 'UAH'];

        pivxCurrencyList = currencies.filter(c =>
            fiatCurrencies.includes(c.currency.toUpperCase())
        ).sort((a, b) => a.currency.localeCompare(b.currency));

        return pivxCurrencyList;
    } catch (err) {
        console.error('Failed to fetch currencies:', err);
        return [];
    }
}

/**
 * Fetches the current PIVX price in the preferred currency
 * @returns {Promise<Object|null>} Price info or null
 */
async function fetchPivxPrice() {
    if (!pivxPreferredCurrency) {
        // Load preference from DB or use locale default
        try {
            const saved = await invoke('pivx_get_preferred_currency');
            pivxPreferredCurrency = saved || detectDefaultCurrency();
        } catch {
            pivxPreferredCurrency = detectDefaultCurrency();
        }
    }

    try {
        pivxCurrentPrice = await invoke('pivx_get_price', { currency: pivxPreferredCurrency });
        return pivxCurrentPrice;
    } catch (err) {
        console.error('Failed to fetch PIVX price:', err);
        return null;
    }
}

/**
 * Formats a fiat value with currency symbol
 * @param {number} value - The fiat value
 * @param {string} currency - Currency code
 * @returns {string} Formatted value (e.g., "$1.23", "€1.23")
 */
function formatFiatValue(value, currency) {
    try {
        return new Intl.NumberFormat(navigator.language || 'en-US', {
            style: 'currency',
            currency: currency,
            minimumFractionDigits: 2,
            maximumFractionDigits: 2
        }).format(value);
    } catch {
        // Fallback if currency not supported
        return `${value.toFixed(2)} ${currency}`;
    }
}

/**
 * Shows the send dialog for sending PIVX to the current chat
 */
async function showPivxSendDialog() {
    if (!strOpenChat) {
        showToast('Open a chat first to send PIVX');
        return;
    }

    const recipientEl = document.getElementById('pivx-send-recipient');
    const amountEl = document.getElementById('pivx-send-amount');
    const availableEl = document.getElementById('pivx-send-available-amount');
    const promoListEl = document.getElementById('pivx-send-promo-list');
    const promoSectionEl = document.getElementById('pivx-send-promo-section');
    const customSectionEl = document.getElementById('pivx-send-custom-section');
    const confirmBtn = document.getElementById('pivx-send-confirm');

    // Get the chat name for display
    const chatName = getChatDisplayName(strOpenChat);
    if (recipientEl) recipientEl.textContent = chatName || 'this chat';

    // Reset state
    pivxSendSelectedPromo = null;
    pivxSendMode = 'quick';
    if (amountEl) amountEl.value = '';

    // Show promo section, hide custom section
    if (promoSectionEl) promoSectionEl.style.display = '';
    if (customSectionEl) customSectionEl.style.display = 'none';

    // Disable send button while loading
    if (confirmBtn) {
        confirmBtn.classList.add('loading');
        confirmBtn.disabled = true;
    }

    // Show loading in promo list
    if (promoListEl) {
        promoListEl.innerHTML = `
            <div class="pivx-send-promo-loading">
                <div class="pivx-spinner"></div>
                <span>Loading...</span>
            </div>
        `;
    }

    if (domPivxSendOverlay) {
        domPivxSendOverlay.classList.add('active');
    }

    // Fetch promos with balances
    try {
        pivxSendPromos = await invoke('pivx_refresh_balances');
        // Filter to only promos with balance, sort by amount descending
        const promosWithBalance = pivxSendPromos
            .filter(p => p.balance_piv > 0)
            .sort((a, b) => b.balance_piv - a.balance_piv);

        pivxSendAvailableBalance = promosWithBalance.reduce((sum, p) => sum + p.balance_piv, 0);
        if (availableEl) availableEl.textContent = pivxSendAvailableBalance.toFixed(2);

        if (promoListEl) {
            if (promosWithBalance.length === 0) {
                promoListEl.innerHTML = `
                    <div class="pivx-send-promo-empty">
                        No funds available to send.<br>
                        Deposit PIVX first.
                    </div>
                `;
            } else {
                promoListEl.innerHTML = promosWithBalance.map(promo => `
                    <div class="pivx-send-promo-item" data-code="${promo.gift_code}" data-amount="${promo.balance_piv}">
                        <span class="pivx-send-promo-item-amount">${promo.balance_piv.toFixed(2)} PIV</span>
                        <span class="pivx-send-promo-item-code">${promo.gift_code}</span>
                    </div>
                `).join('');

                // Add click handlers and staggered animation
                const items = promoListEl.querySelectorAll('.pivx-send-promo-item');
                const totalAnimTime = 0.3;
                const maxDelay = 0.06;
                const staggerDelay = items.length > 1 ? Math.min(maxDelay, totalAnimTime / (items.length - 1)) : 0;

                items.forEach((item, index) => {
                    item.onclick = () => selectPivxSendPromo(item);
                    item.style.animationDelay = `${index * staggerDelay}s`;
                    item.classList.add('animate-in');
                });
            }
        }
    } catch (err) {
        console.error('Failed to fetch promos for send:', err);
        pivxSendAvailableBalance = 0;
        pivxSendPromos = [];
        if (availableEl) availableEl.textContent = '0.00';
        if (promoListEl) {
            promoListEl.innerHTML = `
                <div class="pivx-send-promo-empty">
                    Failed to load wallet.
                </div>
            `;
        }
    } finally {
        // Re-enable send button after loading
        if (confirmBtn) {
            confirmBtn.classList.remove('loading');
            confirmBtn.disabled = false;
        }
    }
}

/**
 * Select a promo for quick send
 */
function selectPivxSendPromo(itemEl) {
    // Deselect others
    document.querySelectorAll('.pivx-send-promo-item').forEach(el => {
        el.classList.remove('selected');
    });

    // Select this one
    itemEl.classList.add('selected');
    pivxSendSelectedPromo = {
        gift_code: itemEl.dataset.code,
        amount: parseFloat(itemEl.dataset.amount)
    };
}

/**
 * Toggle to custom amount mode
 */
function showPivxSendCustomMode() {
    pivxSendMode = 'custom';
    pivxSendSelectedPromo = null;

    const promoSectionEl = document.getElementById('pivx-send-promo-section');
    const customSectionEl = document.getElementById('pivx-send-custom-section');

    if (promoSectionEl) promoSectionEl.style.display = 'none';
    if (customSectionEl) customSectionEl.style.display = '';
}

/**
 * Toggle back to quick send mode
 */
function showPivxSendQuickMode() {
    pivxSendMode = 'quick';

    const promoSectionEl = document.getElementById('pivx-send-promo-section');
    const customSectionEl = document.getElementById('pivx-send-custom-section');
    const amountEl = document.getElementById('pivx-send-amount');

    if (promoSectionEl) promoSectionEl.style.display = '';
    if (customSectionEl) customSectionEl.style.display = 'none';
    if (amountEl) amountEl.value = '';
}

/**
 * Closes the send dialog
 */
function closePivxSendDialog() {
    if (domPivxSendOverlay) {
        domPivxSendOverlay.classList.remove('active');
    }
}

/**
 * Shows the settings dialog with current wallet address and currency selector
 */
async function showPivxSettingsDialog() {
    // Show dialog immediately
    if (domPivxSettingsOverlay) {
        domPivxSettingsOverlay.classList.add('active');
    }

    // Load wallet address (fast local query)
    invoke('pivx_get_wallet_address').then(address => {
        const addressInput = document.getElementById('pivx-wallet-address-input');
        if (addressInput) {
            addressInput.value = address || '';
        }
    }).catch(err => {
        console.error('Failed to get wallet address:', err);
    });

    // Load currency selector (may be slow, API call)
    const currencySelect = document.getElementById('pivx-currency-select');
    if (currencySelect) {
        // Show loading state
        currencySelect.innerHTML = '<option value="">Loading...</option>';
        currencySelect.disabled = true;

        Promise.all([
            fetchPivxCurrencies(),
            invoke('pivx_get_preferred_currency').catch(() => null)
        ]).then(([currencies, savedCurrency]) => {
            const currentCurrency = savedCurrency || pivxPreferredCurrency || detectDefaultCurrency();
            pivxPreferredCurrency = currentCurrency;

            currencySelect.innerHTML = '';
            for (const curr of currencies) {
                const option = document.createElement('option');
                option.value = curr.currency.toUpperCase();
                option.textContent = curr.currency.toUpperCase();
                if (curr.currency.toUpperCase() === currentCurrency.toUpperCase()) {
                    option.selected = true;
                }
                currencySelect.appendChild(option);
            }
            currencySelect.disabled = false;
        }).catch(err => {
            console.error('Failed to load currencies:', err);
            currencySelect.innerHTML = '<option value="USD">USD</option>';
            currencySelect.disabled = false;
        });
    }
}

/**
 * Closes the settings dialog
 */
function closePivxSettingsDialog() {
    if (domPivxSettingsOverlay) {
        domPivxSettingsOverlay.classList.remove('active');
    }
}

// Withdraw dialog state
let pivxWithdrawAvailableBalance = 0;

/**
 * Shows the withdraw dialog
 */
async function showPivxWithdrawDialog() {
    const withdrawOverlay = document.getElementById('pivx-withdraw-overlay');
    const addressInput = document.getElementById('pivx-withdraw-address');
    const amountInput = document.getElementById('pivx-withdraw-amount');
    const availableEl = document.getElementById('pivx-withdraw-available-amount');
    const confirmBtn = document.getElementById('pivx-withdraw-confirm');

    // Reset inputs
    if (addressInput) addressInput.value = '';
    if (amountInput) amountInput.value = '';
    if (confirmBtn) {
        confirmBtn.disabled = false;
        confirmBtn.textContent = 'Withdraw';
    }

    // Get available balance
    try {
        pivxWithdrawAvailableBalance = await invoke('pivx_get_wallet_balance');
        if (availableEl) {
            availableEl.textContent = pivxWithdrawAvailableBalance.toFixed(2);
        }
    } catch (err) {
        console.error('Failed to get balance:', err);
        pivxWithdrawAvailableBalance = 0;
        if (availableEl) availableEl.textContent = '0.00';
    }

    if (withdrawOverlay) {
        withdrawOverlay.classList.add('active');
    }
}

/**
 * Closes the withdraw dialog
 */
function closePivxWithdrawDialog() {
    const withdrawOverlay = document.getElementById('pivx-withdraw-overlay');
    if (withdrawOverlay) {
        withdrawOverlay.classList.remove('active');
    }
}

/**
 * Executes a PIVX withdrawal
 */
async function executePivxWithdraw() {
    const addressInput = document.getElementById('pivx-withdraw-address');
    const amountInput = document.getElementById('pivx-withdraw-amount');
    const confirmBtn = document.getElementById('pivx-withdraw-confirm');

    const address = addressInput?.value?.trim() || '';
    const amount = parseFloat(amountInput?.value || '0');

    // Validate address
    if (!address || !address.startsWith('D') || address.length < 30 || address.length > 36) {
        showToast('Invalid PIVX address');
        return;
    }

    // Validate amount
    if (amount <= 0) {
        showToast('Enter a valid amount');
        return;
    }

    if (amount > pivxWithdrawAvailableBalance) {
        showToast('Insufficient balance');
        return;
    }

    // Disable button during withdraw
    if (confirmBtn) {
        confirmBtn.disabled = true;
        confirmBtn.textContent = 'Withdrawing...';
    }

    try {
        const result = await invoke('pivx_withdraw', {
            destAddress: address,
            amountPiv: amount
        });

        closePivxWithdrawDialog();
        showToast(`Withdrawn ${amount.toFixed(2)} PIV`);
        refreshPivxWallet();

        // Log change if any
        if (result.change_piv > 0) {
            console.log(`Withdrawal change: ${result.change_piv} PIV saved to new promo`);
        }
    } catch (err) {
        console.error('Withdrawal failed:', err);
        showToast('Withdrawal failed: ' + (err.message || err));
    } finally {
        if (confirmBtn) {
            confirmBtn.disabled = false;
            confirmBtn.textContent = 'Withdraw';
        }
    }
}

/**
 * Sends a PIVX payment to the current chat
 */
async function sendPivxPayment() {
    const confirmBtn = document.getElementById('pivx-send-confirm');

    if (!strOpenChat) {
        showToast('No chat selected');
        return;
    }

    // Disable button during send
    if (confirmBtn) {
        confirmBtn.disabled = true;
        confirmBtn.textContent = 'Sending...';
    }

    try {
        if (pivxSendMode === 'quick') {
            // Quick send mode - send an existing whole promo
            if (!pivxSendSelectedPromo) {
                showToast('Select an amount to send');
                if (confirmBtn) {
                    confirmBtn.disabled = false;
                    confirmBtn.textContent = 'Send to Chat';
                }
                return;
            }

            await invoke('pivx_send_existing_promo', {
                receiver: strOpenChat,
                giftCode: pivxSendSelectedPromo.gift_code
            });

            closePivxSendDialog();
            showToast(`Sent ${pivxSendSelectedPromo.amount.toFixed(2)} PIV`);
            refreshPivxWallet();
        } else {
            // Custom amount mode
            const amountEl = document.getElementById('pivx-send-amount');
            const amount = parseFloat(amountEl?.value || '0');

            if (amount <= 0) {
                showToast('Enter a valid amount');
                if (confirmBtn) {
                    confirmBtn.disabled = false;
                    confirmBtn.textContent = 'Send to Chat';
                }
                return;
            }

            if (amount > pivxSendAvailableBalance) {
                showToast(`Insufficient funds (max: ${pivxSendAvailableBalance.toFixed(2)} PIV)`);
                if (confirmBtn) {
                    confirmBtn.disabled = false;
                    confirmBtn.textContent = 'Send to Chat';
                }
                return;
            }

            // Send custom amount via coin selection
            await invoke('pivx_send_payment', {
                receiver: strOpenChat,
                amountPiv: amount
            });

            closePivxSendDialog();
            showToast(`Sent ${amount.toFixed(2)} PIV`);
            refreshPivxWallet();
        }
    } catch (err) {
        console.error('Failed to send PIVX payment:', err);
        showToast('Failed to send: ' + (err.message || err));
    } finally {
        if (confirmBtn) {
            confirmBtn.disabled = false;
            confirmBtn.textContent = 'Send to Chat';
        }
    }
}

/**
 * Saves the PIVX wallet settings
 */
async function savePivxSettings() {
    const addressInput = document.getElementById('pivx-wallet-address-input');
    const currencySelect = document.getElementById('pivx-currency-select');
    const address = addressInput?.value?.trim() || '';
    const currency = currencySelect?.value || '';

    // Basic validation for PIVX address (starts with D, proper length)
    if (address && (!address.startsWith('D') || address.length < 30 || address.length > 36)) {
        showToast('Invalid PIVX address format');
        return;
    }

    try {
        // Save address (empty string clears the setting)
        await invoke('pivx_set_wallet_address', { address });

        if (currency) {
            await invoke('pivx_set_preferred_currency', { currency });
            // Update cached preference and refresh price
            const oldCurrency = pivxPreferredCurrency;
            pivxPreferredCurrency = currency;
            // Re-fetch price if currency changed
            if (oldCurrency !== currency) {
                pivxCurrentPrice = null;
                fetchPivxPrice();
            }
        }

        closePivxSettingsDialog();
        showToast('Wallet settings saved');

        // Refresh wallet to show updated fiat value
        refreshPivxWallet();
    } catch (err) {
        console.error('Failed to save settings:', err);
        showToast('Failed to save settings');
    }
}

/**
 * Claims a PIVX payment from a received message
 * @param {string} giftCode - The promo code to claim
 * @param {HTMLElement} bubbleEl - The payment bubble element
 */
async function claimPivxPayment(giftCode, bubbleEl) {
    if (!giftCode) return;
    if (bubbleEl?.classList.contains('claimed')) return;
    if (bubbleEl?.classList.contains('claiming')) return; // Prevent double-click

    // Mark as claiming to prevent multiple clicks
    if (bubbleEl) {
        bubbleEl.classList.add('claiming');
    }

    // Update hint to show progress
    const hint = bubbleEl?.querySelector('.msg-pivx-payment-hint');
    if (hint) {
        hint.textContent = 'Claiming...';
    }

    try {
        const result = await invoke('pivx_claim_from_message', { giftCode });

        if (bubbleEl) {
            bubbleEl.classList.remove('claiming');
            bubbleEl.classList.add('claimed');
        }
        if (hint) {
            hint.textContent = 'Claimed!';
        }

        showToast(`Claimed ${result.amount_piv?.toFixed(2) || ''} PIV`);
        refreshPivxWallet();
    } catch (err) {
        console.error('Failed to claim PIVX:', err);
        if (bubbleEl) {
            bubbleEl.classList.remove('claiming');
        }
        if (hint) {
            hint.textContent = 'Claim failed - tap to retry';
        }
        showToast('Failed to claim: ' + (err.message || err));
    }
}

/**
 * Renders a PIVX payment bubble for a message
 * @param {string} giftCode - The promo code
 * @param {number} amountPiv - Amount in PIV
 * @param {boolean} isMine - Whether this is my payment (sent by me)
 * @param {string} address - Optional PIVX address for balance checking
 * @returns {HTMLElement} The payment bubble element
 */
function renderPivxPaymentBubble(giftCode, amountPiv, isMine, address) {
    const bubble = document.createElement('div');
    bubble.className = 'msg-pivx-payment';
    bubble.dataset.giftCode = giftCode;
    if (address) bubble.dataset.address = address;

    // PIVX logo image
    const img = document.createElement('img');
    img.src = './icons/pivx.svg';
    bubble.appendChild(img);

    // Amount and hint on the right
    const info = document.createElement('div');
    info.className = 'msg-pivx-payment-info';

    const amountDiv = document.createElement('div');
    amountDiv.className = 'msg-pivx-payment-amount';
    amountDiv.textContent = `${amountPiv.toFixed(2)} PIV`;
    info.appendChild(amountDiv);

    // Show fiat equivalent if we have a cached price
    if (pivxCurrentPrice?.value && pivxPreferredCurrency) {
        const fiatValue = amountPiv * pivxCurrentPrice.value;
        const fiatDiv = document.createElement('div');
        fiatDiv.className = 'msg-pivx-payment-fiat';
        fiatDiv.textContent = `~${fiatValue.toFixed(2)} ${pivxPreferredCurrency}`;
        info.appendChild(fiatDiv);
    }

    const hint = document.createElement('div');
    hint.className = 'msg-pivx-payment-hint';

    // If address is available, start in syncing state while we check balance
    if (address) {
        bubble.classList.add('syncing');
        hint.textContent = 'Syncing...';
    } else {
        hint.textContent = isMine ? 'Click to reclaim' : 'Click to claim';
    }
    info.appendChild(hint);

    bubble.appendChild(info);

    // Make the whole bubble clickable (disabled if claimed/syncing)
    bubble.onclick = () => {
        if (!bubble.classList.contains('claimed') && !bubble.classList.contains('syncing')) {
            claimPivxPayment(giftCode, bubble);
        }
    };

    // If address is available, check balance to determine claimed state
    if (address) {
        checkPivxPaymentClaimedState(bubble, address, hint, isMine);
    }

    return bubble;
}

/**
 * Check if a PIVX payment has been claimed by checking the address balance
 * @param {HTMLElement} bubble - The payment bubble element
 * @param {string} address - PIVX address to check
 * @param {HTMLElement} hintEl - The hint element to update
 * @param {boolean} isMine - Whether this is my payment
 * @param {number} retryCount - Number of retries attempted (for unconfirmed tx propagation)
 */
async function checkPivxPaymentClaimedState(bubble, address, hintEl, isMine, retryCount = 0) {
    try {
        // Use force=true on retries to bypass cache
        const force = retryCount > 0;
        const balance = await __TAURI__.core.invoke('pivx_check_address_balance', { address, force });
        bubble.classList.remove('syncing');
        if (balance <= 0) {
            // Balance is 0 - could be claimed OR unconfirmed tx not yet visible
            // Retry a few times with delay to handle tx propagation delay
            if (retryCount < 3) {
                bubble.classList.add('syncing');
                hintEl.textContent = 'Confirming...';
                setTimeout(() => {
                    checkPivxPaymentClaimedState(bubble, address, hintEl, isMine, retryCount + 1);
                }, 3000); // Retry after 3 seconds
                return;
            }
            // After retries, mark as claimed
            bubble.classList.add('claimed');
            hintEl.textContent = 'Claimed';
        } else {
            // Has balance - show claim option
            hintEl.textContent = isMine ? 'Click to reclaim' : 'Click to claim';
        }
    } catch (err) {
        // If balance check fails, allow claiming anyway
        console.warn('Failed to check PIVX payment balance:', err);
        bubble.classList.remove('syncing');
        hintEl.textContent = isMine ? 'Click to reclaim' : 'Click to claim';
    }
}

/**
 * Gets the display name for a chat
 * @param {string} chatId - The chat ID
 * @returns {string} The display name
 */
function getChatDisplayName(chatId) {
    // Check if it's a group chat
    const chat = arrChats.find(c => c.id === chatId);
    if (chat && chat.chat_type === 'MlsGroup') {
        return chat.metadata?.custom_fields?.name || `Group ${chatId.substring(0, 8)}...`;
    }

    // Otherwise it's a DM - get profile name
    const profile = getProfile(chatId);
    if (profile?.nickname) return profile.nickname;
    if (profile?.name) return profile.name;

    // Fallback to truncated pubkey
    return chatId.substring(0, 8) + '...';
}

// ========== End PIVX Wallet Functions ==========

/**
 * Animate attachment panel items with staggered fade-in effect
 */
function animateAttachmentPanelItems(container) {
    const items = container.querySelectorAll('.attachment-panel-item');
    const totalAnimTime = 0.35; // Total stagger duration in seconds
    const maxDelay = 0.08; // Cap at 80ms per item for small counts
    const staggerDelay = items.length > 1 ? Math.min(maxDelay, totalAnimTime / (items.length - 1)) : 0;

    items.forEach((item, index) => {
        // Remove any existing animation
        item.classList.remove('animate-in');
        item.style.animationDelay = '';

        // Force reflow to restart animation
        void item.offsetWidth;

        // Add animation with staggered delay
        item.style.animationDelay = `${index * staggerDelay}s`;
        item.classList.add('animate-in');

        // Remove animate-in class when animation finishes to avoid conflicts with other animations
        item.addEventListener('animationend', () => {
            item.classList.remove('animate-in');
            item.style.animationDelay = '';
        }, { once: true });
    });
}

/**
 * Loads and renders the Mini Apps history in the panel
 * PIVX is treated as a virtual app and positioned based on usage history
 */
async function loadMiniAppsHistory() {
    try {
        let history = await invoke('miniapp_get_history', { limit: null });

        // Pre-install default apps for new users (empty history)
        const preInstallDone = localStorage.getItem('miniapps_preinstall_done') === 'true';
        if (history.length === 0 && !preInstallDone) {
            // Mark as done first to prevent re-triggering
            localStorage.setItem('miniapps_preinstall_done', 'true');

            // Clear any existing items first (except Marketplace button)
            const existingItems = domMiniAppsGrid.querySelectorAll('.attachment-panel-item:not(#attachment-panel-marketplace), .attachment-panel-empty');
            existingItems.forEach(item => item.remove());

            // Default apps to pre-install (by app ID with display names)
            const defaultApps = [
                { id: 'vectify', name: 'Vectify' },
                { id: 'deadlock', name: 'State of Surveillance' }
            ];

            // IMMEDIATELY show placeholder UI before fetching marketplace data
            const staggerDelay = 0.03; // Same delay used for regular mini apps
            let animIndex = 0;

            for (const { id: appId, name: displayName } of defaultApps) {
                const item = document.createElement('button');
                item.className = 'attachment-panel-item attachment-panel-miniapp animate-in';
                item.id = `miniapp-downloading-${appId}`;
                item.draggable = false;
                item.style.position = 'relative';
                item.style.animationDelay = `${animIndex * staggerDelay}s`;

                // Show generic loading state initially
                item.innerHTML = `
                    <div class="attachment-panel-btn attachment-panel-miniapp-btn">
                        <span class="icon icon-play"></span>
                        <div class="miniapp-downloading-overlay">
                            <div class="miniapp-downloading-spinner" data-app-id="${escapeHtml(appId)}"></div>
                        </div>
                    </div>
                    <span class="attachment-panel-label cutoff">${escapeHtml(displayName)}</span>
                `;
                item.dataset.appName = displayName.toLowerCase();
                item.addEventListener('animationend', () => {
                    item.classList.remove('animate-in');
                    item.style.animationDelay = '';
                }, { once: true });
                domMiniAppsGrid.appendChild(item);
                animIndex++;
            }

            // Add PIVX after the downloading apps
            const pivxBtn = document.createElement('button');
            pivxBtn.className = 'attachment-panel-item animate-in';
            pivxBtn.id = 'attachment-panel-pivx';
            pivxBtn.draggable = false;
            pivxBtn.style.animationDelay = `${animIndex * staggerDelay}s`;
            pivxBtn.innerHTML = `
                <div class="attachment-panel-btn attachment-panel-pivx-btn">
                    <span class="icon icon-pivx"></span>
                </div>
                <span class="attachment-panel-label">PIVX</span>
            `;
            pivxBtn.dataset.appName = 'pivx';
            pivxBtn.addEventListener('mouseenter', () => showGlobalTooltip('PIVX Wallet', pivxBtn));
            pivxBtn.addEventListener('mouseleave', () => hideGlobalTooltip());
            pivxBtn.addEventListener('animationend', () => {
                pivxBtn.classList.remove('animate-in');
                pivxBtn.style.animationDelay = '';
            }, { once: true });
            pivxBtn.onclick = () => {
                if (miniAppsEditMode) return;
                hideGlobalTooltip();
                showPivxWalletPanel();
            };
            domMiniAppsGrid.appendChild(pivxBtn);

            // Now fetch marketplace data and start downloads in background
            try {
                await fetchMarketplaceApps(true);

                const appsToInstall = [];

                // Update placeholders with actual app metadata and start downloads
                for (const { id: appId } of defaultApps) {
                    const app = marketplaceApps.find(a => a.id === appId);
                    const placeholder = document.getElementById(`miniapp-downloading-${appId}`);

                    if (app && placeholder) {
                        appsToInstall.push(app);

                        // Update with actual icon and name
                        const iconSrc = app.icon_cached ? convertFileSrc(app.icon_cached) : app.icon_url;
                        const iconHtml = iconSrc
                            ? `<img src="${escapeHtml(iconSrc)}" style="width: 100%; height: 100%; object-fit: cover; border-radius: inherit;" onerror="this.outerHTML='<span class=\\'icon icon-play\\'></span>'">`
                            : '<span class="icon icon-play"></span>';

                        placeholder.innerHTML = `
                            <div class="attachment-panel-btn attachment-panel-miniapp-btn">
                                ${iconHtml}
                                <div class="miniapp-downloading-overlay">
                                    <div class="miniapp-downloading-spinner" data-app-id="${escapeHtml(appId)}"></div>
                                </div>
                            </div>
                            <span class="attachment-panel-label cutoff">${escapeHtml(app.name)}</span>
                        `;
                        placeholder.dataset.appName = app.name.toLowerCase();
                    } else if (!app && placeholder) {
                        console.warn(`[Mini Apps] Default app "${appId}" not found in marketplace`);
                        placeholder.remove();
                    }
                }

                // Start all downloads in parallel - transform placeholders when complete
                const installPromises = appsToInstall.map(async (app) => {
                    try {
                        await installMarketplaceApp(app.id);

                        // Transform the downloading placeholder into a normal clickable app
                        const placeholder = document.getElementById(`miniapp-downloading-${app.id}`);
                        if (placeholder) {
                            // Remove the downloading overlay
                            const overlay = placeholder.querySelector('.miniapp-downloading-overlay');
                            if (overlay) overlay.remove();

                            // Remove the temporary ID
                            placeholder.removeAttribute('id');

                            // Add click handler to open the app
                            placeholder.onclick = async () => {
                                if (miniAppsEditMode) return;
                                hideGlobalTooltip();
                                // Fetch the updated app info from history
                                const historyApps = await invoke('miniapp_get_history', { limit: null });
                                const historyApp = historyApps.find(h => h.marketplace_id === app.id || h.name === app.name);
                                if (historyApp) {
                                    await openMiniAppFromHistory(historyApp);
                                }
                            };

                            // Add tooltip
                            placeholder.addEventListener('mouseenter', () => showGlobalTooltip(app.name, placeholder));
                            placeholder.addEventListener('mouseleave', () => hideGlobalTooltip());
                        }

                        return { success: true, app };
                    } catch (installErr) {
                        console.error(`[Mini Apps] Failed to pre-install ${app.id}:`, installErr);
                        // Remove failed placeholder
                        const placeholder = document.getElementById(`miniapp-downloading-${app.id}`);
                        if (placeholder) placeholder.remove();
                        return { success: false, app, error: installErr };
                    }
                });

                // Wait for all installs to complete (UI updates happen individually above)
                await Promise.all(installPromises);

                // Return early - don't do the normal render since we already set up the UI
                return;
            } catch (preInstallErr) {
                console.error('[Mini Apps] Pre-install failed:', preInstallErr);
            }
        }

        // Clear existing Mini App items (keep only Marketplace)
        const existingItems = domMiniAppsGrid.querySelectorAll('.attachment-panel-item:not(#attachment-panel-marketplace), .attachment-panel-empty');
        existingItems.forEach(item => item.remove());

        // Check if PIVX is hidden by user
        const pivxHidden = localStorage.getItem('pivx_hidden') === 'true';

        // Get PIVX last used timestamp from localStorage (stored in ms, convert to seconds)
        const pivxLastUsedMs = parseInt(localStorage.getItem('pivx_last_used') || '0', 10);
        const pivxLastOpenedAt = Math.floor(pivxLastUsedMs / 1000);

        // Create combined list of apps with PIVX as a virtual entry (if not hidden)
        const allApps = [
            // Add PIVX as a virtual app entry (only if not hidden)
            ...(!pivxHidden ? [{ name: 'PIVX', isPivx: true, last_opened_at: pivxLastOpenedAt }] : []),
            // Add history apps with their timestamps (backend uses last_opened_at in seconds)
            ...history.map(app => ({ ...app, isPivx: false, last_opened_at: app.last_opened_at || 0 }))
        ];

        // Sort by last_opened_at timestamp (most recent first)
        allApps.sort((a, b) => b.last_opened_at - a.last_opened_at);

        // Add all app items in sorted order
        for (const app of allApps) {
            if (app.isPivx) {
                // Create PIVX button
                const pivxBtn = document.createElement('button');
                pivxBtn.className = 'attachment-panel-item';
                pivxBtn.id = 'attachment-panel-pivx';
                pivxBtn.draggable = false;
                pivxBtn.innerHTML = `
                    <div class="attachment-panel-btn attachment-panel-pivx-btn">
                        <span class="icon icon-pivx"></span>
                    </div>
                    <span class="attachment-panel-label">PIVX</span>
                `;
                // Store app name for search filtering
                pivxBtn.dataset.appName = 'pivx';

                // Add tooltip on hover
                pivxBtn.addEventListener('mouseenter', () => {
                    showGlobalTooltip('PIVX Wallet', pivxBtn);
                });
                pivxBtn.addEventListener('mouseleave', () => {
                    hideGlobalTooltip();
                });

                pivxBtn.onclick = () => {
                    // Don't launch if in edit mode
                    if (miniAppsEditMode) return;
                    hideGlobalTooltip();
                    showPivxWalletPanel();
                };
                domMiniAppsGrid.appendChild(pivxBtn);
            } else {
                // Create Mini App button
                const item = document.createElement('button');
                item.className = 'attachment-panel-item attachment-panel-miniapp';
                item.draggable = false;

                // Start with a placeholder icon, then load the actual icon
                item.innerHTML = `
                    <div class="attachment-panel-btn attachment-panel-miniapp-btn">
                        <span class="icon icon-play"></span>
                    </div>
                    <span class="attachment-panel-label cutoff">${escapeHtml(app.name)}</span>
                `;

                // Store the app name for search filtering
                item.dataset.appName = app.name.toLowerCase();

                // Add tooltip on hover for the entire item
                item.addEventListener('mouseenter', () => {
                    showGlobalTooltip(app.name, item);
                });
                item.addEventListener('mouseleave', () => {
                    hideGlobalTooltip();
                });

                item.onclick = async () => {
                    // Don't launch if in edit mode
                    if (miniAppsEditMode) return;
                    hideGlobalTooltip();
                    // Open the Mini App using the stored attachment reference
                    await openMiniAppFromHistory(app);
                };
                domMiniAppsGrid.appendChild(item);

                // Load the Mini App icon asynchronously
                loadMiniAppIcon(app, item.querySelector('.attachment-panel-btn'));
            }
        }

        // If no apps at all (only PIVX which is always there), show empty message
        if (history.length === 0 && pivxLastOpenedAt === 0) {
            const emptyMsg = document.createElement('div');
            emptyMsg.className = 'attachment-panel-empty';
            emptyMsg.textContent = 'No recent Mini Apps';
            domMiniAppsGrid.appendChild(emptyMsg);
        }
    } catch (e) {
        console.error('Failed to load Mini Apps history:', e);
    }
}

/**
 * Filters the Mini Apps grid based on search query
 * Hides Marketplace when search is active
 * @param {string} query - The search query
 */
function filterMiniApps(query) {
    const normalizedQuery = query.toLowerCase().trim();
    const isSearching = normalizedQuery.length > 0;

    // Get all items in the grid
    const items = domMiniAppsGrid.querySelectorAll('.attachment-panel-item');
    let visibleCount = 0;

    items.forEach(item => {
        const appName = item.dataset.appName;

        // Always hide Marketplace when searching
        if (item.id === 'attachment-panel-marketplace') {
            item.classList.toggle('hidden-by-search', isSearching);
            if (!isSearching) visibleCount++;
            return;
        }

        // Filter all apps (including PIVX) by name
        if (appName) {
            const matches = !isSearching || appName.includes(normalizedQuery);
            item.classList.toggle('hidden-by-search', !matches);
            if (matches) visibleCount++;
        }
    });

    // Also hide/show empty message based on visible items
    const emptyMsg = domMiniAppsGrid.querySelector('.attachment-panel-empty');
    if (emptyMsg) {
        emptyMsg.classList.toggle('hidden-by-search', isSearching);
    }

    // Show/hide "no results" message
    let noResultsMsg = domMiniAppsGrid.querySelector('.miniapps-no-results');
    if (isSearching && visibleCount === 0) {
        if (!noResultsMsg) {
            noResultsMsg = document.createElement('div');
            noResultsMsg.className = 'miniapps-no-results';
            noResultsMsg.innerHTML = `
                <p>No Mini Apps found</p>
                <p class="miniapps-no-results-hint">Try a different search, or check out the Nexus!</p>
            `;
            domMiniAppsGrid.appendChild(noResultsMsg);
        }
        noResultsMsg.style.display = '';
    } else if (noResultsMsg) {
        noResultsMsg.style.display = 'none';
    }
}

// ========== Mini Apps Edit Mode ==========

let miniAppsEditMode = false;
let miniAppsHoldTimer = null;
let miniAppsEditModeJustActivated = false;

/**
 * Activates edit mode for the Mini Apps grid
 * Shows red X badges on all apps (except Marketplace) for removal
 */
function activateMiniAppsEditMode() {
    if (miniAppsEditMode) return;
    miniAppsEditMode = true;
    miniAppsEditModeJustActivated = true;

    // Add edit-mode class for wobble animation
    domMiniAppsGrid.classList.add('edit-mode');

    // Add delete badges to all apps except Marketplace
    const items = domMiniAppsGrid.querySelectorAll('.attachment-panel-item:not(#attachment-panel-marketplace)');
    items.forEach(item => {
        // Don't add badge if already exists
        if (item.querySelector('.miniapp-delete-badge')) return;

        const badge = document.createElement('div');
        badge.className = 'miniapp-delete-badge';
        badge.innerHTML = '<span class="icon icon-x"></span>';

        // Get app info for deletion
        const appName = item.dataset.appName;
        const isPivx = item.id === 'attachment-panel-pivx';

        badge.onclick = async (e) => {
            e.stopPropagation();
            e.preventDefault();

            const displayName = isPivx ? 'PIVX Wallet' : item.querySelector('.attachment-panel-label')?.textContent || appName;

            const confirmed = await popupConfirm(
                'Remove App?',
                `Are you sure you want to remove <b>${escapeHtml(displayName)}</b> from your recent Mini Apps?`,
                false
            );

            if (confirmed) {
                if (isPivx) {
                    // For PIVX, set hidden flag (can be restored in Settings)
                    localStorage.setItem('pivx_hidden', 'true');
                } else {
                    // For regular apps, remove from backend history
                    try {
                        await invoke('miniapp_remove_from_history', { name: displayName });
                    } catch (err) {
                        console.error('Failed to remove Mini App from history:', err);
                    }
                }

                // Deactivate edit mode and refresh the list
                deactivateMiniAppsEditMode();
                await loadMiniAppsHistory();
                animateAttachmentPanelItems(domMiniAppsGrid);
            }
        };

        item.appendChild(badge);
    });

    // Add global click listener to exit edit mode
    // Remove any existing listener first to prevent duplicates
    document.removeEventListener('click', handleEditModeClickOutside, true);
    // Add with a small delay so the mouseup click from hold doesn't immediately trigger it
    setTimeout(() => {
        if (miniAppsEditMode) {
            document.addEventListener('click', handleEditModeClickOutside, true);
        }
    }, 50);
}

/**
 * Deactivates edit mode for the Mini Apps grid
 */
function deactivateMiniAppsEditMode() {
    if (!miniAppsEditMode) return;
    miniAppsEditMode = false;
    miniAppsEditModeJustActivated = false;

    // Remove edit-mode class
    domMiniAppsGrid.classList.remove('edit-mode');

    // Remove all delete badges
    const badges = domMiniAppsGrid.querySelectorAll('.miniapp-delete-badge');
    badges.forEach(badge => badge.remove());

    // Remove global click listener
    document.removeEventListener('click', handleEditModeClickOutside, true);
}

/**
 * Handles clicks outside of delete badges to exit edit mode
 */
function handleEditModeClickOutside(e) {
    // If we just activated edit mode, ignore this click (it's from the hold release)
    if (miniAppsEditModeJustActivated) {
        miniAppsEditModeJustActivated = false;
        e.preventDefault();
        e.stopPropagation();
        return;
    }

    // If clicking on a delete badge, let it handle itself
    if (e.target.closest('.miniapp-delete-badge')) return;

    // If clicking on a popup, let it handle itself (don't exit edit mode)
    if (e.target.closest('#popup-container')) return;

    // Otherwise, deactivate edit mode
    e.preventDefault();
    e.stopPropagation();
    deactivateMiniAppsEditMode();
}

/**
 * Starts the hold timer for entering edit mode
 * @param {Event} e - The mousedown/touchstart event
 */
function startMiniAppHold(e) {
    // Don't start hold on Marketplace or if already in edit mode
    if (miniAppsEditMode) return;
    const item = e.target.closest('.attachment-panel-item');
    if (!item || item.id === 'attachment-panel-marketplace') return;

    // Prevent drag behavior on hold
    e.preventDefault();
    e.stopPropagation();

    // Clear any existing timer
    if (miniAppsHoldTimer) {
        clearTimeout(miniAppsHoldTimer);
    }

    miniAppsHoldTimer = setTimeout(() => {
        miniAppsHoldTimer = null;
        activateMiniAppsEditMode();
    }, 500); // 0.5 second hold
}

/**
 * Cancels the hold timer
 */
function cancelMiniAppHold() {
    if (miniAppsHoldTimer) {
        clearTimeout(miniAppsHoldTimer);
        miniAppsHoldTimer = null;
    }
}

/**
 * Suppresses click events right after edit mode activation
 */
function suppressClickAfterEditMode(e) {
    if (miniAppsEditModeJustActivated) {
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation();
    }
}

/**
 * Sets up hold-to-edit event listeners on the Mini Apps grid
 */
function setupMiniAppsEditMode() {
    if (!domMiniAppsGrid) return;

    // Prevent any drag behavior on the grid items
    domMiniAppsGrid.addEventListener('dragstart', (e) => {
        e.preventDefault();
        return false;
    });

    // Mouse events
    domMiniAppsGrid.addEventListener('mousedown', startMiniAppHold);
    domMiniAppsGrid.addEventListener('mouseup', cancelMiniAppHold);
    domMiniAppsGrid.addEventListener('mouseleave', cancelMiniAppHold);

    // Suppress clicks immediately after edit mode activation
    domMiniAppsGrid.addEventListener('click', suppressClickAfterEditMode, true);

    // Touch events for mobile
    domMiniAppsGrid.addEventListener('touchstart', startMiniAppHold, { passive: false });
    domMiniAppsGrid.addEventListener('touchend', cancelMiniAppHold);
    domMiniAppsGrid.addEventListener('touchcancel', cancelMiniAppHold);
    domMiniAppsGrid.addEventListener('touchmove', cancelMiniAppHold);
}

/**
 * Load Mini App icon asynchronously and update the button
 */
async function loadMiniAppIcon(app, btnElement) {
    try {
        const info = await invoke('miniapp_load_info', { filePath: app.src_url });
        if (info && info.icon_data) {
            // Replace the placeholder icon with the actual Mini App icon
            btnElement.innerHTML = `<img src="${info.icon_data}" alt="${escapeHtml(app.name)}" class="attachment-panel-miniapp-icon">`;
        }
    } catch (e) {
        // Keep the placeholder icon if loading fails
        console.debug('Failed to load Mini App icon:', e);
    }
}

/**
 * Opens a Mini App from history
 */
// Store the pending Mini App for the launch dialog
let pendingMiniAppLaunch = null;

/**
 * Check if a Mini App is a game based on its categories
 * @param {Object} app - The app object with categories field
 * @returns {boolean} True if the app is a game
 */
function isMiniAppGame(app) {
    // If no categories, default to game
    if (!app.categories) {
        return true;
    }
    
    // Categories can be a comma-separated string or an array
    let cats = app.categories;
    if (typeof cats === 'string') {
        cats = cats.split(',').map(c => c.trim().toLowerCase()).filter(c => c);
    } else if (Array.isArray(cats)) {
        cats = cats.map(c => c.toLowerCase());
    } else {
        return true; // Default to game
    }
    
    // It's a game if it has "game" tag OR doesn't have "app" tag
    return cats.includes('game') || !cats.includes('app');
}

/**
 * Show the Mini App launch dialog
 */
async function showMiniAppLaunchDialog(app) {
    pendingMiniAppLaunch = app;
    
    // Set the app name
    domMiniAppLaunchName.textContent = app.name;
    
    // Determine if this is a game or app and update button text accordingly
    const isGame = isMiniAppGame(app);
    const actionText = isGame ? 'Play' : 'Open';
    domMiniAppLaunchSolo.textContent = actionText;
    domMiniAppLaunchInvite.textContent = `${actionText} & Invite`;
    
    // Try to load the Mini App icon
    try {
        const info = await invoke('miniapp_load_info', { filePath: app.src_url });
        if (info && info.icon_data) {
            domMiniAppLaunchIconContainer.innerHTML = `<img src="${info.icon_data}" alt="${escapeHtml(app.name)}">`;
        } else {
            domMiniAppLaunchIconContainer.innerHTML = '<span class="icon icon-play"></span>';
        }
    } catch (e) {
        // Fallback to generic icon
        domMiniAppLaunchIconContainer.innerHTML = '<span class="icon icon-play"></span>';
    }
    
    // Show the overlay
    domMiniAppLaunchOverlay.classList.add('active');
}

/**
 * Close the Mini App launch dialog
 */
function closeMiniAppLaunchDialog() {
    domMiniAppLaunchOverlay.classList.remove('active');
    pendingMiniAppLaunch = null;
}

/**
 * Play Mini App solo (from original attachment)
 */
async function playMiniAppSolo() {
    if (!pendingMiniAppLaunch) return;

    const app = pendingMiniAppLaunch;
    closeMiniAppLaunchDialog();

    // Check permissions for marketplace apps
    const shouldContinue = await checkMiniAppPermissions(app);
    if (!shouldContinue) {
        return; // User cancelled
    }

    try {
        // Open the Mini App directly using the cached file path
        // Use a placeholder chat_id and message_id for solo play
        await invoke('miniapp_open', {
            filePath: app.src_url,
            chatId: 'solo',
            messageId: `solo_${Date.now()}`,
            href: null,
            topicId: null,
        });
    } catch (e) {
        console.error('Failed to open Mini App:', e);
    }
}

/**
 * Play Mini App and invite (send to current chat, then open from the new message)
 */
async function playMiniAppAndInvite() {
    if (!pendingMiniAppLaunch) return;

    const app = pendingMiniAppLaunch;
    const targetChatId = strOpenChat;

    // Check if we have an active chat
    if (!targetChatId) {
        console.error('No active chat to send Mini App to');
        closeMiniAppLaunchDialog();
        // Fallback to solo play
        await playMiniAppSoloInternal(app);
        return;
    }

    // Check permissions for marketplace apps before doing anything
    const shouldContinue = await checkMiniAppPermissions(app);
    if (!shouldContinue) {
        closeMiniAppLaunchDialog();
        return; // User cancelled
    }

    // Show loading state on the invite button
    const inviteBtn = domMiniAppLaunchInvite;
    const originalText = inviteBtn.textContent;
    inviteBtn.disabled = true;
    inviteBtn.innerHTML = '<span class="icon icon-loading"></span>';
    
    // Helper to reset button and close dialog
    const finishAndClose = () => {
        inviteBtn.disabled = false;
        inviteBtn.textContent = originalText;
        closeMiniAppLaunchDialog();
    };
    
    // Determine if this is a group chat (MLS) or DM
    const chat = arrChats.find(c => c.id === targetChatId);
    const isGroupChat = chat && chat.chat_type === 'MlsGroup';
    const eventName = isGroupChat ? 'mls_message_new' : 'message_new';
    
    // Set up a one-time listener to catch the new message and open the Mini App
    let unlisten = null;
    const timeoutId = setTimeout(() => {
        // Timeout after 30 seconds - just in case the message doesn't arrive
        if (unlisten) unlisten();
        console.warn('Timeout waiting for Mini App message to be sent');
        finishAndClose();
    }, 30000);
    
    unlisten = await listen(eventName, async (evt) => {
        const message = isGroupChat ? evt.payload?.message : evt.payload?.message;
        const chatId = isGroupChat ? evt.payload?.group_id : evt.payload?.chat_id;
        
        console.log('Play & Invite: Received message event', { chatId, targetChatId, messageId: message?.id, attachments: message?.attachments });
        
        // Log full attachment details for debugging
        if (message?.attachments) {
            message.attachments.forEach((a, i) => {
                console.log(`Play & Invite: Attachment ${i}:`, { name: a.name, filename: a.filename, path: a.path, mime: a.mime, ext: a.ext });
            });
        }
        
        // Check if this is our message in the target chat with a .miniapp or .xdc attachment
        if (chatId === targetChatId && message && message.attachments) {
            const miniAppAttachment = message.attachments.find(a => {
                const filename = a.name || a.filename || '';
                const path = a.path || '';
                const ext = a.ext || '';
                // Check for .xdc extensions
                const isMiniApp = filename.toLowerCase().endsWith('.xdc') ||
                                  path.toLowerCase().endsWith('.xdc') ||
                                  ext.toLowerCase() === 'xdc';
                return isMiniApp;
            });
            
            console.log('Play & Invite: Found miniapp attachment?', miniAppAttachment);
            
            if (miniAppAttachment) {
                // Found our Mini App message - clean up the message listener
                clearTimeout(timeoutId);
                if (unlisten) unlisten();
                
                const filePath = miniAppAttachment.path || app.src_url;
                
                // Check if this is a pending message - if so, wait for the real ID
                if (message.id.startsWith('pending')) {
                    console.log('Play & Invite: Message is pending, waiting for real ID...');
                    
                    // Track if we've already handled the update
                    let updateHandled = false;
                    let updateTimeoutId = null;
                    
                    // Set up a listener for the message_update event to get the real ID
                    const updateUnlisten = await listen('message_update', async (updateEvt) => {
                        if (updateHandled) return;
                        if (updateEvt.payload.old_id === message.id && updateEvt.payload.chat_id === targetChatId) {
                            updateHandled = true;
                            const realMessage = updateEvt.payload.message;
                            console.log('Play & Invite: Got real message ID:', realMessage.id);
                            
                            // Clear timeout and unlisten
                            if (updateTimeoutId) clearTimeout(updateTimeoutId);
                            updateUnlisten();
                            
                            // Get the topic ID from the real message's attachment
                            let topicId = null;
                            if (realMessage.attachments && realMessage.attachments.length > 0) {
                                const miniappAttachment = realMessage.attachments.find(a =>
                                    a.extension === 'xdc' || a.path?.endsWith('.xdc')
                                );
                                if (miniappAttachment) {
                                    topicId = miniappAttachment.webxdc_topic;
                                    console.log('Play & Invite: Got topic ID from attachment:', topicId);
                                }
                            }
                            
                            // Open the Mini App with the real message ID and topic
                            try {
                                await invoke('miniapp_open', {
                                    filePath: filePath,
                                    chatId: targetChatId,
                                    messageId: realMessage.id,
                                    href: null,
                                    topicId: topicId,
                                });
                            } catch (e) {
                                console.error('Failed to open Mini App from forwarded message:', e);
                            }
                            
                            finishAndClose();
                        }
                    });
                    
                    // Set a timeout for the update listener too
                    updateTimeoutId = setTimeout(() => {
                        if (updateHandled) return;
                        updateUnlisten();
                        console.warn('Timeout waiting for message update');
                        finishAndClose();
                    }, 30000);
                } else {
                    // Message already has a real ID, open immediately
                    console.log('Opening Mini App from forwarded message:', message.id, 'path:', filePath);
                    
                    // Get the topic ID from the message's attachment
                    let topicId = null;
                    if (message.attachments && message.attachments.length > 0) {
                        const miniappAttachment = message.attachments.find(a =>
                            a.extension === 'xdc' || a.path?.endsWith('.xdc')
                        );
                        if (miniappAttachment) {
                            topicId = miniappAttachment.webxdc_topic;
                            console.log('Play & Invite: Got topic ID from attachment:', topicId);
                        }
                    }
                    
                    try {
                        await invoke('miniapp_open', {
                            filePath: filePath,
                            chatId: targetChatId,
                            messageId: message.id,
                            href: null,
                            topicId: topicId,
                        });
                    } catch (e) {
                        console.error('Failed to open Mini App from forwarded message:', e);
                    }
                    
                    finishAndClose();
                }
            }
        }
    });
    
    try {
        // Send the Mini App file to the current chat
        await invoke('file_message', {
            receiver: targetChatId,
            repliedTo: '',
            filePath: app.src_url,
        });
        
        console.log('Mini App sent to chat successfully');
    } catch (e) {
        console.error('Failed to send Mini App to chat:', e);
        // Clean up listener
        clearTimeout(timeoutId);
        if (unlisten) unlisten();
        // Close dialog and reset button
        finishAndClose();
        // Fallback to solo play if sending fails
        await playMiniAppSoloInternal(app);
    }
}

/**
 * Check and show permission prompt for a Mini App from history
 * @param {Object} app - The app from history (MiniAppHistoryEntry)
 * @returns {Promise<boolean>} True if we should continue opening, false if cancelled
 */
async function checkMiniAppPermissions(app) {
    // Only marketplace apps have permissions
    if (!app.marketplace_id) {
        return true;
    }

    try {
        // Get the marketplace app info to check for requested permissions and blossom_hash
        const marketplaceApp = await invoke('marketplace_get_app', { appId: app.marketplace_id });

        if (!marketplaceApp || !marketplaceApp.requested_permissions || marketplaceApp.requested_permissions.length === 0) {
            return true; // No permissions requested
        }

        // Use blossom_hash as the permission identifier (content-based security)
        if (!marketplaceApp.blossom_hash) {
            return true; // No hash available, continue
        }

        // Check if we've already prompted for permissions using the file hash
        const hasBeenPrompted = await invoke('miniapp_has_permission_prompt', { fileHash: marketplaceApp.blossom_hash });

        if (hasBeenPrompted) {
            return true; // Already prompted, continue
        }

        // Show the permission prompt (using the function from marketplace.js)
        const userGranted = await showPermissionPrompt(marketplaceApp);
        return userGranted;
    } catch (e) {
        console.error('Failed to check Mini App permissions:', e);
        return true; // Continue on error
    }
}

/**
 * Check and show permission prompt for a Mini App opened from a chat attachment
 * Uses the file hash to look up if there's a matching marketplace app with permissions
 * @param {string} filePath - Path to the .xdc file
 * @returns {Promise<boolean>} True if we should continue opening, false if cancelled
 */
async function checkChatMiniAppPermissions(filePath) {
    try {
        // Load the Mini App info to get the file hash
        const miniAppInfo = await invoke('miniapp_load_info', { filePath });
        if (!miniAppInfo || !miniAppInfo.file_hash) {
            return true; // No hash available, continue
        }

        const fileHash = miniAppInfo.file_hash;

        // Check if we've already prompted for permissions using this file hash
        const hasBeenPrompted = await invoke('miniapp_has_permission_prompt', { fileHash });
        if (hasBeenPrompted) {
            return true; // Already prompted, continue
        }

        // Look up if there's a marketplace app with this hash
        const marketplaceApp = await invoke('marketplace_get_app_by_hash', { fileHash });
        if (!marketplaceApp || !marketplaceApp.requested_permissions || marketplaceApp.requested_permissions.length === 0) {
            return true; // No matching marketplace app or no permissions requested
        }

        // Show the permission prompt (using the function from marketplace.js)
        const userGranted = await showPermissionPrompt(marketplaceApp);
        return userGranted;
    } catch (e) {
        console.error('Failed to check chat Mini App permissions:', e);
        return true; // Continue on error
    }
}

/**
 * Internal function to play Mini App solo
 */
async function playMiniAppSoloInternal(app) {
    try {
        // Check permissions for marketplace apps
        const shouldContinue = await checkMiniAppPermissions(app);
        if (!shouldContinue) {
            return; // User cancelled
        }

        // Open the Mini App directly using the cached file path
        await invoke('miniapp_open', {
            filePath: app.src_url,
            chatId: 'solo',
            messageId: `solo_${Date.now()}`,
            href: null,
            topicId: null,
        });
    } catch (e) {
        console.error('Failed to open Mini App:', e);
    }
}

/**
 * Open Mini App from history - shows the launch dialog
 */
async function openMiniAppFromHistory(app) {
    await showMiniAppLaunchDialog(app);
}

/**
 * Helper function to escape HTML
 */
function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

/**
 * Truncates a string to a maximum number of grapheme clusters (visual characters).
 * Unlike substring(), this properly handles emojis and other multi-byte characters.
 */
function truncateGraphemes(text, maxLength) {
    const segmenter = new Intl.Segmenter('en', { granularity: 'grapheme' });
    const segments = [...segmenter.segment(text)];
    if (segments.length <= maxLength) return text;
    return segments.slice(0, maxLength).map(s => s.segment).join('') + '…';
}

/**
 * Renders the Recently Used emojis immediately, then renders
 * the All Emojis grid after the last recent emoji image loads.
 */
function renderEmojiPanel() {
    const recentsGrid = document.getElementById('emoji-recents-grid');
    const allGrid = document.getElementById('emoji-all-grid');
    
    recentsGrid.innerHTML = '';
    allGrid.innerHTML = '';
    
    // Build Recently Used with DocumentFragment
    const recentsFragment = document.createDocumentFragment();
    const recentEmojis = getMostUsedEmojis().slice(0, 24);
    
    recentEmojis.forEach(emoji => {
        const span = document.createElement('span');
        span.textContent = emoji.emoji;
        span.title = emoji.name;
        recentsFragment.appendChild(span);
    });
    
    recentsGrid.appendChild(recentsFragment);
    twemojify(recentsGrid);
    
    // Find the last <img> in recents and wait for it to load
    const lastRecentImage = recentsGrid.lastElementChild?.querySelector('img');
    
    if (lastRecentImage) {
        lastRecentImage.addEventListener('load', () => {
            renderAllEmojisGrid(allGrid);
        }, { once: true });
    } else {
        // No recent emojis, render all grid immediately
        renderAllEmojisGrid(allGrid);
    }
    
    // Load favorites section
    loadFavoritesSection();
}

/**
 * Renders the full All Emojis grid using DocumentFragment.
 */
function renderAllEmojisGrid(allGrid) {
    const allFragment = document.createDocumentFragment();
    
    arrEmojis.forEach(emoji => {
        const span = document.createElement('span');
        span.textContent = emoji.emoji;
        span.title = emoji.name;
        allFragment.appendChild(span);
    });
    
    allGrid.appendChild(allFragment);
    twemojify(allGrid);
}

/**
 * Loads the favorites section separately.
 */
function loadFavoritesSection() {
    const favoritesGrid = document.getElementById('emoji-favorites-grid');
    favoritesGrid.innerHTML = '';
    
    const favoritesFragment = document.createDocumentFragment();
    arrFavoriteEmojis.slice(0, 24).forEach(emoji => {
        const span = document.createElement('span');
        span.textContent = emoji.emoji;
        span.title = emoji.name;
        favoritesFragment.appendChild(span);
    });
    
    favoritesGrid.appendChild(favoritesFragment);
    twemojify(favoritesGrid);
}

function loadEmojiSections() {
    // Legacy function - now calls renderEmojiPanel
    renderEmojiPanel();
    
    // Initialize collapsible sections after loading emojis
    initCollapsibleSections();
}

// Track if we've already initialized to prevent duplicates
let collapsiblesInitialized = false;

function initCollapsibleSections() {
    if (collapsiblesInitialized) return; // Prevent duplicate initialization
    
    document.querySelectorAll('.emoji-section-header').forEach(header => {
        header.addEventListener('click', (e) => {
            e.stopPropagation(); // Prevent closing the picker
            const section = header.parentElement;
            section.classList.toggle('collapsed');
        });
    });
    
    collapsiblesInitialized = true;
}

// Function to reset emoji picker state
function resetEmojiPicker() {
    // Clear search input
    emojiSearch.value = '';
    
    // Restore search icon opacity
    if (emojiSearchIcon) emojiSearchIcon.style.opacity = '';
    
    // Show all sections
    document.querySelectorAll('.emoji-section').forEach(section => {
        section.style.display = 'block';
    });
    
    // Remove search results container
    const existingResults = document.getElementById('emoji-search-results-container');
    if (existingResults) {
        existingResults.remove();
    }
}

// Update the emoji search event listener
emojiSearch.addEventListener('input', (e) => {
    // Skip emoji search if in GIF mode (GIF search is handled separately)
    if (pickerMode === PICKER_MODE_GIF) return;

    const search = e.target.value.toLowerCase();

    if (search) {
         if (emojiSearchIcon) emojiSearchIcon.style.opacity = '0';
        // Hide all sections and show search results
        document.querySelectorAll('.emoji-section').forEach(section => {
            section.style.display = 'none';
        });

        const results = searchEmojis(search);
        const resultsContainer = document.createElement('div');
        resultsContainer.className = 'emoji-section';
        resultsContainer.id = 'emoji-search-results-container';
        resultsContainer.innerHTML = `
            <div class="emoji-section-header">
                <span class="header-text">Search Results</span>
            </div>
            <div class="emoji-grid" id="emoji-search-results"></div>
        `;

        const existingResults = document.getElementById('emoji-search-results-container');
        if (existingResults) {
            existingResults.remove();
        }

        document.querySelector('.emoji-main').prepend(resultsContainer);

        const resultsGrid = document.getElementById('emoji-search-results');
        resultsGrid.innerHTML = '';

        // STRICT FILTERING: Only show emojis that contain the search term in their name
        const filteredResults = results.filter(emoji =>
            emoji.name.toLowerCase().includes(search)
        );

        filteredResults.slice(0, 48).forEach(emoji => {
            const span = document.createElement('span');
            span.textContent = emoji.emoji;
            span.title = emoji.name;
            resultsGrid.appendChild(span);
        });

        twemojify(resultsGrid);
    } else {
        if (emojiSearchIcon) emojiSearchIcon.style.opacity = ''; // Restore opacity when cleared
        resetEmojiPicker();
    }
});

// Update the category button click handler
document.querySelectorAll('.emoji-category-btn').forEach(btn => {
    btn.addEventListener('click', (e) => {
        e.stopPropagation(); // Prevent closing the picker
        const category = btn.dataset.category;
        
        // Update active state
        document.querySelectorAll('.emoji-category-btn').forEach(b => {
            b.classList.toggle('active', b === btn);
        });
        
        // Scroll to the selected section
        const section = document.getElementById(`emoji-${category}`);
        section.scrollIntoView({ behavior: 'smooth', block: 'start' });
    });
});

/**
 * Insert text at the cursor position in the chat input
 * If no selection, inserts at cursor. If text is selected, replaces it.
 * @param {string} text - The text to insert
 * @param {boolean} autoSpace - If true, adds spaces around inserted text when adjacent to non-whitespace
 */
function insertAtCursor(text, autoSpace = false) {
    const input = domChatMessageInput;
    const start = input.selectionStart;
    const end = input.selectionEnd;
    const value = input.value;

    // Auto-space: add spaces if inserting next to non-whitespace characters
    let prefix = '';
    let suffix = '';
    if (autoSpace) {
        const charBefore = start > 0 ? value[start - 1] : '';
        const charAfter = end < value.length ? value[end] : '';
        if (charBefore && !/\s/.test(charBefore)) prefix = ' ';
        if (charAfter && !/\s/.test(charAfter)) suffix = ' ';
    }

    const before = value.substring(0, start);
    const after = value.substring(end);
    const insertText = prefix + text + suffix;
    input.value = before + insertText + after;
    // Move cursor to end of inserted text
    const newPos = start + insertText.length;
    input.setSelectionRange(newPos, newPos);
    // Trigger input event to update send/mic button state
    input.dispatchEvent(new Event('input', { bubbles: true }));
}

// Emoji selection handler
picker.addEventListener('click', (e) => {
    if (e.target.tagName === 'SPAN' && e.target.parentElement.classList.contains('emoji-grid')) {
        const emoji = e.target.getAttribute('title');
        const cEmoji = arrEmojis.find(e => e.name === emoji);
        
        if (cEmoji) {
            // Register usage
            cEmoji.used++;
            addToRecentEmojis(cEmoji);
            
            // Handle the emoji selection
            if (strCurrentReactionReference) {
                // Reaction handling 
                for (const cChat of arrChats) {
                    const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
                    if (!cMsg) continue;

                    const strReceiverPubkey = cChat.id;
                    const spanReaction = document.createElement('span');
                    spanReaction.classList.add('reaction');
                    spanReaction.textContent = `${cEmoji.emoji} 1`;
                    twemojify(spanReaction);

                    const divMessage = document.getElementById(cMsg.id);
                    divMessage.querySelector(`.msg-extras span`).replaceWith(spanReaction);
                    invoke('react_to_message', { referenceId: strCurrentReactionReference, chatId: strReceiverPubkey, emoji: cEmoji.emoji });
                }
            } else {
                // Add to message input at cursor position (with auto-spacing)
                insertAtCursor(cEmoji.emoji, true);
            }

            // Close the picker - use class instead of inline style
            picker.classList.remove('visible');
            // Focus chat input (desktop only - mobile keyboards are disruptive)
            if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
                domChatMessageInput.focus();
            }
        }
    }
});

// When hitting Enter on the emoji search - choose the first emoji/GIF
emojiSearch.onkeydown = async (e) => {
    if ((e.code === 'Enter' || e.code === 'NumpadEnter')) {
        e.preventDefault();

        // Handle GIF mode - select first GIF
        if (pickerMode === PICKER_MODE_GIF) {
            const firstGif = document.querySelector('#gif-grid .gif-item');
            if (firstGif && firstGif.dataset.gifId) {
                selectGif(firstGif.dataset.gifId);
            }
            return;
        }

        // Find the first emoji in search results or recent emojis
        let emojiElement;
        if (emojiSearch.value) {
            emojiElement = document.querySelector('#emoji-search-results span:first-child');
        } else {
            emojiElement = document.querySelector('#emoji-recents-grid span:first-child');
        }

        if (!emojiElement) return;

        // Register the selection in the emoji-dex
        const cEmoji = arrEmojis.find(a => a.name === emojiElement.getAttribute('title'));
        if (!cEmoji) return;
        
        cEmoji.used++;
        addToRecentEmojis(cEmoji);

        // If this is a Reaction - use the original reaction handling
        if (strCurrentReactionReference) {
            // Grab the referred message to find it's chat pubkey
            for (const cChat of arrChats) {
                const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
                if (!cMsg) continue;

                // Found the message!
                const strReceiverPubkey = cChat.id;

                // Add a 'decoy' reaction for good UX (no waiting for the network to register the reaction)
                const spanReaction = document.createElement('span');
                spanReaction.classList.add('reaction');
                spanReaction.textContent = `${cEmoji.emoji} 1`;
                twemojify(spanReaction);

                // Replace the Reaction button
                // DOM Tree: msg-(them/me) -> msg-extras -> add-reaction
                const divMessage = document.getElementById(cMsg.id);
                divMessage.querySelector(`.msg-extras span`).replaceWith(spanReaction);

                // Send the Reaction to the network (protocol-agnostic)
                invoke('react_to_message', { referenceId: strCurrentReactionReference, chatId: strReceiverPubkey, emoji: cEmoji.emoji });
            }
        } else {
            // Add to message input at cursor position (with auto-spacing)
            insertAtCursor(cEmoji.emoji, true);
        }

        // Reset the UI state - use class instead of inline style
        emojiSearch.value = '';
        picker.classList.remove('visible');
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;

        // Bring the focus back to the chat (desktop only - mobile keyboards are disruptive)
        if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
            domChatMessageInput.focus();
        }
    } else if (e.code === 'Escape') {
        // Close the Mini App launch dialog if open
        if (domMiniAppLaunchOverlay.classList.contains('active')) {
            closeMiniAppLaunchDialog();
            return;
        }

        // Close the emoji dialog - use class instead of inline style
        emojiSearch.value = '';
        picker.classList.remove('visible');
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;

        // Close the attachment panel if open
        if (domAttachmentPanel.classList.contains('visible')) {
            closeAttachmentPanel();
        }

        // Bring the focus back to the chat (desktop only - mobile keyboards are disruptive)
        if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
            domChatMessageInput.focus();
        }
    }
};

// Add contextmenu event for right-click to favorite
picker.addEventListener('contextmenu', (e) => {
    if (e.target.tagName === 'SPAN' && e.target.parentElement.classList.contains('emoji-grid')) {
        e.preventDefault();
        const emoji = e.target.textContent;
        const emojiData = arrEmojis.find(e => e.emoji === emoji);
        
        if (emojiData) {
            const added = toggleFavoriteEmoji(emojiData);
            if (added) {
                // Visual feedback for adding to favorites
                e.target.style.transform = 'scale(1.3)';
                e.target.style.backgroundColor = 'rgba(255, 215, 0, 0.3)';
                setTimeout(() => {
                    e.target.style.transform = '';
                    e.target.style.backgroundColor = '';
                }, 500);
            }
        }
    }
});

// Emoji selection
picker.addEventListener('click', (e) => {
    if (e.target.tagName === 'IMG') {
        // Register the click in the emoji-dex
        const cEmoji = arrEmojis.find(a => a.emoji === e.target.alt);
        cEmoji.used++;

        // If this is a Reaction - let's send it!
        if (strCurrentReactionReference) {
            // Grab the referred message to find it's chat pubkey
            for (const cChat of arrChats) {
                const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
                if (!cMsg) continue;

                // Found the message!
                const strReceiverPubkey = cChat.id;

                // Add a 'decoy' reaction for good UX (no waiting for the network to register the reaction)
                const spanReaction = document.createElement('span');
                spanReaction.classList.add('reaction');
                spanReaction.textContent = `${cEmoji.emoji} 1`;
                twemojify(spanReaction);

                // Replace the Reaction button
                // DOM Tree: msg-(them/me) -> msg-extras -> add-reaction
                const divMessage = document.getElementById(cMsg.id);
                divMessage.querySelector(`.msg-extras span`).replaceWith(spanReaction);

                // Send the Reaction to the network (protocol-agnostic)
                invoke('react_to_message', { referenceId: strCurrentReactionReference, chatId: strReceiverPubkey, emoji: cEmoji.emoji });
            }
        }
    }
});

// ==================== GIF PICKER ====================

const gifPickerContent = document.querySelector('.gif-picker-content');
const emojiPickerContent = document.querySelector('.emoji-picker-content');
const gifGrid = document.getElementById('gif-grid');
const gifLoading = document.getElementById('gif-loading');
const pickerModeButtons = document.querySelectorAll('.picker-mode-btn');

/** Picker mode enum for fast comparison */
const PICKER_MODE_EMOJI = 0;
const PICKER_MODE_GIF = 1;

/** Current picker mode */
let pickerMode = PICKER_MODE_EMOJI;

/** GIF search debounce timer */
let gifSearchTimeout = null;

/** Track if trending GIFs have been loaded */
let trendingGifsLoaded = false;

/** IntersectionObserver for lazy loading GIF previews */
let gifLazyLoadObserver = null;

/** Cached trending GIFs data and timestamp */
let cachedTrendingGifs = null;
let cachedTrendingTimestamp = 0;
const GIF_CACHE_TTL = 5 * 60 * 1000; // 5 minutes

/** Maximum number of cached search queries (LRU eviction) */
const GIF_SEARCH_CACHE_MAX_SIZE = 10;

/** Cached search results (LRU-style) */
const gifSearchCache = new Map();

/** GIFGalaxy API base URL */
const GIF_API_BASE = 'https://gifverse.net';

/** Preconnect link element for GIFGalaxy */
let gifPreconnectLink = null;

/** Pagination state for GIFs */
// Use smaller page size on macOS (WebKit) due to autoplay limitations
const gifPageSize = navigator.userAgent.includes('Mac') && !navigator.userAgent.includes('Firefox') ? 6 : 12;
let gifCurrentOffset = 0;
let gifHasMore = true;
let gifIsLoadingMore = false;
let gifCurrentMode = 'trending'; // 'trending' or 'search'
let gifCurrentQuery = '';

/**
 * Shows skeleton/ghost placeholders in the GIF grid while loading
 * @param {number} count - Number of skeleton items to show
 */
function showGifSkeletons(count) {
    gifGrid.innerHTML = '';
    const fragment = document.createDocumentFragment();
    for (let i = 0; i < count; i++) {
        const skeleton = document.createElement('div');
        skeleton.className = 'gif-item gif-skeleton';
        fragment.appendChild(skeleton);
    }
    gifGrid.appendChild(fragment);
}

/**
 * Establish early connection to GIF API server
 * Called when opening a chat to warm up connection before user needs GIFs
 */
function preconnectGifServer() {
    if (gifPreconnectLink) return; // Already connected
    gifPreconnectLink = document.createElement('link');
    gifPreconnectLink.rel = 'preconnect';
    gifPreconnectLink.href = 'https://gifverse.net';
    gifPreconnectLink.crossOrigin = 'anonymous';
    document.head.appendChild(gifPreconnectLink);
}

/**
 * Prefetch trending GIFs in background when emoji panel opens
 * Caches the API response for instant display
 */
function prefetchTrendingGifs() {
    // Skip if cache is still fresh
    if (cachedTrendingGifs && Date.now() - cachedTrendingTimestamp < GIF_CACHE_TTL) {
        return;
    }

    // Prefetch trending in background (use dynamic page size)
    fetch(`${GIF_API_BASE}/api/v1/trending?limit=${gifPageSize}&offset=0&sort=popular`)
        .then(res => res.json())
        .then(data => {
            if (data.results && data.results.length > 0) {
                cachedTrendingGifs = data.results;
                cachedTrendingTimestamp = Date.now();
            }
        })
        .catch(() => {}); // Silently fail - this is just an optimization
}

// ===== Blurhash Decoder (Rust Backend) =====
// Uses efficient Rust-based decoder with LRU caching
/** @type {Map<string, string>} Cache of decoded blurhash -> data URL */
const blurhashCache = new Map();
/** Maximum cached blurhash entries */
const BLURHASH_CACHE_MAX_SIZE = 200;

/**
 * Get a decoded blurhash data URL from cache, or decode via Rust backend
 * Returns cached value synchronously if available, otherwise triggers async decode
 * @param {string} blurhash - The blurhash string
 * @returns {string|null} - Cached data URL or null (will be decoded async)
 */
function getCachedBlurhash(blurhash) {
    if (!blurhash || blurhash.length < 6) return null;
    return blurhashCache.get(blurhash) || null;
}

/**
 * Pre-decode blurhashes for a batch of GIFs using the Rust backend
 * Results are cached for synchronous access during rendering
 * @param {Array<{b?: string}>} gifs - Array of GIF objects with optional blurhash field 'b'
 */
async function predecodeBlurhashes(gifs) {
    const uncached = gifs.filter(g => g.b && !blurhashCache.has(g.b));
    if (uncached.length === 0) return;

    // Decode in parallel (Rust backend is efficient)
    const decodePromises = uncached.map(async (gif) => {
        try {
            const dataUrl = await invoke('decode_blurhash', {
                blurhash: gif.b,
                width: 32,
                height: 32
            });
            if (dataUrl && dataUrl.startsWith('data:')) {
                // LRU eviction if cache is full
                if (blurhashCache.size >= BLURHASH_CACHE_MAX_SIZE) {
                    const firstKey = blurhashCache.keys().next().value;
                    blurhashCache.delete(firstKey);
                }
                blurhashCache.set(gif.b, dataUrl);
            }
        } catch {
            // Silently fail - blurhash is just for placeholder
        }
    });

    await Promise.all(decodePromises);
}

// ===== Video Format Detection =====
// Detect best supported video format for GIF previews (AV1 > WebM > MP4 > GIF)
// Also builds a fallback chain starting from the best supported format
const gifFormatFallbackChain = (() => {
    const video = document.createElement('video');
    const allFormats = [
        { ext: 'video.av1', type: 'video', test: 'video/mp4; codecs="av01.0.05M.08"' },
        { ext: 'video.webm', type: 'video', test: 'video/webm; codecs="vp9"' },
        { ext: 'video.mp4', type: 'video', test: 'video/mp4; codecs="avc1.42E01E"' },
        { ext: 'original.gif', type: 'image', test: null } // Always supported
    ];

    // Find the first supported format and build chain from there
    let startIndex = allFormats.findIndex(f =>
        f.test === null || video.canPlayType(f.test) === 'probably' || video.canPlayType(f.test) === 'maybe'
    );
    if (startIndex === -1) startIndex = allFormats.length - 1; // Fallback to GIF

    return allFormats.slice(startIndex);
})();
const gifPreviewFormat = gifFormatFallbackChain[0];

/**
 * Switches between emoji and GIF picker modes
 * @param {number} mode - PICKER_MODE_EMOJI or PICKER_MODE_GIF
 */
function setPickerMode(mode) {
    pickerMode = mode;

    // Update button states (data-mode uses strings for readability)
    const modeStr = mode === PICKER_MODE_GIF ? 'gif' : 'emoji';
    pickerModeButtons.forEach(btn => {
        btn.classList.toggle('active', btn.dataset.mode === modeStr);
    });

    if (mode === PICKER_MODE_EMOJI) {
        emojiPickerContent.style.display = '';
        gifPickerContent.style.display = 'none';
        emojiSearch.placeholder = 'Search Emojis...';
    } else {
        emojiPickerContent.style.display = 'none';
        gifPickerContent.style.display = 'flex';
        emojiSearch.placeholder = 'Search GIFs...';

        // Load trending GIFs if not already loaded
        if (!trendingGifsLoaded) {
            loadTrendingGifs();
        }
    }

    // Auto-focus search box on desktop only (mobile keyboards are intrusive)
    // Only focus if the picker is actually visible to avoid stealing focus
    if (!platformFeatures.is_mobile && picker.classList.contains('visible')) {
        emojiSearch.focus();
    }
}

/**
 * Fetches trending GIFs from GIFGalaxy API
 * Uses cached data if available and fresh
 */
async function loadTrendingGifs() {
    // Reset pagination state
    gifCurrentOffset = 0;
    gifHasMore = true;
    gifIsLoadingMore = false;
    gifCurrentMode = 'trending';
    gifCurrentQuery = '';

    // Use cached data if fresh (only for first page)
    if (cachedTrendingGifs && Date.now() - cachedTrendingTimestamp < GIF_CACHE_TTL) {
        gifGrid.innerHTML = '';
        // Blurhashes should already be cached, but ensure they are
        await predecodeBlurhashes(cachedTrendingGifs);
        renderGifs(cachedTrendingGifs, false);
        gifCurrentOffset = cachedTrendingGifs.length;
        gifHasMore = cachedTrendingGifs.length >= gifPageSize;
        trendingGifsLoaded = true;
        gifLoading.style.display = 'none';
        return;
    }

    // Show skeleton placeholders while fetching
    showGifSkeletons(gifPageSize);

    try {
        const response = await fetch(`${GIF_API_BASE}/api/v1/trending?limit=${gifPageSize}&offset=0&sort=popular`);
        const data = await response.json();

        if (data.results && data.results.length > 0) {
            // Cache the results
            cachedTrendingGifs = data.results;
            cachedTrendingTimestamp = Date.now();
            // Pre-decode blurhashes before rendering
            await predecodeBlurhashes(data.results);
            renderGifs(data.results, false);
            gifCurrentOffset = data.results.length;
            gifHasMore = data.results.length >= gifPageSize;
            trendingGifsLoaded = true;
        } else {
            showGifEmptyState('No trending GIFs found');
            gifHasMore = false;
        }
    } catch (error) {
        console.error('[GIF] Failed to load trending:', error);
        showGifEmptyState('Failed to load GIFs');
        gifHasMore = false;
    } finally {
        gifLoading.style.display = 'none';
    }
}

/**
 * Searches GIFs using the GIFGalaxy API
 * @param {string} query - The search query
 */
async function searchGifs(query) {
    if (!query.trim()) {
        // Reset to trending when search is cleared
        trendingGifsLoaded = false;
        return loadTrendingGifs();
    }

    // Reset pagination state for new search
    gifCurrentOffset = 0;
    gifHasMore = true;
    gifIsLoadingMore = false;
    gifCurrentMode = 'search';
    gifCurrentQuery = query.trim();

    const cacheKey = gifCurrentQuery.toLowerCase();

    // Check cache first (only for first page)
    if (gifSearchCache.has(cacheKey)) {
        const cached = gifSearchCache.get(cacheKey);
        // Move to end (most recently used)
        gifSearchCache.delete(cacheKey);
        gifSearchCache.set(cacheKey, cached);
        gifGrid.innerHTML = '';
        // Blurhashes should already be cached, but ensure they are
        await predecodeBlurhashes(cached);
        renderGifs(cached, false);
        gifCurrentOffset = cached.length;
        gifHasMore = cached.length >= gifPageSize;
        return;
    }

    // Show skeleton placeholders while fetching
    showGifSkeletons(gifPageSize);

    try {
        const encodedQuery = encodeURIComponent(gifCurrentQuery);
        const response = await fetch(`${GIF_API_BASE}/api/v1/search?q=${encodedQuery}&limit=${gifPageSize}&offset=0&sort=relevant`);
        const data = await response.json();

        if (data.results && data.results.length > 0) {
            // Cache the results (LRU eviction if > 10 entries)
            if (gifSearchCache.size >= GIF_SEARCH_CACHE_MAX_SIZE) {
                // Delete oldest entry (first key)
                const oldestKey = gifSearchCache.keys().next().value;
                gifSearchCache.delete(oldestKey);
            }
            gifSearchCache.set(cacheKey, data.results);

            // Pre-decode blurhashes before rendering
            await predecodeBlurhashes(data.results);
            renderGifs(data.results, false);
            gifCurrentOffset = data.results.length;
            gifHasMore = data.results.length >= gifPageSize;
        } else {
            showGifEmptyState(`No GIFs found for "${query}"`);
            gifHasMore = false;
        }
    } catch (error) {
        console.error('[GIF] Search failed:', error);
        showGifEmptyState('Search failed');
        gifHasMore = false;
    } finally {
        gifLoading.style.display = 'none';
    }
}

/**
 * Loads more GIFs for infinite scroll pagination
 * Appends additional results to the existing grid
 */
async function loadMoreGifs() {
    if (gifIsLoadingMore || !gifHasMore) return;
    gifIsLoadingMore = true;

    // Show skeleton placeholders for the new batch
    const fragment = document.createDocumentFragment();
    for (let i = 0; i < gifPageSize; i++) {
        const skeleton = document.createElement('div');
        skeleton.className = 'gif-item gif-skeleton gif-loading-more';
        fragment.appendChild(skeleton);
    }
    gifGrid.appendChild(fragment);

    try {
        let url;
        if (gifCurrentMode === 'trending') {
            url = `${GIF_API_BASE}/api/v1/trending?limit=${gifPageSize}&offset=${gifCurrentOffset}&sort=popular`;
        } else {
            const encodedQuery = encodeURIComponent(gifCurrentQuery);
            url = `${GIF_API_BASE}/api/v1/search?q=${encodedQuery}&limit=${gifPageSize}&offset=${gifCurrentOffset}&sort=relevant`;
        }

        const response = await fetch(url);
        const data = await response.json();

        // Remove skeleton placeholders
        gifGrid.querySelectorAll('.gif-loading-more').forEach(el => el.remove());

        if (data.results && data.results.length > 0) {
            // Pre-decode blurhashes before rendering
            await predecodeBlurhashes(data.results);
            renderGifs(data.results, true); // Append mode
            gifCurrentOffset += data.results.length;
            gifHasMore = data.results.length >= gifPageSize;
        } else {
            gifHasMore = false;
        }
    } catch (error) {
        console.error('[GIF] Failed to load more:', error);
        // Remove skeleton placeholders on error
        gifGrid.querySelectorAll('.gif-loading-more').forEach(el => el.remove());
        gifHasMore = false;
    } finally {
        gifIsLoadingMore = false;
    }
}

/**
 * Renders GIF items to the grid with lazy loading
 * Uses IntersectionObserver to only load visible items + one row ahead
 * @param {Array} gifs - Array of GIF data from API
 * @param {boolean} append - If true, append to existing grid instead of replacing
 */
function renderGifs(gifs, append = false) {
    const mediaUrl = `${GIF_API_BASE}/media`;

    if (!append) {
        // Clean up previous observer when replacing content
        if (gifLazyLoadObserver) {
            gifLazyLoadObserver.disconnect();
        }

        gifGrid.innerHTML = '';

        // Create IntersectionObserver for lazy loading AND play/pause management
        // Keep observing to handle play/pause when items scroll in/out of view
        gifLazyLoadObserver = new IntersectionObserver((entries) => {
            for (const entry of entries) {
                const gifItem = entry.target;

                // Load media if visible and not yet loaded
                if (entry.isIntersecting && !gifItem.dataset.mediaLoaded) {
                    loadGifMedia(gifItem, mediaUrl);
                }

                // Play/pause video based on visibility
                // Only manage videos that have already initialized (data-ready set by canplay)
                const video = gifItem.querySelector('video');
                if (video && video.dataset.ready) {
                    if (entry.isIntersecting) {
                        video.play().catch(() => {});
                    } else {
                        video.pause();
                    }
                }
            }
        }, {
            root: gifGrid,
            rootMargin: '0px 0px 200px 0px',
            threshold: 0.1
        });
    }

    const fragment = document.createDocumentFragment();

    for (const gif of gifs) {
        const gifItem = document.createElement('div');
        gifItem.className = 'gif-item';
        gifItem.dataset.gifId = gif.i;
        gifItem.dataset.gifTitle = gif.ti || '';

        // Create placeholder with blurhash background
        const placeholder = document.createElement('div');
        placeholder.className = 'gif-placeholder';

        // Apply cached blurhash as background (pre-decoded via Rust backend)
        if (gif.b) {
            const cachedBlurhash = getCachedBlurhash(gif.b);
            if (cachedBlurhash) {
                placeholder.style.backgroundImage = `url(${cachedBlurhash})`;
                placeholder.style.backgroundSize = 'cover';
            }
        }
        placeholder.innerHTML = '<span class="loading-spinner"></span>';
        gifItem.appendChild(placeholder);

        fragment.appendChild(gifItem);

        // Observe for lazy loading (after appending to fragment)
        gifLazyLoadObserver.observe(gifItem);
    }

    gifGrid.appendChild(fragment);
}

/**
 * Loads the media (video or image) for a GIF item when it becomes visible
 * @param {HTMLElement} gifItem - The GIF item container
 * @param {string} mediaUrl - Base URL for media
 */
function loadGifMedia(gifItem, mediaUrl) {
    // Mark as loaded to prevent re-loading
    gifItem.dataset.mediaLoaded = 'true';

    const gifId = gifItem.dataset.gifId;
    const gifTitle = gifItem.dataset.gifTitle;
    const placeholder = gifItem.querySelector('.gif-placeholder');

    // Try loading with fallback chain
    loadGifWithFallback(gifItem, mediaUrl, gifId, gifTitle, placeholder, 0);
}

/**
 * Attempts to load a GIF using the format at the given index in the fallback chain
 * On error, tries the next format in the chain
 */
function loadGifWithFallback(gifItem, mediaUrl, gifId, gifTitle, placeholder, formatIndex) {
    if (formatIndex >= gifFormatFallbackChain.length) {
        // All formats failed - show error icon
        if (placeholder) placeholder.innerHTML = '<span class="icon icon-image"></span>';
        return;
    }

    const format = gifFormatFallbackChain[formatIndex];

    if (format.type === 'video') {
        // Use video element for efficient formats
        // Use setAttribute for WebKit/Safari compatibility (properties may not work)
        const video = document.createElement('video');
        video.setAttribute('autoplay', '');
        video.setAttribute('loop', '');
        video.setAttribute('muted', '');
        video.setAttribute('playsinline', '');
        video.setAttribute('preload', 'auto');
        // Also set properties for browsers that need them
        video.muted = true;
        video.playsInline = true;
        video.autoplay = true;
        video.loop = true;

        // Use canplay event - fires when enough data to start playing
        video.addEventListener('canplay', () => {
            if (placeholder) placeholder.remove();
            video.dataset.ready = 'true'; // Mark as ready for observer to manage
            // Only auto-play if video is actually visible (not just preloaded in margin)
            // Check if element is in the visible viewport
            const rect = gifItem.getBoundingClientRect();
            const gridRect = gifGrid.getBoundingClientRect();
            const isVisible = rect.top < gridRect.bottom && rect.bottom > gridRect.top;
            if (isVisible) {
                video.play().catch(() => {});
            }
        }, { once: true });

        video.onerror = () => {
            // Try next format in the fallback chain
            video.remove();
            loadGifWithFallback(gifItem, mediaUrl, gifId, gifTitle, placeholder, formatIndex + 1);
        };

        // Set src and explicitly call load() for WebKit
        video.src = `${mediaUrl}/${gifId}/${format.ext}`;
        gifItem.appendChild(video);
        video.load();
    } else {
        // Image format (GIF)
        const img = document.createElement('img');
        img.alt = gifTitle || 'GIF';
        img.src = `${mediaUrl}/${gifId}/${format.ext}`;

        img.onload = () => {
            if (placeholder) placeholder.remove();
        };

        img.onerror = () => {
            // Try next format in the fallback chain (if any)
            img.remove();
            loadGifWithFallback(gifItem, mediaUrl, gifId, gifTitle, placeholder, formatIndex + 1);
        };

        gifItem.appendChild(img);
    }
}

/**
 * Shows an empty state message in the GIF grid
 * @param {string} message - The message to display
 */
function showGifEmptyState(message) {
    gifGrid.innerHTML = `
        <div class="gif-empty-state" style="grid-column: 1 / -1;">
            <span class="icon icon-image"></span>
            <span>${message}</span>
        </div>
    `;
}

/**
 * Handles GIF selection - inserts GIF URL at cursor position
 * If input is empty, auto-sends the GIF. Otherwise, just inserts the URL.
 * @param {string} gifId - The GIF ID
 */
function selectGif(gifId) {
    const gifUrl = `${GIF_API_BASE}/media/${gifId}/original.gif`;
    const wasEmpty = !domChatMessageInput.value.trim();

    // Insert the GIF URL at cursor position (with auto-spacing)
    insertAtCursor(gifUrl, true);

    // Close the picker
    picker.classList.remove('visible');
    picker.style.bottom = '';
    domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;

    // Reset picker state
    emojiSearch.value = '';
    setPickerMode(PICKER_MODE_EMOJI);
    trendingGifsLoaded = false;

    // Focus the input (desktop only)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }

    // Auto-send only if input was empty (just the GIF)
    if (wasEmpty) {
        domChatMessageInputSend.click();
    }
}

// Mode toggle button click handlers
pickerModeButtons.forEach(btn => {
    btn.addEventListener('click', (e) => {
        e.stopPropagation();
        setPickerMode(btn.dataset.mode === 'gif' ? PICKER_MODE_GIF : PICKER_MODE_EMOJI);
    });
});

// GIF grid click handler
gifGrid.addEventListener('click', (e) => {
    e.stopPropagation(); // Prevent bubbling to emoji picker handler
    const gifItem = e.target.closest('.gif-item');
    if (gifItem && gifItem.dataset.gifId) {
        selectGif(gifItem.dataset.gifId);
    }
});

// GIF grid scroll handler for infinite scroll
gifGrid.addEventListener('scroll', () => {
    // Check if scrolled near bottom (within 100px)
    const scrollBottom = gifGrid.scrollHeight - gifGrid.scrollTop - gifGrid.clientHeight;
    if (scrollBottom < 100 && gifHasMore && !gifIsLoadingMore) {
        loadMoreGifs();
    }
});

// Modify the emoji search input to handle GIF search
const originalEmojiSearchHandler = emojiSearch.oninput;
emojiSearch.addEventListener('input', (e) => {
    if (pickerMode === PICKER_MODE_GIF) {
        // Debounce GIF search
        clearTimeout(gifSearchTimeout);
        gifSearchTimeout = setTimeout(() => {
            searchGifs(e.target.value);
        }, 300);
    }
    // Emoji search is handled by the existing handler
});

// Reset picker mode when panel closes
const originalCloseObserver = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
        if (mutation.attributeName === 'class') {
            if (!picker.classList.contains('visible')) {
                // Reset to emoji mode when panel closes
                setPickerMode(PICKER_MODE_EMOJI);
                trendingGifsLoaded = false;
            }
        }
    }
});
originalCloseObserver.observe(picker, { attributes: true });

// ==================== END GIF PICKER ====================

/**
 * Represents a user profile.
 * @typedef {Object} Profile
 * @property {string} id - Unique identifier for the profile.
 * @property {string} name - The name of the user.
 * @property {string} avatar - URL to the user's avatar image.
 * @property {string} last_read - ID of the last message that was read.
 * @property {Status} status - The current status of the user.
 * @property {number} last_updated - Timestamp indicating when the profile was last updated.
 * @property {number} typing_until - Timestamp until which the user is considered typing.
 * @property {boolean} mine - Indicates if this profile belongs to the current user.
 */

/**
 * Represents a message in the system.
 * @typedef {Object} Message
 * @property {string} id - Unique identifier for the message.
 * @property {string} content - The content of the message.
 * @property {string} replied_to - ID of the message this is replying to, if any.
 * @property {Object} preview_metadata - Metadata for link previews, if any.
 * @property {Attachment[]} attachments - Array of file attachments.
 * @property {Reaction[]} reactions - An array of reactions to this message.
 * @property {number} at - Timestamp when the message was sent.
 * @property {boolean} pending - Whether the message is still being sent.
 * @property {boolean} failed - Whether the message failed to send.
 * @property {boolean} mine - Indicates if this message was sent by the current user.
 */

/**
 * Represents a file attachment in a message.
 * @typedef {Object} Attachment
 * @property {string} id - The unique file ID (encryption nonce).
 * @property {string} key - The encryption key.
 * @property {string} nonce - The encryption nonce.
 * @property {string} extension - The file extension.
 * @property {string} url - The host URL, typically a NIP-96 server.
 * @property {string} path - The storage directory path.
 * @property {number} size - The download size of the encrypted file.
 * @property {boolean} downloading - Whether the file is currently being downloaded.
 * @property {boolean} downloaded - Whether the file has been downloaded.
 */

/**
 * Represents metadata for a website preview.
 * @typedef {Object} SiteMetadata
 * @property {string} domain - The domain of the website.
 * @property {string} [og_title] - Open Graph title.
 * @property {string} [og_description] - Open Graph description.
 * @property {string} [og_image] - Open Graph image URL.
 * @property {string} [og_url] - Open Graph URL.
 * @property {string} [og_type] - Open Graph content type.
 * @property {string} [title] - Website title.
 * @property {string} [description] - Website description.
 * @property {string} [favicon] - Website favicon URL.
 */

/**
 * Represents the status of a user.
 * @typedef {Object} Status
 * @property {string} title - The title of the status.
 * @property {string} purpose - Description or purpose of the status.
 * @property {string} url - URL associated with the status, if any.
 */

/**
 * Represents a reaction to a message.
 * @typedef {Object} Reaction
 * @property {string} id - Unique identifier for the reaction.
 * @property {string} reference_id - The HEX Event ID of the message being reacted to.
 * @property {string} author_id - The HEX ID of the author who reacted.
 * @property {string} emoji - The emoji used for the reaction.
 */

/**
 * Represents a chat between users.
 * @typedef {Object} Chat
 * @property {string} id - Chat ID (npub for DMs).
 * @property {string} chat_type - Type of chat (DirectMessage, Group, etc).
 * @property {string[]} participants - Array of participant npubs.
 * @property {Message[]} messages - Array of messages in this chat.
 * @property {string} last_read - ID of the last read message.
 * @property {number} created_at - Timestamp when chat was created.
 * @property {Object} metadata - Additional chat metadata.
 * @property {boolean} muted - Whether the chat is muted.
 */

/**
 * Represents an MLS group invite.
 * @typedef {Object} MLSWelcome
 * @property {string} id - Unique identifier for the welcome/invite.
 * @property {string} group_id - The MLS group ID.
 * @property {string} group_name - Name of the group.
 * @property {string} welcomer_pubkey - Pubkey of the person who invited.
 * @property {number} member_count - Number of members in the group.
 * @property {string} [image] - Optional group avatar image.
 * @property {string} [description] - Optional group description.
 */

/**
 * Represents an MLS message record.
 * @typedef {Object} MLSMessageRecord
 * @property {string} inner_event_id - The inner event ID for deduplication.
 * @property {string} wrapper_event_id - The wrapper event ID.
 * @property {string} author_pubkey - The sender's pubkey.
 * @property {string} content - The message content.
 * @property {number} created_at - Timestamp in seconds.
 * @property {Array<Array<string>>} tags - Nostr tags.
 * @property {boolean} mine - Whether this message was sent by the current user.
 */

/**
 * A cache of all profiles (without messages)
 * @type {Profile[]}
 */
let arrProfiles = [];

/**
 * A cache of all chats (with messages)
 * @type {Chat[]}
 */
let arrChats = [];

/**
 * A cache of MLS group invites
 * @type {MLSWelcome[]}
 */
let arrMLSInvites = [];

/**
 * The current open chat (by npub)
 */
let strOpenChat = "";

/**
 * The chat ID we came from when opening a profile (to return to on back)
 */
let previousChatBeforeProfile = "";

/**
 * Interval ID for periodic profile refresh while viewing profile tab
 */
let profileRefreshInterval = null;

/**
 * Get a DM chat for a user
 * @param {string} npub - The user's npub
 * @returns {Chat|undefined} - The chat if it exists
 */
function getDMChat(npub) {
    return arrChats.find(c => c.chat_type === 'DirectMessage' && c.id === npub);
}

/**
 * Get a group chat by ID
 * @param {string} groupId - The group's ID
 * @returns {Chat|undefined} - The chat if it exists
 */
function getGroupChat(groupId) {
    return arrChats.find(c => c.chat_type === 'MlsGroup' && c.id === groupId);
}

/**
 * Get a chat by ID (works for both DMs and Group Chats)
 * @param {string} id - The chat ID (npub for DM, group_id for MlsGroup)
 * @returns {Chat|undefined} - The chat if it exists
 */
function getChat(id) {
    return arrChats.find(c => c.id === id);
}

/**
 * Get or create a chat (DM or MLS Group)
 * @param {string} id - The chat ID (npub for DM, group_id for MlsGroup)
 * @param {string} chatType - 'DirectMessage' or 'MlsGroup'
 * @returns {Chat} - The chat (existing or newly created)
 */
function getOrCreateChat(id, chatType = 'DirectMessage') {
    let chat = chatType === 'MlsGroup' ? getGroupChat(id) : getDMChat(id);
    if (!chat) {
        chat = {
            id: id,
            chat_type: chatType,
            participants: chatType === 'MlsGroup' ? [] : [id],
            messages: [],
            last_read: '',
            created_at: Math.floor(Date.now() / 1000),
            metadata: chatType === 'MlsGroup' ? { group_id: id } : {},
            muted: false
        };
        arrChats.push(chat);
    }
    return chat;
}

/**
 * Get or create a DM chat for a user
 * @param {string} npub - The user's npub
 * @returns {Chat} - The chat (existing or newly created)
 */
function getOrCreateDMChat(npub) {
    return getOrCreateChat(npub, 'DirectMessage');
}

/**
 * Compute a timestamp for sorting chats, falling back to metadata for empty groups.
 * @param {Chat} chat
 * @returns {number}
 */
function getChatSortTimestamp(chat) {
    const lastMessage = chat.messages?.length ? chat.messages[chat.messages.length - 1] : null;
    let lastActivity = lastMessage?.at || 0;

    if (chat.chat_type === 'MlsGroup') {
        const updatedAt = chat.metadata?.updated_at || chat.metadata?.custom_fields?.updated_at || 0;
        if (updatedAt > lastActivity) {
            lastActivity = updatedAt;
        }
    }

    if (!lastActivity) {
        lastActivity =
            chat.metadata?.created_at ||
            chat.metadata?.custom_fields?.created_at ||
            chat.created_at ||
            0;
    }

    return lastActivity || 0;
}

/**
 * Apply backend-provided metadata to an MLS group chat object.
 * @param {Object} metadata - The metadata from Rust.
 * @returns {{chat: Chat|null, changed: boolean}}
 */
function applyMlsGroupMetadata(metadata) {
    if (!metadata || !metadata.group_id) {
        return { chat: null, changed: false };
    }

    const chat = getOrCreateChat(metadata.group_id, 'MlsGroup');
    chat.metadata = chat.metadata || {};
    chat.metadata.custom_fields = chat.metadata.custom_fields || {};

    let changed = false;
    const assignIfChanged = (target, key, value) => {
        if (value === undefined) return;
        if (target[key] !== value) {
            target[key] = value;
            changed = true;
        }
    };

    assignIfChanged(chat.metadata, 'group_id', metadata.group_id);
    assignIfChanged(chat.metadata, 'engine_group_id', metadata.engine_group_id);
    assignIfChanged(chat.metadata, 'creator_pubkey', metadata.creator_pubkey);
    assignIfChanged(chat.metadata, 'avatar_ref', metadata.avatar_ref ?? null);
    assignIfChanged(chat.metadata, 'created_at', metadata.created_at);
    assignIfChanged(chat.metadata, 'updated_at', metadata.updated_at);
    assignIfChanged(chat.metadata, 'evicted', metadata.evicted);
    assignIfChanged(chat.metadata.custom_fields, 'name', metadata.name);
    if (metadata.member_count !== undefined) {
        assignIfChanged(chat.metadata.custom_fields, 'member_count', metadata.member_count);
    }

    return { chat, changed };
}

/**
 * Hydrate all MLS group metadata from the backend store.
 */
async function hydrateMLSGroupMetadata() {
    try {
        const metadataList = await invoke('get_mls_group_metadata');
        let changed = false;
        for (const metadata of metadataList || []) {
            const result = applyMlsGroupMetadata(metadata);
            if (result.changed) {
                changed = true;
            }
        }
        if (changed && !fInit) {
            renderChatlist();
        }
    } catch (e) {
        console.error('Failed to hydrate MLS group metadata:', e);
    }
}

/**
 * Get a profile by npub
 * @param {string} npub - The user's npub
 * @returns {Profile|undefined} - The profile if it exists
 */
function getProfile(npub) {
    return arrProfiles.find(p => p.id === npub);
}

/**
 * Get the best avatar URL for a profile
 * Prefers cached local file (for offline support), falls back to remote URL
 * @param {Profile} profile - The profile object
 * @returns {string|null} - The avatar URL to use, or null if none available
 */
function getProfileAvatarSrc(profile) {
    if (!profile) return null;
    // Prefer cached local path for offline support
    if (profile.avatar_cached) {
        return convertFileSrc(profile.avatar_cached);
    }
    // Fall back to remote URL
    return profile.avatar || null;
}

/**
 * Get the best banner URL for a profile
 * Prefers cached local file (for offline support), falls back to remote URL
 * @param {Profile} profile - The profile object
 * @returns {string|null} - The banner URL to use, or null if none available
 */
function getProfileBannerSrc(profile) {
    if (!profile) return null;
    // Prefer cached local path for offline support
    if (profile.banner_cached) {
        return convertFileSrc(profile.banner_cached);
    }
    // Fall back to remote URL
    return profile.banner || null;
}

/**
 * Create an avatar image element with automatic fallback to placeholder on error
 * @param {string} src - The image source URL
 * @param {number} size - The size of the avatar in pixels
 * @param {boolean} isGroup - Whether this is a group avatar (affects placeholder)
 * @returns {HTMLElement} - Either an img element or a placeholder div
 */
function createAvatarImg(src, size, isGroup = false) {
    if (!src) {
        return createPlaceholderAvatar(isGroup, size);
    }

    const img = document.createElement('img');
    img.src = src;
    img.style.width = size + 'px';
    img.style.height = size + 'px';
    img.style.objectFit = 'cover';
    img.style.borderRadius = '50%';

    // On error, replace with placeholder
    img.onerror = function() {
        const placeholder = createPlaceholderAvatar(isGroup, size);
        // Copy over any classes from the failed img
        placeholder.className = img.className;
        img.replaceWith(placeholder);
    };

    return img;
}

/**
 * Tracks if we're in the initial chat open period for auto-scrolling
 */
let chatOpenAutoScrollTimer = null;

/**
 * Tracks the timestamp when a chat was opened for media load auto-scrolling
 */
let chatOpenTimestamp = 0;

/**
 * Synchronise all messages from the backend
 */
async function init() {
    // Check if account is selected
    try {
        await invoke("get_current_account");
    } catch (e) {
        console.log('[Init] No account selected, triggering fetch_messages');
        await invoke("fetch_messages", { init: true });
        return;
    }

    // Set up UI maintenance interval first (before any async calls that might fail)
    // Runs every 5 seconds to clear expired typing indicators and update timestamps
    let maintenanceTick = 0;
    setInterval(() => {
        maintenanceTick++;

        // Clear expired typing indicators (every tick)
        const now = Date.now() / 1000;
        arrChats.forEach(chat => {
            if (chat.active_typers && chat.active_typers.length > 0) {
                // Clear the array if we haven't received an update in 30 seconds
                if (!chat.last_typing_update || now - chat.last_typing_update > 30) {
                    chat.active_typers = [];

                    // If this is the open chat, refresh the display
                    if (strOpenChat === chat.id) {
                        updateChatHeaderSubtext(chat);
                    }

                    // Refresh chat list
                    if (domChats.style.display !== 'none') {
                        renderChatlist();
                    }
                }
            }
        });

        // Update chatlist timestamps every 6th tick (~30 seconds)
        if (maintenanceTick % 6 === 0 && domChats.style.display !== 'none') {
            updateChatlistTimestamps();
        }
    }, 5000);

    // Proceed to load and decrypt the database, and begin iterative Nostr synchronisation
    await invoke("fetch_messages", { init: true });

    // Begin an asynchronous loop to refresh profile data
    fetchProfiles().finally(async () => {
        setAsyncInterval(fetchProfiles, 45000);
    });

    // Display Invites (MLS Welcomes)
    await loadMLSInvites();
}

/**
 * Refresh and cache the member count for a given MLS group.
 * Also updates open chat header and re-renders chat list.
 */
async function refreshGroupMemberCount(groupId) {
    try {
        const result = await invoke('get_mls_group_members', { groupId });
        const chat = getOrCreateChat(groupId, 'MlsGroup');
        
        // get_mls_group_members returns { group_id, members, admins }
        if (result && result.members) {
            chat.metadata = chat.metadata || {};
            chat.metadata.custom_fields = chat.metadata.custom_fields || {};
            chat.metadata.custom_fields.member_count = result.members.length;
            chat.participants = result.members.slice();
            
            console.log(`[MLS] Updated member count for ${groupId.substring(0, 8)}: ${result.members.length} members`);
        }
        if (strOpenChat === groupId) {
            // Update the chat header subtext (respects typing indicators)
            updateChatHeaderSubtext(chat);
        }
    } catch (e) {
        console.warn('Failed to refresh group member count for', groupId, e);
    }
}

/**
 * Load pending MLS invites and render them
 */
async function loadMLSInvites() {
    try {
        const raw = await invoke('list_pending_mls_welcomes');
        // Normalize shape: backend should return an array; support {welcomes} or {items} fallback
        const welcomes = Array.isArray(raw) ? raw : (raw?.welcomes || raw?.items || []);
        arrMLSInvites = (welcomes || []).filter(Boolean);

        // Make sure to notify if there's pending invites
        updateChatBackNotification();
    } catch (e) {
        console.error('Failed to load MLS invites:', e);
    }
}

/**
 * Accept an MLS group invite
 * @param {string} welcomeEventId - The welcome event ID
 */
async function acceptMLSInvite(welcomeEventId) {
    try {
        console.log('Accepting MLS invite:', welcomeEventId);
        const success = await invoke('accept_mls_welcome', {
            welcomeEventIdHex: welcomeEventId
        });
        
        if (success) {
            // Reload invites
            await loadMLSInvites();

            // After rendering UI changes, ensure layout recalculates to prevent oversized chat list
            adjustSize();
        }
    } catch (e) {
        console.error('Failed to accept invite:', e);
        popupConfirm('Error', 'Failed to join group: ' + e, true, '', 'vector_warning.svg');
    }
}

/**
 * Decline an MLS invite (UI-only hide; backend filtering handles persistence)
 * @param {string} welcomeEventId - The welcome event ID
 */
function declineMLSInvite(welcomeEventId) {
    // Remove from UI; next backend fetch will exclude if server persisted dismissal
    arrMLSInvites = arrMLSInvites.filter(i => i.id !== welcomeEventId);

    // Make sure to notify if there's still pending invites, or remove the notification
    updateChatBackNotification();

    // After rendering UI changes, ensure layout recalculates to prevent oversized chat list
    adjustSize();
}

/**
 * A "thread" function dedicated to refreshing Profile data in the background
 * Also runs periodic maintenance tasks (cache cleanup, etc.)
 */
async function fetchProfiles() {
    // Use the new profile sync system
    await invoke("sync_all_profiles");

    // Run periodic maintenance (cache cleanup, memory optimization)
    invoke("run_maintenance").catch(() => {});
}

// Track pending status hide timeout
let statusHideTimeout = null;

/**
 * Update the chat header subtext (status/typing indicator) for the currently open chat
 * @param {Object} chat - The chat object
 */
function updateChatHeaderSubtext(chat) {
    if (!chat) return;

    // Clear any pending hide timeout
    if (statusHideTimeout) {
        clearTimeout(statusHideTimeout);
        statusHideTimeout = null;
    }

    let newStatusText = '';
    let shouldAddGradient = false;

    const isGroup = chat.chat_type === 'MlsGroup';
    const fNotes = chat.id === strPubkey;

    // Check for typing indicators first (shared logic)
    const typingText = generateTypingText(chat);

    if (fNotes) {
        newStatusText = 'Encrypted Notes to Self';
        shouldAddGradient = false;
    } else if (typingText) {
        // Someone is typing - use shared helper
        newStatusText = typingText;
        shouldAddGradient = true;
    } else if (isGroup) {
        // Not typing - show member count
        const memberCount = chat.metadata?.custom_fields?.member_count ? parseInt(chat.metadata.custom_fields.member_count) : null;
        if (typeof memberCount === 'number') {
            const label = memberCount === 1 ? 'member' : 'members';
            newStatusText = `${memberCount} ${label}`;
        } else {
            newStatusText = 'Members syncing...';
        }
        shouldAddGradient = false;
    } else {
        // DM - not typing, show profile status
        const profile = getProfile(chat.id);
        newStatusText = profile?.status?.title || '';
        shouldAddGradient = false;
    }
    
    const currentHasStatus = !!domChatContactStatus.textContent && !domChatContactStatus.classList.contains('status-hidden');
    const newHasStatus = !!newStatusText;
    
    if (newHasStatus) {
        // Show status: remove hidden class, update content, ensure visible
        domChatContactStatus.classList.remove('status-hidden');
        domChatContactStatus.style.display = ''; // Reset display in case it was hidden by else branch
        domChatContactStatus.textContent = newStatusText;
        domChatContactStatus.classList.toggle('text-gradient', shouldAddGradient);
        if (!shouldAddGradient) {
            twemojify(domChatContactStatus);
        }
        domChatContact.classList.remove('chat-contact');
        domChatContact.classList.add('chat-contact-with-status');
    } else if (currentHasStatus) {
        // Hide status: add hidden class, wait for animation, then clear content
        domChatContactStatus.classList.add('status-hidden');
        domChatContact.classList.remove('chat-contact-with-status');
        domChatContact.classList.add('chat-contact');
        
        // Clear content after animation completes (300ms matches CSS transition)
        statusHideTimeout = setTimeout(() => {
            domChatContactStatus.textContent = '';
            domChatContactStatus.classList.remove('text-gradient');
            statusHideTimeout = null;
        }, 300);
    }
    // If both are false (no status before, no status now), do nothing
}

// Store a hash of the last rendered state to detect actual changes
let lastChatlistStateHash = '';

/**
 * Generate a hash representing the current state of all chats
 */
function generateChatlistStateHash() {
    // Build a simple array of state values (faster than creating objects)
    const states = [];
    
    // Add invite IDs first
    for (const inv of arrMLSInvites) {
        states.push(inv.id || inv.welcome_event_id || inv.group_id);
    }
    
    // Add chat states (including chat ID to capture order changes)
    for (const chat of arrChats) {
        const isGroup = chat.chat_type === 'MlsGroup';
        const profile = !isGroup && chat.participants.length === 1 ? getProfile(chat.id) : null;
        const cLastMsg = chat.messages[chat.messages.length - 1];
        const nUnread = (chat.muted || (profile && profile.muted)) ? 0 : countUnreadMessages(chat);
        const activeTypers = chat.active_typers || [];
        
        // Push values directly (faster than creating object)
        // Include chat.id to ensure order changes are detected
        states.push(
            chat.id,
            nUnread,
            activeTypers.length,
            cLastMsg?.id,
            cLastMsg?.pending,
            profile?.nickname || profile?.name,
            profile?.avatar,
            profile?.avatar_cached,
            chat.muted
        );
    }
    
    return JSON.stringify(states);
}

/**
 * A "thread" function dedicated to rendering the Chat UI in real-time
 */
function renderChatlist() {
    if (fInit) return;

    // Generate a hash of the current RENDERABLE state
    const currentStateHash = generateChatlistStateHash();

    // If the renderable state hasn't changed, skip rendering entirely
    if (currentStateHash === lastChatlistStateHash) return;
    lastChatlistStateHash = currentStateHash;

    // Prep a fragment to re-render the full list in one sweep
    const fragment = document.createDocumentFragment();
    
    // Render invites first (at the top of the chat list)
    for (const invite of arrMLSInvites) {
        const divInvite = renderInviteItem(invite);
        fragment.appendChild(divInvite);
    }

    // Then render regular chats
    for (const chat of arrChats) {
        // For groups, we show them even if they have no messages yet
        // For DMs, we only show them if they have messages
        if (chat.chat_type !== 'MlsGroup' && chat.messages.length === 0) continue;

        // Do not render our own profile: it is accessible via the Bookmarks/Notes section
        if (chat.id === strPubkey) continue;

        const divContact = renderChat(chat);
        fragment.appendChild(divContact);
    }

    // Give the final element a bottom-margin boost to allow scrolling past the fadeout
    if (fragment.lastElementChild) fragment.lastElementChild.style.marginBottom = `50px`;

    // Nuke the existing list
    while (domChatList.firstChild) {
        domChatList.removeChild(domChatList.firstChild);
    }
    
    // Append our new fragment
    domChatList.appendChild(fragment);

    // Add a fade-in
    const divFade = document.createElement('div');
    divFade.classList.add(`fadeout-bottom`);
    divFade.style.bottom = `65px`;
    domChatList.appendChild(divFade);
    
    // Update the back button notification
    updateChatBackNotification();
}

/**
 * Render an MLS invite as a chat-like item
 * @param {MLSWelcome} invite - The invite we're rendering
 */
function renderInviteItem(invite) {
    const groupId = invite.group_id || invite.id || '';
    const groupName =
        invite.group_name ||
        invite.name ||
        (groupId ? `Group ${String(groupId).substring(0, 8)}...` : 'Unnamed Group');

    const memberCount =
        (invite.member_count ??
            (Array.isArray(invite.members) ? invite.members.length : invite.memberCount)) || 0;

    // Create the invite container styled like a chat item
    const divInvite = document.createElement('div');
    divInvite.classList.add('chatlist-contact', 'chatlist-invite');
    divInvite.id = `invite-${invite.id || invite.welcome_event_id || groupId}`;
    divInvite.style.borderColor = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();

    // Avatar container with group placeholder
    const divAvatarContainer = document.createElement('div');
    divAvatarContainer.style.position = 'relative';
    divAvatarContainer.appendChild(createPlaceholderAvatar(true, 50));
    divInvite.appendChild(divAvatarContainer);

    // Preview container with group name and member count
    const divPreviewContainer = document.createElement('div');
    divPreviewContainer.classList.add('chatlist-contact-preview');

    // Group name
    const h4Name = document.createElement('h4');
    h4Name.textContent = groupName;
    h4Name.classList.add('cutoff');
    divPreviewContainer.appendChild(h4Name);

    // Member count as subtext
    const pMemberCount = document.createElement('p');
    pMemberCount.classList.add('cutoff');
    pMemberCount.textContent = `${memberCount} ${memberCount === 1 ? 'member' : 'members'}`;
    divPreviewContainer.appendChild(pMemberCount);

    divInvite.appendChild(divPreviewContainer);

    // Action buttons container (replaces timestamp area)
    const divActions = document.createElement('div');
    divActions.classList.add('invite-action-buttons');

    // Accept button (green check)
    const btnAccept = document.createElement('button');
    btnAccept.classList.add('invite-action-btn', 'invite-accept-btn');
    btnAccept.title = 'Accept Invite';
    btnAccept.onclick = (e) => {
        e.stopPropagation();
        acceptMLSInvite(invite.id || invite.welcome_event_id || groupId);
    };
    const acceptIcon = document.createElement('span');
    acceptIcon.classList.add('icon', 'icon-check');
    acceptIcon.style.width = '16px';
    acceptIcon.style.height = '16px';
    acceptIcon.style.backgroundColor = '#59fcb3';
    btnAccept.appendChild(acceptIcon);

    // Decline button (danger color X)
    const btnDecline = document.createElement('button');
    btnDecline.classList.add('invite-action-btn', 'invite-decline-btn');
    btnDecline.title = 'Decline Invite';
    btnDecline.onclick = (e) => {
        e.stopPropagation();
        declineMLSInvite(invite.id || invite.welcome_event_id || groupId);
    };
    const declineIcon = document.createElement('span');
    declineIcon.classList.add('icon', 'icon-x');
    declineIcon.style.width = '16px';
    declineIcon.style.height = '16px';
    declineIcon.style.backgroundColor = '#ff2ea9';
    btnDecline.appendChild(declineIcon);

    divActions.appendChild(btnAccept);
    divActions.appendChild(btnDecline);
    divInvite.appendChild(divActions);

    return divInvite;
}

/**
 * Generate typing indicator text for a chat
 * @param {Chat} chat - The chat object
 * @returns {string|null} - The typing text, or null if no one is typing
 */
function generateTypingText(chat) {
    const activeTypers = chat.active_typers || [];
    if (activeTypers.length === 0) return null;

    const isGroup = chat.chat_type === 'MlsGroup';

    // DMs just show "Typing..." since we already know who it is
    if (!isGroup) return 'Typing...';

    // Groups show names
    if (activeTypers.length === 1) {
        const typer = getProfile(activeTypers[0]);
        const name = typer?.nickname || typer?.name || 'Someone';
        return `${name} is typing...`;
    } else if (activeTypers.length === 2) {
        const typer1 = getProfile(activeTypers[0]);
        const typer2 = getProfile(activeTypers[1]);
        const name1 = typer1?.nickname || typer1?.name || 'Someone';
        const name2 = typer2?.nickname || typer2?.name || 'Someone';
        return `${name1} and ${name2} are typing...`;
    } else if (activeTypers.length === 3) {
        const typer1 = getProfile(activeTypers[0]);
        const typer2 = getProfile(activeTypers[1]);
        const typer3 = getProfile(activeTypers[2]);
        const name1 = typer1?.nickname || typer1?.name || 'Someone';
        const name2 = typer2?.nickname || typer2?.name || 'Someone';
        const name3 = typer3?.nickname || typer3?.name || 'Someone';
        return `${name1}, ${name2}, and ${name3} are typing...`;
    } else {
        return 'Several people are typing...';
    }
}

/**
 * Generate chat preview text for the chatlist
 * @param {Chat} chat - The chat object
 * @returns {{ text: string, isTyping: boolean, needsTwemoji: boolean }}
 */
function generateChatPreviewText(chat) {
    const isGroup = chat.chat_type === 'MlsGroup';
    const cLastMsg = chat.messages[chat.messages.length - 1];

    // Handle typing indicators
    const typingText = generateTypingText(chat);
    if (typingText) {
        return { text: typingText, isTyping: true, needsTwemoji: false };
    }

    // No messages
    if (!cLastMsg) {
        if (isGroup) {
            const memberCount = chat.metadata?.custom_fields?.member_count ? parseInt(chat.metadata.custom_fields.member_count) : null;
            return {
                text: (memberCount != null) ? `${memberCount} ${memberCount === 1 ? 'member' : 'members'}` : 'No messages yet',
                isTyping: false,
                needsTwemoji: false
            };
        } else {
            return { text: 'Start a conversation', isTyping: false, needsTwemoji: false };
        }
    }

    // Pending message
    if (cLastMsg.pending) {
        return { text: 'Sending...', isTyping: false, needsTwemoji: false };
    }

    // Build sender prefix for groups
    let senderPrefix = '';
    if (cLastMsg.mine) {
        senderPrefix = 'You: ';
    } else if (isGroup && cLastMsg.npub) {
        const senderProfile = getProfile(cLastMsg.npub);
        const senderName = senderProfile?.nickname || senderProfile?.name || cLastMsg.npub.substring(0, 16);
        senderPrefix = `${senderName}: `;
    }

    // Attachment message
    if (!cLastMsg.content && cLastMsg.attachments?.length) {
        return {
            text: senderPrefix + 'Sent a ' + getFileTypeInfo(cLastMsg.attachments[0].extension).description,
            isTyping: false,
            needsTwemoji: false
        };
    }

    // PIVX payment message
    if (cLastMsg.pivx_payment) {
        return { text: senderPrefix + 'Sent a PIVX Payment', isTyping: false, needsTwemoji: false };
    }

    // System event (member joined/left, etc.)
    if (cLastMsg.system_event) {
        return { text: cLastMsg.content, isTyping: false, needsTwemoji: false };
    }

    // Regular text message
    return { text: senderPrefix + cLastMsg.content, isTyping: false, needsTwemoji: true };
}

/**
 * Render a Chat Preview for the Chat List
 * @param {Chat} chat - The profile we're rendering
 */
function renderChat(chat) {
    // For groups, we don't have a profile, for DMs we do
    const isGroup = chat.chat_type === 'MlsGroup';
    const profile = !isGroup && chat.participants.length === 1 ? getProfile(chat.id) : null;
    
    // Collect the Unread Message count for 'Unread' emphasis and badging
    // Ensure muted chats OR muted profiles do not show unread glow
    const nUnread = (chat.muted || (profile && profile.muted)) ? 0 : countUnreadMessages(chat);

    // The Chat container (The ID is the Contact's npub)
    const divContact = document.createElement('div');
    if (nUnread) divContact.style.borderColor = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
    divContact.classList.add('chatlist-contact');
    divContact.id = `chatlist-${chat.id}`;

    // The Username + Message Preview container
    const divPreviewContainer = document.createElement('div');
    divPreviewContainer.classList.add('chatlist-contact-preview');

    // The avatar, if one exists
    const divAvatarContainer = document.createElement('div');
    divAvatarContainer.style.position = `relative`;
    
    if (isGroup) {
        // For groups, show the group placeholder SVG
        divAvatarContainer.appendChild(createPlaceholderAvatar(true, 50));
    } else {
        const avatarSrc = getProfileAvatarSrc(profile);
        if (avatarSrc) {
            const imgAvatar = document.createElement('img');
            imgAvatar.src = avatarSrc;
            // Fallback to placeholder if image fails to load
            imgAvatar.onerror = () => {
                imgAvatar.replaceWith(createPlaceholderAvatar(false, 50));
            };
            divAvatarContainer.appendChild(imgAvatar);
        } else {
            // Otherwise, generate a placeholder avatar
            divAvatarContainer.appendChild(createPlaceholderAvatar(false, 50));
        }
    }

    // Add the "Status Icon" to the avatar, then plug-in the avatar container
    // TODO: currently, we "emulate" the status; messages in the last 5m are "online", messages in the last 30m are "away", otherwise; offline.
    if (!isGroup) {
        const divStatusIcon = document.createElement('div');
        divStatusIcon.classList.add('avatar-status-icon');
        
        // Find the last message from the contact (not from the user)
        let cLastContactMsg = null;
        for (let i = chat.messages.length - 1; i >= 0; i--) {
            if (!chat.messages[i].mine) {
                cLastContactMsg = chat.messages[i];
                break;
            }
        }
        
        if (cLastContactMsg && cLastContactMsg.at > Date.now() - 60000 * 5) {
            // set the divStatusIcon .backgroundColor to green (online)
            divStatusIcon.style.backgroundColor = '#59fcb3';
            divAvatarContainer.appendChild(divStatusIcon);
        }
        else if (cLastContactMsg && cLastContactMsg.at > Date.now() - 60000 * 30) {
            // set to orange (away)
            divStatusIcon.style.backgroundColor = '#fce459';
            divAvatarContainer.appendChild(divStatusIcon);
        }
        // offline... don't show status icon at all (no need to append the divStatusIcon)
    }
    
    divContact.appendChild(divAvatarContainer);

    // Add the name to the chat preview
    const h4ContactName = document.createElement('h4');
    if (isGroup) {
        // For groups, extract name from metadata or use a default
        h4ContactName.textContent = chat.metadata?.custom_fields?.name || `Group ${chat.id.substring(0, 8)}...`;
    } else {
        h4ContactName.textContent = profile?.nickname || profile?.name || chat.id;
        if (profile?.nickname || profile?.name) twemojify(h4ContactName);
        
        // Add bot icon if this is a bot profile
        if (profile?.bot) {
            const botIconContainer = document.createElement('span');
            botIconContainer.className = 'icon icon-bot';
            botIconContainer.style.width = '14px';
            botIconContainer.style.height = '14px';
            botIconContainer.style.marginLeft = '6px';
            botIconContainer.style.display = 'inline-block';
            botIconContainer.style.verticalAlign = 'initial';
            botIconContainer.style.position = 'relative';
            botIconContainer.style.backgroundColor = '#59fcb3';
            h4ContactName.appendChild(botIconContainer);
        }
    }
    h4ContactName.classList.add('cutoff')
    divPreviewContainer.appendChild(h4ContactName);

    // Display either their Last Message or Typing Indicator
    const cLastMsg = chat.messages[chat.messages.length - 1];
    const pChatPreview = document.createElement('p');
    pChatPreview.classList.add('cutoff');

    const preview = generateChatPreviewText(chat);
    pChatPreview.classList.toggle('text-gradient', preview.isTyping);
    pChatPreview.textContent = preview.text;
    if (preview.needsTwemoji) twemojify(pChatPreview);

    divPreviewContainer.appendChild(pChatPreview);

    // Add the Chat Preview to the contact UI
    divContact.appendChild(divPreviewContainer);

    // Display the "last message" time
    const pTimeAgo = document.createElement('p');
    pTimeAgo.classList.add('chatlist-contact-timestamp');
    if (cLastMsg) {
        pTimeAgo.textContent = timeAgo(cLastMsg.at);
    }
    // Apply 'Unread' final styling
    if (nUnread) {
        pTimeAgo.style.color = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
    } else {
        // Add 'read' class for smaller font size when no unread messages
        pTimeAgo.classList.add('read');
    }
    divContact.appendChild(pTimeAgo);

    return divContact;
}

/**
 * Update only the preview text and timestamp for a specific chat in the chatlist
 * This is more efficient than re-rendering the entire chatlist for a single message edit
 * @param {string} chatId - The chat ID to update
 */
function updateChatlistPreview(chatId) {
    const chatElement = document.getElementById(`chatlist-${chatId}`);
    if (!chatElement) {
        // Chat not in DOM - fallback to full render
        renderChatlist();
        return;
    }

    const cChat = getChat(chatId);
    if (!cChat) return;

    // Find the preview text element (p.cutoff inside the preview container)
    const previewContainer = chatElement.querySelector('.chatlist-contact-preview');
    if (!previewContainer) return;

    const pChatPreview = previewContainer.querySelector('p.cutoff');
    const pTimeAgo = chatElement.querySelector('.chatlist-contact-timestamp');

    if (pChatPreview) {
        const preview = generateChatPreviewText(cChat);
        pChatPreview.classList.toggle('text-gradient', preview.isTyping);
        pChatPreview.textContent = preview.text;
        if (preview.needsTwemoji) twemojify(pChatPreview);
    }

    // Update timestamp
    const cLastMsg = cChat.messages[cChat.messages.length - 1];
    if (pTimeAgo && cLastMsg) {
        pTimeAgo.textContent = timeAgo(cLastMsg.at);
    }
}

/**
 * Count the quantity of unread messages
 * @param {Chat} chat - The Chat we're checking
 * @returns {number} - The amount of unread messages, if any
 */
function countUnreadMessages(chat) {
    // If no messages, return 0
    if (!chat.messages || !chat.messages.length) return 0;
    
    // Walk backwards from the end to count unread messages
    // Stop when we hit: 1) our own message, or 2) the last_read message
    let unreadCount = 0;

    for (let i = chat.messages.length - 1; i >= 0; i--) {
        const msg = chat.messages[i];
        
        // If we hit our own message, stop - we clearly read everything before it
        if (msg.mine) {
            break;
        }
        
        // If we hit the last_read message, stop - everything at and before this is read
        if (chat.last_read && msg.id === chat.last_read) {
            break;
        }
        
        // Count this message as unread
        unreadCount++;
    }

    return unreadCount;
}

/**
 * Update the notification dot on the chat back button
 * Shows the dot if there are unread messages in OTHER chats (not the currently open one) OR unanswered invites
 */
function updateChatlistTimestamps() {
    // Get all chatlist items that are currently displayed
    const chatListItems = document.querySelectorAll('.chatlist-contact');
    
    // For each chat item, find and update the timestamp and status
    chatListItems.forEach(item => {
        // Extract chat ID from the item's ID (format: chatlist-{chatId})
        const chatId = item.id.substring(9);
        
        // Find the corresponding chat in our array
        const chat = arrChats.find(c => c.id === chatId);
        
        if (chat && chat.messages.length > 0) {
            // Get the last message timestamp
            const lastMessage = chat.messages[chat.messages.length - 1];
            
            // Skip updating if the message is older than 1 week (for performance)
            // Messages older than 1 week display as "1w", "2w", etc. and are unlikely to change
            if (lastMessage?.at < Date.now() - 604800000) return;
            
            // Find the timestamp element in this chat item
            const timestampElement = item.querySelector('.chatlist-contact-timestamp');
            
            if (timestampElement) {
                // Update the timestamp text using timeAgo function
                timestampElement.textContent = timeAgo(lastMessage.at);
            }
            
            // Update status indicator if needed (for DMs only)
            const avatarContainer = item.querySelector('.avatar-status-icon')?.parentElement;
            if (avatarContainer && chat.chat_type !== 'MlsGroup') {
                // Remove existing status icon if present
                const existingStatusIcon = avatarContainer.querySelector('.avatar-status-icon');
                if (existingStatusIcon) {
                    existingStatusIcon.remove();
                }
                
                // Add new status icon based on last message time
                const divStatusIcon = document.createElement('div');
                divStatusIcon.classList.add('avatar-status-icon');
                
                // Find the last message from the contact (not from the user)
                let cLastContactMsg = null;
                for (let i = chat.messages.length - 1; i >= 0; i--) {
                    if (!chat.messages[i].mine) {
                        cLastContactMsg = chat.messages[i];
                        break;
                    }
                }
                
                if (cLastContactMsg && cLastContactMsg.at > Date.now() - 60000 * 5) {
                    // set the divStatusIcon .backgroundColor to green (online)
                    divStatusIcon.style.backgroundColor = '#59fcb3';
                    avatarContainer.appendChild(divStatusIcon);
                }
                else if (cLastContactMsg && cLastContactMsg.at > Date.now() - 60000 * 30) {
                    // set to orange (away)
                    divStatusIcon.style.backgroundColor = '#fce459';
                    avatarContainer.appendChild(divStatusIcon);
                }
                // offline... don't show status icon at all (no need to append the divStatusIcon)
            }
        }
    });
}

function updateChatBackNotification() {
    if (!domChatBackNotificationDot) return;
    
    // Check if we're currently in a chat
    if (!strOpenChat) {
        domChatBackNotificationDot.style.display = 'none';
        return;
    }
    
    // Check if there are any unanswered MLS invites
    const hasUnansweredInvites = arrMLSInvites.length > 0;
    
    // Check if any OTHER chat has unread messages
    const hasOtherUnreads = arrChats.some(chat => {
        // Skip the currently open chat
        if (chat.id === strOpenChat) return false;
        
        // Skip chats with no messages (same as chatlist rendering)
        if (!chat.messages || chat.messages.length === 0) return false;
        
        // Skip our own profile (bookmarks/notes)
        if (chat.id === strPubkey) return false;
        
        // Get profile for DM chats
        const isGroup = chat.chat_type === 'MlsGroup';
        const profile = !isGroup && chat.participants.length === 1 ? getProfile(chat.id) : null;
        
        // Skip muted chats or muted profiles (same logic as chatlist rendering)
        if (chat.muted || (profile && profile.muted)) return false;
        
        // Check if this chat has unread messages
        return countUnreadMessages(chat) > 0;
    });
    
    // Show or hide the notification dot (show if there are unread messages OR unanswered invites)
    domChatBackNotificationDot.style.display = (hasOtherUnreads || hasUnansweredInvites) ? '' : 'none';
}

/**
 * Sets a specific message as the last read message
 * @param {Chat} chat - The Chat to update
 * @param {Message|string} message - The Message to set as last read
 */
function markAsRead(chat, message) {
    // If we have a chat, and we haven't already marked as read, update its last_read and notify backend
    if (chat && message.id !== chat.last_read) {
        chat.last_read = message.id;

        // Persist via backend using chat-based API
        invoke("mark_as_read", { chatId: chat.id, messageId: message.id });
    }
}

/**
 * Send a NIP-17 message to a Nostr user
 * @param {string} pubkey - The user's pubkey
 * @param {string} content - The content of the message
 * @param {string?} replied_to - The reference of the message, if any
 */
async function message(pubkey, content, replied_to) {
    await invoke("message", { receiver: pubkey, content: content, repliedTo: replied_to });
}

/**
 * Send a file via NIP-96 server to a Nostr user or group
 * @param {string} pubkey - The user's pubkey or group_id
 * @param {string?} replied_to - The reference of the message, if any
 * @param {string} filepath - The absolute file path
 */
async function sendFile(pubkey, replied_to, filepath) {
    try {
        // Use the protocol-agnostic file_message command for both DMs and MLS groups
        await invoke("file_message", { receiver: pubkey, repliedTo: replied_to, filePath: filepath });
    } catch (e) {
        // Notify of an attachment send failure
        popupConfirm(e, '', true, '', 'vector_warning.svg');
    }
    nLastTypingIndicator = 0;
}

/**
 * Setup our Rust Event listeners, used for relaying the majority of backend changes
 */
async function setupRustListeners() {
    // Listen for MLS message events
    await listen('mls_message_new', async (evt) => {
        const { group_id, message } = evt.payload;
        console.log('MLS message received for group:', group_id, 'pending:', message?.pending, 'attachments:', message?.attachments?.length);
        
        // Validate message has required fields
        if (!message || !message.id || !message.at) {
            console.warn('Invalid message received (missing id or timestamp):', message);
            return;
        }
        
        // Find or create the group chat
        const chat = getOrCreateChat(group_id, 'MlsGroup');
        
        // Check for duplicates in chat.messages
        const existingMsg = chat.messages?.find(m => m.id === message.id);
        if (existingMsg) {
            return;
        }
        
        // Add to event cache
        // During sync, only add if this chat is currently open (to avoid cache flooding)
        // After sync complete, always add to cache
        const shouldAddToCache = fSyncComplete || group_id === strOpenChat;
        if (shouldAddToCache) {
            const added = eventCache.addEvent(group_id, message);
            if (!added) return;
        }
        
        // Clear typing indicator for the sender when they send a message
        if (!message.mine && chat.active_typers) {
            // For group chats, use npub if available; for DMs, use sender identifier
            chat.active_typers = chat.active_typers.filter(npub => npub !== message.npub);
        }
        
        // Find the correct position to insert the message based on timestamp (efficient binary search)
        const messages = chat.messages;
        
        // Check if the array is empty or the new message is newer than the newest message
        if (messages.length === 0 || message.at > messages[messages.length - 1].at) {
            // Insert at the end (newest)
            messages.push(message);
        }
        // Check if the new message is older than the oldest message
        else if (message.at < messages[0].at) {
            // Insert at the beginning (oldest)
            messages.unshift(message);
        }
        // Otherwise, find the correct position in the middle using binary search
        else {
            // Binary search for better performance with large message arrays
            let low = 0;
            let high = messages.length - 1;
            
            while (low <= high) {
                const mid = Math.floor((low + high) / 2);
                
                if (messages[mid].at < message.at) {
                    low = mid + 1;
                } else {
                    high = mid - 1;
                }
            }
            
            // Insert the message at the correct position (low is now the index where it should go)
            messages.splice(low, 0, message);
        }
        
        // If this group has the open chat, update it
        if (strOpenChat === group_id) {
            updateChat(chat, [message]);
            // Increment rendered count since we're adding a new message
            proceduralScrollState.renderedMessageCount++;
            proceduralScrollState.totalMessageCount++;
        } else {
            console.log('Group chat not open, message added to background chat');
        }
        
        // Resort chat list order so recent groups bubble up (fallback to metadata)
        arrChats.sort((a, b) => getChatSortTimestamp(b) - getChatSortTimestamp(a));

        // Re-render chat list
        renderChatlist();

        // Update the back button notification dot (for unread messages in other chats)
        updateChatBackNotification();
    });

    // Listen for MLS invite received events (real-time)
    await listen('mls_invite_received', async (evt) => {
        console.log('MLS invite received in real-time, refreshing invites list');
        // Reload invites list to show the new invite
        await loadMLSInvites();
        // Re-render chatlist to display the new invite
        renderChatlist();
    });

    // Listen for MLS group metadata updates
    await listen('mls_group_metadata', async (evt) => {
        try {
            const metadata = evt.payload?.metadata;
            const { chat, changed } = applyMlsGroupMetadata(metadata);
            if (!chat || !changed) return;

            if (strOpenChat === chat.id) {
                const groupName = chat.metadata?.custom_fields?.name || `Group ${strOpenChat.substring(0, 10)}...`;
                domChatContact.textContent = groupName;
                updateChatHeaderSubtext(chat);
            }

            const overviewGroupId = domGroupOverview.getAttribute('data-group-id');
            if (overviewGroupId && overviewGroupId === chat.id) {
                await renderGroupOverview(chat);
            }

            renderChatlist();
        } catch (e) {
            console.error('Error handling mls_group_metadata event:', e);
        }
    });

    // Listen for MLS welcome accepted events
    await listen('mls_welcome_accepted', async (evt) => {
        console.log('MLS welcome accepted, refreshing groups and invites');
        // Reload invites
        await loadMLSInvites();
    });

    // Listen for MLS group updates (member additions, removals, etc.)
    await listen('mls_group_updated', async (evt) => {
        try {
            const { group_id } = evt.payload || {};
            console.log('MLS group updated:', group_id);
            
            // Refresh member count
            await refreshGroupMemberCount(group_id);
            
            // If the group overview is currently open for THIS SPECIFIC group, refresh it
            if (domGroupOverview.style.display !== 'none') {
                // Check if the overview is for the same group that was updated
                const overviewGroupId = domGroupOverview.getAttribute('data-group-id');
                if (overviewGroupId === group_id) {
                    const currentChat = arrChats.find(c => c.id === group_id);
                    if (currentChat) {
                        await renderGroupOverview(currentChat);
                    }
                }
            }
            
            // Refresh chat list to update any UI elements
            renderChatlist();
        } catch (e) {
            console.error('Error handling mls_group_updated event:', e);
        }
    });
    
    // Listen for MLS group left events
    await listen('mls_group_left', async (evt) => {
        try {
            const { group_id } = evt.payload || {};
            console.log('[MLS][Frontend] Received mls_group_left event for group:', group_id?.substring(0, 16));
            
            // Remove the group from the chat list
            const chatIndex = arrChats.findIndex(c => c.id === group_id);
            if (chatIndex !== -1) {
                console.log('[MLS][Frontend] Removing group from arrChats at index:', chatIndex);
                arrChats.splice(chatIndex, 1);
            } else {
                console.log('[MLS][Frontend] Group not found in arrChats');
            }
            
            // Close the group overview if it's open for this group
            const overviewGroupId = domGroupOverview.getAttribute('data-group-id');
            if (overviewGroupId === group_id) {
                domGroupOverview.style.display = 'none';
                domGroupOverview.removeAttribute('data-group-id');
            }
            
            // If this was the active chat, close it and return to chat list
            if (strOpenChat === group_id) {
                await closeChat();
            }
            
            // Refresh chat list
            renderChatlist();
        } catch (e) {
            console.error('Error handling mls_group_left event:', e);
        }
    });
    
    // Listen for MLS initial sync completion after joining a group
    await listen('mls_group_initial_sync', async (evt) => {
        try {
            const { group_id, processed, new: newCount } = evt.payload || {};
            console.log('MLS initial group sync complete:', group_id, 'processed:', processed, 'new:', newCount);

            // Ensure the group chat exists even if there are no messages yet
            getOrCreateChat(group_id, 'MlsGroup');
            await refreshGroupMemberCount(group_id);

            // Resort chat list order by last activity (message or metadata)
            arrChats.sort((a, b) => getChatSortTimestamp(b) - getChatSortTimestamp(a));

            // Re-render the chat list so empty groups show with "No messages yet" preview
            renderChatlist();
        } catch (e) {
            console.error('Error handling mls_group_initial_sync:', e);
        }
    });

    // Listen for system events (member joined/left, etc.)
    await listen('system_event', async (evt) => {
        try {
            const { conversation_id, event_id, event_type, member_pubkey, member_name } = evt.payload || {};
            console.log('[System Event] Received:', event_id, event_type);

            // Deduplication by event_id
            const chat = arrChats.find(c => c.id === conversation_id);
            if (chat && chat.messages.some(msg => msg.id === event_id)) {
                console.log('[System Event] Skipping duplicate:', event_id);
                return;
            }

            // Build display content
            const displayName = member_name || member_pubkey?.substring(0, 12) + '...';
            let content;
            switch (event_type) {
                case SystemEventType.MemberLeft:
                    content = `${displayName} has left`;
                    break;
                case SystemEventType.MemberJoined:
                    content = `${displayName} has joined`;
                    break;
                case SystemEventType.MemberRemoved:
                    content = `${displayName} was removed`;
                    break;
                default:
                    content = `System event ${event_type}: ${displayName}`;
            }

            // Create system event message using the event_id
            const systemMsg = {
                id: event_id,
                at: Date.now(),
                content: content,
                mine: false,
                attachments: [],
                system_event: {
                    event_type: event_type,
                    member_npub: member_pubkey,
                }
            };

            // Add to chat messages via cache (handles deduplication)
            // Note: chat.messages and cache share the same array reference, so only use cache
            eventCache.addEvent(conversation_id, systemMsg);

            // If this chat is currently open, render the system event
            if (strOpenChat === conversation_id && domChatMessages) {
                const systemElement = insertSystemEvent(content);
                domChatMessages.appendChild(systemElement);
                softChatScroll();
            }

            // Re-render chatlist
            renderChatlist();
        } catch (e) {
            console.error('Error handling system_event:', e);
        }
    });

    // Listen for Synchronisation Finish updates
    await listen('sync_finished', async (_) => {
        // Mark sync as complete - this allows real-time messages to be cached
        fSyncComplete = true;
        
        // Fade out the sync line
        domSyncLine.classList.remove('active');
        domSyncLine.classList.add('fade-out');
        
        // Wait for fade animation to complete, then reset
        setTimeout(() => {
            domSyncLine.classList.remove('fade-out');
            if (!strOpenChat) adjustSize();
        }, 300);
    });

    // Listen for Synchronisation Slice updates
    await listen('sync_slice_finished', (_) => {
        // Continue synchronising until event `sync_finished` is emitted
        invoke("fetch_messages", { init: false });
    });

    // Listen for Synchronisation Progress updates
    await listen('sync_progress', (evt) => {
        // Show and activate the sync line when syncing is in progress
        // Only add 'active' if it's not already active to avoid restarting the animation
        if (!fInit && !domSyncLine.classList.contains('active')) {
            domSyncLine.classList.remove('fade-out');
            domSyncLine.classList.add('active');
        }
        if (!strOpenChat) adjustSize();
    });

    // Listen for Attachment Upload Progress events
    await listen('attachment_upload_progress', async (evt) => {
        if (strOpenChat) {
            let divUpload = document.getElementById(evt.payload.id + '_file');
            if (divUpload) {
                // Update the Download Progress bar
                divUpload.style.width = `${evt.payload.progress}%`;
            }
        }
    });

    // Listen for Attachment Download Progress events
    await listen('attachment_download_progress', async (evt) => {
        // Update the in-memory attachment
        if (strOpenChat) {
            let divDownload = document.getElementById(evt.payload.id);
            if (divDownload) {
                // Check if we need a text label (for non-image files)
                let iLabel = divDownload.querySelector('.download-label');
                if (iLabel) {
                    // Update the text with progress
                    iLabel.textContent = `Downloading... (${evt.payload.progress}%)`;
                }
                
                let divBar = divDownload.querySelector('.progress-bar');
                if (divBar) {
                    // Update the Download Progress bar
                    divBar.style.width = `${evt.payload.progress}%`;
                } else {
                    // Create the Download Progress container
                    let newDivDownload = document.createElement('div');
                    newDivDownload.id = evt.payload.id;
                    newDivDownload.style.minWidth = `200px`;
                    newDivDownload.style.textAlign = `center`;
                    
                    // For non-image files, add a text label
                    // Check if the element being updated is a non-image file by looking for existing download button attributes
                    const attachmentElement = document.getElementById(evt.payload.id);
                    const isNonImageAttachment = attachmentElement && (attachmentElement.hasAttribute('download') || attachmentElement.classList.contains('btn'));
                    
                    if (isNonImageAttachment) {
                        const iLabel = document.createElement('i');
                        iLabel.classList.add('download-label');
                        iLabel.textContent = `Downloading... (${evt.payload.progress}%)`;
                        iLabel.style.display = `block`;
                        iLabel.style.marginBottom = `5px`;
                        newDivDownload.appendChild(iLabel);
                    }

                    // Create the Download Progress bar
                    divBar = document.createElement('div');
                    divBar.classList.add('progress-bar');
                    divBar.style.width = `0%`;
                    divBar.style.height = `5px`;
                    divBar.style.marginTop = `0`;
                    newDivDownload.appendChild(divBar);

                    // Replace the previous UI
                    divDownload.replaceWith(newDivDownload);
                }
            }
        }
    });

    // Listen for Attachment Download Results
    await listen('attachment_download_result', async (evt) => {
        // Update the in-memory attachment (works for both DMs and Group Chats)
        let cChat = getChat(evt.payload.profile_id);
        if (!cChat) return;
        
        let cMsg = cChat.messages.find(m => m.id === evt.payload.msg_id);
        if (!cMsg) return;
    
        // When an attachment is being updated (i.e: post-hashing ID change), we reference the original nonce-based hash via old_id, otherwise, we use ID, as nothing changed
        let cAttachment = cMsg.attachments.find(a => a.id === evt.payload?.old_id || evt.payload.id);
        if (!cAttachment) return;

        cAttachment.downloading = false;
        if (evt.payload.success) {
            cAttachment.downloaded = true;
            // Update our ID and Path
            if (evt.payload.old_id) {
                cAttachment.id = evt.payload.id;
                cAttachment.path = cAttachment.path.replace(evt.payload.old_id, evt.payload.id);
            }

            // If this user has an open chat, then update the rendered message
            if (strOpenChat === evt.payload.profile_id) {
                const domMsg = document.getElementById(evt.payload.msg_id);
                const profile = getProfile(evt.payload.profile_id);
                domMsg?.replaceWith(renderMessage(cMsg, profile, evt.payload.msg_id));

                // Scroll accordingly
                softChatScroll();
            }
        } else {
            // Display the reason the download failed and allow restarting it
            const divDownload = document.getElementById(evt.payload.id);
            const iFailed = document.createElement('i');
            iFailed.id = evt.payload.id;
            iFailed.toggleAttribute('download', true);
            iFailed.setAttribute('npub', evt.payload.profile_id);
            iFailed.setAttribute('msg', evt.payload.msg_id);
            iFailed.classList.add('btn');
            iFailed.textContent = `Retry Download (${evt.payload.result})`;
            divDownload.replaceWith(iFailed);
        }
    });

    // Listen for profile updates
    await listen('profile_update', (evt) => {
        // Check if the frontend is already aware
        const nProfileIdx = arrProfiles.findIndex(p => p.id === evt.payload.id);
        let avatarCacheChanged = false;
        if (nProfileIdx >= 0) {
            // Check if avatar cache changed (for triggering chatlist re-render)
            avatarCacheChanged = arrProfiles[nProfileIdx].avatar_cached !== evt.payload.avatar_cached;

            // Update our frontend memory
            arrProfiles[nProfileIdx] = evt.payload;

            // If this is our profile, make sure to render it's changes
            if (arrProfiles[nProfileIdx].mine) {
                renderCurrentProfile(arrProfiles[nProfileIdx]);
            }
        } else {
            // Add the new profile
            arrProfiles.push(evt.payload);
            avatarCacheChanged = !!evt.payload.avatar_cached;
        }

        // If this user has an open chat, then soft-update the chat header
        if (strOpenChat === evt.payload.id) {
            const chat = getDMChat(evt.payload.id);
            const profile = getProfile(evt.payload.id);
            if (chat && profile) {
                updateChat(chat, [], profile);
            }
        }

        // Re-render chatlist if avatar cache changed (so cached images show up)
        if (avatarCacheChanged && !strOpenChat) {
            renderChatlist();
        }
        
        // Update any profile previews in the chat messages for this npub (regardless of which chat is open)
        const profilePreviews = document.querySelectorAll(`.msg-profile-preview[data-npub="${evt.payload.id}"]`);
        profilePreviews.forEach(preview => {
            updateNostrProfilePreview(preview, evt.payload);
        });
        
        // If this user is being viewed in the Expanded Profile View, update it
        if (domProfileId.textContent === evt.payload.id) {
            renderProfileTab(evt.payload);
        }
        
        // Render the Chat List
        renderChatlist();
    });

    await listen('profile_muted', (evt) => {
        // Update the chat's muted status
        const cChat = getDMChat(evt.payload.profile_id);
        if (cChat) {
            cChat.muted = evt.payload.value;
        }
        
        // Also update profile if it exists
        const cProfile = getProfile(evt.payload.profile_id);
        if (cProfile) {
            cProfile.muted = evt.payload.value;
        }

        // If this profile is Expanded, update the Mute UI
        if (domProfileId.textContent === evt.payload.profile_id && cProfile) {
            domProfileOptionMute.querySelector('span').classList.replace('icon-volume-' + (cProfile.muted ? 'max' : 'mute'), 'icon-volume-' + (cProfile.muted ? 'mute' : 'max'));
            domProfileOptionMute.querySelector('p').innerText = cProfile.muted ? 'Unmute' : 'Mute';
        }

        // Re-render the chat list to immediately reflect glow/badge changes
        renderChatlist();
    });

    await listen('profile_nick_changed', (evt) => {
        // Update the profile's nickname
        const cProfile = getProfile(evt.payload.profile_id);
        if (cProfile) {
            cProfile.nickname = evt.payload.value;

            // If this profile is Expanded, update the UI
            if (domProfileId.textContent === evt.payload.profile_id) {
                renderProfileTab(cProfile);
            }
        }
    });

    // Listen for PIVX payment events
    await listen('pivx_payment_received', (evt) => {
        const { conversation_id, gift_code, amount_piv, address, message_id, sender, is_mine } = evt.payload;

        // Find the chat
        const chat = arrChats.find(c => c.id === conversation_id);
        if (!chat) {
            console.warn('PIVX payment: chat not found for', conversation_id);
            return;
        }

        // Check if this payment message already exists in chat
        const existingMsg = chat.messages?.find(m => m.id === message_id);
        if (existingMsg) {
            return;
        }

        // Create a synthetic message object for the PIVX payment
        const pivxMsg = {
            id: message_id,
            at: evt.payload.at || Date.now(),
            content: '',
            mine: is_mine,
            attachments: [],
            npub: sender,
            pivx_payment: {
                gift_code,
                amount_piv,
                address
            }
        };

        // Add to chat messages in sorted order by timestamp
        if (!chat.messages) chat.messages = [];

        // Add to event cache so procedural scroll includes it
        eventCache.addEvent(conversation_id, pivxMsg);

        // Check if this is the newest message (should be appended at end)
        const isNewest = chat.messages.length === 0 || pivxMsg.at >= chat.messages[chat.messages.length - 1].at;

        if (isNewest) {
            // Newest message - append to end
            chat.messages.push(pivxMsg);

            // If this chat is currently open, append to DOM and scroll
            if (strOpenChat === conversation_id) {
                const profile = chat.chat_type === 'MlsGroup' ? null : getProfile(conversation_id);
                const msgEl = renderMessage(pivxMsg, profile);
                domChatMessages.appendChild(msgEl);
                softChatScroll();
            }
        } else {
            // Historical message during resync - insert at correct position in array
            // but don't manipulate DOM (user will see it on scroll/reopen)
            let insertIdx = 0;
            for (let i = chat.messages.length - 1; i >= 0; i--) {
                if (chat.messages[i].at <= pivxMsg.at) {
                    insertIdx = i + 1;
                    break;
                }
            }
            chat.messages.splice(insertIdx, 0, pivxMsg);
        }

        // Update chatlist
        renderChatlist();
    });

    // Listen for typing indicator updates (both DMs and Groups)
    await listen('typing-update', (evt) => {
        const { conversation_id, typers } = evt.payload;

        // Find the chat (could be DM or group)
        const chat = arrChats.find(c => c.id === conversation_id);
        if (!chat) return;

        // Store the typers array and update timestamp
        chat.active_typers = typers || [];
        chat.last_typing_update = Date.now() / 1000;

        // If this chat is currently open, update the chat header subtext
        if (strOpenChat === conversation_id) {
            updateChatHeaderSubtext(chat);
        }

        // Update the chat list preview
        renderChatlist();
    });

    // Listen for incoming DM messages
    await listen('message_new', (evt) => {
        // Get the chat for this message (chat_id is the npub for DMs)
        let chat = getOrCreateDMChat(evt.payload.chat_id);
        
        // Get the new message
        const newMessage = evt.payload.message;
        
        // Add to event cache
        // During sync, only add if this chat is currently open (to avoid cache flooding)
        // After sync complete, always add to cache
        const shouldAddToCache = fSyncComplete || chat.id === strOpenChat;
        if (shouldAddToCache) {
            const added = eventCache.addEvent(chat.id, newMessage);
            if (!added) return;
        }

        // Clear typing indicator for the sender when they send a message
        if (!newMessage.mine && chat.active_typers) {
            // Remove the sender from active typers
            chat.active_typers = chat.active_typers.filter(npub => npub !== chat.id);
        }

        // Find the correct position to insert the message based on timestamp
        const messages = chat.messages;

        // Check if the array is empty or the new message is newer than (or equal to) the newest message
        if (messages.length === 0 || newMessage.at >= messages[messages.length - 1].at) {
            // Insert at the end (newest)
            messages.push(newMessage);

            // Sort chats by most recent activity (message or metadata fallback)
            arrChats.sort((a, b) => getChatSortTimestamp(b) - getChatSortTimestamp(a));
        }
        // Check if the new message is older than the oldest message
        else if (newMessage.at < messages[0].at) {
            // Insert at the beginning (oldest)
            messages.unshift(newMessage);
        }
        // Otherwise, find the correct position in the middle
        else {
            // Binary search for better performance with large message arrays
            let low = 0;
            let high = messages.length - 1;

            while (low <= high) {
                const mid = Math.floor((low + high) / 2);

                if (messages[mid].at < newMessage.at) {
                    low = mid + 1;
                } else {
                    high = mid - 1;
                }
            }

            // Insert the message at the correct position (low is now the index where it should go)
            messages.splice(low, 0, newMessage);
        }

        // If this user has the open chat, then update the chat too
        if (strOpenChat === chat.id) {
            updateChat(chat, [newMessage]);
            // Increment rendered count since we're adding a new message
            proceduralScrollState.renderedMessageCount++;
            proceduralScrollState.totalMessageCount++;
        }

        // Render the Chat List (only when user is viewing it)
        if (!strOpenChat) renderChatlist();

        // Update the back button notification dot (for unread messages in other chats)
        updateChatBackNotification();
    });

    // Listen for existing message updates (works for both DMs and MLS groups)
    await listen('message_update', (evt) => {
        // Find the message we're updating
        const cChat = getChat(evt.payload.chat_id);
        if (!cChat) return;

        const nMsgIdx = cChat.messages.findIndex(m => m.id === evt.payload.old_id);
        if (nMsgIdx === -1) return;

        // Update it
        cChat.messages[nMsgIdx] = evt.payload.message;
        
        // Also update the event cache
        // This is important for pending->sent transitions where the ID changes
        if (eventCache.has(evt.payload.chat_id)) {
            const cachedEvents = eventCache.getEvents(evt.payload.chat_id);
            if (cachedEvents) {
                const cacheIdx = cachedEvents.findIndex(m => m.id === evt.payload.old_id);
                if (cacheIdx !== -1) {
                    cachedEvents[cacheIdx] = evt.payload.message;
                }
            }
        }

        // If this chat is open, then update the rendered message
        if (strOpenChat === evt.payload.chat_id) {
            // TODO: is there a slight possibility of a race condition here? i.e: `message_update` calls before `message_new` and thus domMsg isn't found?
            const domMsg = document.getElementById(evt.payload.old_id);

            // For DMs, get the profile; for groups, profile will be null
            const profile = getProfile(evt.payload.chat_id);
            domMsg?.replaceWith(renderMessage(evt.payload.message, profile, evt.payload.old_id));

            // If the old ID was a pending ID (our message), make sure to update and scroll accordingly
            if (evt.payload.old_id.startsWith('pending')) {
                strLastMsgID = evt.payload.message.id;
                softChatScroll();
            }

            // Update any reply contexts that quote this edited message
            const editedMsgId = evt.payload.message.id;
            const newContent = evt.payload.message.content;

            // Find all messages that reply to this edited message and update their reply preview
            const replyElements = document.querySelectorAll(`[id="r-${editedMsgId}"]`);
            for (const replyEl of replyElements) {
                const replyTextSpan = replyEl.querySelector('.msg-reply-text');
                if (replyTextSpan && newContent) {
                    // Truncate using same method as renderMessage
                    replyTextSpan.textContent = truncateGraphemes(newContent, 50);
                    twemojify(replyTextSpan);
                }
            }

            // Also update the replied_to_content in cached message data
            for (const msg of cChat.messages) {
                if (msg.replied_to === editedMsgId) {
                    msg.replied_to_content = newContent;
                }
            }
        }

        // Update chatlist preview if the edited message is the last message in the chat
        // This efficiently updates just the preview text instead of re-rendering the entire chatlist
        const isLastMessage = nMsgIdx === cChat.messages.length - 1;
        if (isLastMessage) {
            updateChatlistPreview(evt.payload.chat_id);
        }
    });

    // Listen for Vector Voice AI (Whisper) model download progression updates
    await listen('whisper_download_progress', async (evt) => {
        // Update the progression UI
        const spanProgression = document.getElementById('voice-model-download-progression');
        if (spanProgression) spanProgression.textContent = `(${evt.payload.progress}%)`;
    });

    // Listen for Windows-specific Overlay Icon update requests
    // Note: this API seems unavailable in Tauri's Rust backend, so we're using the JS API as a workaround
    await listen('update_overlay_icon', async (evt) => {
        // Enable or Disable our notification badge Overlay Icon
        await getCurrentWindow().setOverlayIcon(evt.payload.enable ? "./icons/icon_badge_notification.png" : undefined);
    });

    // Listen for relay status changes
    await listen('relay_status_change', (evt) => {
        // Update the relay status in the network list
        const relayItem = document.querySelector(`[data-relay-url="${evt.payload.url}"]`);
        if (relayItem) {
            const statusElement = relayItem.querySelector('.relay-status');
            if (statusElement) {
                // Remove all status classes
                statusElement.classList.remove('connected', 'connecting', 'disconnected', 'pending', 'initialized', 'terminated', 'banned', 'sleeping');
                // Add the new status class
                statusElement.classList.add(evt.payload.status);
                // Update the text
                statusElement.textContent = evt.payload.status;
            }
        }

        // Also update the info dialog if it's open for this relay
        if (currentRelayInfo && currentRelayInfo.url.toLowerCase() === evt.payload.url.toLowerCase()) {
            const dialogStatus = document.getElementById('relay-info-status');
            if (dialogStatus) {
                dialogStatus.textContent = evt.payload.status;
                dialogStatus.className = `relay-status ${evt.payload.status}`;
            }
            currentRelayInfo.status = evt.payload.status;
        }
    });

    // Listen for Mini App realtime status updates (peer count changes)
    await listen('miniapp_realtime_status', (evt) => {
        const { topic, peer_count, is_active, has_pending_peers } = evt.payload;
        console.log('[MINIAPP] Realtime status update:', topic, 'peers:', peer_count, 'active:', is_active, 'pending:', has_pending_peers);
        
        // Find all Mini App attachments with this topic and update their status
        const attachments = document.querySelectorAll(`.miniapp-attachment[data-webxdc-topic="${topic}"]`);
        console.log('[MINIAPP] Found', attachments.length, 'attachments for topic', topic);
        
        attachments.forEach(attachment => {
            // Use the stored update function if available
            if (attachment._updateMiniAppStatus) {
                attachment._updateMiniAppStatus(is_active, peer_count);
            }
        });
    });

    // Note: Deep link listener is set up early in DOMContentLoaded, before login flow
    // This ensures deep links work even when the app is opened from a closed state
}

/**
 * A flag that indicates when Vector is still in it's initiation sequence
 */
let fInit = true;

/**
 * Execute a deep link action (profile, etc.)
 * @param {Object} payload - The action payload with action_type and target
 */
async function executeDeepLinkAction(payload) {
    const { action_type, target } = payload;
    if (action_type === 'profile') {
        // Open the profile for the given npub
        // First, try to find an existing profile in our cache
        let profile = arrProfiles.find(p => p.id === target);
        
        if (!profile) {
            // Profile not in cache - create a minimal profile object
            // The openProfile function will trigger a refresh from the network
            profile = { id: target };
        }
        
        // Store the current chat so we can return to it
        previousChatBeforeProfile = strOpenChat;
        
        // Open the profile view
        await openProfile(profile);
    }
}

/**
 * A flag that indicates when the initial sync is complete
 * This is separate from fInit because sync continues after UI init
 */
let fSyncComplete = false;

/**
 * Renders the relay list and media servers in the Settings Network section
 */
async function renderRelayList() {
    try {
        const relays = await invoke('get_relays');
        const mediaServers = await invoke('get_media_servers');
        const networkList = document.getElementById('network-list');

        // Clear existing content
        networkList.innerHTML = '';

        // Add Nostr Relays header with info and add buttons
        const relaysTitleContainer = document.createElement('div');
        relaysTitleContainer.className = 'relay-section-header';

        const relaysTitle = document.createElement('h3');
        relaysTitle.className = 'network-section-title';
        relaysTitle.style.display = 'inline-flex';
        relaysTitle.style.alignItems = 'center';
        relaysTitle.textContent = 'Nostr Relays';

        const relaysInfoBtn = document.createElement('span');
        relaysInfoBtn.className = 'icon icon-info btn';
        relaysInfoBtn.style.width = '16px';
        relaysInfoBtn.style.height = '16px';
        relaysInfoBtn.style.position = 'relative';
        relaysInfoBtn.style.display = 'inline-block';
        relaysInfoBtn.style.marginLeft = '8px';
        relaysInfoBtn.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm('Nostr Relays', 'Nostr Relays are <b>decentralized servers that store and relay your messages</b> across the Nostr network.<br><br>Vector connects to multiple relays simultaneously to ensure your messages are delivered reliably and are censorship-resistant.', true);
        };

        const addRelayBtn = document.createElement('button');
        addRelayBtn.className = 'relay-add-btn';
        addRelayBtn.textContent = '+';
        addRelayBtn.title = 'Add Custom Relay';
        addRelayBtn.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            openAddRelayDialog();
        };

        relaysTitle.appendChild(relaysInfoBtn);
        relaysTitleContainer.appendChild(relaysTitle);
        relaysTitleContainer.appendChild(addRelayBtn);
        networkList.appendChild(relaysTitleContainer);

        // Create relay items
        relays.forEach(relay => {
            const relayItem = document.createElement('div');
            relayItem.className = 'relay-item' + (relay.enabled ? '' : ' disabled');
            relayItem.setAttribute('data-relay-url', relay.url);
            relayItem.setAttribute('data-relay-is-default', relay.is_default);
            relayItem.setAttribute('data-relay-is-custom', relay.is_custom);

            // Content container (clickable area)
            const relayContent = document.createElement('div');
            relayContent.className = 'relay-item-content';
            relayContent.onclick = () => openRelayInfoDialog(relay);

            const relayUrl = document.createElement('span');
            relayUrl.className = 'relay-url';
            relayUrl.textContent = relay.url.replace(/^wss?:\/\//, '');

            // Mode badge (only for custom relays or non-default modes)
            if (relay.is_custom && relay.mode !== 'both') {
                const modeBadge = document.createElement('span');
                modeBadge.className = 'relay-mode-badge';
                modeBadge.textContent = relay.mode === 'read' ? 'R' : 'W';
                relayContent.appendChild(modeBadge);
            }

            // Default badge
            if (relay.is_default) {
                const defaultBadge = document.createElement('span');
                defaultBadge.className = 'relay-default-badge';
                defaultBadge.textContent = 'default';
                relayContent.appendChild(defaultBadge);
            }

            relayContent.appendChild(relayUrl);

            // Status badge
            const relayStatus = document.createElement('span');
            relayStatus.className = `relay-status ${relay.status}`;
            relayStatus.textContent = relay.status;

            // Actions container
            const actionsContainer = document.createElement('div');
            actionsContainer.className = 'relay-item-actions';

            // Toggle switch
            const toggle = document.createElement('input');
            toggle.type = 'checkbox';
            toggle.className = 'relay-toggle';
            toggle.checked = relay.enabled;
            toggle.onclick = (e) => e.stopPropagation();
            toggle.onchange = async (e) => {
                const enabled = e.target.checked;
                try {
                    if (relay.is_default) {
                        // Show warning for default relays
                        if (!enabled) {
                            const confirmed = await popupConfirm(
                                'Disable Default Relay?',
                                'This is a <b>default relay</b>. Disabling it may affect message delivery and sync reliability.<br><br>Are you sure you want to disable it?',
                                false
                            );
                            if (!confirmed) {
                                e.target.checked = true;
                                return;
                            }
                        }
                        await invoke('toggle_default_relay', { url: relay.url, enabled });
                    } else {
                        await invoke('toggle_custom_relay', { url: relay.url, enabled });
                    }
                    // Refresh the list
                    renderRelayList();
                } catch (err) {
                    console.error('Failed to toggle relay:', err);
                    e.target.checked = !enabled; // Revert on error
                }
            };

            actionsContainer.appendChild(relayStatus);
            actionsContainer.appendChild(toggle);

            relayItem.appendChild(relayContent);
            relayItem.appendChild(actionsContainer);
            networkList.appendChild(relayItem);
        });
        
        // Add Media Servers subtitle with info button - wrap in container for centering
        const mediaTitleContainer = document.createElement('div');
        mediaTitleContainer.style.textAlign = 'center';
        mediaTitleContainer.style.marginTop = '2rem';
        
        const mediaTitle = document.createElement('h3');
        mediaTitle.className = 'network-section-title';
        mediaTitle.style.display = 'inline-flex';
        mediaTitle.style.alignItems = 'center';
        mediaTitle.textContent = 'Media Servers';
        
        const mediaInfoBtn = document.createElement('span');
        mediaInfoBtn.className = 'icon icon-info btn';
        mediaInfoBtn.style.width = '16px';
        mediaInfoBtn.style.height = '16px';
        mediaInfoBtn.style.position = 'relative';
        mediaInfoBtn.style.display = 'inline-block';
        mediaInfoBtn.style.marginLeft = '8px';
        mediaInfoBtn.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm('Media Servers', 'Media Servers are <b>Blossom-compatible servers that store your files</b> (images, videos, documents) for sharing in messages and for storage in an encrypted cloud.', true);
        };
        
        mediaTitle.appendChild(mediaInfoBtn);
        mediaTitleContainer.appendChild(mediaTitle);
        networkList.appendChild(mediaTitleContainer);
        
        // Create media server items
        mediaServers.forEach(serverUrl => {
            const serverItem = document.createElement('div');
            serverItem.className = 'relay-item media-server-item';
            serverItem.setAttribute('data-server-url', serverUrl);
            
            const serverUrlSpan = document.createElement('span');
            serverUrlSpan.className = 'relay-url';
            serverUrlSpan.textContent = serverUrl;
            
            const serverStatus = document.createElement('span');
            serverStatus.className = 'relay-status connected';
            serverStatus.textContent = 'active';
            
            serverItem.appendChild(serverUrlSpan);
            serverItem.appendChild(serverStatus);
            networkList.appendChild(serverItem);
        });
    } catch (error) {
        console.error('Failed to fetch network info:', error);
    }
}

// =============================================================================
// Relay Dialog Management
// =============================================================================

/** Currently selected relay for info dialog */
let currentRelayInfo = null;
/** Interval for refreshing relay info dialog data */
let relayInfoRefreshInterval = null;

/**
 * Opens the Add Relay dialog
 */
function openAddRelayDialog() {
    const overlay = document.getElementById('add-relay-overlay');
    const urlInput = document.getElementById('add-relay-url');
    const modeSelect = document.getElementById('add-relay-mode');

    // Reset form
    urlInput.value = '';
    modeSelect.value = 'both';

    // Show dialog
    overlay.classList.add('active');
    urlInput.focus();
}

/**
 * Closes the Add Relay dialog
 */
function closeAddRelayDialog() {
    const overlay = document.getElementById('add-relay-overlay');
    overlay.classList.remove('active');
}

/**
 * Handles adding a new relay from the dialog
 */
async function handleAddRelay() {
    const urlInput = document.getElementById('add-relay-url');
    const modeSelect = document.getElementById('add-relay-mode');
    let url = urlInput.value.trim();
    const mode = modeSelect.value;

    if (!url) {
        popupConfirm('Invalid URL', 'Please enter a relay URL.', true);
        return;
    }

    // Normalize URL: strip protocol if present and add wss://
    url = url.replace(/^wss?:\/\//i, '');
    url = 'wss://' + url;

    try {
        await invoke('add_custom_relay', { url, mode });
        closeAddRelayDialog();
        renderRelayList();
    } catch (err) {
        popupConfirm('Failed to Add Relay', err.toString(), true);
    }
}

/**
 * Refreshes the data displayed in the Relay Info dialog
 */
async function refreshRelayInfoDialog() {
    if (!currentRelayInfo) return;

    const url = currentRelayInfo.url;

    // Fetch fresh relay data
    try {
        const relays = await invoke('get_relays');
        const freshRelay = relays.find(r => r.url.toLowerCase() === url.toLowerCase());
        if (freshRelay) {
            currentRelayInfo = freshRelay;

            // Update status
            const statusEl = document.getElementById('relay-info-status');
            statusEl.textContent = freshRelay.status;
            statusEl.className = `relay-status ${freshRelay.status}`;

            // Update disable button text
            const disableBtn = document.getElementById('relay-info-disable');
            if (freshRelay.is_default) {
                disableBtn.textContent = freshRelay.enabled ? 'Disable Relay' : 'Enable Relay';
            }
        }
    } catch (err) {
        console.error('Failed to refresh relay data:', err);
    }

    // Refresh metrics
    try {
        const metrics = await invoke('get_relay_metrics', { url });
        document.getElementById('relay-info-ping').textContent = metrics.ping_ms ? `${metrics.ping_ms}ms` : '--';
        if (metrics.last_check) {
            const lastCheck = new Date(metrics.last_check * 1000);
            const now = new Date();
            const diffSecs = Math.floor((now - lastCheck) / 1000);
            let lastCheckText;
            if (diffSecs < 60) {
                lastCheckText = `${diffSecs}s ago`;
            } else if (diffSecs < 3600) {
                lastCheckText = `${Math.floor(diffSecs / 60)}m ago`;
            } else {
                lastCheckText = lastCheck.toLocaleTimeString();
            }
            document.getElementById('relay-info-last-check').textContent = lastCheckText;
        } else {
            document.getElementById('relay-info-last-check').textContent = '--';
        }
    } catch (err) {
        console.error('Failed to load relay metrics:', err);
    }

    // Refresh logs
    try {
        const logs = await invoke('get_relay_logs', { url });
        const logsList = document.getElementById('relay-info-logs');
        logsList.innerHTML = '';

        if (logs.length === 0) {
            const emptyLi = document.createElement('li');
            emptyLi.className = 'relay-log-empty';
            emptyLi.textContent = 'No activity recorded yet';
            logsList.appendChild(emptyLi);
        } else {
            logs.forEach(log => {
                const li = document.createElement('li');
                const time = new Date(log.timestamp * 1000).toLocaleTimeString();
                li.innerHTML = `<span class="relay-log-time">${time}</span><span class="relay-log-message ${log.level}">${log.message}</span>`;
                logsList.appendChild(li);
            });
        }
    } catch (err) {
        console.error('Failed to load relay logs:', err);
    }
}

/**
 * Opens the Relay Info dialog
 * @param {Object} relay - The relay object
 */
async function openRelayInfoDialog(relay) {
    // Clear any existing interval
    if (relayInfoRefreshInterval) {
        clearInterval(relayInfoRefreshInterval);
        relayInfoRefreshInterval = null;
    }

    currentRelayInfo = relay;
    const overlay = document.getElementById('relay-info-overlay');
    const urlEl = document.getElementById('relay-info-url');
    const modeSelect = document.getElementById('relay-info-mode');

    // Set static info (URL doesn't change)
    urlEl.textContent = relay.url.replace(/^wss?:\/\//, '');

    // Set mode (only editable for custom relays)
    modeSelect.value = relay.mode || 'both';
    modeSelect.disabled = relay.is_default;

    // Initial data load
    await refreshRelayInfoDialog();

    // Start refresh interval (every 1 second)
    relayInfoRefreshInterval = setInterval(refreshRelayInfoDialog, 1000);

    // Show dialog
    overlay.classList.add('active');
}

/**
 * Closes the Relay Info dialog
 */
function closeRelayInfoDialog() {
    // Clear the refresh interval
    if (relayInfoRefreshInterval) {
        clearInterval(relayInfoRefreshInterval);
        relayInfoRefreshInterval = null;
    }

    const overlay = document.getElementById('relay-info-overlay');
    overlay.classList.remove('active');
    currentRelayInfo = null;
}

/**
 * Handles mode change from the info dialog
 */
async function handleRelayModeChange() {
    if (!currentRelayInfo || currentRelayInfo.is_default) return;

    const modeSelect = document.getElementById('relay-info-mode');
    const newMode = modeSelect.value;

    try {
        await invoke('update_relay_mode', { url: currentRelayInfo.url, mode: newMode });
        currentRelayInfo.mode = newMode;
        renderRelayList();
    } catch (err) {
        console.error('Failed to update relay mode:', err);
        popupConfirm('Error', 'Failed to update relay mode: ' + err.toString(), true);
    }
}

/**
 * Handles disable/remove button from info dialog
 */
async function handleRelayDisable() {
    if (!currentRelayInfo) return;

    const relay = currentRelayInfo;

    if (relay.is_default) {
        // Toggle default relay
        const newEnabled = !relay.enabled;
        if (!newEnabled) {
            // Show warning before disabling default relay
            const confirmed = await popupConfirm(
                'Disable Default Relay?',
                'This is a <b>default relay</b>. Disabling it may affect message delivery and sync reliability.<br><br>Are you sure you want to disable it?',
                false
            );
            if (confirmed) {
                try {
                    await invoke('toggle_default_relay', { url: relay.url, enabled: false });
                    closeRelayInfoDialog();
                    renderRelayList();
                } catch (err) {
                    popupConfirm('Error', 'Failed to disable relay: ' + err.toString(), true);
                }
            }
        } else {
            // Re-enable without warning
            try {
                await invoke('toggle_default_relay', { url: relay.url, enabled: true });
                closeRelayInfoDialog();
                renderRelayList();
            } catch (err) {
                popupConfirm('Error', 'Failed to enable relay: ' + err.toString(), true);
            }
        }
    } else {
        // Remove custom relay
        const confirmed = await popupConfirm(
            'Remove Relay?',
            `Are you sure you want to remove <b>${relay.url.replace(/^wss?:\/\//, '')}</b>?`,
            false
        );
        if (confirmed) {
            try {
                await invoke('remove_custom_relay', { url: relay.url });
                closeRelayInfoDialog();
                renderRelayList();
            } catch (err) {
                popupConfirm('Error', 'Failed to remove relay: ' + err.toString(), true);
            }
        }
    }
}

/**
 * Initialize relay dialog event listeners
 */
function initRelayDialogs() {
    // Add Relay Dialog
    document.getElementById('add-relay-close').onclick = closeAddRelayDialog;
    document.getElementById('add-relay-cancel').onclick = closeAddRelayDialog;
    document.getElementById('add-relay-confirm').onclick = handleAddRelay;
    document.getElementById('add-relay-overlay').onclick = (e) => {
        if (e.target.id === 'add-relay-overlay') closeAddRelayDialog();
    };

    // Allow Enter key to submit
    document.getElementById('add-relay-url').onkeydown = (e) => {
        if (e.key === 'Enter') handleAddRelay();
    };

    // Relay Info Dialog
    document.getElementById('relay-info-close').onclick = closeRelayInfoDialog;
    document.getElementById('relay-info-done').onclick = closeRelayInfoDialog;
    document.getElementById('relay-info-disable').onclick = handleRelayDisable;
    document.getElementById('relay-info-mode').onchange = handleRelayModeChange;
    document.getElementById('relay-info-overlay').onclick = (e) => {
        if (e.target.id === 'relay-info-overlay') closeRelayInfoDialog();
    };

    // Copy logs button
    document.getElementById('relay-logs-copy').onclick = copyRelayLogs;
}

/**
 * Copies relay logs to clipboard in a formatted way
 */
function copyRelayLogs() {
    if (!currentRelayInfo) return;

    // Read logs from the displayed DOM to avoid async clipboard permission issues
    const logsList = document.getElementById('relay-info-logs');
    const logItems = logsList.querySelectorAll('li:not(.relay-log-empty)');

    let text;
    if (logItems.length === 0) {
        text = 'No activity recorded yet';
    } else {
        const header = `Relay Logs: ${currentRelayInfo.url.replace(/^wss?:\/\//, '')}\n${'='.repeat(50)}\n`;
        const logs = Array.from(logItems).map(li => {
            const time = li.querySelector('.relay-log-time')?.textContent || '';
            const msg = li.querySelector('.relay-log-message')?.textContent || '';
            const level = li.querySelector('.relay-log-message')?.classList.contains('error') ? 'ERROR' :
                          li.querySelector('.relay-log-message')?.classList.contains('warn') ? 'WARN' : 'INFO';
            return `[${time}] [${level}] ${msg}`;
        }).join('\n');
        text = header + logs;
    }

    navigator.clipboard.writeText(text).then(() => {
        // Visual feedback - change icon briefly
        const copyBtn = document.getElementById('relay-logs-copy');
        const icon = copyBtn.querySelector('.icon');
        icon.classList.remove('icon-copy');
        icon.classList.add('icon-check');
        setTimeout(() => {
            icon.classList.remove('icon-check');
            icon.classList.add('icon-copy');
        }, 1500);
    }).catch(err => {
        console.error('Failed to copy relay logs:', err);
    });
}

// =============================================================================

/**
 * Login to the Nostr network
 */
async function login() {
    if (strPubkey) {
        // Connect to Nostr
        await invoke("connect");

        // Setup our Rust Event listeners for efficient back<-->front sync
        await setupRustListeners();

        // Setup unified progress operation event listener
        await listen('progress_operation', (evt) => {
            const { type, current, total, message } = evt.payload;
            
            switch (type) {
                case 'start':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.add('text-gradient');
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
                    domLoginEncryptTitle.classList.remove('text-gradient');
                    break;
                    
                case 'error':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.remove('text-gradient');
                    domLoginEncryptTitle.style.color = 'red';
                    break;
            }
        });


        // Setup a Rust Listener for the backend's init finish
        await listen('init_finished', async (evt) => {
            // The backend now sends both profiles (without messages) and chats (with messages)
            arrProfiles = evt.payload.profiles || [];
            arrChats = evt.payload.chats || [];

            await hydrateMLSGroupMetadata();

            // Load the file hash index for attachment deduplication
            // This is done asynchronously and doesn't block the UI
            eventCache.loadFileHashIndex().catch(() => {});

            // Fadeout the login and encryption UI
            domLogin.classList.add('fadeout-anim');
            domLogin.addEventListener('animationend', async () => {
                domLogin.classList.remove('fadeout-anim');
                domLoginInput.value = "";
                domLogin.style.display = 'none';
                domLoginEncrypt.style.display = 'none';

                // Fade-in the navbar
                domNavbar.style.display = '';
                domNavbar.classList.add('fadein-anim');
                domNavbar.addEventListener('animationend', () => {
                    domNavbar.classList.remove('fadein-anim');

                    // Fade-in the bookmarks icon
                    domChatBookmarksBtn.style.display = 'flex';
                    domChatBookmarksBtn.classList.add('fadein-anim');
                    domChatBookmarksBtn.addEventListener('animationend', () => domChatBookmarksBtn.classList.remove('fadein-anim'), { once: true });
                }, { once: true });

                // Render our profile with an intro animation
                const cProfile = arrProfiles.find(p => p.mine);
                renderCurrentProfile(cProfile);
                domAccount.style.display = ``;
                domAccount.classList.add('fadein-anim');
                domAccount.addEventListener('animationend', () => domAccount.classList.remove('fadein-anim'), { once: true });

                // Refresh our own profile from the network to catch any changes made on other clients
                if (cProfile?.id) {
                    invoke("queue_profile_sync", {
                        npub: cProfile.id,
                        priority: "critical",
                        forceRefresh: true
                    });
                }

                // Finished boot!
                fInit = false;

                // Render the chatlist with an intro animation
                domChatList.classList.add('intro-anim');
                renderChatlist();
                domChatList.addEventListener('animationend', () => domChatList.classList.remove('intro-anim'), { once: true });

                // Show and animate the New Chat buttons
                if (domChatNewDM) {
                    domChatNewDM.style.display = '';
                    domChatNewDM.classList.add('intro-anim');
                    domChatNewDM.onclick = openNewChat;
                    domChatNewDM.addEventListener('animationend', () => domChatNewDM.classList.remove('intro-anim'), { once: true });
                }
                if (domChatNewGroup) {
                    domChatNewGroup.style.display = '';
                    domChatNewGroup.classList.add('intro-anim');
                    domChatNewGroup.onclick = openCreateGroup;
                    domChatNewGroup.addEventListener('animationend', () => domChatNewGroup.classList.remove('intro-anim'), { once: true });
                }

                // Adjust the Chat List sizes to prevent mismatches
                adjustSize();

                // Setup a subscription for new websocket messages
                invoke("notifs");

                // Setup our Unread Counters
                await invoke("update_unread_counter");

                // Monitor relay connections
                invoke("monitor_relay_connections");

                // Render the initial relay list
                renderRelayList();
                
                // Initialize the updater
                initializeUpdater();
                
                // Execute any pending deep link action that was received before login
                // The Rust backend stores deep links received before the frontend was ready
                setTimeout(async () => {
                    try {
                        const pendingAction = await invoke('get_pending_deep_link');
                        if (pendingAction) {
                            console.log('Executing pending deep link action:', pendingAction);
                            await executeDeepLinkAction(pendingAction);
                        }
                    } catch (e) {
                        console.error('Failed to check for pending deep link:', e);
                    }
                }, 1000);
            }, { once: true });
        });

        // Load and Decrypt our database; fetching the full chat state from disk for immediate bootup
        domLoginEncryptTitle.textContent = `Decrypting Database...`;

        // Note: this also begins the Rust backend's iterative sync, thus, init should ONLY be called once, to initiate it
        init();
    }
}

/**
 * Renders the user's own profile UI in the chat list
 * @param {Profile} cProfile 
 */
function renderCurrentProfile(cProfile) {
    /* Chatlist Tab */

    // Clear and render avatar
    domAccountAvatarContainer.innerHTML = '';
    const accountAvatarSrc = getProfileAvatarSrc(cProfile);
    const domAvatar = createAvatarImg(accountAvatarSrc, 22, false);
    domAvatar.classList.add('btn');
    domAvatar.onclick = () => openProfile();
    domAccountAvatarContainer.appendChild(domAvatar);

    // Render our Display Name
    domAccountName.textContent = cProfile?.nickname || cProfile?.name || strPubkey.substring(0, 10) + '…';
    domAccountName.onclick = () => openProfile();
    if (cProfile?.nickname || cProfile?.name) twemojify(domAccountName);

    // Render our status
    domAccountStatus.textContent = cProfile?.status?.title || 'Set a Status';
    domAccountStatus.onclick = askForStatus;
    twemojify(domAccountStatus);

    /* Start Chat Tab */
    // Render our Share npub
    domShareNpub.textContent = strPubkey;
}

/**
 * Render the Profile tab based on a given profile
 * @param {Profile} cProfile 
 */
function renderProfileTab(cProfile) {
    // Header Mini Avatar
    domProfileHeaderAvatarContainer.innerHTML = '';
    const headerAvatarSrc = getProfileAvatarSrc(cProfile);
    const domHeaderAvatar = createAvatarImg(headerAvatarSrc, 22, false);
    domHeaderAvatar.classList.add('btn');
    domProfileHeaderAvatarContainer.appendChild(domHeaderAvatar);

    // Display Name - use profile's npub as fallback
    domProfileName.innerHTML = cProfile?.nickname || cProfile?.name || (cProfile?.id ? cProfile.id.substring(0, 10) + '…' : 'Unknown');
    if (cProfile?.nickname || cProfile?.name) twemojify(domProfileName);

    // Status
    const strStatusPlaceholder = cProfile.mine ? 'Set a Status' : '';
    domProfileStatus.innerHTML = cProfile?.status?.title || strStatusPlaceholder;
    if (cProfile?.status?.title) twemojify(domProfileStatus);

    // Adjust our Profile Name class to manage space according to Status visibility
    domProfileName.classList.toggle('chat-contact', !domProfileStatus.textContent);
    domProfileName.classList.toggle('chat-contact-with-status', !!domProfileStatus.textContent);

    // Banner - keep original structure but add click handler
    const bannerSrc = getProfileBannerSrc(cProfile);
    if (bannerSrc) {
        if (domProfileBanner.tagName === 'DIV') {
            const newBanner = document.createElement('img');
            domProfileBanner.replaceWith(newBanner);
            domProfileBanner = newBanner;
        }
        domProfileBanner.src = bannerSrc;
        // On error, replace with solid color placeholder
        domProfileBanner.onerror = function() {
            const placeholder = document.createElement('div');
            placeholder.style.backgroundColor = 'rgb(27, 27, 27)';
            placeholder.classList.add('profile-banner');
            if (cProfile.mine) {
                placeholder.classList.add('btn');
                placeholder.onclick = askForBanner;
            }
            domProfileBanner.replaceWith(placeholder);
            domProfileBanner = placeholder;
        };
    } else {
        if (domProfileBanner.tagName === 'IMG') {
            const newBanner = document.createElement('div');
            newBanner.style.backgroundColor = 'rgb(27, 27, 27)';
            domProfileBanner.replaceWith(newBanner);
            domProfileBanner = newBanner;
        }
    }
    domProfileBanner.classList.add('profile-banner');
    domProfileBanner.onclick = cProfile.mine ? askForBanner : null;
    if (cProfile.mine) domProfileBanner.classList.add('btn');

    // Avatar - keep original structure but add click handler
    const profileAvatarSrc = getProfileAvatarSrc(cProfile);
    if (profileAvatarSrc) {
        if (domProfileAvatar.tagName === 'DIV') {
            const newAvatar = document.createElement('img');
            domProfileAvatar.replaceWith(newAvatar);
            domProfileAvatar = newAvatar;
        }
        domProfileAvatar.src = profileAvatarSrc;
        // On error, replace with placeholder
        domProfileAvatar.onerror = function() {
            const placeholder = createPlaceholderAvatar(false, 175);
            placeholder.classList.add('profile-avatar');
            if (cProfile.mine) {
                placeholder.classList.add('btn');
                placeholder.onclick = askForAvatar;
            }
            domProfileAvatar.replaceWith(placeholder);
            domProfileAvatar = placeholder;
        };
    } else {
        const newAvatar = createPlaceholderAvatar(false, 175);
        domProfileAvatar.replaceWith(newAvatar);
        domProfileAvatar = newAvatar;
    }
    domProfileAvatar.classList.add('profile-avatar');
    domProfileAvatar.onclick = cProfile.mine ? askForAvatar : null;
    if (cProfile.mine) domProfileAvatar.classList.add('btn');

    // Secondary Display Name - use profile's npub as fallback
    const strNamePlaceholder = cProfile.mine ? 'Set a Display Name' : (cProfile?.id ? cProfile.id.substring(0, 10) + '…' : '');
    domProfileNameSecondary.innerHTML = cProfile?.nickname || cProfile?.name || strNamePlaceholder;
    if (cProfile?.nickname || cProfile?.name) twemojify(domProfileNameSecondary);

    // Secondary Status
    domProfileStatusSecondary.innerHTML = domProfileStatus.innerHTML;

    // Badges
    domProfileBadgeInvite.style.display = 'none';
    invoke("get_invited_users", { npub: cProfile.id }).then(count => {
        if (count > 0) {
            domProfileBadgeInvite.style.display = '';
            domProfileBadgeInvite.onclick = () => {
                popupConfirm('Vector Beta Inviter', `Acquired by inviting <b>${count} ${count === 1 ? 'user' : 'users'}</b> to the Vector Beta!`, true, '', 'vector_badge_placeholder.svg');
            }
        }
    }).catch(e => {});

    // Guy Fawkes Day Badge (5th November 2025 - Vector v0.2 Open Beta)
    domProfileBadgeFawkes.style.display = 'none';
    invoke("check_fawkes_badge", { npub: cProfile.id }).then(hasBadge => {
        if (hasBadge) {
            domProfileBadgeFawkes.style.display = '';
            domProfileBadgeFawkes.onclick = () => {
                popupConfirm('V for Vector Badge', `Acquired by logging in on Guy Fawkes Day&nbsp;(November 5, 2025).<br><br><i style="opacity: 0.5; font-size: 13px;">Remember, remember the 5th of November...</i>`, true, '', 'fawkes_mask.svg');
            };
        }
    }).catch(e => {});

    // npub display
    const profileNpub = document.getElementById('profile-npub');
    if (profileNpub) {
        profileNpub.textContent = cProfile.id;
    }

    // Description
    const strDescriptionPlaceholder = cProfile.mine ? (cProfile?.about || 'Set an About Me') : '';
    domProfileDescription.textContent = cProfile?.about || strDescriptionPlaceholder;
    twemojify(domProfileDescription);

    // npub
    domProfileId.textContent = cProfile.id;

    // Add npub copy functionality
    document.getElementById('profile-npub-copy')?.addEventListener('click', (e) => {
        const npub = document.getElementById('profile-npub')?.textContent;
        if (npub) {
            // Copy the full profile URL for easy sharing
            const profileUrl = `https://vectorapp.io/profile/${npub}`;
            navigator.clipboard.writeText(profileUrl).then(() => {
                const copyBtn = e.target.closest('.profile-npub-copy');
                if (copyBtn) {
                    copyBtn.innerHTML = '<span class="icon icon-check"></span>';
                    setTimeout(() => {
                        copyBtn.innerHTML = '<span class="icon icon-copy"></span>';
                    }, 2000);
                }
            });
        }
    });

    // If this is OUR profile: make the elements clickable, hide the "Contact Options"
    if (cProfile.mine) {
        // Hide Contact Options
        domProfileOptions.style.display = 'none';

        // Show edit buttons and set their click handlers
        document.querySelector('.profile-avatar-edit').style.display = 'flex';
        document.querySelector('.profile-avatar-edit').onclick = askForAvatar;
        
        document.querySelector('.profile-banner-edit').style.display = 'flex';
        document.querySelector('.profile-banner-edit').onclick = askForBanner;
        
        // Hide the 'Back' button and deregister its clickable function
        domProfileBackBtn.style.display = 'none';
        domProfileBackBtn.onclick = null;

        // Force banner on profile edit screen
        domProfileBanner.backgroundColor = 'rgb(27, 27, 27)';
        domProfileBanner.height = '';
        
        // Display the Navbar
        domNavbar.style.display = '';

        // Configure other clickables
        domProfileName.onclick = askForUsername;
        domProfileName.classList.add('btn');
        domProfileStatus.onclick = askForStatus;
        domProfileStatus.classList.add('btn');
        domProfileNameSecondary.onclick = askForUsername;
        domProfileNameSecondary.classList.add('btn');
        domProfileStatusSecondary.onclick = askForStatus;
        domProfileStatusSecondary.classList.add('btn');
        domProfileDescription.onclick = editProfileDescription;
        domProfileDescription.classList.add('btn');
    } else {
        // Show Contact Options
        domProfileOptions.style.display = '';

        // Setup Mute option
        domProfileOptionMute.querySelector('span').classList.replace('icon-volume-' + (cProfile.muted ? 'max' : 'mute'), 'icon-volume-' + (cProfile.muted ? 'mute' : 'max'));
        domProfileOptionMute.querySelector('p').innerText = cProfile.muted ? 'Unmute' : 'Mute';
        domProfileOptionMute.onclick = () => invoke('toggle_muted', { npub: cProfile.id });

        // Setup Message option
        domProfileOptionMessage.onclick = () => openChat(cProfile.id);

        // Setup Nickname option
        domProfileOptionNickname.onclick = async () => {
            const nick = await popupConfirm('Choose a Nickname', '', false, 'Nickname');
            // Check if they cancelled the nicknaming (resetting a nickname with an empty '' result is fine, though)
            if (nick === false) return;
            // Ensure it's not massive
            if (nick.length >= 30) return popupConfirm('Woah woah!', 'A ' + nick.length + '-character nickname seems excessive!', true, '', 'vector_warning.svg');
            await invoke('set_nickname', { npub: cProfile.id, nickname: nick });
        }

        // Hide edit buttons
        document.querySelector('.profile-avatar-edit').style.display = 'none';
        document.querySelector('.profile-banner-edit').style.display = 'none';
        
        // Remove click handlers from avatar and banner
        domProfileAvatar.onclick = null;
        domProfileAvatar.classList.remove('btn');
        domProfileBanner.onclick = null;
        domProfileBanner.classList.remove('btn');
        if (!cProfile.banner) {
            domProfileBanner.style.backgroundColor = '';
            domProfileBanner.style.height = '115px';
        } else {
            domProfileBanner.style.backgroundColor = 'rgb(27, 27, 27)';
            domProfileBanner.style.height = '';
        }
        
        // Show the 'Back' button and link it to the profile's chat
        domProfileBackBtn.style.display = '';
        domProfileBackBtn.onclick = () => {
            // If we came from a chat (especially a group chat), return to it
            if (previousChatBeforeProfile) {
                const chatToOpen = previousChatBeforeProfile;
                previousChatBeforeProfile = ''; // Clear before opening to avoid loops
                openChat(chatToOpen);
            } else {
                // Default to opening DM with this user
                openChat(cProfile.id);
            }
        };
        
        // Hide the Navbar
        domNavbar.style.display = 'none';

        // Remove other clickables
        domProfileName.onclick = null;
        domProfileName.classList.remove('btn');
        domProfileStatus.onclick = null;
        domProfileStatus.classList.remove('btn');
        domProfileNameSecondary.onclick = null;
        domProfileNameSecondary.classList.remove('btn');
        domProfileStatusSecondary.onclick = null;
        domProfileStatusSecondary.classList.remove('btn');
        domProfileDescription.onclick = null;
        domProfileDescription.classList.remove('btn');
    }
}

/**
 * Display the Invite code input flow.
 * @param {string} pkey - A private key to encrypt.
 */
function openInviteFlow(pkey) {
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
            showWelcomeScreen(pkey);
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
 * @param {string} pkey - A private key to encrypt after the welcome screen
 */
function showWelcomeScreen(pkey) {
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
        openEncryptionFlow(pkey, false);
    }, 5000);
}

/**
 * Display the Encryption/Decryption flow.
 * @param {string} pkey - A private key to encrypt.
 * @param {boolean} fUnlock - Whether we're unlocking an existing key, or encrypting the given one.
 */
function openEncryptionFlow(pkey, fUnlock = false) {
    domLoginStart.style.display = 'none';
    domLoginImport.style.display = 'none';
    domLoginInvite.style.display = 'none';
    domLoginEncrypt.style.display = '';

    let strPinLast = []; // Stores the first entered PIN for confirmation
    let strPinCurrent = Array(6).fill('-'); // Current PIN being entered, '-' represents an empty digit

    // Reusable Message Constants
    const DECRYPTION_PROMPT = `Enter your Decryption Pin`;
    const INITIAL_ENCRYPTION_PROMPT = `Enter your Pin`;
    const RE_ENTER_PROMPT = `Re-enter your Pin`;
    const DECRYPTING_MSG = `Decrypting your keys...`;
    const ENCRYPTING_MSG = `Encrypting your keys...`;
    const INCORRECT_PIN_MSG = `Incorrect pin, try again`;
    const MISMATCH_PIN_MSG = `Pin doesn't match, re-try`;

    const arrPinDOMs = document.querySelectorAll('.pin-row input');
    const pinContainer = arrPinDOMs[0].closest('.pin-row');

    /** Updates the status message displayed to the user. */
    function updateStatusMessage(message, isProcessing = false) {
        domLoginEncryptTitle.textContent = message;
        if (isProcessing) {
            domLoginEncryptTitle.classList.add('startup-subtext-gradient');
            domLoginEncryptPinRow.style.display = 'none'; // Hide PIN inputs during processing
        } else {
            domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
            domLoginEncryptPinRow.style.display = ''; // Ensure PIN inputs are visible
        }
    }

    /** Resets the PIN input fields and optionally reverts the title from an error state. */
    function resetPinDisplay(focusFirst = true, revertTitleFromErrorState = true) {
        strPinCurrent = Array(6).fill('-');
        arrPinDOMs.forEach(input => input.value = '');

        if (revertTitleFromErrorState) {
            const currentTitle = domLoginEncryptTitle.textContent;
            // If an error message is shown, change it back to the appropriate prompt
            if (currentTitle === INCORRECT_PIN_MSG || currentTitle === MISMATCH_PIN_MSG) {
                const newTitle = fUnlock ? DECRYPTION_PROMPT : (strPinLast.length > 0 ? RE_ENTER_PROMPT : INITIAL_ENCRYPTION_PROMPT);
                updateStatusMessage(newTitle);
            }
        }
        if (focusFirst && arrPinDOMs.length > 0) {
            arrPinDOMs[0].focus();
        }
    }

    /** Focuses the PIN input at the specified index. */
    function focusPinInput(index) {
        if (index >= 0 && index < arrPinDOMs.length) {
            arrPinDOMs[index].focus();
        } else if (index >= arrPinDOMs.length && arrPinDOMs.length > 0) { // Wrap to first on last input
            arrPinDOMs[0].focus(); // Reached end, focus first (or handle submission if all filled)
        }
        // If index < 0 (e.g., backspace from the first input), focus remains on the current (first) input.
    }

    /** Flag to prevent multiple PIN submissions */
    let pinProcessing = false;

    /** Handles the logic once all PIN digits have been entered. */
    async function handleFullPinEntered() {
        // Prevent multiple submissions
        if (pinProcessing) {
            return;
        }
        pinProcessing = true;
        
        const currentPinString = strPinCurrent.join('');

        if (strPinLast.length === 0) { // Initial PIN entry (for decryption or first step of new encryption)
            if (fUnlock) {
                updateStatusMessage(DECRYPTING_MSG, true);
                try {
                    const decryptedPkey = await loadAndDecryptPrivateKey(currentPinString);
                    const { public: pubKey /*, _private: privKey */ } = await invoke("login", { importKey: decryptedPkey });
                    strPubkey = pubKey; // Store public key
                    login(); // Proceed to login
                } catch (e) {
                    updateStatusMessage(INCORRECT_PIN_MSG);
                    resetPinDisplay(true, false); // Keep error message, reset input fields
                    pinProcessing = false; // Reset flag on error to allow retry
                }
            } else { // First PIN entry for new encryption
                strPinLast = [...strPinCurrent]; // Store the entered PIN
                updateStatusMessage(RE_ENTER_PROMPT);
                resetPinDisplay(true, false); // Keep "Re-enter" message, reset input fields
                pinProcessing = false; // Reset flag to allow second PIN entry
            }
        } else { // Second PIN entry (confirmation for new encryption)
            const isMatching = strPinLast.every((char, idx) => char === strPinCurrent[idx]);
            if (isMatching) {
                updateStatusMessage(ENCRYPTING_MSG, true);
                await saveAndEncryptPrivateKey(pkey, strPinLast.join(''));
                login(); // Proceed to login
            } else {
                updateStatusMessage(MISMATCH_PIN_MSG);
                strPinLast = []; // Clear the stored first PIN, requiring user to start over
                resetPinDisplay(true, true); // Reset inputs and revert title from error to the initial prompt
                pinProcessing = false; // Reset flag on mismatch to allow retry
            }
        }
    }

    // --- Event Handlers (Delegated to pinContainer) ---

    /** Handles keydown events, primarily for Backspace and preventing non-numeric input. */
    function handleKeyDown(event) {
        const targetInput = event.target;
        // Ensure the event target is one of our designated PIN input fields
        if (!Array.from(arrPinDOMs).includes(targetInput)) {
            return;
        }

        const nIndex = Array.from(arrPinDOMs).indexOf(targetInput);

        if (event.key === 'Backspace') {
            event.preventDefault(); // Prevent default browser backspace behavior (e.g., navigation)

            // If an error message is currently displayed, revert it to the relevant prompt for clarity
            const currentTitle = domLoginEncryptTitle.textContent;
            if (currentTitle === INCORRECT_PIN_MSG || currentTitle === MISMATCH_PIN_MSG) {
                const newTitle = fUnlock ? DECRYPTION_PROMPT : (strPinLast.length > 0 ? RE_ENTER_PROMPT : INITIAL_ENCRYPTION_PROMPT);
                updateStatusMessage(newTitle);
            }

            targetInput.value = ''; // Clear the input field's value
            strPinCurrent[nIndex] = '-'; // Update the current PIN state
            if (nIndex > 0) {
                focusPinInput(nIndex - 1); // Move focus to the previous input field
            }
        } else if (event.key.length === 1 && !event.key.match(/^[0-9]$/)) {
            // Prevent single character non-numeric keys (allows Tab, Shift, Ctrl, Meta, etc.)
            event.preventDefault();
        }
    }

    /** Handles input events for digit entry, sanitization, and moving focus forward. */
    async function handleInput(event) {
        const targetInput = event.target;
        if (!Array.from(arrPinDOMs).includes(targetInput)) {
            return;
        }

        const nIndex = Array.from(arrPinDOMs).indexOf(targetInput);
        let sanitizedValue = targetInput.value.replace(/[^0-9]/g, ''); // Keep only digits

        if (sanitizedValue.length > 1) { // If multiple digits were pasted, use only the first
            sanitizedValue = sanitizedValue.charAt(0);
        }
        targetInput.value = sanitizedValue; // Update the input field with the sanitized value

        if (sanitizedValue) { // If there's a digit
            strPinCurrent[nIndex] = sanitizedValue;
            focusPinInput(nIndex + 1); // Move focus to the next input field or wrap around
        } else {
            // If input became empty (e.g., via 'Delete' key or invalid paste), update state
            strPinCurrent[nIndex] = '-';
        }

        // Check if all PIN digits have been entered
        if (!strPinCurrent.includes('-')) {
            await handleFullPinEntered();
        }
    }

    // --- Initial Setup ---
    updateStatusMessage(fUnlock ? DECRYPTION_PROMPT : INITIAL_ENCRYPTION_PROMPT);
    resetPinDisplay(true, false); // Ensure inputs are clear, set focus, keep initial message

    // Attach the event listeners to the common parent container
    pinContainer.addEventListener('keydown', handleKeyDown);
    pinContainer.addEventListener('input', handleInput);
}



/**
 * A simple state tracker for the last message ID, if it changes, we auto-scroll
 */
let strLastMsgID = "";

/**
 * The current Message ID being replied to
 */
let strCurrentReplyReference = "";

/**
 * The current Message ID being edited (if in edit mode)
 */
let strCurrentEditMessageId = "";

/**
 * The original content of the message being edited (for cancel restoration)
 */
let strCurrentEditOriginalContent = "";

/**
 * Updates the current chat (to display incoming and outgoing messages)
 * @param {Chat} chat - The chat to update
 * @param {Array<Message>} arrMessages - The messages to efficiently insert into the chat
 * @param {Profile} profile - Optional profile for display info
 * @param {boolean} fClicked - Whether the chat was opened manually or not
 */
async function updateChat(chat, arrMessages = [], profile = null, fClicked = false) {
    // Queue profiles for this chat
    if (chat) {
        await invoke("queue_chat_profiles_sync", {
            chatId: chat.id,
            isOpening: true
        });
    }
    
    // Check if this is a group chat
    const isGroup = chat?.chat_type === 'MlsGroup';

    // If no profile is provided and it's not a group, try to get it from the chat ID
    if (!profile && chat && !isGroup) {
        profile = getProfile(chat.id);
    }
    
    // If this chat is our own npub: then we consider this our Bookmarks/Notes section
    const fNotes = strOpenChat === strPubkey;

    if (chat?.messages.length || arrMessages.length) {
        // Render chat header avatar
        domChatHeaderAvatarContainer.innerHTML = '';
        let domChatAvatar;
        if (fNotes) {
            // Notes: no avatar, just show the title "Notes"
            domChatAvatar = null;
        } else if (isGroup) {
            // Group: use group placeholder
            domChatAvatar = document.createElement('img');
            domChatAvatar.src = './icons/group-placeholder.svg';
            domChatAvatar.classList.add('btn');
            domChatAvatar.onclick = () => {
                closeChat();
                openGroupOverview(chat);
            };
        } else {
            // DM: use profile avatar or placeholder
            const chatAvatarSrc = getProfileAvatarSrc(profile);
            domChatAvatar = createAvatarImg(chatAvatarSrc, 22, false);
            domChatAvatar.classList.add('btn');
            domChatAvatar.onclick = () => {
                previousChatBeforeProfile = strOpenChat;
                openProfile(profile);
            };
        }
        if (domChatAvatar) {
            domChatHeaderAvatarContainer.appendChild(domChatAvatar);
        }

        // Prefer displaying their name, otherwise, npub/group name
        if (fNotes) {
            domChatContact.textContent = 'Notes';
            domChatContact.classList.remove('btn');
        } else if (isGroup) {
            domChatContact.textContent = chat.metadata?.custom_fields?.name || `Group ${strOpenChat.substring(0, 10)}...`;
            // When the group name is clicked, expand the Group Overview
            domChatContact.onclick = () => {
                closeChat();
                openGroupOverview(chat);
            };
            domChatContact.classList.add('btn');
        } else {
            domChatContact.textContent = profile?.nickname || profile?.name || strOpenChat.substring(0, 10) + '…';
            if (profile?.nickname || profile?.name) twemojify(domChatContact);
            // When the name or status is clicked, expand their Profile
            domChatContact.onclick = () => {
                // Store the current chat so we can return to it
                previousChatBeforeProfile = strOpenChat;
                openProfile(profile);
            };
            domChatContact.classList.add('btn');
        }

        // Display either their Status or Typing Indicator
        updateChatHeaderSubtext(chat);

        // Auto-mark messages as read when chat is opened AND window is focused
        if (chat?.messages?.length) {
            // Check window focus before auto-marking
            const isWindowFocused = (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios')
                ? await getCurrentWindow().isFocused()
                : true;
            
            if (isWindowFocused) {
                // Find the latest message from the other person (not from current user)
                let lastContactMsg = null;
                for (let i = chat.messages.length - 1; i >= 0; i--) {
                    if (!chat.messages[i].mine) {
                        lastContactMsg = chat.messages[i];
                        break;
                    }
                }
                
                // If we found a message and it's not already marked as read, update the read status
                if (lastContactMsg && chat.last_read !== lastContactMsg.id) {
                    // Update the chat's last_read
                    markAsRead(chat, lastContactMsg);
                }
            }
        }

        if (!arrMessages.length) return;

        // Sort messages by timestamp (oldest first) to ensure correct insertion order
        // This is critical for timestamp insertion logic - without this, newer messages
        // get inserted first and older messages compare gaps against distant ancestors
        // instead of their actual chronological neighbors
        const sortedMessages = [...arrMessages].sort((a, b) => a.at - b.at);

        // Track last message time for timestamp insertion
        let nLastMsgTime = null;

        /* Dedup guard: skip any message already present in the DOM by ID */
         // Process each message for insertion
        for (const msg of sortedMessages) {
            // Guard against duplicate insertions if the DOM already contains this message ID
            if (document.getElementById(msg.id)) {
                continue;
            }
            // Quick check for empty chat - simple append
            if (domChatMessages.children.length === 0) {
                domChatMessages.appendChild(renderMessage(msg, profile));
                continue;
            }

            // Messages are managed by the procedural scroll system

            // Direct comparison with newest and oldest messages (most common cases)
            // This avoids expensive DOM operations for the common cases

            // Get the newest message in the DOM
            const newestMsgElement = domChatMessages.lastElementChild;
            const newestMsg = chat.messages.find(m => m.id === newestMsgElement.id);
            if (newestMsg && msg.at > newestMsg.at) {
                // It's the newest message, append it

                // Add timestamp if needed
                if (nLastMsgTime === null) {
                    nLastMsgTime = newestMsg.at;
                }

                if (msg.at - nLastMsgTime > 600 * 1000) {
                    insertTimestamp(msg.at, domChatMessages);
                    nLastMsgTime = msg.at;
                }

                // Render message post-time-insert for improved message rendering context
                const domMsg = renderMessage(msg, profile);
                if (!msg.mine && arrMessages.length === 1) {
                    domMsg.classList.add('new-anim');
                    domMsg.addEventListener('animationend', () => {
                        // Remove the animation class once it finishes
                        domMsg?.classList?.remove('new-anim');
                    }, { once: true });
                }

                domChatMessages.appendChild(domMsg);

                // If this was our pending message, then snap the view to the bottom
                if (msg.mine && msg.pending) scrollToBottom(domChatMessages, false);
                continue;
            }

            // Get the oldest message in the DOM
            let oldestMsgElement = null;
            for (let i = 0; i < domChatMessages.children.length; i++) {
                const child = domChatMessages.children[i];
                if (child.getAttribute('sender')) {
                    oldestMsgElement = child;
                    break;
                }
            }

            if (oldestMsgElement) {
                const oldestMsg = chat.messages.find(m => m.id === oldestMsgElement.id);
                if (oldestMsg && msg.at < oldestMsg.at) {
                    // It's the oldest message, prepend it
                    // Pass oldestMsgElement as context so renderMessage knows what comes after
                    const domMsg = renderMessage(msg, profile, '', oldestMsgElement);
                    domChatMessages.insertBefore(domMsg, oldestMsgElement);

                    // Update the next message's top corner if same sender (since new message is now before it)
                    if (oldestMsgElement.getAttribute('sender') === domMsg.getAttribute('sender')) {
                        const nextP = oldestMsgElement.querySelector('p');
                        if (nextP) {
                            const cornerProp = msg.mine ? 'borderTopRightRadius' : 'borderTopLeftRadius';
                            nextP.style[cornerProp] = '0px';
                            const audioPlayer = nextP.querySelector('.custom-audio-player');
                            if (audioPlayer) audioPlayer.style[cornerProp] = '0px';
                        }
                    }
                    continue;
                }
            }

            // If we get here, the message belongs somewhere in the middle
            // This is a less common case, so we'll do a linear scan
            let inserted = false;

            // Get the message elements sorted by time (oldest to newest)
            // We'll do a linear scan since we expect this to be rare and the chat isn't likely huge
            let messageNodes = [];
            for (let i = 0; i < domChatMessages.children.length; i++) {
                const child = domChatMessages.children[i];
                if (child.id && child.getAttribute('sender')) {
                    const childMsg = chat.messages.find(m => m.id === child.id);
                    if (childMsg) {
                        messageNodes.push({ element: child, message: childMsg });
                    }
                }
            }

            // Sort by timestamp if needed (they might not be in order in the DOM)
            messageNodes.sort((a, b) => a.message.at - b.message.at);

            // Find the correct position to insert
            for (let i = 0; i < messageNodes.length - 1; i++) {
                const currentNode = messageNodes[i];
                const nextNode = messageNodes[i + 1];

                if (currentNode.message.at <= msg.at && msg.at <= nextNode.message.at) {
                    // Add timestamp if needed
                    if (msg.at - currentNode.message.at > 600 * 1000) {
                        const timestamp = insertTimestamp(msg.at);
                        domChatMessages.insertBefore(timestamp, nextNode.element);
                    }

                    // Insert between these two messages
                    // Pass nextNode.element as context so renderMessage knows what comes after
                    const domMsg = renderMessage(msg, profile, '', nextNode.element);
                    domChatMessages.insertBefore(domMsg, nextNode.element);

                    // Update the next message's top corner if same sender (since new message is now before it)
                    if (nextNode.element.getAttribute('sender') === domMsg.getAttribute('sender')) {
                        const nextP = nextNode.element.querySelector('p');
                        if (nextP) {
                            const cornerProp = msg.mine ? 'borderTopRightRadius' : 'borderTopLeftRadius';
                            nextP.style[cornerProp] = '0px';
                            const audioPlayer = nextP.querySelector('.custom-audio-player');
                            if (audioPlayer) audioPlayer.style[cornerProp] = '0px';
                        }
                    }
                    inserted = true;
                    break;
                }
            }

            // If somehow not inserted by the above logic, append as fallback
            if (!inserted) {
                // Check if we need a timestamp
                const lastMsg = messageNodes[messageNodes.length - 1]?.message;
                if (lastMsg && msg.at - lastMsg.at > 600 * 1000) {
                    insertTimestamp(msg.at, domChatMessages);
                }

                const domMsg = renderMessage(msg, profile);
                domChatMessages.appendChild(domMsg);
            }
        }

        // Auto-scroll on new messages (if the user hasn't scrolled up, or on manual chat open)
        const pxFromBottom = domChatMessages.scrollHeight - domChatMessages.scrollTop - domChatMessages.clientHeight;
        if (pxFromBottom < 500 || fClicked) {
            const cLastMsg = chat.messages[chat.messages.length - 1];
            if (strLastMsgID !== cLastMsg.id || fClicked) {
                strLastMsgID = cLastMsg.id;
                adjustSize();
                // Force an auto-scroll, given soft-scrolling won't accurately work when the entire list has just rendered
                scrollToBottom(domChatMessages, false);
            }
        }
    } else {
        // Probably a 'New Chat', as such, we'll mostly render an empty chat
        // Clear existing messages when opening a new chat (fClicked = true)
        // This prevents messages from the previous chat from showing
        if (fClicked) {
            domChatMessages.innerHTML = '';
        }
        
        // Render chat header avatar
        domChatHeaderAvatarContainer.innerHTML = '';
        let domChatAvatar;
        if (fNotes) {
            // Notes: no avatar icon
            domChatAvatar = null;
        } else if (isGroup) {
            // Group: use group placeholder
            domChatAvatar = document.createElement('img');
            domChatAvatar.src = './icons/group-placeholder.svg';
            domChatAvatar.classList.add('btn');
            domChatAvatar.onclick = () => {
                closeChat();
                openGroupOverview(chat);
            };
        } else {
            // DM: use profile avatar or placeholder
            const dmAvatarSrc = getProfileAvatarSrc(profile);
            domChatAvatar = createAvatarImg(dmAvatarSrc, 22, false);
        }
        if (domChatAvatar) domChatHeaderAvatarContainer.appendChild(domChatAvatar);

        if (fNotes) {
            domChatContact.textContent = 'Notes';
            domChatContact.onclick = null;
            domChatContact.classList.remove('btn');
            domChatContactStatus.textContent = 'Encrypted Notes to Self';
            domChatContactStatus.classList.remove('text-gradient');
        } else if (isGroup) {
            domChatContact.textContent = chat?.metadata?.custom_fields?.name || `Group ${strOpenChat.substring(0, 10)}...`;
            domChatContact.onclick = () => {
                closeChat();
                openGroupOverview(chat);
            };
            domChatContact.classList.add('btn');

            // Ensure the member count/status renders even before the first message
            updateChatHeaderSubtext(chat);
        } else {
            domChatContact.textContent = profile?.nickname || profile?.name || strOpenChat.substring(0, 10) + '…';
            domChatContact.onclick = null;
            domChatContact.classList.remove('btn');
            domChatContactStatus.textContent = '';
            domChatContactStatus.classList.remove('text-gradient');
        }

        domChatContact.classList.toggle('chat-contact', !domChatContactStatus.textContent);
        domChatContact.classList.toggle('chat-contact-with-status', !!domChatContactStatus.textContent);
        domChatContactStatus.style.display = !domChatContactStatus.textContent ? 'none' : '';
    }

    adjustSize();
    
    // Update the back button notification dot after chat updates
    updateChatBackNotification();
}

/**
 * Helper function to create and insert a timestamp
 * @param {number} timestamp - Unix timestamp in seconds
 * @param {HTMLElement} parent - Optional parent to append to
 * @returns {HTMLElement} - The created timestamp element
 */
function insertTimestamp(timestamp, parent = null) {
    const pTimestamp = document.createElement('p');
    pTimestamp.classList.add('msg-inline-timestamp');
    const messageDate = new Date(timestamp);
    const timeStr = messageDate.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', hour12: true });

    // Render the time contextually (day/date in bold)
    if (isToday(messageDate)) {
        pTimestamp.innerHTML = `<strong>Today</strong>, ${timeStr}`;
    } else if (isYesterday(messageDate)) {
        pTimestamp.innerHTML = `<strong>Yesterday</strong>, ${timeStr}`;
    } else {
        const dateStr = messageDate.toLocaleDateString();
        pTimestamp.innerHTML = `<strong>${dateStr}</strong>, ${timeStr}`;
    }

    if (parent) {
        parent.appendChild(pTimestamp);
    }

    return pTimestamp;
}

/**
 * Helper function to create and insert a system event (member joined/left, etc.)
 * Uses the same styling as timestamps (centered, lower opacity)
 * @param {string} content - The system event text (e.g., "John has left")
 * @param {HTMLElement} parent - Optional parent to append to
 * @returns {HTMLElement} - The created system event element
 */
function insertSystemEvent(content, parent = null) {
    const pSystemEvent = document.createElement('p');
    pSystemEvent.classList.add('msg-inline-timestamp'); // Reuse timestamp styling
    pSystemEvent.textContent = content;

    if (parent) {
        parent.appendChild(pSystemEvent);
    }

    return pSystemEvent;
}

/**
 * Convert a Message in to a rendered HTML Element
 * @param {Message} msg - the Message to be converted
 * @param {Profile} sender - the Profile of the message sender
 * @param {string?} editID - the ID of the message being edited, used for improved renderer context
 * @param {HTMLElement?} contextElement - the DOM element to use for context (for prepending)
 */
function renderMessage(msg, sender, editID = '', contextElement = null) {
    // Helper to apply border radius to both p element and any custom-audio-player inside
    const applyBorderRadius = (pEl, property, value) => {
        if (!pEl) return;
        pEl.style[property] = value;
        const audioPlayer = pEl.querySelector('.custom-audio-player');
        if (audioPlayer) audioPlayer.style[property] = value;
    };

    // Construct the message container (the DOM ID is the HEX Nostr Event ID)
    const divMessage = document.createElement('div');
    divMessage.id = msg.id;

    // Add a subset of the sender's ID so we have context of WHO sent it, even in group contexts
    // For group chats, use msg.npub; for DMs, use sender.id
    const otherId = sender?.id || msg.npub || '';
    const strShortSenderID = (msg.mine ? strPubkey : otherId).substring(0, 8);
    divMessage.setAttribute('sender', strShortSenderID);

    // Check for PIVX payment - render special bubble
    if (msg.pivx_payment) {
        divMessage.classList.add('msg-' + (msg.mine ? 'me' : 'them'));
        const pivxBubble = renderPivxPaymentBubble(
            msg.pivx_payment.gift_code,
            msg.pivx_payment.amount_piv,
            msg.mine,
            msg.pivx_payment.address
        );
        divMessage.appendChild(pivxBubble);
        return divMessage;
    }

    // Check for system event - render like a timestamp (centered, lower opacity)
    if (msg.system_event) {
        return insertSystemEvent(msg.content);
    }

    // Check if we're in a group chat
    const currentChat = arrChats.find(c => c.id === strOpenChat);
    const isGroupChat = currentChat?.chat_type === 'MlsGroup';

    // Render it appropriately depending on who sent it
    divMessage.classList.add('msg-' + (msg.mine ? 'me' : 'them'));

    // Prepare the message container
    const pMessage = document.createElement('p');

    // Prepare our message container - including avatars and contextual bubble rendering
    // If contextElement is provided (prepending), use it; otherwise use lastElementChild (appending)
    const domPrevMsg = editID ? document.getElementById(editID).previousElementSibling :
                       (contextElement ? contextElement.previousElementSibling : domChatMessages.lastElementChild);
    const fIsMsg = !!domPrevMsg?.getAttribute('sender');
    
    // Find the last actual message (skip timestamps and other non-message elements)
    let lastActualMessage = domPrevMsg;
    while (lastActualMessage && !lastActualMessage.getAttribute('sender')) {
        lastActualMessage = lastActualMessage.previousElementSibling;
    }
    
    // Check if this is truly a new streak (different sender from last actual message)
    // Also treat as new streak if there's a timestamp between (fIsMsg is false when immediate previous element isn't a message)
    const isNewStreak = !lastActualMessage || lastActualMessage.getAttribute('sender') != strShortSenderID || !fIsMsg;
    
    if (isNewStreak) {
        // Add an avatar if this is not OUR message
        if (!msg.mine) {
            let avatarEl = null;
            // Resolve sender profile for group chats
            // For group chats, use msg.npub; for DMs, use sender
            const otherFullId = msg.npub || sender?.id || '';
            const authorProfile = sender || (otherFullId ? getProfile(otherFullId) : null);
            
            // If no profile exists, queue for immediate fetch
            if (!authorProfile && otherFullId) {
                invoke("queue_profile_sync", {
                    npub: otherFullId,
                    priority: "critical",
                    forceRefresh: false
                });
            }
            
            const msgAvatarSrc = getProfileAvatarSrc(authorProfile);
            avatarEl = createAvatarImg(msgAvatarSrc, 35, false);
            avatarEl.classList.add('avatar', 'btn');
            // Wire profile click if we have an identifiable user
            if (otherFullId) {
                avatarEl.onclick = () => {
                    const prof = getProfile(otherFullId) || authorProfile;
                    previousChatBeforeProfile = strOpenChat;
                    openProfile(prof || { id: otherFullId });
                };
            }
            
            // Create a container for avatar and username
            if (avatarEl) {
                const avatarContainer = document.createElement('div');
                avatarContainer.style.position = 'relative';
                avatarContainer.style.marginRight = '10px';
                
                // Only add username label in group chats
                if (isGroupChat) {
                    const usernameLabel = document.createElement('span');
                    usernameLabel.classList.add('msg-username-label', 'btn');
                    const displayName = authorProfile?.nickname || authorProfile?.name || '';
                    usernameLabel.textContent = displayName || otherFullId.substring(0, 8);
                    if (displayName) twemojify(usernameLabel);
                    
                    // Make username clickable to open profile
                    if (otherFullId) {
                        usernameLabel.onclick = () => {
                            const prof = getProfile(otherFullId) || authorProfile;
                            // Store the current chat so we can return to it
                            previousChatBeforeProfile = strOpenChat;
                            openProfile(prof || { id: otherFullId });
                        };
                    }
                    
                    avatarContainer.appendChild(usernameLabel);
                }
                
                avatarContainer.appendChild(avatarEl);
                
                // Remove the margin from the avatar since container handles it
                avatarEl.style.marginRight = '0';
                divMessage.appendChild(avatarContainer);
            }
        }

        // If there is an actual message before this one (not just any element), apply additional edits
        if (lastActualMessage) {
            // Check if the previous message was from the contact (!mine)
            const prevSenderID = lastActualMessage.getAttribute('sender');
            const wasPrevMsgFromContact = prevSenderID !== strPubkey.substring(0, 8);

            // Curve the previous message's bottom border since a new sender is starting
            // For "mine" messages: bottom-RIGHT corner; for "them" messages: bottom-LEFT corner
            if (!wasPrevMsgFromContact) {
                // Previous was from "me" - check if it needs rounding as last in streak
                // Look back to see if it had previous messages from same sender (with no timestamp between)
                const prevPrevElement = lastActualMessage.previousElementSibling;
                const hasTimestampBefore = prevPrevElement && !prevPrevElement.getAttribute('sender');

                let prevPrevMsg = prevPrevElement;
                while (prevPrevMsg && !prevPrevMsg.getAttribute('sender')) {
                    prevPrevMsg = prevPrevMsg.previousElementSibling;
                }
                // Only round if it's part of a multi-message streak (no timestamp before)
                const hadPreviousFromSameSender = !hasTimestampBefore && prevPrevMsg && prevPrevMsg.getAttribute('sender') === prevSenderID;

                if (hadPreviousFromSameSender) {
                    const pMsg = lastActualMessage.querySelector('p');
                    if (pMsg) {
                        applyBorderRadius(pMsg, 'borderBottomRightRadius', '15px');
                    }
                }
            } else {
                // The previous message was from the contact - check if it needs rounding as last in streak
                // Look back to see if it had previous messages from same sender (with no timestamp between)
                const prevPrevElement = lastActualMessage.previousElementSibling;
                // Check if there's a timestamp immediately before (which would break the streak visually)
                const hasTimestampBefore = prevPrevElement && !prevPrevElement.getAttribute('sender');

                let prevPrevMsg = prevPrevElement;
                // Skip non-messages to find actual previous message
                while (prevPrevMsg && !prevPrevMsg.getAttribute('sender')) {
                    prevPrevMsg = prevPrevMsg.previousElementSibling;
                }
                // Only consider it part of same streak if NO timestamp between them
                const hadPreviousFromSameSender = !hasTimestampBefore && prevPrevMsg && prevPrevMsg.getAttribute('sender') === prevSenderID;

                // Look forward to see if there are more messages from same sender after this one
                // (which would make this a middle message, not the last)
                let prevNextMsg = lastActualMessage.nextElementSibling;
                // Skip non-messages
                while (prevNextMsg && !prevNextMsg.getAttribute('sender')) {
                    prevNextMsg = prevNextMsg.nextElementSibling;
                }
                const hasNextFromSameSender = prevNextMsg && prevNextMsg.getAttribute('sender') === prevSenderID;

                // Only round if it had previous messages AND no next messages from same sender (making it the last)
                if (hadPreviousFromSameSender && !hasNextFromSameSender) {
                    const pMsg = lastActualMessage.querySelector('p');
                    if (pMsg && !pMsg.classList.contains('no-background')) {
                        applyBorderRadius(pMsg, 'borderBottomLeftRadius', '15px');
                    }
                }
            }

            // Add some additional margin to separate the senders visually (extra space for username in groups)
            if (!msg.mine) divMessage.style.marginTop = isGroupChat ? `20px` : `15px`;
        }
        
        // For group chats, add margin-top to the <p> element for the first message in a streak
        if (isGroupChat) {
            pMessage.style.marginTop = !msg.mine ? `25px` : `10px`;
        } else if (msg.mine) pMessage.style.marginTop = `10px`;

        // Flatten bottom corner like a "new first message" (anticipating more messages may follow)
        if (msg.mine) {
            pMessage.style.borderBottomRightRadius = `0`;
        } else {
            pMessage.style.borderBottomLeftRadius = `0`;
        }

        // Check if this is a singular message (no next message from same sender)
        // This check happens after the message is rendered (at the end of the function)
    } else {
        // Add additional margin to simulate avatar space
        // We always reserve space for non-mine messages since we render an avatar or placeholder for the first in a streak
        if (!msg.mine) {
            pMessage.style.marginLeft = `44px`;
        }

        // Flatten the top border to act as a visual continuation
        const pMsg = domPrevMsg.querySelector('p');
        if (pMsg) {
            if (msg.mine) {
                pMessage.style.borderTopRightRadius = `0`;
            } else {
                pMessage.style.borderTopLeftRadius = `0`;
            }
        }
    }

    // If we're replying to this, give it a glowing border
    const fReplying = strCurrentReplyReference === msg.id;
    const strEmojiCleaned = msg.content.replace(/\s/g, '');
    const fEmojiOnly = isEmojiOnly(strEmojiCleaned) && strEmojiCleaned.length <= 6;
    if (fReplying) {
        // Only display if replying
        pMessage.style.borderColor = getComputedStyle(document.documentElement).getPropertyValue('--reply-highlight-border').trim();
    }

    // If it's a reply: inject a preview of the replied-to message
    // Uses backend-provided reply context (works for old messages not in cache)
    // Falls back to in-memory search for pending messages or backwards compatibility
    if (msg.replied_to) {
        // Check if we have reply context from the backend (preferred - always available)
        const hasBackendContext = msg.replied_to_content !== undefined || msg.replied_to_has_attachment;

        // Try to find the referenced message in the current chat (fallback for pending messages)
        const chat = sender ? getDMChat(sender.id) : arrChats.find(c => c.id === strOpenChat);
        const cMsg = chat?.messages.find(m => m.id === msg.replied_to);

        // Use backend context if available, otherwise fall back to in-memory message
        if (hasBackendContext || cMsg) {
            // Render the reply in a quote-like fashion
            const divRef = document.createElement('div');
            divRef.classList.add('msg-reply', 'btn');

            // Add theme-based styling when replying to the other person's message
            // Use cMsg.mine if available, otherwise check backend-provided npub
            const repliedToMine = cMsg?.mine ?? (msg.replied_to_npub === strPubkey);
            if (!repliedToMine) {
                divRef.classList.add('msg-reply-them');
            }
            divRef.id = `r-${msg.replied_to}`;

            // Name + Message
            const spanName = document.createElement('span');
            spanName.style.color = `rgba(255, 255, 255, 0.7)`;

            // Determine the sender of the replied-to message
            let cSenderProfile;
            if (hasBackendContext) {
                // Use backend-provided npub
                if (msg.replied_to_npub) {
                    cSenderProfile = getProfile(msg.replied_to_npub);
                    // Check if it's our own message
                    if (msg.replied_to_npub === strPubkey) {
                        cSenderProfile = getProfile(strPubkey);
                    }
                } else {
                    // DM without npub - it's from the other participant
                    cSenderProfile = sender;
                }
            } else if (cMsg) {
                // Fallback to in-memory message data
                cSenderProfile = !cMsg.mine
                    ? (cMsg.npub ? getProfile(cMsg.npub) : sender)
                    : getProfile(strPubkey);
            }

            if (cSenderProfile?.nickname || cSenderProfile?.name) {
                spanName.textContent = cSenderProfile.nickname || cSenderProfile.name;
                twemojify(spanName);
            } else {
                const fallbackId = (hasBackendContext ? msg.replied_to_npub : cMsg?.npub) || cSenderProfile?.id || '';
                spanName.textContent = fallbackId ? fallbackId.substring(0, 10) + '…' : 'Unknown';
            }

            // Replied-to content (Text or Attachment)
            let spanRef;
            const replyContent = hasBackendContext ? msg.replied_to_content : cMsg?.content;
            const hasAttachment = hasBackendContext ? msg.replied_to_has_attachment : cMsg?.attachments?.length > 0;

            if (replyContent) {
                spanRef = document.createElement('span');
                spanRef.classList.add('msg-reply-text');
                spanRef.style.color = `rgba(255, 255, 255, 0.45)`;
                spanRef.textContent = truncateGraphemes(replyContent, 50);
                twemojify(spanRef);
            } else if (hasAttachment) {
                // For Attachments, we display an additional icon for quickly inferring the replied-to content
                spanRef = document.createElement('div');
                spanRef.style.display = `flex`;

                // Use in-memory message for detailed attachment info if available, otherwise show generic
                const attachmentExt = cMsg?.attachments?.[0]?.extension;
                const cFileType = attachmentExt ? getFileTypeInfo(attachmentExt) : { icon: 'attachment', description: 'Attachment' };

                // Icon
                const spanIcon = document.createElement('span');
                spanIcon.classList.add('icon', 'icon-' + cFileType.icon);
                spanIcon.style.position = `relative`;
                spanIcon.style.backgroundColor = `rgba(255, 255, 255, 0.45)`;
                spanIcon.style.width = `18px`;
                spanIcon.style.height = `18px`;
                spanIcon.style.margin = `0px`;

                // Description
                const spanDesc = document.createElement('span');
                spanDesc.style.color = `rgba(255, 255, 255, 0.45)`;
                spanDesc.style.marginLeft = `5px`;
                spanDesc.textContent = cFileType.description;

                // Combine
                spanRef.append(spanIcon, spanDesc);
            }

            divRef.appendChild(spanName);
            divRef.appendChild(document.createElement('br'));
            if (spanRef) {
                divRef.appendChild(spanRef);
            }
            pMessage.appendChild(divRef);
        }
    }

    // Pre-detect npub to potentially modify displayed content
    // If npub is at the end of the message, we'll strip it from the text display
    const npubInfoEarly = detectNostrProfile(msg.content);
    let displayContent = msg.content;
    if (npubInfoEarly && npubInfoEarly.isAtEnd && npubInfoEarly.textWithoutNpub) {
        displayContent = npubInfoEarly.textWithoutNpub;
    }
    
    // Render the text - if it's emoji-only and/or file-only, and less than four emojis, format them nicely
    const spanMessage = document.createElement('span');
    if (fEmojiOnly) {
        // Preserve linebreaks for creative emoji rendering (tophats on wolves)
        spanMessage.textContent = displayContent;
        spanMessage.style.whiteSpace = `pre-wrap`;
        // Add an emoji-only CSS format
        pMessage.classList.add('emoji-only');
        spanMessage.classList.add('emoji-only-content');
        // Align the emoji depending on who sent it
        spanMessage.style.textAlign = msg.mine ? 'right' : 'left';
    } else {
        // Render their text content (using our custom Markdown renderer)
        spanMessage.innerHTML = parseMarkdown(displayContent.trim());

        // Make URLs clickable (after markdown parsing, before twemojify)
        linkifyUrls(spanMessage);

        // Process inline image URLs (async - will load images in background)
        processInlineImages(spanMessage);
    }

    // Only process Text Content if any exists
    if (spanMessage.textContent) {
        // Twemojify!
        twemojify(spanMessage);

        // Append the message contents
        pMessage.appendChild(spanMessage);
    }

    // Append attachments
    let strRevealAttachmentPath = '';
    if (msg.attachments.length) {
        // Float the content depending on who's it is
        pMessage.style.float = msg.mine ? 'right' : 'left';
        // Remove any message bubbles
        pMessage.classList.add('no-background');
        pMessage.style.overflow = 'visible';
    }
    for (const cAttachment of msg.attachments) {
        if (cAttachment.downloaded) {
            // Save the path for our File Explorer shortcut
            strRevealAttachmentPath = cAttachment.path;

            // Convert the absolute file path to a Tauri asset
            const assetUrl = convertFileSrc(cAttachment.path);

            // Render the attachment appropriately for it's type
            if (['png', 'jpeg', 'jpg', 'gif', 'webp', 'svg', 'bmp', 'tiff', 'tif', 'ico'].includes(cAttachment.extension)) {
                // Images
                const imgContainer = document.createElement('div');
                imgContainer.style.position = 'relative';
                imgContainer.style.display = 'inline-block';
                
                const imgPreview = document.createElement('img');
                // SVGs need a specific width to scale properly
                if (cAttachment.extension === 'svg') {
                    imgPreview.style.width = `25vw`;
                } else {
                    imgPreview.style.maxWidth = `100%`;
                }
                imgPreview.style.height = `auto`;
                imgPreview.style.borderRadius = `8px`;
                imgPreview.src = assetUrl;

                // Add event listener for auto-scrolling
                imgPreview.addEventListener('load', () => {
                    // Auto-scroll if within 100ms of chat opening
                    if (chatOpenTimestamp && Date.now() - chatOpenTimestamp < 100) {
                        scrollToBottom(domChatMessages, false);
                    } else if (proceduralScrollState.isLoadingOlderMessages) {
                        // Correct scroll position for media loading during procedural scroll
                        correctScrollForMediaLoad();
                    } else {
                        // Normal soft scroll for layout adjustments
                        softChatScroll();
                    }
                }, { once: true });

                // Attach image preview handler
                attachImagePreview(imgPreview);

                imgContainer.appendChild(imgPreview);

                // Add file extension badge (handles size checking automatically)
                attachFileExtBadge(imgPreview, imgContainer, cAttachment.extension);

                pMessage.appendChild(imgContainer);
                } else if (platformFeatures.os !== 'linux' && ['wav', 'mp3', 'flac', 'aac', 'm4a', 'ogg', 'opus'].includes(cAttachment.extension)) {
                // Audio
                handleAudioAttachment(cAttachment, assetUrl, pMessage, msg);
                } else if (platformFeatures.os !== 'linux' && ['mp4', 'webm', 'mov'].includes(cAttachment.extension)) {
                // Videos
                const handleMetadataLoaded = (video) => {
                    // Seek a tiny amount to force the frame 'poster' to load
                    video.currentTime = 0.1;
                    
                    // Auto-scroll if within 100ms of chat opening
                    if (chatOpenTimestamp && Date.now() - chatOpenTimestamp < 100) {
                        scrollToBottom(domChatMessages, false);
                    } else if (proceduralScrollState.isLoadingOlderMessages) {
                        // Correct scroll position for media loading during procedural scroll
                        correctScrollForMediaLoad();
                    } else {
                        // Normal soft scroll for layout adjustments
                        softChatScroll();
                    }
                };
                
                // Platform-specific video creation
                if (platformFeatures.os === 'android') {
                    // Android always uses blob method with size limit
                    createAndroidVideo(assetUrl, cAttachment, handleMetadataLoaded, (element) => {
                        pMessage.appendChild(element);
                    });
                } else {
                    // Standard video element for other platforms
                    const vidPreview = document.createElement('video');
                    vidPreview.setAttribute('controlsList', 'nodownload');
                    vidPreview.controls = true;
                    vidPreview.style.width = `100%`;
                    vidPreview.style.height = `auto`;
                    vidPreview.style.borderRadius = `8px`;
                    vidPreview.style.cursor = `pointer`;
                    vidPreview.preload = "metadata";
                    vidPreview.playsInline = true;
                    vidPreview.src = assetUrl;
                    
                    // Add metadata loaded handler
                    vidPreview.addEventListener('loadedmetadata', () => {
                        handleMetadataLoaded(vidPreview);
                    }, { once: true });
                    
                    pMessage.appendChild(vidPreview);
                }
            } else {
                // File Attachment
                const ext = cAttachment.extension.toLowerCase();
                const fileTypeInfo = getFileTypeInfo(ext);
                const isMiniApp = fileTypeInfo.isMiniApp === true;
                
                const fileDiv = document.createElement('div');
                fileDiv.setAttribute('filepath', cAttachment.path);
                if (isMiniApp) {
                    fileDiv.classList.add('miniapp-attachment');
                }

                // Create the main container
                const btnDiv = document.createElement('div');
                btnDiv.className = 'btn custom-audio-player';
                btnDiv.style.display = 'flex';
                btnDiv.style.alignItems = 'center';
                btnDiv.style.padding = '10px';
                btnDiv.style.paddingRight = '15px';

                // Create the icon element (span for regular files, img for Mini Apps with icons)
                let iconElement;
                if (isMiniApp) {
                    // For Mini Apps, create an img element that will be populated with the icon
                    iconElement = document.createElement('img');
                    iconElement.style.marginLeft = '5px';
                    iconElement.style.width = '40px';
                    iconElement.style.height = '40px';
                    iconElement.style.borderRadius = '8px';
                    iconElement.style.objectFit = 'cover';
                    iconElement.style.backgroundColor = 'rgba(255, 255, 255, 0.1)';
                    // Set a placeholder initially
                    iconElement.src = 'data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="%23fff"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z"/></svg>';
                } else {
                    iconElement = document.createElement('span');
                    iconElement.className = `icon icon-${fileTypeInfo.icon}`;
                    iconElement.style.marginLeft = '5px';
                    iconElement.style.width = '50px';
                    iconElement.style.backgroundColor = 'rgba(255, 255, 255, 0.75)';
                }

                // Create the text container span
                const textContainerSpan = document.createElement('span');
                textContainerSpan.style.color = 'rgba(255, 255, 255, 0.85)';
                textContainerSpan.style.marginLeft = isMiniApp ? '15px' : '50px';
                textContainerSpan.style.lineHeight = '1.2';

                // Create the description span
                const descriptionSpan = document.createElement('span');
                descriptionSpan.style.display = 'block';
                descriptionSpan.style.color = 'var(--icon-color-primary)';
                descriptionSpan.style.fontWeight = '400';
                descriptionSpan.innerText = fileTypeInfo.description;

                // Create the small element for file details
                const smallElement = document.createElement('small');

                if (isMiniApp) {
                    // Mini App: show status and optionally peer count
                    // Make smallElement a flex container for proper alignment
                    smallElement.style.display = 'flex';
                    smallElement.style.alignItems = 'center';
                    smallElement.style.gap = '10px';
                    
                    const playSpan = document.createElement('span');
                    playSpan.style.color = 'rgba(255, 255, 255, 0.7)';
                    playSpan.style.fontWeight = '400';
                    playSpan.innerText = 'Click to Play';
                    smallElement.appendChild(playSpan);
                    
                    // Create peer count badge (hidden by default)
                    const peerBadge = document.createElement('span');
                    peerBadge.style.padding = '2px 8px';
                    peerBadge.style.borderRadius = '10px';
                    peerBadge.style.backgroundColor = 'rgba(46, 213, 115, 0.3)';
                    peerBadge.style.color = '#2ed573';
                    peerBadge.style.fontSize = '0.85em';
                    peerBadge.style.fontWeight = '500';
                    peerBadge.style.display = 'none';
                    smallElement.appendChild(peerBadge);
                    
                    // Store topic for event updates
                    const topicId = cAttachment.webxdc_topic;
                    if (topicId) {
                        fileDiv.setAttribute('data-webxdc-topic', topicId);
                    }
                    
                    // Helper function to update the peer badge
                    const updatePeerBadge = (peerCount, isPlaying) => {
                        const totalPlayers = isPlaying ? peerCount + 1 : peerCount;
                        if (totalPlayers > 0) {
                            // Create inline icon
                            const groupIcon = document.createElement('img');
                            groupIcon.src = 'icons/group-placeholder.svg';
                            groupIcon.style.width = '14px';
                            groupIcon.style.height = '14px';
                            groupIcon.style.verticalAlign = 'middle';
                            groupIcon.style.marginRight = '4px';
                            
                            // Clear and rebuild badge content
                            peerBadge.innerHTML = '';
                            peerBadge.appendChild(groupIcon);
                            peerBadge.appendChild(document.createTextNode(`${totalPlayers} online`));
                            peerBadge.style.display = 'inline-flex';
                            peerBadge.style.alignItems = 'center';
                        } else {
                            peerBadge.style.display = 'none';
                        }
                    };
                    
                    // Helper function to update the UI based on status
                    const updateMiniAppStatus = (isPlaying, peerCount) => {
                        if (isPlaying) {
                            // We're playing - show "Playing" and make unclickable
                            playSpan.innerText = 'Playing';
                            playSpan.style.color = '#2ed573';
                            fileDiv.style.cursor = 'default';
                            fileDiv.style.opacity = '0.9';
                            fileDiv.setAttribute('data-playing', 'true');
                        } else if (peerCount > 0) {
                            // Others are playing - show "Click to Join"
                            playSpan.innerText = 'Click to Join';
                            playSpan.style.color = 'rgba(255, 255, 255, 0.7)';
                            fileDiv.style.cursor = 'pointer';
                            fileDiv.style.opacity = '1';
                            fileDiv.removeAttribute('data-playing');
                        } else {
                            // No one playing - show "Click to Play"
                            playSpan.innerText = 'Click to Play';
                            playSpan.style.color = 'rgba(255, 255, 255, 0.7)';
                            fileDiv.style.cursor = 'pointer';
                            fileDiv.style.opacity = '1';
                            fileDiv.removeAttribute('data-playing');
                        }
                        updatePeerBadge(peerCount, isPlaying);
                    };
                    
                    // Load Mini App info asynchronously to get name and icon
                    loadMiniAppInfo(cAttachment.path).then(info => {
                        if (info) {
                            // Update the name
                            descriptionSpan.innerText = info.name || 'Mini App';
                            
                            // Update the icon if available
                            if (info.icon_data) {
                                iconElement.src = info.icon_data;
                            }
                        }
                    }).catch(err => {
                        console.warn('Failed to load Mini App info:', err);
                    });
                    
                    // Check for realtime channel status if we have a topic
                    if (topicId) {
                        invoke('miniapp_get_realtime_status', { topicId })
                            .then(status => {
                                console.log('[MiniApp] Realtime status:', status);
                                // Use peer_count if we have a channel, otherwise use pending_peer_count
                                // peer_count is from active Iroh channel, pending_peer_count is from advertisements
                                const peerCount = (status?.peer_count || 0) > 0
                                    ? status.peer_count
                                    : (status?.pending_peer_count || 0);
                                updateMiniAppStatus(status?.active || false, peerCount);
                            })
                            .catch(err => {
                                // Silently ignore - realtime status is optional
                                console.debug('Could not get realtime status:', err);
                            });
                        
                        // Listen for realtime events to update the UI
                        // Store the update function on the element for event handler access
                        fileDiv._updateMiniAppStatus = updateMiniAppStatus;
                    }
                } else {
                    // Regular file: show extension and size
                    const extSpan = document.createElement('span');
                    extSpan.style.color = 'white';
                    extSpan.style.fontWeight = '400';
                    extSpan.innerText = `.${ext}`;

                    const sizeSpan = document.createElement('span');
                    sizeSpan.innerText = ` — ${formatBytes(cAttachment.size)}`;

                    smallElement.appendChild(extSpan);
                    smallElement.appendChild(sizeSpan);
                }

                // Assemble the structure
                textContainerSpan.appendChild(descriptionSpan);
                textContainerSpan.appendChild(smallElement);
                btnDiv.appendChild(iconElement);
                btnDiv.appendChild(textContainerSpan);
                fileDiv.appendChild(btnDiv);

                // Click handler
                fileDiv.addEventListener('click', async (e) => {
                    const path = e.currentTarget.getAttribute('filepath');
                    if (!path) return;

                    if (isMiniApp) {
                        // Check if we're already playing
                        if (e.currentTarget.getAttribute('data-playing') === 'true') {
                            console.log('[MiniApp] Already playing, ignoring click');
                            return;
                        }

                        // Open Mini App in a new window
                        try {
                            // Find the attachment to get the webxdc_topic
                            const attachment = msg.attachments.find(a => a.path === path);
                            const topicId = attachment?.webxdc_topic || null;

                            // Check permissions before opening
                            const shouldOpen = await checkChatMiniAppPermissions(path);
                            if (!shouldOpen) {
                                return; // User cancelled the permission prompt
                            }

                            await openMiniApp(path, strOpenChat, msg.id, null, topicId);
                            
                            // Update UI to show "Playing" after opening
                            // Safety check: e.currentTarget may be null when called from non-click context
                            if (e.currentTarget && e.currentTarget._updateMiniAppStatus) {
                                // Get current peer count and update status
                                if (topicId) {
                                    invoke('miniapp_get_realtime_status', { topicId })
                                        .then(status => {
                                            e.currentTarget._updateMiniAppStatus(true, status?.peer_count || 0);
                                        })
                                        .catch(() => {
                                            e.currentTarget._updateMiniAppStatus(true, 0);
                                        });
                                } else {
                                    e.currentTarget._updateMiniAppStatus(true, 0);
                                }
                            }
                        } catch (err) {
                            console.error('Failed to open Mini App:', err);
                        }
                    } else {
                        // Regular file: reveal in explorer
                        revealItemInDir(path);
                    }
                });

                pMessage.appendChild(fileDiv);
            }

            // If the message is mine, and pending: display an uploading status
            if (msg.mine && msg.pending) {
                // Lower the attachment opacity (if element exists - may be async for Android videos)
                if (pMessage.lastElementChild) {
                    pMessage.lastElementChild.style.opacity = 0.25;
                }

                // Create the Progress Bar
                const divBar = document.createElement('div');
                divBar.id = msg.id + '_file';
                divBar.classList.add('progress-bar');
                divBar.style.width = `100%`;
                divBar.style.height = `5px`;
                divBar.style.marginTop = `0`;
                divBar.style.transitionDuration = `0.75s`;
                divBar.style.width = `0%`;
                pMessage.appendChild(divBar);
            }
        } else if (cAttachment.downloading) {
            // For images, show blurhash preview while downloading (only for formats that support blurhash)
            if (['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'].includes(cAttachment.extension)) {
                // Generate blurhash preview for downloading image
                // For group chats, use chat ID; for DMs, use sender.id
                const blurhashNpub2 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                invoke('generate_blurhash_preview', { npub: blurhashNpub2, msgId: msg.id })
                    .then(base64Image => {
                        const imgPreview = document.createElement('img');
                        imgPreview.style.width = `100%`;
                        imgPreview.style.height = `auto`;
                        imgPreview.style.borderRadius = `8px`;
                        imgPreview.style.opacity = `0.7`;
                        imgPreview.src = base64Image;
                        // Add scroll correction on blurhash load
                        imgPreview.addEventListener('load', () => {
                            if (proceduralScrollState.isLoadingOlderMessages) {
                                correctScrollForMediaLoad();
                            } else {
                                softChatScroll();
                            }
                        }, { once: true });
                        
                        // Create container for relative positioning
                        const container = document.createElement('div');
                        container.style.position = `relative`;
                        container.appendChild(imgPreview);
                        
                        // Add downloading indicator (for progress bar targeting)
                        const iDownloading = document.createElement('i');
                        iDownloading.id = cAttachment.id;
                        iDownloading.textContent = `Downloading`;
                        iDownloading.style.position = `absolute`;
                        iDownloading.style.top = `50%`;
                        iDownloading.style.left = `50%`;
                        iDownloading.style.transform = `translate(-50%, -50%)`;
                        iDownloading.style.backgroundColor = `rgba(0, 0, 0, 0.7)`;
                        iDownloading.style.padding = `5px 10px`;
                        iDownloading.style.borderRadius = `4px`;
                        iDownloading.style.color = `white`;
                        container.appendChild(iDownloading);
                        
                        pMessage.appendChild(container);
                    })
                    .catch(() => {
                        // Fallback to simple downloading indicator if blurhash fails
                        const iDownloading = document.createElement('i');
                        iDownloading.id = cAttachment.id;
                        iDownloading.textContent = `Downloading`;
                        pMessage.appendChild(iDownloading);
                    });
            } else {
                // Display download progression UI for non-images
                const iDownloading = document.createElement('i');
                iDownloading.id = cAttachment.id;
                iDownloading.textContent = `Downloading`;
                pMessage.appendChild(iDownloading);
            }
            } else {
                // Check if this attachment will auto-download
                const willAutoDownload = cAttachment.size > 0 && cAttachment.size <= MAX_AUTO_DOWNLOAD_BYTES;

                // For images, show blurhash preview with download button (unless auto-downloading)
                if (['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'].includes(cAttachment.extension)) {
                    // Generate blurhash preview for undownloaded image
                    // For group chats, use chat ID; for DMs, use sender.id
                    const blurhashNpub = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                    invoke('generate_blurhash_preview', { npub: blurhashNpub, msgId: msg.id })
                        .then(base64Image => {
                            const imgPreview = document.createElement('img');
                            imgPreview.style.width = `100%`;
                            imgPreview.style.height = `auto`;
                            imgPreview.style.borderRadius = `8px`;
                            imgPreview.style.opacity = willAutoDownload ? `0.8` : `0.6`;
                            imgPreview.src = base64Image;
                            // Add scroll correction on blurhash load
                            imgPreview.addEventListener('load', () => {
                                if (proceduralScrollState.isLoadingOlderMessages) {
                                    correctScrollForMediaLoad();
                                } else {
                                    softChatScroll();
                                }
                            }, { once: true });
                            
                            // Create container for relative positioning
                            const container = document.createElement('div');
                            container.style.position = `relative`;
                            container.appendChild(imgPreview);

                            // Only show download button if NOT auto-downloading
                            if (!willAutoDownload) {
                                // Determine and display file size
                                let strSize = 'Unknown Size';
                                if (cAttachment.size > 0) strSize = formatBytes(cAttachment.size);

                                // Create download button overlay
                                const iDownload = document.createElement('i');
                                iDownload.id = cAttachment.id;
                                iDownload.toggleAttribute('download', true);
                                // For group chats, use chat ID; for DMs, use sender.id
                                const downloadNpub2 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                                iDownload.setAttribute('npub', downloadNpub2);
                                iDownload.setAttribute('msg', msg.id);
                                iDownload.classList.add('btn');
                                iDownload.textContent = `Download ${cAttachment.extension.toUpperCase()} (${strSize})`;
                                iDownload.style.position = `absolute`;
                                iDownload.style.top = `50%`;
                                iDownload.style.left = `50%`;
                                iDownload.style.transform = `translate(-50%, -50%)`;
                                iDownload.style.backgroundColor = `rgba(0, 0, 0, 0.8)`;
                                iDownload.style.padding = `8px 15px`;
                                iDownload.style.borderRadius = `6px`;
                                iDownload.style.color = `white`;
                                iDownload.style.cursor = `pointer`;
                                iDownload.style.fontSize = `12px`;
                                iDownload.style.whiteSpace = `nowrap`;
                                iDownload.style.textAlign = `center`;
                                iDownload.style.maxWidth = `90%`;
                                iDownload.style.overflow = `hidden`;
                                iDownload.style.textOverflow = `ellipsis`;
                                container.appendChild(iDownload);
                            } else {
                                // For auto-downloading images, create a hidden element for progress bar targeting
                                const iHidden = document.createElement('i');
                                iHidden.id = cAttachment.id;
                                iHidden.style.display = `none`;
                                container.appendChild(iHidden);
                            }
                            
                            pMessage.appendChild(container);
                        })
                        .catch(() => {
                            // Fallback when blurhash fails
                            if (!willAutoDownload) {
                                // Manual download: show download button
                                let strSize = 'Unknown Size';
                                if (cAttachment.size > 0) strSize = formatBytes(cAttachment.size);

                                const iDownload = document.createElement('i');
                                iDownload.id = cAttachment.id;
                                iDownload.toggleAttribute('download', true);
                                // For group chats, use chat ID; for DMs, use sender.id
                                const downloadNpub3 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                                iDownload.setAttribute('npub', downloadNpub3);
                                iDownload.setAttribute('msg', msg.id);
                                iDownload.classList.add('btn');
                                iDownload.textContent = `Download ${cAttachment.extension.toUpperCase()} (${strSize})`;
                                pMessage.appendChild(iDownload);
                            } else {
                                // Auto-download: show downloading indicator for progress bar targeting
                                const iDownloading = document.createElement('i');
                                iDownloading.id = cAttachment.id;
                                iDownloading.textContent = `Downloading image...`;
                                iDownloading.style.textAlign = `center`;
                                iDownloading.style.display = `block`;
                                pMessage.appendChild(iDownloading);
                            }
                        });
                } else if (!willAutoDownload) {
                    // Only show download prompt for non-images if NOT auto-downloading
                    // Determine and display file size
                    let strSize = 'Unknown Size';
                    if (cAttachment.size > 0) strSize = formatBytes(cAttachment.size);

                    // Display download prompt UI for non-images
                    const iDownload = document.createElement('i');
                    iDownload.id = cAttachment.id;
                    iDownload.toggleAttribute('download', true);
                    // For group chats, use chat ID; for DMs, use sender.id
                    const downloadNpub = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                    iDownload.setAttribute('npub', downloadNpub);
                    iDownload.setAttribute('msg', msg.id);
                    iDownload.classList.add('btn');
                    iDownload.textContent = `Download ${cAttachment.extension.toUpperCase()} (${strSize})`;
                    pMessage.appendChild(iDownload);
                }

                // If the size is known and within auto-download range; immediately begin downloading
                if (willAutoDownload) {
                    // For non-images (which don't have blurhash previews), create a placeholder for progress bar targeting
                    if (!['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico'].includes(cAttachment.extension)) {
                        const iDownloading = document.createElement('i');
                        iDownloading.id = cAttachment.id;
                        iDownloading.textContent = `Starting download...`;
                        iDownloading.style.textAlign = `center`;
                        iDownloading.style.display = `block`;
                        pMessage.appendChild(iDownloading);
                    }
                    
                    // For group chats, use chat ID; for DMs, use sender.id
                    const downloadNpub4 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                    invoke('download_attachment', { npub: downloadNpub4, msgId: msg.id, attachmentId: cAttachment.id });
                }
            }
    }

    // Sync border radius from pMessage to any custom-audio-player inside (for streak continuation)
    const audioPlayer = pMessage.querySelector('.custom-audio-player');
    if (audioPlayer) {
        if (pMessage.style.borderTopLeftRadius) {
            audioPlayer.style.borderTopLeftRadius = pMessage.style.borderTopLeftRadius;
        }
        if (pMessage.style.borderTopRightRadius) {
            audioPlayer.style.borderTopRightRadius = pMessage.style.borderTopRightRadius;
        }
        if (pMessage.style.borderBottomLeftRadius) {
            audioPlayer.style.borderBottomLeftRadius = pMessage.style.borderBottomLeftRadius;
        }
        if (pMessage.style.borderBottomRightRadius) {
            audioPlayer.style.borderBottomRightRadius = pMessage.style.borderBottomRightRadius;
        }
    }

    // Append Payment Shortcuts (i.e: Bitcoin Payment URIs, etc)
    const cAddress = detectCryptoAddress(msg.content);
    if (cAddress) {
        // Render the Payment UI
        pMessage.appendChild(renderCryptoAddress(cAddress));
    }

    // Append Nostr Profile Previews (for shared npubs and vectorapp.io profile links)
    // Reuse the early detection result if available
    const npubInfo = npubInfoEarly;
    if (npubInfo) {
        // Check if the message is ONLY an npub (with optional whitespace)
        // If so, hide the text span since the preview shows all the info
        const isOnlyNpub = msg.content.trim() === npubInfo.originalMatch;
        if (isOnlyNpub) {
            const msgSpan = pMessage.querySelector('span');
            if (msgSpan) {
                msgSpan.style.display = 'none';
            }
            // Remove padding from the message bubble when it's only an npub
            pMessage.style.padding = '0';
        }
        
        // Check if we already have the profile cached
        const cachedProfile = getProfile(npubInfo.npub);
        const profilePreview = renderNostrProfilePreview(npubInfo, cachedProfile, isOnlyNpub);
        
        // Add click handler for the "View Profile" button
        const btnViewProfile = profilePreview.querySelector('.msg-profile-btn');
        if (btnViewProfile) {
            btnViewProfile.addEventListener('click', (e) => {
                e.stopPropagation();
                const npub = btnViewProfile.getAttribute('data-npub');
                // openProfile accepts a profile object with at least an 'id' field
                // If we have a cached profile, use it; otherwise create a minimal one
                openProfile(getProfile(npub) || { id: npub });
            });
        }
        
        // Add click handler for the copy button
        const btnCopy = profilePreview.querySelector('.msg-profile-copy-btn');
        if (btnCopy) {
            btnCopy.addEventListener('click', async (e) => {
                e.stopPropagation();
                const npub = btnCopy.getAttribute('data-npub');
                // Always copy the full profile URL for easy sharing
                const profileUrl = `https://vectorapp.io/profile/${npub}`;
                await navigator.clipboard.writeText(profileUrl);
                // Show checkmark feedback
                btnCopy.innerHTML = '<span class="icon icon-check"></span>';
                setTimeout(() => {
                    btnCopy.innerHTML = '<span class="icon icon-copy"></span>';
                }, 2000);
            });
        }
        
        // If we don't have the profile yet, queue a high-priority fetch
        if (!cachedProfile) {
            invoke('queue_profile_sync', {
                npub: npubInfo.npub,
                priority: 'high',
                forceRefresh: false
            }).catch(err => console.warn('Failed to queue profile sync for npub preview:', err));
        }
        
        pMessage.appendChild(profilePreview);
    }

    // Append Metadata Previews (i.e: OpenGraph data from URLs, etc) - only if enabled
    // Skip web preview if we already rendered a profile preview (e.g., vectorapp.io/profile links)
    const skipWebPreview = npubInfoEarly && npubInfoEarly.type === 'link';
    if (!msg.pending && !msg.failed && fWebPreviewsEnabled && !skipWebPreview) {
        // Check if we have metadata with either an image OR a title/description
        const hasMetadata = msg.preview_metadata && (
            msg.preview_metadata.og_image ||
            msg.preview_metadata.og_title ||
            msg.preview_metadata.title ||
            msg.preview_metadata.og_description ||
            msg.preview_metadata.description
        );
        
        if (hasMetadata) {
            // Setup the Preview container
            const divPrevContainer = document.createElement('div');
            divPrevContainer.classList.add('msg-preview-container', 'btn');
            divPrevContainer.setAttribute('url', msg.preview_metadata.og_url || msg.preview_metadata.domain);

            // Check if we have both description and image
            const description = msg.preview_metadata.og_description || msg.preview_metadata.description;
            const hasImage = !!msg.preview_metadata.og_image;
            
            // If we have both description and image, set bottom padding to 0
            if (description && hasImage) {
                divPrevContainer.style.paddingBottom = '0';
            }

            // Setup the Favicon
            const imgFavicon = document.createElement('img');
            imgFavicon.classList.add('favicon');
            imgFavicon.src = msg.preview_metadata.favicon;
            imgFavicon.addEventListener('load', () => {
                if (proceduralScrollState.isLoadingOlderMessages) {
                    correctScrollForMediaLoad();
                } else {
                    softChatScroll();
                }
            }, { once: true });

            // Add the title (prefixed with the Favicon)
            const spanPreviewTitle = document.createElement('span');
            spanPreviewTitle.appendChild(imgFavicon);
            const spanText = document.createTextNode(msg.preview_metadata.title || msg.preview_metadata.og_title || 'Link Preview');
            spanPreviewTitle.appendChild(spanText);
            divPrevContainer.appendChild(spanPreviewTitle);

            // Add description if available (especially useful for Twitter/X posts)
            if (description) {
                const spanDescription = document.createElement('span');
                spanDescription.classList.add('msg-preview-description');
                
                // Safely render text with line breaks by manually creating text nodes and <br> elements
                // Split by <br> tags first (from Twitter HTML), then by \n (from other sources)
                const parts = description.split(/<br\s*\/?>/i);
                parts.forEach((part, index) => {
                    // For each part, split by \n and add text nodes with <br> between them
                    const subParts = part.split('\n');
                    subParts.forEach((subPart, subIndex) => {
                        if (subPart) {
                            spanDescription.appendChild(document.createTextNode(subPart));
                        }
                        if (subIndex < subParts.length - 1) {
                            spanDescription.appendChild(document.createElement('br'));
                        }
                    });
                    if (index < parts.length - 1) {
                        spanDescription.appendChild(document.createElement('br'));
                    }
                });
                
                // If there's an image, remove border radius so description sits flush
                if (hasImage) {
                    spanDescription.style.borderRadius = '0';
                }
                divPrevContainer.appendChild(spanDescription);
            }

            // Load the Preview image if available
            if (hasImage) {
                const imgPreview = document.createElement('img');
                imgPreview.classList.add('msg-preview-img');
                imgPreview.src = msg.preview_metadata.og_image;
                // Auto-scroll the chat to correct against container resizes
                imgPreview.addEventListener('load', () => {
                    if (proceduralScrollState.isLoadingOlderMessages) {
                        correctScrollForMediaLoad();
                    } else {
                        softChatScroll();
                    }
                }, { once: true });
                divPrevContainer.appendChild(imgPreview);
            }

            // Render the Preview
            pMessage.appendChild(divPrevContainer);
        } else if (!msg.preview_metadata && msg.content) {
            // Grab the message's metadata (currently, only URLs can have extracted metadata)
            // Skip fetching metadata for direct image URLs (they render inline instead)
            if (msg.content.includes('https') && !isImageUrl(msg.content)) {
                // Pass the chat ID so backend can find both DMs and group chats
                invoke("fetch_msg_metadata", { chatId: strOpenChat, msgId: msg.id });
            }
        }
    }

    // If the message is pending or failed, let's adjust it
    if (msg.pending && !msg.attachments.length) {
        divMessage.style.opacity = 0.75;
    }
    if (msg.failed) {
        pMessage.style.color = 'red';
    }

    // If the message has been edited, show an indicator
    if (msg.edited) {
        const spanEdited = document.createElement('span');
        spanEdited.classList.add('msg-edited-indicator');
        spanEdited.textContent = '(edited)';
        // If there's edit history, make it clickable to show history
        if (msg.edit_history && msg.edit_history.length > 0) {
            spanEdited.classList.add('btn');
            spanEdited.setAttribute('data-msg-id', msg.id);
            spanEdited.title = 'Click to view edit history';
        }
        pMessage.appendChild(spanEdited);
    }

    // Add message reactions
    // TODO: while currently limited to one; add support for multi-reactions with a nice UX
    const cReaction = msg.reactions[0];
    let spanReaction;
    if (cReaction) {
        // Aggregate the 'reactions' of this reaction's type
        const nReacts = msg.reactions.reduce((a, b) => b.emoji === cReaction.emoji ? a + 1 : a, 0);
        spanReaction = document.createElement('span');
        spanReaction.classList.add('reaction');
        spanReaction.textContent = `${cReaction.emoji} ${nReacts}`;
        twemojify(spanReaction);
    } else if (!msg.mine) {
        // No reaction on the contact's message, so let's display the 'Add Reaction' UI
        spanReaction = document.createElement('span');
        spanReaction.classList.add('add-reaction', 'hideable', 'icon', 'icon-smile-face');
    }

    // Construct our "extras" (reactions, reply button, etc)
    // TODO: placeholder style, looks awful, but works!
    const divExtras = document.createElement('div');
    divExtras.classList.add('msg-extras');
    if (msg.mine) divExtras.style.marginRight = `5px`;
    else divExtras.style.marginLeft = `5px`;
    
    // Apply the same top margin to divExtras if this is the first message in a streak (group chats only)
    if (isNewStreak && isGroupChat && !msg.mine) {
        // Match the pMessage top margin for proper alignment with username labels
        divExtras.style.marginTop = `25px`;
    }

    // These can ONLY be shown on fully sent messages (inherently does not apply to received msgs)
    if (!msg.pending && !msg.failed) {
        // Reactions
        if (spanReaction) {
            if (msg.mine) {
                // My message: reactions on the left
                spanReaction.style.marginLeft = '-10px';
            }
            divExtras.append(spanReaction);
        } else {
            // No reactions: just render the message
            divMessage.appendChild(pMessage);
        }

        // Reply Icon (if we're not already replying!)
        if (!fReplying) {
            const spanReply = document.createElement('span');
            spanReply.classList.add('reply-btn', 'hideable', 'icon', 'icon-reply');
            divExtras.append(spanReply);
        }

        // Edit Icon (only for my own text messages)
        if (msg.mine && msg.content && !msg.attachments.length) {
            const spanEdit = document.createElement('span');
            spanEdit.classList.add('edit-btn', 'hideable', 'icon', 'icon-edit');
            spanEdit.setAttribute('data-msg-id', msg.id);
            spanEdit.setAttribute('data-msg-content', msg.content);
            divExtras.append(spanEdit);
        }

        // File Reveal Icon (if a file was attached)
        if (strRevealAttachmentPath) {
            const spanReveal = document.createElement('span');
            spanReveal.setAttribute('filepath', strRevealAttachmentPath);
            spanReveal.classList.add('hideable', 'icon', 'icon-file-search');
            divExtras.append(spanReveal);
        }
    }

    // Depending on who it is: render the extras appropriately
    if (msg.mine) {
        // Wrap message and status in a container for proper stacking
        const msgWrapper = document.createElement('div');
        msgWrapper.classList.add('msg-wrapper');
        msgWrapper.appendChild(pMessage);

        // Add status indicator for outgoing messages
        const statusEl = document.createElement('span');
        statusEl.classList.add('msg-status');
        if (msg.failed) {
            statusEl.classList.add('msg-status-failed');
            statusEl.textContent = 'Failed';
        } else if (msg.pending) {
            statusEl.textContent = 'Sending...';
        } else {
            // Show "Sent" status - will be hidden on previous messages in setTimeout
            statusEl.innerHTML = 'Sent <span class="icon icon-check-circle"></span>';
        }
        msgWrapper.appendChild(statusEl);

        divMessage.append(divExtras, msgWrapper);
    } else {
        divMessage.append(pMessage, divExtras);
    }

    // After rendering, check message corner styling
    // This needs to be done post-render when the message is in the DOM
    setTimeout(() => {
        if (domChatMessages.contains(divMessage)) {
            const nextMsg = divMessage.nextElementSibling;
            const prevMsg = divMessage.previousElementSibling;

            // Check if previous message exists and is from a different sender
            const isFirstFromSender = !prevMsg || prevMsg.getAttribute('sender') !== strShortSenderID;

            // Check if next message exists and is from the same sender
            const hasNextFromSameSender = nextMsg && nextMsg.getAttribute('sender') === strShortSenderID;

            // Determine which corner to use based on sender (mine = right, them = left)
            const cornerProperty = msg.mine ? 'borderBottomRightRadius' : 'borderBottomLeftRadius';

            // If we're continuing a message streak (not first from sender), we need to update the previous message
            if (!isFirstFromSender && prevMsg) {
                // The previous message is no longer the last in the streak, so flatten its bottom corner
                const prevPMsg = prevMsg.querySelector('p');
                if (prevPMsg) {
                    // Flatten the corner (0px) - this handles both CSS defaults and explicitly rounded corners
                    applyBorderRadius(prevPMsg, cornerProperty, '0px');
                }
            }

            // Now style the current message appropriately
            // Singular messages (first AND last) keep CSS default corners - no modification needed
            // Only modify corners for multi-message streaks
            if (!isFirstFromSender && !hasNextFromSameSender) {
                // This is the last message in a multi-message streak - apply rounded corner to close the bubble group
                const pMsg = divMessage.querySelector('p');
                if (pMsg) {
                    applyBorderRadius(pMsg, cornerProperty, '15px');
                }
            }

            // Update message status visibility for outgoing messages
            // Only the last msg-me should show "Sent" status
            if (msg.mine) {
                // Hide status on all other msg-me messages (keep only last visible)
                const allMsgMe = domChatMessages.querySelectorAll('.msg-me');
                allMsgMe.forEach((msgEl, index) => {
                    const isLast = index === allMsgMe.length - 1;
                    const statusEl = msgEl.querySelector('.msg-status:not(.msg-status-failed)');
                    if (statusEl && !statusEl.textContent.includes('Sending')) {
                        if (isLast) {
                            statusEl.classList.remove('msg-status-hidden');
                        } else {
                            statusEl.classList.add('msg-status-hidden');
                        }
                    }
                });
            }
        }
    }, 0);

    return divMessage;
}

/**
 * Select a message to begin replying to
 * @param {MouseEvent} e 
 */
function selectReplyingMessage(e) {
    // Cancel any existing reply-focus
    if (strCurrentReplyReference) {
        document.getElementById(strCurrentReplyReference).querySelector('p').style.borderColor = ``;
    }
    // Get the reply ID
    strCurrentReplyReference = e.target.parentElement.parentElement.id;
    // Hide the File UI and Display the cancel UI
    domChatMessageInputFile.style.display = 'none';
    domChatMessageInputCancel.style.display = '';
    // Display a replying placeholder
    domChatMessageInput.setAttribute('placeholder', 'Enter reply...');
    // Focus the message input (desktop only - mobile keyboards are disruptive)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }
    // Add a reply-focus
    e.target.parentElement.parentElement.querySelector('p').style.borderColor = getComputedStyle(document.documentElement).getPropertyValue('--reply-highlight-border').trim();
}

/**
 * Cancel any ongoing replies and reset the messaging interface
 */
function cancelReply() {
    // Reset the message UI
    domChatMessageInputFile.style.display = '';
    domChatMessageInputCancel.style.display = 'none';
    domChatMessageInput.setAttribute('placeholder', strOriginalInputPlaceholder);

    // Focus the message input (desktop only - mobile keyboards are disruptive)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }

    // Cancel any existing reply-focus
    if (strCurrentReplyReference) {
        let domMsg = document.getElementById(strCurrentReplyReference);
        if (domMsg) domMsg.querySelector('p').style.borderColor = ``;
    }

    // Remove the reply ID
    strCurrentReplyReference = '';

    // Reset send button state based on current input
    const hasText = domChatMessageInput.value.trim().length > 0;
    if (hasText) {
        domChatMessageInputSend.classList.add('active');
        domChatMessageInputSend.style.display = '';
        domChatMessageInputVoice.style.display = 'none';
    } else {
        domChatMessageInputSend.classList.remove('active');
        domChatMessageInputSend.style.display = 'none';
        domChatMessageInputVoice.style.display = '';
    }
}

/**
 * Start editing a message
 * @param {string} messageId - The ID of the message to edit
 * @param {string} content - The current content of the message
 */
function startEditMessage(messageId, content) {
    // Cancel any existing reply first
    if (strCurrentReplyReference) {
        cancelReply();
    }

    // Cancel any existing edit
    if (strCurrentEditMessageId) {
        cancelEdit();
    }

    // Store the edit state
    strCurrentEditMessageId = messageId;
    strCurrentEditOriginalContent = content;

    // Populate the input with the message content
    domChatMessageInput.value = content;

    // Show the cancel button, hide file button
    domChatMessageInputFile.style.display = 'none';
    domChatMessageInputCancel.style.display = '';

    // Update placeholder
    domChatMessageInput.setAttribute('placeholder', 'Editing message...');

    // Highlight the message being edited
    const msgElement = document.getElementById(messageId);
    if (msgElement) {
        const pElement = msgElement.querySelector('p');
        if (pElement) {
            pElement.style.borderColor = '#ffa500'; // Orange border for editing
        }
    }

    // Show the send button (since we have text)
    domChatMessageInputSend.classList.add('active');
    domChatMessageInputSend.style.display = '';
    domChatMessageInputVoice.style.display = 'none';

    // Focus the input and move cursor to end
    domChatMessageInput.focus();
    domChatMessageInput.setSelectionRange(content.length, content.length);

    // Auto-resize the input
    autoResizeChatInput();
}

/**
 * Cancel editing and restore the input to normal state
 */
function cancelEdit() {
    // Remove the highlight from the message being edited
    if (strCurrentEditMessageId) {
        const msgElement = document.getElementById(strCurrentEditMessageId);
        if (msgElement) {
            const pElement = msgElement.querySelector('p');
            if (pElement) {
                pElement.style.borderColor = '';
            }
        }
    }

    // Clear the edit state
    strCurrentEditMessageId = '';
    strCurrentEditOriginalContent = '';

    // Clear the input
    domChatMessageInput.value = '';

    // Reset the message UI
    domChatMessageInputFile.style.display = '';
    domChatMessageInputCancel.style.display = 'none';
    domChatMessageInput.setAttribute('placeholder', strOriginalInputPlaceholder);

    // Reset send button state
    domChatMessageInputSend.classList.remove('active');
    domChatMessageInputSend.style.display = 'none';
    domChatMessageInputVoice.style.display = '';

    // Focus the input (desktop only)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }

    // Auto-resize the input back to normal
    autoResizeChatInput();
}

/**
 * Show the edit history popup for a message
 * @param {string} messageId - The ID of the message to show history for
 * @param {HTMLElement} targetElement - The element that was clicked (for positioning)
 */
let strCurrentEditHistoryMsgId = '';

function showEditHistory(messageId, targetElement) {
    const popup = document.getElementById('edit-history-popup');
    const content = document.getElementById('edit-history-content');
    if (!popup || !content) return;

    // If clicking the same message that's already open, ignore
    if (strCurrentEditHistoryMsgId === messageId && popup.style.display !== 'none') {
        return;
    }

    // Find the message in the current chat
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return;

    const msg = chat.messages.find(m => m.id === messageId);
    if (!msg || !msg.edit_history || msg.edit_history.length === 0) {
        return;
    }

    // Track which message's history is open
    strCurrentEditHistoryMsgId = messageId;

    // Clear previous content
    content.innerHTML = '';

    // Format date/time
    const formatTime = (timestamp) => {
        const date = new Date(timestamp);
        return date.toLocaleString(undefined, {
            month: 'short',
            day: 'numeric',
            hour: '2-digit',
            minute: '2-digit'
        });
    };

    // Build edit history entries (oldest to newest)
    const totalEntries = msg.edit_history.length;
    const entryElements = [];
    msg.edit_history.forEach((entry, index) => {
        const div = document.createElement('div');
        div.classList.add('edit-history-entry');

        // Mark original and current
        const isOriginal = index === 0;
        const isCurrent = index === totalEntries - 1;
        if (isOriginal) div.classList.add('original');
        if (isCurrent) div.classList.add('current');

        // Time and label
        const timeDiv = document.createElement('div');
        timeDiv.classList.add('edit-history-time');
        timeDiv.textContent = formatTime(entry.edited_at);
        if (isOriginal) {
            const label = document.createElement('span');
            label.classList.add('edit-history-label');
            label.textContent = 'Original';
            timeDiv.appendChild(label);
        } else if (isCurrent) {
            const label = document.createElement('span');
            label.classList.add('edit-history-label');
            label.textContent = 'Current';
            timeDiv.appendChild(label);
        }

        // Content
        const textDiv = document.createElement('div');
        textDiv.classList.add('edit-history-text');
        textDiv.textContent = entry.content;

        div.appendChild(timeDiv);
        div.appendChild(textDiv);
        content.appendChild(div);
        entryElements.push(div);
    });

    // Find the message bubble (p element) for positioning
    const msgBubble = targetElement.closest('p');
    const rect = msgBubble ? msgBubble.getBoundingClientRect() : targetElement.getBoundingClientRect();

    // Reset position and show popup to measure its actual dimensions
    popup.style.top = '0';
    popup.style.left = '0';
    popup.style.visibility = 'hidden';
    popup.style.display = 'block';

    // Force layout recalculation then measure
    const popupHeight = popup.getBoundingClientRect().height;
    const popupWidth = popup.getBoundingClientRect().width;

    // Position above or below the bubble depending on space
    let top = rect.top - popupHeight - 4;
    const showBelow = top < 10;
    if (showBelow) {
        top = rect.bottom + 4;
    }

    // Apply staggered animation delays based on position
    // Above: latest (bottom) fades first, oldest (top) last
    // Below: oldest (top) fades first, latest (bottom) last
    entryElements.forEach((el, index) => {
        const delay = showBelow ? index * 50 : (totalEntries - 1 - index) * 50;
        el.style.animationDelay = `${delay}ms`;
    });

    // Align horizontally with the bubble edge, keep within viewport
    let left = rect.left;
    left = Math.max(10, Math.min(left, window.innerWidth - popupWidth - 10));

    popup.style.left = `${left}px`;
    popup.style.top = `${top}px`;
    popup.style.visibility = 'visible';

    // Scroll to show the current (latest) entry
    content.scrollTop = content.scrollHeight;
}

/**
 * Hide the edit history popup
 */
function hideEditHistory() {
    const popup = document.getElementById('edit-history-popup');
    if (popup) {
        popup.style.display = 'none';
    }
    strCurrentEditHistoryMsgId = '';
}

/**
 * Open a chat with a particular contact
 * @param {string} contact
 */
async function openChat(contact) {
    // Display the Chat UI
    navbarSelect('chat-btn');
    domProfile.style.display = 'none';
    domChatNew.style.display = 'none';
    domChats.style.display = 'none';
    domGroupOverview.style.display = 'none';
    domChat.style.display = '';
    domSettingsBtn.style.display = 'none';

    // Hide the Navbar
    domNavbar.style.display = `none`;

    // Warm up GIF server connection early (non-blocking)
    preconnectGifServer();

    // Get the chat (could be DM or Group)
    const chat = arrChats.find(c => c.id === contact);
    const isGroup = chat?.chat_type === 'MlsGroup';
    const profile = !isGroup ? getProfile(contact) : null;
    strOpenChat = contact;
    
    // Queue profile sync for DMs (on-demand refresh when opening)
    if (!isGroup && contact) {
        invoke('queue_profile_sync', {
            npub: contact,
            priority: 'high',
            forceRefresh: false
        }).catch(err => console.error('Failed to queue DM profile sync:', err));
    }
    
    if (isGroup) { refreshGroupMemberCount(contact); }

    // Clear any existing auto-scroll timer
    if (chatOpenAutoScrollTimer) {
        clearTimeout(chatOpenAutoScrollTimer);
        chatOpenAutoScrollTimer = null;
    }

    // Record when the chat was opened
    chatOpenTimestamp = Date.now();

    // After 100ms, stop auto-scrolling on media loads
    chatOpenAutoScrollTimer = setTimeout(() => {
        chatOpenTimestamp = 0; // Reset timestamp to disable auto-scrolling
        chatOpenAutoScrollTimer = null;
    }, 100);

    // Load events from cache (on-demand loading)
    // This uses the LRU event cache for efficient memory management
    // Load events from cache (will fetch from DB if not cached)
    const initialMessages = await eventCache.loadInitialEvents(
        contact,
        proceduralScrollState.messagesPerBatch
    );

    // Load PIVX payments for this chat and merge them into messages
    try {
        const pivxPayments = await invoke('pivx_get_chat_payments', { conversationId: contact });
        if (pivxPayments && pivxPayments.length > 0) {
            // Convert PIVX payments to message format with pivx_payment property
            for (const payment of pivxPayments) {
                // Check if this payment already exists in messages
                const existing = initialMessages.find(m => m.id === payment.message_id);
                if (!existing) {
                    const paymentMsg = {
                        id: payment.message_id,
                        at: payment.at,
                        content: '',
                        mine: payment.is_mine,
                        attachments: [],
                        npub: payment.sender,
                        pivx_payment: {
                            gift_code: payment.gift_code,
                            amount_piv: payment.amount_piv,
                            address: payment.address,
                            message: payment.message
                        }
                    };
                    // Add to cache (which also adds to initialMessages since they share the same array reference)
                    eventCache.addEvent(contact, paymentMsg);
                }
            }
            // Re-sort by timestamp after adding PIVX payments
            initialMessages.sort((a, b) => a.at - b.at);
        }
    } catch (e) {
        console.warn('Failed to load PIVX payments:', e);
    }

    // Load system events (member joined/left, etc.) for this chat and merge them
    try {
        const systemEvents = await invoke('get_system_events', { conversationId: contact });
        if (systemEvents && systemEvents.length > 0) {
            for (const event of systemEvents) {
                // Check if this event already exists in messages
                const existing = initialMessages.find(m => m.id === event.id);
                if (!existing) {
                    const systemMsg = {
                        id: event.id,
                        at: event.at,
                        content: event.content,
                        mine: false,
                        attachments: [],
                        system_event: {
                            event_type: event.event_type,
                            member_npub: event.member_npub,
                        }
                    };
                    // Add to cache (which also adds to initialMessages since they share the same array reference)
                    eventCache.addEvent(contact, systemMsg);
                }
            }
            // Re-sort by timestamp after adding system events
            initialMessages.sort((a, b) => a.at - b.at);
        }
    } catch (e) {
        console.warn('Failed to load system events:', e);
    }

    // Get cache stats for procedural scroll
    const cacheStats = eventCache.getStats(contact);
    const totalMessages = cacheStats?.totalInDb || initialMessages.length;

    // Update the chat object's messages array for compatibility
    // (Some parts of the code still reference chat.messages)
    if (chat) {
        chat.messages = initialMessages;
    }

    // Initialize procedural scroll state with actual counts
    initProceduralScrollWithCache(contact, initialMessages.length, totalMessages);
    
    updateChat(chat, initialMessages, profile, true);

    // If the opened chat has messages, mark them as read (last message)
    if (initialMessages) {
        const lastMsg = initialMessages[initialMessages.length - 1];
        markAsRead(chat, lastMsg);
    }
    
    // Update the back button notification dot
    updateChatBackNotification();

    // Focus chat input on desktop (mobile keyboards are intrusive)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }
}

/**
 * Open the dialog for starting a new chat
 */
function openNewChat() {
    // Display the UI
    domChatNew.style.display = '';
    domChats.style.display = 'none';
    domChat.style.display = 'none';

    // Hide the Navbar
    domNavbar.style.display = 'none';
}

/**
 * Closes the current chat, taking the user back to the chat list
 */
async function closeChat() {
    // Clear any auto-scroll timer
    if (chatOpenAutoScrollTimer) {
        clearTimeout(chatOpenAutoScrollTimer);
        chatOpenAutoScrollTimer = null;
    }

    // Attempt to completely release memory (force garbage collection...) of in-chat media
    while (domChatMessages.firstElementChild) {
        const domChild = domChatMessages.firstElementChild;

        // For media (images, audio, video); we ensure they're fully unloaded
        const domMedias = domChild?.querySelectorAll('img, audio, video');
        for (const domMedia of domMedias) {
            // Streamable media (audio + video) should be paused, then force-unloaded
            if (domMedia instanceof HTMLMediaElement) {
                domMedia.pause();
                
                // For Android blob URLs, revoke them before clearing
                if (platformFeatures.os === 'android' && domMedia.src.startsWith('blob:')) {
                    URL.revokeObjectURL(domMedia.src);
                }
                
                domMedia.removeAttribute('src'); // Better than setting to empty string
                domMedia.load();
            }
            // Static media (images) should simply be unloaded
            if (domMedia instanceof HTMLImageElement) {
                // Also check for blob URLs on images if you use them
                if (domMedia.src.startsWith('blob:')) {
                    URL.revokeObjectURL(domMedia.src);
                }
                domMedia.removeAttribute('src');
            }
        }

        // Now we explicitly drop them
        domChild.remove();
    }

    // If the chat had any messages, mark them as read before leaving
    if (strOpenChat) {
        const closedChat = arrChats.find(c => c.id === strOpenChat);
        if (closedChat?.messages?.length) {
            const lastMsg = closedChat.messages[closedChat.messages.length - 1];
            markAsRead(closedChat, lastMsg);
        }
    }

    // Trim the event cache for this chat to free memory
    // (keeps max 100 events, removes older ones loaded during scroll)
    if (strOpenChat) {
        eventCache.trimConversation(strOpenChat);
    }

    // Reset the chat UI
    domProfile.style.display = 'none';
    domGroupOverview.style.display = 'none';
    domSettingsBtn.style.display = '';
    domChatNew.style.display = 'none';
    domChat.style.display = 'none';
    strOpenChat = "";
    previousChatBeforeProfile = ""; // Clear when closing chat
    nLastTypingIndicator = 0;
    
    // Clear the chat header to prevent flicker when opening next chat
    domChatContact.textContent = '';
    domChatContactStatus.textContent = '';
    domChatContactStatus.classList.add('status-hidden');
    domChatContactStatus.classList.remove('text-gradient');
    domChatHeaderAvatarContainer.innerHTML = '';
    
    // Reset procedural scroll state
    resetProceduralScroll();
    
    // Hide the back button notification dot when closing chat
    updateChatBackNotification();

    // Display the Navbar
    domNavbar.style.display = ``;

    // Cancel any ongoing replies or selections
    strCurrentReactionReference = "";
    strCurrentReplyReference = "";
    cancelReply();

    // Navigate back to chat list with animation
    await openChatlist();

    // Update the Chat List
    renderChatlist();

    // Ensure the chat list re-adjusts to fit
    adjustSize();
}

/**
 * Open the Expanded Profile view, optionally with a non-default profile
 * @param {Profile} cProfile - An optional profile to render
 */
async function openProfile(cProfile) {
    navbarSelect('profile-btn');
    domChats.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';
    domGroupOverview.style.display = 'none';
    domChat.style.display = 'none'; // Hide the chat view when opening profile
    domSettingsBtn.style.display = ''; // Ensure settings button is visible (may have been hidden by openChat)

    // Render our own profile by default, but otherwise; the given one
    if (!cProfile) {
        cProfile = arrProfiles.find(a => a.mine);
        // Clear previous chat when opening our own profile from navbar
        previousChatBeforeProfile = '';
    }

    // Force immediate refresh when user views profile
    if (cProfile && cProfile.id) {
        invoke("refresh_profile_now", { npub: cProfile.id });

        // Start periodic refresh while viewing this profile (every 30 seconds)
        clearInterval(profileRefreshInterval);
        profileRefreshInterval = setInterval(() => {
            // Only refresh if profile tab is still open
            if (domProfile.style.display === '') {
                invoke("refresh_profile_now", { npub: cProfile.id });
            } else {
                // Profile tab closed, stop refreshing
                clearInterval(profileRefreshInterval);
                profileRefreshInterval = null;
            }
        }, 30000);
    }

    renderProfileTab(cProfile);

    if (domProfile.style.display !== '') {
        // Run a subtle fade-in animation
        domProfile.classList.add('fadein-subtle-anim');
        domProfile.addEventListener('animationend', () => domProfile.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domProfile.style.display = '';
    }
}

/**
 * Open the Group Overview view for a specific group chat
 * @param {Chat} chat - The group chat object
 */
async function openGroupOverview(chat) {
    if (!chat || chat.chat_type !== 'MlsGroup') return;
    
    navbarSelect('chat-btn');
    domChats.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';
    domProfile.style.display = 'none';
    domChat.style.display = 'none';

    // Store which group is being viewed
    domGroupOverview.setAttribute('data-group-id', chat.id);

    // Render the group overview
    await renderGroupOverview(chat);

    if (domGroupOverview.style.display !== '') {
        // Run a subtle fade-in animation
        domGroupOverview.classList.add('fadein-subtle-anim');
        domGroupOverview.addEventListener('animationend', () => domGroupOverview.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domGroupOverview.style.display = '';
    }
}

/**
 * Render the Group Overview tab based on a given group chat
 * @param {Chat} chat - The group chat object
 */
async function renderGroupOverview(chat) {
    const groupName = chat.metadata?.custom_fields?.name || `Group ${chat.id.substring(0, 10)}...`;
    
    // Fetch fresh member list and admins from the engine
    let members = [];
    let admins = [];
    
    try {
        const result = await invoke('get_mls_group_members', { groupId: chat.id });
        // The result is an object with 'members' and 'admins' array properties
        members = result?.members || [];
        admins = result?.admins || [];
    } catch (e) {
        console.error('Failed to fetch group members:');
        console.error(e);
    }
    
    // Use actual member count from engine, not cached metadata
    const memberCount = members.length;
    
    // Display Name (top header)
    domGroupOverviewName.innerHTML = groupName;
    domGroupOverviewStatus.textContent = `${memberCount} ${memberCount === 1 ? 'member' : 'members'}`;
    
    // Secondary name
    domGroupOverviewNameSecondary.innerHTML = groupName;
    
    // Group description (if available)
    const description = chat.metadata?.custom_fields?.description;
    if (description) {
        domGroupOverviewDescription.textContent = description;
        domGroupOverviewDescription.style.display = '';
    } else {
        domGroupOverviewDescription.style.display = 'none';
    }
    
    // Function to render the member list (can be called for search filtering)
    const renderMemberList = (searchQuery = '') => {
        domGroupOverviewMembers.innerHTML = '';
        
        if (!members || members.length === 0) {
            const noMembers = document.createElement('p');
            noMembers.textContent = 'No members found';
            noMembers.style.textAlign = 'center';
            noMembers.style.color = '#999';
            noMembers.style.padding = '20px';
            domGroupOverviewMembers.appendChild(noMembers);
            return;
        }
        
        // Sort members: admins first, then regular members
        const sortedMembers = [...members].sort((a, b) => {
            const aIsAdmin = admins.includes(a);
            const bIsAdmin = admins.includes(b);
            if (aIsAdmin && !bIsAdmin) return -1;
            if (!aIsAdmin && bIsAdmin) return 1;
            return 0;
        });
        
        // Filter members based on search query
        const filteredMembers = sortedMembers.filter(member => {
            if (!searchQuery) return true;
            
            const memberProfile = getProfile(member);
            const query = searchQuery.toLowerCase();
            
            // Search in nickname, name, and npub
            const nickname = (memberProfile?.nickname || '').toLowerCase();
            const name = (memberProfile?.name || '').toLowerCase();
            const npub = member.toLowerCase();
            
            return nickname.includes(query) || name.includes(query) || npub.includes(query);
        });
        
        if (filteredMembers.length === 0) {
            const noResults = document.createElement('p');
            noResults.textContent = 'No members match your search';
            noResults.style.textAlign = 'center';
            noResults.style.color = '#999';
            noResults.style.padding = '20px';
            domGroupOverviewMembers.appendChild(noResults);
            return;
        }
        
        for (const member of filteredMembers) {
            const isAdmin = admins.includes(member);
            const memberDiv = document.createElement('div');
            memberDiv.style.display = 'flex';
            memberDiv.style.alignItems = 'center';
            memberDiv.style.padding = '5px 10px';
            memberDiv.style.borderRadius = '6px';
            memberDiv.style.transition = 'background 0.2s ease';
            memberDiv.style.isolation = 'isolate';
            memberDiv.style.cursor = 'pointer';
            
            // Add hover effect with theme-based gradient using ::before pseudo-element approach
            const bgDiv = document.createElement('div');
            bgDiv.style.position = 'absolute';
            bgDiv.style.top = '0';
            bgDiv.style.left = '0';
            bgDiv.style.right = '0';
            bgDiv.style.bottom = '0';
            bgDiv.style.borderRadius = '6px';
            bgDiv.style.opacity = '0';
            bgDiv.style.transition = 'opacity 0.2s ease';
            bgDiv.style.pointerEvents = 'none';
            bgDiv.style.zIndex = '0';
            
            memberDiv.style.position = 'relative';
            memberDiv.appendChild(bgDiv);
            
            memberDiv.addEventListener('mouseenter', () => {
                const primaryColor = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
                bgDiv.style.background = `linear-gradient(to right, ${primaryColor}40, transparent)`;
                bgDiv.style.opacity = '1';
            });
            memberDiv.addEventListener('mouseleave', () => {
                bgDiv.style.opacity = '0';
            });
            
            // Get member profile
            const memberProfile = getProfile(member);
            if (!memberProfile && member) {
                invoke("queue_profile_sync", {
                    npub: member,
                    priority: "normal",
                    forceRefresh: false
                }).catch(console.error);
            }
            const openMemberProfile = () => {
                const profile = getProfile(member) || memberProfile || { id: member, mine: false };
                const originChatId = domGroupOverview.getAttribute('data-group-id') || strOpenChat || chat.id;
                previousChatBeforeProfile = originChatId;
                openProfile(profile);
            };
            memberDiv.onclick = openMemberProfile;
            
            // Crown icon for admins (or invisible spacer for alignment)
            const crownContainer = document.createElement('span');
            crownContainer.style.width = '20px';
            crownContainer.style.height = '25px';
            crownContainer.style.display = 'inline-flex';
            crownContainer.style.alignItems = 'center';
            crownContainer.style.justifyContent = 'center';
            crownContainer.style.marginRight = '5px';
            crownContainer.style.position = 'relative';
            crownContainer.style.zIndex = '1';
            if (isAdmin) {
                crownContainer.innerHTML = '<span class="icon icon-crown" style="width: 16px; height: 16px; background-color: #fce459;"></span>';
            }
            memberDiv.appendChild(crownContainer);
            
            // Member avatar
            let avatar;
            const memberAvatarSrc = getProfileAvatarSrc(memberProfile);
            avatar = createAvatarImg(memberAvatarSrc, 25, false);
            avatar.style.marginRight = '10px';
            avatar.style.position = 'relative';
            avatar.style.zIndex = '1';
            memberDiv.appendChild(avatar);
            
            // Member name
            const nameSpan = document.createElement('div');
            nameSpan.className = 'compact-member-name';
            nameSpan.textContent = memberProfile?.nickname || memberProfile?.name || member.substring(0, 10) + '...';
            nameSpan.style.color = '#f7f4f4';
            nameSpan.style.fontSize = '14px';
            nameSpan.style.flex = '1';
            nameSpan.style.textAlign = 'left';
            nameSpan.style.position = 'relative';
            nameSpan.style.zIndex = '1';
            if (memberProfile?.nickname || memberProfile?.name) twemojify(nameSpan);
            memberDiv.appendChild(nameSpan);
            
            // Kick button (only visible to admins, and not for themselves)
            const isMe = member === strPubkey;
            const iAmAdmin = admins.includes(strPubkey);
            if (iAmAdmin && !isMe) {
                const kickBtn = document.createElement('button');
                kickBtn.textContent = 'Kick';
                kickBtn.style.padding = '4px 12px';
                kickBtn.style.fontSize = '12px';
                kickBtn.style.borderRadius = '4px';
                kickBtn.style.border = 'none';
                kickBtn.style.background = '#ff4444';
                kickBtn.style.color = 'white';
                kickBtn.style.cursor = 'pointer';
                kickBtn.style.transition = 'background 0.2s ease';
                kickBtn.style.position = 'relative';
                kickBtn.style.zIndex = '1';
                kickBtn.style.marginLeft = '10px';
                
                kickBtn.addEventListener('mouseenter', () => {
                    kickBtn.style.background = '#ff6666';
                });
                kickBtn.addEventListener('mouseleave', () => {
                    kickBtn.style.background = '#ff4444';
                });
                
                kickBtn.onclick = async (e) => {
                    e.stopPropagation();
                    
                    // Prevent double-clicks
                    if (kickBtn.disabled) return;
                    
                    const memberName = memberProfile?.nickname || memberProfile?.name || member.substring(0, 10) + '...';
                    const confirmed = await popupConfirm(
                        `Remove ${memberName} from the group?`,
                        'This will remove them from the group immediately.'
                    );
                    
                    if (!confirmed) return;
                    
                    // Disable button and show loading state
                    kickBtn.disabled = true;
                    kickBtn.style.opacity = '0.5';
                    kickBtn.style.cursor = 'not-allowed';
                    const originalText = kickBtn.textContent;
                    kickBtn.textContent = 'Removing...';
                    
                    try {
                        // Call the remove_mls_member_device command
                        // We don't have device_id, so we pass an empty string (backend will handle it)
                        await window.__TAURI__.core.invoke('remove_mls_member_device', {
                            groupId: chat.id,
                            memberNpub: member,
                            deviceId: ''
                        });
                        
                        console.log(`[MLS] Successfully kicked member: ${member}`);
                        
                        // The mls_group_updated event will trigger a refresh
                        // But we can also manually refresh the overview
                        setTimeout(async () => {
                            await renderGroupOverview(chat);
                        }, 500);
                    } catch (error) {
                        console.error('[MLS] Failed to kick member:', error);
                        alert(`Failed to remove member: ${error}`);
                        
                        // Re-enable button on error
                        kickBtn.disabled = false;
                        kickBtn.style.opacity = '1';
                        kickBtn.style.cursor = 'pointer';
                        kickBtn.textContent = originalText;
                    }
                };
                
                memberDiv.appendChild(kickBtn);
            }
            
            domGroupOverviewMembers.appendChild(memberDiv);
        }
    };
    
    // Initial render of member list
    renderMemberList();
    
    // Add search functionality
    domGroupMemberSearchInput.value = '';
    domGroupMemberSearchInput.oninput = (e) => {
        renderMemberList(e.target.value);
    };
    
    // Check if current user is an admin to show/hide invite button
    const myProfile = arrProfiles.find(p => p.mine);
    const isAdmin = myProfile && admins.includes(myProfile.id);
    
    if (isAdmin) {
        domGroupInviteMemberBtn.style.display = 'flex';
        // Invite Member button - open member selection UI
        domGroupInviteMemberBtn.onclick = async () => {
            await openInviteMemberToGroup(chat);
        };
    } else {
        domGroupInviteMemberBtn.style.display = 'none';
    }
    
    // Leave Group button
    domGroupLeaveBtn.style.display = 'flex';
    domGroupLeaveBtn.onclick = async () => {
        const groupName = chat.metadata?.custom_fields?.name || `Group ${chat.id.substring(0, 10)}...`;

        // Check if user is the group creator
        const isCreator = myProfile && myProfile.id === chat.metadata?.creator_pubkey;

        // Creators cannot leave unless they are the only member
        if (isCreator && memberCount > 1) {
            await popupConfirm(
                'Cannot Leave Group',
                'You are the creator of this group. You must remove all other members before you can leave.<br><br>Please kick all members first, then you can leave the group.',
                true, // Notice only, no cancel button
                '', // No input
                'vector_warning.svg'
            );
            return;
        }

        // Confirm before leaving using popupConfirm
        const confirmed = await popupConfirm(
            'Leave Group',
            `Are you sure you want to leave "<b>${groupName}</b>"?<br><br>You will need to be re-invited to rejoin.`,
            false, // Not a notice, show cancel button
            '', // No input
            'vector_warning.svg'
        );

        if (!confirmed) return;

        try {
            await invoke('leave_mls_group', { groupId: chat.id });

            // Close the group overview
            domGroupOverview.style.display = 'none';
            domGroupOverview.removeAttribute('data-group-id');

            // Open the chat list
            openChatlist();

            // The group will be removed from the chat list via the mls_group_left event
        } catch (error) {
            console.error('Failed to leave group:', error);
            await popupConfirm('Failed to Leave Group', error.toString(), true, '', 'vector_warning.svg');
        }
    };
    
    // Back button - return to the group chat
    domGroupOverviewBackBtn.onclick = () => {
        domGroupOverview.style.display = 'none';
        domGroupOverview.removeAttribute('data-group-id');
        openChat(chat.id);
    };
}

/**
 * Open the invite member UI for a specific group
 * @param {Chat} chat - The group chat object
 */
async function openInviteMemberToGroup(chat) {
    // Get current group members to exclude them from selection
    let currentMembers = [];
    try {
        const result = await invoke('get_mls_group_members', { groupId: chat.id });
        currentMembers = result?.members || [];
    } catch (e) {
        console.error('Failed to fetch group members:', e);
    }
    
    // Create a modal/popup for member selection
    const modal = document.createElement('div');
    modal.style.position = 'fixed';
    modal.style.top = '0';
    modal.style.left = '0';
    modal.style.right = '0';
    modal.style.bottom = '0';
    modal.style.backgroundColor = 'rgba(0, 0, 0, 0.8)';
    modal.style.display = 'flex';
    modal.style.alignItems = 'center';
    modal.style.justifyContent = 'center';
    modal.style.zIndex = '10000';
    modal.style.padding = '20px';
    
    const container = document.createElement('div');
    container.style.backgroundColor = '#0a0a0a';
    container.style.borderRadius = '12px';
    container.style.padding = '24px';
    container.style.maxWidth = '500px';
    container.style.width = '100%';
    container.style.maxHeight = '80vh';
    container.style.display = 'flex';
    container.style.flexDirection = 'column';
    container.style.borderStyle = 'solid';
    container.style.borderColor = '#1c1c1c';
    container.style.borderWidth = '1px';
    
    // Header
    const header = document.createElement('div');
    header.style.display = 'flex';
    header.style.justifyContent = 'space-between';
    header.style.alignItems = 'center';
    header.style.marginBottom = '20px';
    
    const title = document.createElement('h3');
    title.textContent = 'Invite Member';
    title.style.margin = '0';
    title.style.color = '#f7f4f4';
    header.appendChild(title);
    
    const closeBtn = document.createElement('button');
    closeBtn.textContent = '✕';
    closeBtn.className = 'btn';
    closeBtn.style.padding = '8px 12px';
    closeBtn.style.fontSize = '18px';
    closeBtn.onclick = () => modal.remove();
    header.appendChild(closeBtn);
    
    container.appendChild(header);
    
    // Search input
    const searchContainer = document.createElement('div');
    searchContainer.className = 'emoji-search-container';
    searchContainer.style.padding = '10px 0px';
    searchContainer.style.marginBottom = '16px';
    searchContainer.style.background = 'transparent';
    
    const searchIcon = document.createElement('span');
    searchIcon.className = 'emoji-search-icon icon icon-search';
    searchIcon.style.setProperty('width', '25px', 'important');
    searchIcon.style.setProperty('height', '25px', 'important');
    searchContainer.appendChild(searchIcon);
    
    const searchInput = document.createElement('input');
    searchInput.placeholder = 'Search contacts...';
    searchInput.style.padding = '10px 40px';
    searchInput.style.backgroundColor = 'transparent';
    searchInput.style.border = '1px solid rgba(57, 57, 57, 0.5)';
    searchInput.style.width = '100%';
    searchContainer.appendChild(searchInput);
    
    container.appendChild(searchContainer);
    
    // Member list (matching the group overview member list style)
    const memberList = document.createElement('div');
    memberList.style.flex = '1';
    memberList.style.overflowY = 'auto';
    memberList.style.marginBottom = '16px';
    memberList.style.border = '1px solid rgba(57, 57, 57, 0.5)';
    memberList.style.borderRadius = '8px';
    memberList.style.padding = '6px';
    
    // Status message
    const statusMsg = document.createElement('p');
    statusMsg.style.textAlign = 'center';
    statusMsg.style.color = '#999';
    statusMsg.style.margin = '10px 0';
    statusMsg.style.display = 'none';
    container.appendChild(statusMsg);
    
    // Invite button
    const inviteBtn = document.createElement('button');
    inviteBtn.textContent = 'Invite';
    inviteBtn.className = 'btn';
    inviteBtn.style.padding = '12px 24px';
    inviteBtn.style.background = 'linear-gradient(135deg, var(--icon-color-primary), var(--icon-color-secondary))';
    inviteBtn.style.borderRadius = '6px';
    inviteBtn.style.fontWeight = '500';
    inviteBtn.style.width = '100%';
    inviteBtn.disabled = true;
    inviteBtn.style.opacity = '0.5';
    
    let selectedMember = null;
    
    const renderMemberList = (filterText = '') => {
        memberList.innerHTML = '';
        const filter = filterText.toLowerCase();
        
        // Get mine profile to exclude self
        const mine = arrProfiles.find(p => p.mine)?.id;
        
        // Filter available contacts (exclude self and current members)
        const availableContacts = arrProfiles.filter(p => {
            if (!p || !p.id || p.id === mine) return false;
            if (currentMembers.includes(p.id)) return false;
            
            if (filter) {
                const name = (p.nickname || p.name || '').toLowerCase();
                const npub = p.id.toLowerCase();
                return name.includes(filter) || npub.includes(filter);
            }
            return true;
        });
        
        if (availableContacts.length === 0) {
            const empty = document.createElement('p');
            empty.textContent = filter ? 'No matches' : 'No contacts available to invite';
            empty.style.textAlign = 'center';
            empty.style.color = '#999';
            empty.style.padding = '20px';
            memberList.appendChild(empty);
            return;
        }
        
        for (const contact of availableContacts) {
            const contactProfile = getProfile(contact.id);
            const name = contactProfile?.nickname || contactProfile?.name || '';
            
            const row = document.createElement('div');
            row.style.display = 'flex';
            row.style.alignItems = 'center';
            row.style.padding = '5px 10px';
            row.style.borderRadius = '6px';
            row.style.transition = 'background 0.2s ease';
            row.style.isolation = 'isolate';
            row.style.cursor = 'pointer';
            row.style.position = 'relative';
            
            // Add hover effect with theme-based gradient
            const bgDiv = document.createElement('div');
            bgDiv.style.position = 'absolute';
            bgDiv.style.top = '0';
            bgDiv.style.left = '0';
            bgDiv.style.right = '0';
            bgDiv.style.bottom = '0';
            bgDiv.style.borderRadius = '6px';
            bgDiv.style.opacity = '0';
            bgDiv.style.transition = 'opacity 0.2s ease';
            bgDiv.style.pointerEvents = 'none';
            bgDiv.style.zIndex = '0';
            row.appendChild(bgDiv);
            
            row.addEventListener('mouseenter', () => {
                const primaryColor = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
                bgDiv.style.background = `linear-gradient(to right, ${primaryColor}40, transparent)`;
                bgDiv.style.opacity = '1';
            });
            row.addEventListener('mouseleave', () => {
                bgDiv.style.opacity = '0';
            });
            
            // Avatar (compact size like member list)
            let avatar;
            const contactAvatarSrc = getProfileAvatarSrc(contactProfile);
            avatar = createAvatarImg(contactAvatarSrc, 25, false);
            avatar.style.marginRight = '10px';
            avatar.style.position = 'relative';
            avatar.style.zIndex = '1';
            row.appendChild(avatar);
            
            // Name
            const nameSpan = document.createElement('div');
            nameSpan.className = 'compact-member-name';
            nameSpan.textContent = name || contact.id.substring(0, 10) + '...';
            nameSpan.style.color = '#f7f4f4';
            nameSpan.style.fontSize = '14px';
            nameSpan.style.flex = '1';
            nameSpan.style.textAlign = 'left';
            nameSpan.style.position = 'relative';
            nameSpan.style.zIndex = '1';
            if (name) twemojify(nameSpan);
            row.appendChild(nameSpan);
            
            // Selection indicator (right-aligned)
            const indicator = document.createElement('div');
            indicator.style.width = '18px';
            indicator.style.height = '18px';
            indicator.style.borderRadius = '50%';
            indicator.style.border = '2px solid var(--icon-color-primary)';
            indicator.style.position = 'relative';
            indicator.style.zIndex = '1';
            indicator.style.flexShrink = '0';
            row.appendChild(indicator);
            
            row.onclick = () => {
                // Deselect previous
                memberList.querySelectorAll('div[data-contact-row]').forEach(r => {
                    const ind = r.querySelector('div:last-child');
                    if (ind) ind.style.backgroundColor = '';
                });
                
                // Select this one
                indicator.style.backgroundColor = 'var(--icon-color-primary)';
                selectedMember = contact.id;
                inviteBtn.disabled = false;
                inviteBtn.style.opacity = '1';
            };
            
            row.setAttribute('data-contact-row', 'true');
            memberList.appendChild(row);
        }
    };
    
    renderMemberList();
    searchInput.oninput = (e) => renderMemberList(e.target.value);
    
    inviteBtn.onclick = async () => {
        if (!selectedMember) return;
        
        inviteBtn.disabled = true;
        inviteBtn.textContent = 'Inviting...';
        statusMsg.style.display = '';
        statusMsg.style.color = '#999';
        statusMsg.textContent = 'Preparing invitation...';
        
        try {
            await invoke('invite_member_to_group', {
                groupId: chat.id,
                memberNpub: selectedMember
            });
            
            statusMsg.style.color = '#4caf50';
            statusMsg.textContent = 'Member invited successfully!';
            
            // Refresh the group overview
            setTimeout(async () => {
                modal.remove();
                await renderGroupOverview(chat);
            }, 1000);
        } catch (e) {
            const errorMsg = (e || '').toString();
            let friendlyMsg = errorMsg;
            
            // Map backend errors to friendly messages
            if (errorMsg.includes('no device keypackag')) {
                const match = errorMsg.match(/for (\S+)/);
                if (match) {
                    const npub = match[1];
                    const prof = arrProfiles.find(p => p.id === npub);
                    const display = prof?.nickname || prof?.name || 'This user';
                    friendlyMsg = `${display} is using an older Vector version! Please ask them to upgrade.`;
                }
            }
            
            statusMsg.style.color = '#f44336';
            statusMsg.textContent = friendlyMsg;
            inviteBtn.disabled = false;
            inviteBtn.textContent = 'Invite';
            
            // Error is already displayed in the modal, no need for popup
        }
    };
    
    container.appendChild(memberList);
    container.appendChild(inviteBtn);
    
    modal.appendChild(container);
    document.body.appendChild(modal);
    
    // Close on background click
    modal.onclick = (e) => {
        if (e.target === modal) modal.remove();
    };
}

async function openChatlist() {
    navbarSelect('chat-btn');
    domProfile.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';
    domGroupOverview.style.display = 'none';
    previousChatBeforeProfile = ""; // Clear when navigating away

    if (domChats.style.display !== '') {
        // Run a subtle fade-in animation
        domChats.classList.add('fadein-subtle-anim');
        domChats.addEventListener('animationend', () => domChats.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domChats.style.display = '';
    }
    
    // Load and display MLS invites in the Chat tab (and adjust layout before/after for consistency)
    adjustSize();
    await loadMLSInvites();
    adjustSize();

    // Refresh timestamps immediately so they're not stale after viewing a chat
    updateChatlistTimestamps();
}

function openSettings() {
    navbarSelect('settings-btn');
    domSettings.style.display = '';

    // Hide the other tabs
    domProfile.style.display = 'none';
    domChats.style.display = 'none';
    domInvites.style.display = 'none';
    domGroupOverview.style.display = 'none';
    previousChatBeforeProfile = ""; // Clear when navigating away

    // Update the Storage Breakdown
    initStorageSection();

    // Show/hide Restore PIVX Wallet button based on hidden state
    const pivxHidden = localStorage.getItem('pivx_hidden') === 'true';
    if (domRestorePivxGroup) {
        domRestorePivxGroup.style.display = pivxHidden ? '' : 'none';
    }

    // Check primary device status when settings are opened
    checkPrimaryDeviceStatus();

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
    navbarSelect('invites-btn');
    domInvites.style.display = '';

    // Hide the other tabs
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
 * Edit the profile description inline
 */
function editProfileDescription() {
    // Get the current profile
    const cProfile = arrProfiles.find(a => a.mine);
    if (!cProfile) return;

    // Set the textarea content to current description
    domProfileDescriptionEditor.value = cProfile.about || '';

    // Hide the span and show the textarea
    domProfileDescription.style.display = 'none';
    domProfileDescriptionEditor.style.display = '';

    // Focus the text
    domProfileDescriptionEditor.focus();

    // Handle blur event to save and return to view mode
    domProfileDescriptionEditor.onblur = () => {
        // Hide textarea and show span
        domProfileDescriptionEditor.style.display = 'none';
        domProfileDescription.style.display = '';

        // Remove the blur event listener
        domProfileDescriptionEditor.onblur = null;

        // If nothing was edited, don't change anything
        if (!domProfileDescriptionEditor.value ||
            domProfileDescriptionEditor.value === cProfile.about
        ) return;

        // Update the profile's about property
        cProfile.about = domProfileDescriptionEditor.value;

        // Update the span content
        domProfileDescription.textContent = cProfile.about;
        twemojify(domProfileDescription);

        // Upload new About Me to Nostr
        setAboutMe(cProfile.about);
    };

    // Resize it to match the content size (CSS cannot scale textareas based on content)
    domProfileDescriptionEditor.style.height = Math.min(domProfileDescriptionEditor.scrollHeight, 100) + 'px';

    // Handle input events to resize the textarea dynamically
    domProfileDescriptionEditor.oninput = () => {
        domProfileDescriptionEditor.style.height = Math.min(domProfileDescriptionEditor.scrollHeight, 100) + 'px';
    };

    // Handle Enter key to submit (excluding Shift+Enter for line breaks)
    domProfileDescriptionEditor.onkeydown = (evt) => {
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            domProfileDescriptionEditor.blur(); // Trigger the blur event to save
        }
    };
}

/**
 * A utility to "select" one Navbar item, deselecting the rest automatically.
 */
function navbarSelect(strSelectionID = '') {
    for (const navItem of domNavbar.querySelectorAll('div')) {
        if (strSelectionID === navItem.id) navItem.classList.remove('navbar-btn-inactive');
        else navItem.classList.add('navbar-btn-inactive');
    }
}

/**
 * Our Bech32 Nostr Public Key
 */
let strPubkey;

/**
 * The timestamp we sent our last typing indicator
 * 
 * Ensure this is wiped when the chat is closed!
 */
let nLastTypingIndicator = 0;

const strOriginalInputPlaceholder = domChatMessageInput.getAttribute('placeholder');

/**
 * Auto-resize the chat input textarea based on content.
 * Expands up to max-height defined in CSS (150px), then scrolls.
 * Only expands when content actually needs more space (multi-line).
 */
function autoResizeChatInput() {
    // Get actual computed styles
    const computed = window.getComputedStyle(domChatMessageInput);
    const lineHeight = parseFloat(computed.lineHeight) || 24;
    const paddingTop = parseFloat(computed.paddingTop) || 10;
    const paddingBottom = parseFloat(computed.paddingBottom) || 10;
    const padding = paddingTop + paddingBottom;
    
    // Single line scrollHeight = lineHeight + padding
    const singleLineScrollHeight = lineHeight + padding;
    
    // Track previous state for scroll adjustment
    const wasExpanded = domChatMessageInput.style.overflowY === 'auto';
    
    // Reset height and ensure overflow is hidden for accurate measurement
    // Setting overflow:hidden before measuring prevents scrollbar space from affecting layout
    domChatMessageInput.style.overflowY = 'hidden';
    domChatMessageInput.style.height = '0';
    
    // Get scrollHeight - this tells us how much space content actually needs
    const scrollHeight = domChatMessageInput.scrollHeight;
    
    // Only expand if content needs more than single line
    if (scrollHeight > singleLineScrollHeight) {
        // Set height to content needs minus padding (CSS height is content-box)
        domChatMessageInput.style.height = (scrollHeight - padding) + 'px';
        domChatMessageInput.style.overflowY = 'auto';
        
        // Soft scroll to keep chat at bottom when expanding
        softChatScroll();
    } else {
        // Single line - use default CSS height, keep overflow hidden
        domChatMessageInput.style.height = '';
        
        // If we just collapsed from multi-line, also soft scroll
        if (wasExpanded) {
            softChatScroll();
        }
    }
}

window.addEventListener("DOMContentLoaded", async () => {
    // Once login fade-in animation ends, remove it
    domLogin.addEventListener('animationend', () => domLogin.classList.remove('fadein-anim'), { once: true });

    // Fetch platform features to determine OS-specific behavior
    await fetchPlatformFeatures();

    // Initialize relay dialog event listeners
    initRelayDialogs();

    // Set up early deep link listener BEFORE login flow
    // This handles deep link events that arrive while the app is running
    // Note: Deep links received before JS loads are stored in Rust and retrieved after login
    await listen('deep_link_action', async (evt) => {
        // If user is not logged in yet (fInit is true), ignore - Rust already stored it
        if (fInit) {
            console.log('Deep link received before login, Rust backend has stored it');
            return;
        }
        
        // User is logged in, execute the action immediately
        await executeDeepLinkAction(evt.payload);
    });

    // Immediately load and apply theme settings (visual only, don't save)
    const strTheme = await invoke('get_theme');
    if (strTheme) {
        applyTheme(strTheme);
    }

    // Show the main window now that content is ready (prevents white flash on startup)
    // The window starts hidden via tauri.conf.json and Rust setup hides it explicitly
    // Only needed on desktop - mobile doesn't have this issue
    if (!platformFeatures.is_mobile) {
        try {
            await getCurrentWebviewWindow().show();
        } catch (e) {
            console.warn('Failed to show main window:', e);
        }
    }

    // [DEBUG MODE] Check if backend already has state from a previous session (hot-reload scenario)
    // This allows skipping the entire login/decrypt flow during development hot-reloads
    let fDebugHotReloaded = false;
    if (platformFeatures.debug_mode) {
        try {
            const hotReloadState = await invoke('debug_hot_reload_sync');
            if (hotReloadState && hotReloadState.success) {
                console.log('[Debug Hot-Reload] Backend state recovered, skipping login flow');
                
                // Hydrate frontend state from backend
                strPubkey = hotReloadState.npub;
                arrProfiles = hotReloadState.profiles || [];
                arrChats = hotReloadState.chats || [];
                
                // Setup Rust listeners
                await setupRustListeners();
                
                // Hydrate MLS group metadata
                await hydrateMLSGroupMetadata();
                
                // Hide login UI and show main UI
                domLogin.style.display = 'none';
                domLoginEncrypt.style.display = 'none';
                domNavbar.style.display = '';
                domChatBookmarksBtn.style.display = 'flex';
                
                // Render our profile
                const cProfile = arrProfiles.find(p => p.mine);
                renderCurrentProfile(cProfile);
                domAccount.style.display = '';
                
                // Mark init as complete so renderChatlist works
                fInit = false;
                
                // Render the chatlist
                renderChatlist();
                
                // Show the New Chat buttons (same as normal login flow)
                if (domChatNewDM) {
                    domChatNewDM.style.display = '';
                    domChatNewDM.onclick = openNewChat;
                }
                if (domChatNewGroup) {
                    domChatNewGroup.style.display = '';
                    domChatNewGroup.onclick = openCreateGroup;
                }
                
                // Adjust sizes
                adjustSize();
                
                // Update unread counter
                await invoke('update_unread_counter');
                
                // Monitor relay connections and render relay list
                invoke("monitor_relay_connections");
                renderRelayList();
                
                // Initialize the updater (version info, update button)
                initializeUpdater();
                
                console.log(`[Debug Hot-Reload] Restored ${arrProfiles.length} profiles, ${arrChats.length} chats`);
                
                // Mark as hot-reloaded so we skip the login flow but continue with button setup
                fDebugHotReloaded = true;
            }
        } catch (e) {
            // Backend not initialized - continue normal flow
            console.log('[Debug Hot-Reload] Backend not initialized, proceeding with normal login');
        }
    }

    // If a local account exists, boot up the decryption UI (skip if hot-reloaded)
    if (!fDebugHotReloaded && await hasAccount()) {
        // Account is available, login screen!
        openEncryptionFlow(null, true);
    }

    // Hook up our static buttons
    domInvitesBtn.onclick = openInvites;
    domProfileBtn.onclick = () => openProfile();
    domChatlistBtn.onclick = openChatlist;
    domSettingsBtn.onclick = openSettings;
    domLoginAccountCreationBtn.onclick = async () => {
        try {
            const { public: pubKey, private: privKey } = await invoke("create_account");
            strPubkey = pubKey;
            
            // Connect to Nostr network
            await invoke("connect");
            
            // Skip invite flow - go directly to encryption
            openEncryptionFlow(privKey, false);
        } catch (e) {
            // Display the backend error
            popupConfirm(e, '', true, '', 'vector_warning.svg');
        }
    };
    domLoginAccountBtn.onclick = () => {
        domLoginImport.style.display = '';
        domLoginStart.style.display = 'none';
    };
    domLoginBtn.onclick = async () => {
        // Import and derive our keys
        try {
            const { public: pubKey, private: privKey } = await invoke("login", { importKey: domLoginInput.value.trim() });
            strPubkey = pubKey;

            // Connect to Nostr
            await invoke("connect");

            // Skip invite flow - go directly to encryption
            openEncryptionFlow(privKey);
        } catch (e) {
            // Display the backend error
            popupConfirm(e, '', true, '', 'vector_warning.svg');
        }
    }
    domChatBackBtn.onclick = closeChat;
    domChatBookmarksBtn.onclick = () => {
        openChat(strPubkey);
    };
    domChatNewBackBtn.onclick = closeChat;
    
    // Add scroll event listener for procedural message loading
    let scrollTimeout;
    domChatMessages.addEventListener('scroll', () => {
        // Debounce scroll events for performance
        if (scrollTimeout) clearTimeout(scrollTimeout);
        scrollTimeout = setTimeout(() => {
            handleProceduralScroll();
        }, 100);
    });
    domChatNewStartBtn.onclick = () => {
        let inputValue = domChatNewInput.value.trim();
        // Parse npub from vectorapp.io profile URL if pasted
        const profileUrlMatch = inputValue.match(/https?:\/\/vectorapp\.io\/profile\/(npub1[a-z0-9]{58})/i);
        if (profileUrlMatch) {
            inputValue = profileUrlMatch[1];
        }
        openChat(inputValue);
        domChatNewInput.value = ``;
    };
    domChatNewInput.onkeydown = async (evt) => {
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            domChatNewStartBtn.click();
        }
    };
    domChatMessageInputCancel.onclick = () => {
        // Cancel edit mode if active, otherwise cancel reply
        if (strCurrentEditMessageId) {
            cancelEdit();
        } else {
            cancelReply();
        }
    };

    // Hook up a scroll handler in the chat to display UI elements at certain scroll depths
    createScrollHandler(domChatMessages, domChatMessagesScrollReturnBtn, { threshold: 500 })

    // Hook up an in-chat File Upload listener
    const isAndroid = platformFeatures.os === 'android';
    
    if (isAndroid) {
        // On Android, use a hidden file input to leverage WebView's built-in file picker
        // This handles content URI permissions correctly
        const androidFileInput = document.createElement('input');
        androidFileInput.type = 'file';
        androidFileInput.style.display = 'none';
        androidFileInput.accept = '*/*';
        document.body.appendChild(androidFileInput);
        
        androidFileInput.onchange = async (e) => {
            const file = e.target.files?.[0];
            if (file) {
                // Reset reply selection while passing a copy of the reference to the backend
                const strReplyRef = strCurrentReplyReference;
                cancelReply();
                
                const fileName = file.name;
                const ext = fileName.split('.').pop()?.toLowerCase() || '';
                
                // Open preview with the File object directly (more efficient)
                await openFilePreviewWithFile(file, fileName, ext, strOpenChat, strReplyRef);
            }
            // Reset the input so the same file can be selected again
            androidFileInput.value = '';
        };
        
        // Toggle attachment panel when clicking the add-file button
        domChatMessageInputFile.onclick = () => {
            toggleAttachmentPanel();
        };

        // Handle File button in attachment panel (Android)
        domAttachmentPanelFile.onclick = () => {
            closeAttachmentPanel();
            androidFileInput.click();
        };
    } else {
        // Toggle attachment panel when clicking the add-file button
        domChatMessageInputFile.onclick = () => {
            toggleAttachmentPanel();
        };

        // Handle File button in attachment panel (Desktop - use Tauri dialog)
        domAttachmentPanelFile.onclick = async () => {
            closeAttachmentPanel();
            let filepath = await selectFile();
            if (filepath) {
                // Reset reply selection while passing a copy of the reference to the backend
                const strReplyRef = strCurrentReplyReference;
                cancelReply();
                // Show file preview instead of sending directly
                await openFilePreview(filepath, strOpenChat, strReplyRef);
            }
        };
    }

    // Handle Mini Apps button in attachment panel - shows the Mini Apps list view
    domAttachmentPanelMiniApps.onclick = async () => {
        await showAttachmentPanelMiniApps();
    };

    // Handle Back button in Mini Apps view - returns to main attachment panel
    domAttachmentPanelBack.onclick = () => {
        showAttachmentPanelMain();
    };

    // Handle search input in Mini Apps view
    if (domMiniAppsSearch) {
        domMiniAppsSearch.addEventListener('input', (e) => {
            filterMiniApps(e.target.value);
        });
    }

    // Setup hold-to-edit mode for Mini Apps
    setupMiniAppsEditMode();

    // Mini App Launch Dialog event handlers
    domMiniAppLaunchCancel.onclick = closeMiniAppLaunchDialog;
    domMiniAppLaunchSolo.onclick = playMiniAppSolo;
    domMiniAppLaunchInvite.onclick = playMiniAppAndInvite;
    
    // Close dialog when clicking outside
    domMiniAppLaunchOverlay.onclick = (e) => {
        if (e.target === domMiniAppLaunchOverlay) {
            closeMiniAppLaunchDialog();
        }
    };

    // Marketplace event handlers
    if (domAttachmentPanelMarketplace) {
        domAttachmentPanelMarketplace.onclick = async () => {
            closeAttachmentPanel();
            showMarketplacePanel();
        };
    }

    if (domMarketplaceBackBtn) {
        domMarketplaceBackBtn.onclick = () => {
            hideMarketplacePanel();
        };
    }

    // PIVX Wallet event handlers
    if (domAttachmentPanelPivx) {
        domAttachmentPanelPivx.onclick = () => {
            showPivxWalletPanel();
        };
    }

    if (domAttachmentPanelPivxBack) {
        domAttachmentPanelPivxBack.onclick = () => {
            hidePivxWalletPanel();
        };
    }

    if (domPivxDepositBtn) {
        domPivxDepositBtn.onclick = () => {
            showPivxDepositDialog();
        };
    }

    if (domPivxSendBtn) {
        domPivxSendBtn.onclick = () => {
            showPivxSendDialog();
        };
    }

    if (domPivxSettingsBtn) {
        domPivxSettingsBtn.onclick = () => {
            showPivxSettingsDialog();
        };
    }

    // PIVX Withdraw button
    const domPivxWithdrawBtn = document.getElementById('pivx-withdraw-btn');
    if (domPivxWithdrawBtn) {
        domPivxWithdrawBtn.onclick = () => {
            showPivxWithdrawDialog();
        };
    }

    // PIVX Dialog close buttons
    document.getElementById('pivx-deposit-close')?.addEventListener('click', closePivxDepositDialog);
    document.getElementById('pivx-send-close')?.addEventListener('click', closePivxSendDialog);
    document.getElementById('pivx-withdraw-close')?.addEventListener('click', closePivxWithdrawDialog);
    document.getElementById('pivx-settings-close')?.addEventListener('click', closePivxSettingsDialog);

    // PIVX copy buttons
    document.getElementById('pivx-copy-address')?.addEventListener('click', () => {
        const address = document.getElementById('pivx-deposit-address')?.textContent;
        if (address) {
            navigator.clipboard.writeText(address);
            showToast('Address copied!');
        }
    });

    // PIVX send confirm
    document.getElementById('pivx-send-confirm')?.addEventListener('click', sendPivxPayment);

    // PIVX available balance click to prefill
    document.getElementById('pivx-send-available')?.addEventListener('click', () => {
        const amountEl = document.getElementById('pivx-send-amount');
        if (amountEl && pivxSendAvailableBalance > 0) {
            amountEl.value = pivxSendAvailableBalance.toFixed(2);
        }
    });

    // PIVX send mode toggles
    document.getElementById('pivx-send-custom-toggle')?.addEventListener('click', showPivxSendCustomMode);
    document.getElementById('pivx-send-back-toggle')?.addEventListener('click', showPivxSendQuickMode);
    document.getElementById('pivx-send-max')?.addEventListener('click', () => {
        const amountEl = document.getElementById('pivx-send-amount');
        if (amountEl && pivxSendAvailableBalance > 0) {
            amountEl.value = pivxSendAvailableBalance.toFixed(2);
        }
    });

    // PIVX withdraw handlers
    document.getElementById('pivx-withdraw-confirm')?.addEventListener('click', executePivxWithdraw);
    document.getElementById('pivx-withdraw-max')?.addEventListener('click', () => {
        const amountEl = document.getElementById('pivx-withdraw-amount');
        if (amountEl && pivxWithdrawAvailableBalance > 0) {
            amountEl.value = pivxWithdrawAvailableBalance.toFixed(2);
        }
    });

    // PIVX settings save
    document.getElementById('pivx-settings-save')?.addEventListener('click', savePivxSettings);

    // PIVX dialog overlay click to close
    domPivxDepositOverlay?.addEventListener('click', (e) => {
        if (e.target === domPivxDepositOverlay) closePivxDepositDialog();
    });
    domPivxSendOverlay?.addEventListener('click', (e) => {
        if (e.target === domPivxSendOverlay) closePivxSendDialog();
    });
    const domPivxWithdrawOverlay = document.getElementById('pivx-withdraw-overlay');
    domPivxWithdrawOverlay?.addEventListener('click', (e) => {
        if (e.target === domPivxWithdrawOverlay) closePivxWithdrawDialog();
    });
    domPivxSettingsOverlay?.addEventListener('click', (e) => {
        if (e.target === domPivxSettingsOverlay) closePivxSettingsDialog();
    });

    // Hook up an in-chat File Paste listener
    document.onpaste = async (evt) => {
        if (strOpenChat) {
            // Check if the clipboard data contains an image
            const arrItems = Array.from(evt.clipboardData.items);
            const imageItem = arrItems.find(item => item.type.startsWith('image/'));
            if (imageItem) {
                evt.preventDefault();

                // Get the image as a blob
                const blob = imageItem.getAsFile();
                if (!blob) return;

                // Read the blob as bytes
                const arrayBuffer = await blob.arrayBuffer();
                const bytes = new Uint8Array(arrayBuffer);

                // Determine file extension from MIME type
                const mimeType = imageItem.type;
                let ext = 'png'; // Default
                if (mimeType.includes('jpeg') || mimeType.includes('jpg')) {
                    ext = 'jpg';
                } else if (mimeType.includes('gif')) {
                    ext = 'gif';
                } else if (mimeType.includes('webp')) {
                    ext = 'webp';
                } else if (mimeType.includes('png')) {
                    ext = 'png';
                } else if (mimeType.includes('tiff')) {
                    ext = 'tiff';
                } else if (mimeType.includes('bmp')) {
                    ext = 'bmp';
                }

                // Generate a filename
                const fileName = `pasted_image.${ext}`;

                // Get reply reference before opening preview
                const strReplyRef = strCurrentReplyReference;
                
                // Cancel the reply UI (the reference is passed to the preview)
                cancelReply();

                // Open the file preview dialog with the pasted image bytes
                openFilePreviewWithBytes(bytes, fileName, ext, bytes.length, strOpenChat, strReplyRef);
            }
        }
    };

// Unified message sending function
async function sendMessage(messageText) {
    if (!messageText || !messageText.trim()) return;

    // Clean tracking parameters from any URLs in the message for privacy (if enabled)
    let cleanedText = messageText.trim();
    if (fStripTrackingEnabled) {
        const urlPattern = /(https?:\/\/[^\s<>"{}|\\^`\[\]]+)/gi;
        cleanedText = cleanedText.replace(urlPattern, (match) => {
            try {
                return cleanTrackingFromUrl(match);
            } catch (e) {
                // If cleaning fails, return original URL
                return match;
            }
        });
    }

    // Check if we're in edit mode
    if (strCurrentEditMessageId) {
        // Don't send if content hasn't changed
        if (cleanedText === strCurrentEditOriginalContent) {
            cancelEdit();
            return;
        }

        // Clear input and show editing state
        domChatMessageInput.value = '';
        resetSendMicButtons(); // Immediately reset to mic button (avoids animation race)
        domChatMessageInput.style.height = '';
        domChatMessageInput.style.overflowY = 'hidden';
        domChatMessageInput.setAttribute('placeholder', 'Saving edit...');

        try {
            const editMsgId = strCurrentEditMessageId;
            const originalContent = strCurrentEditOriginalContent;
            cancelEdit();

            // Instantly update the message in the DOM for responsive UX
            const msgElement = document.getElementById(editMsgId);
            if (msgElement) {
                const spanMessage = msgElement.querySelector('p > span:not(.msg-edited-indicator):not(.msg-reply)');
                if (spanMessage) {
                    spanMessage.innerHTML = parseMarkdown(cleanedText.trim());
                    linkifyUrls(spanMessage);
                    processInlineImages(spanMessage);
                    twemojify(spanMessage);
                }
                // Add edited indicator if not already present
                const pMessage = msgElement.querySelector('p');
                if (pMessage && !pMessage.querySelector('.msg-edited-indicator')) {
                    const spanEdited = document.createElement('span');
                    spanEdited.classList.add('msg-edited-indicator', 'btn');
                    spanEdited.textContent = '(edited)';
                    spanEdited.setAttribute('data-msg-id', editMsgId);
                    spanEdited.title = 'Click to view edit history';
                    pMessage.appendChild(spanEdited);
                }
            }

            // Update in cache as well
            const chat = arrChats.find(c => c.id === strOpenChat);
            if (chat) {
                const msg = chat.messages.find(m => m.id === editMsgId);
                if (msg) {
                    // Build edit history if it doesn't exist yet
                    if (!msg.edit_history) {
                        msg.edit_history = [];
                        // Add original content as first entry
                        msg.edit_history.push({
                            content: originalContent,
                            edited_at: msg.created_at * 1000 // Convert to milliseconds
                        });
                    }
                    // Add new edit entry
                    msg.edit_history.push({
                        content: cleanedText,
                        edited_at: Date.now()
                    });
                    msg.content = cleanedText;
                    msg.edited = true;
                }
            }

            // Send edit to backend (fire and forget for responsiveness)
            invoke('edit_message', {
                messageId: editMsgId,
                chatId: strOpenChat,
                newContent: cleanedText
            }).catch(e => {
                console.error('Failed to edit message:', e);
                // Optionally: revert the UI change on failure
            });

            nLastTypingIndicator = 0;
        } catch(e) {
            console.error('Failed to edit message:', e);
        }
        return;
    }

    // Clear input and show sending state
    domChatMessageInput.value = '';
    resetSendMicButtons(); // Immediately reset to mic button (avoids animation race)
    domChatMessageInput.style.height = ''; // Reset textarea height
    domChatMessageInput.style.overflowY = 'hidden'; // Reset overflow
    domChatMessageInput.setAttribute('placeholder', 'Sending...');

    try {
        const replyRef = strCurrentReplyReference;
        cancelReply();

        // Send message (unified function handles both DMs and MLS groups)
        await message(strOpenChat, cleanedText, replyRef);

        nLastTypingIndicator = 0;
    } catch(e) {
        console.error('Failed to send message:', e);
    }
}

    // Desktop/iOS - traditional keydown approach (not for Android)
    if (platformFeatures.os !== 'android') {
        domChatMessageInput.addEventListener('keydown', async (evt) => {
            if ((evt.key === 'Enter' || evt.keyCode === 13) && !evt.shiftKey) {
                evt.preventDefault();
                await sendMessage(domChatMessageInput.value);
            }
            // ESC key cancels reply/edit mode
            if (evt.key === 'Escape') {
                if (strCurrentEditMessageId) {
                    cancelEdit();
                } else if (strCurrentReplyReference) {
                    cancelReply();
                }
            }
        });
    }

/**
 * Immediately reset send/mic buttons to mic state (no animation)
 * Used after sending messages to avoid animation race conditions
 */
function resetSendMicButtons() {
    // Clear any animation classes
    domChatMessageInputSend.classList.remove('active', 'button-swap-in', 'button-swap-out');
    domChatMessageInputVoice.classList.remove('button-swap-in', 'button-swap-out');
    // Set correct display states
    domChatMessageInputSend.style.display = 'none';
    domChatMessageInputVoice.style.display = '';
}

    // Hook up an 'input' listener on the Message Box for typing indicators
domChatMessageInput.oninput = async () => {
    // Auto-resize the textarea based on content
    autoResizeChatInput();
    
    // Toggle send button active state based on text content
    const hasText = domChatMessageInput.value.trim().length > 0;
    if (hasText) {
        // Swap: Hide mic, show send button with animation
        if (domChatMessageInputVoice.style.display !== 'none') {
            domChatMessageInputVoice.classList.add('button-swap-out');
            domChatMessageInputVoice.addEventListener('animationend', () => {
                domChatMessageInputVoice.style.display = 'none';
                domChatMessageInputVoice.classList.remove('button-swap-out');
                domChatMessageInputSend.style.display = '';
                domChatMessageInputSend.classList.add('button-swap-in');
                domChatMessageInputSend.addEventListener('animationend', () => {
                    domChatMessageInputSend.classList.remove('button-swap-in');
                }, { once: true });
            }, { once: true });
        }
        domChatMessageInputSend.classList.add('active');
    } else {
        // Swap: Hide send, show mic button with animation
        if (domChatMessageInputSend.style.display !== 'none') {
            domChatMessageInputSend.classList.add('button-swap-out');
            domChatMessageInputSend.classList.remove('active');
            domChatMessageInputSend.addEventListener('animationend', () => {
                domChatMessageInputSend.style.display = 'none';
                domChatMessageInputSend.classList.remove('button-swap-out');
                domChatMessageInputVoice.style.display = '';
                domChatMessageInputVoice.classList.add('button-swap-in');
                domChatMessageInputVoice.addEventListener('animationend', () => {
                    domChatMessageInputVoice.classList.remove('button-swap-in');
                }, { once: true });
            }, { once: true });
        }
    }

    // Send a Typing Indicator only when content actually changes and setting is enabled
    // Don't send typing indicators while editing a message (it's not a new message)
    if (fSendTypingIndicators && !strCurrentEditMessageId && nLastTypingIndicator + 30000 < Date.now()) {
        nLastTypingIndicator = Date.now();
        await invoke("start_typing", { receiver: strOpenChat });
    }
};

    // Hook up the send button click handler (handles both text and voice messages)
    domChatMessageInputSend.onclick = async () => {
        // Check if we're in voice preview mode first
        if (recorder.isInPreview) {
            const wavData = recorder.send();
            if (wavData && strOpenChat) {
                domChatMessageInput.setAttribute('placeholder', 'Sending...');
                try {
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    await invoke('voice_message', {
                        receiver: strOpenChat,
                        repliedTo: strReplyRef,
                        bytes: wavData
                    });
                } catch (e) {
                    popupConfirm(e, '', true, '', 'vector_warning.svg');
                }
                domChatMessageInput.setAttribute('placeholder', 'Enter message...');
                nLastTypingIndicator = 0;
            }
            return;
        }
        
        // Otherwise, handle normal text message send
        const messageText = domChatMessageInput.value;
        if (messageText && messageText.trim()) {
            await sendMessage(messageText);
        }
    };

    // Hook up our drag-n-drop listeners
    if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
        await getCurrentWebview().onDragDropEvent(async (event) => {
            // Only accept File Drops if a chat is open
            if (strOpenChat) {
                if (event.payload.type === 'over') {
                    // TODO: add hover effects
                } else if (event.payload.type === 'drop') {
                    // Bring window to foreground when file is dropped
                    try {
                        await getCurrentWindow().setFocus();
                    } catch (e) {
                        console.warn('Failed to focus window:', e);
                    }
                    // Reset reply selection while passing a copy of the reference to the backend
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    // Show file preview instead of sending directly
                    await openFilePreview(event.payload.paths[0], strOpenChat, strReplyRef);
                } else {
                    // TODO: remove hover effects
                }
            }
        });

        await getCurrentWindow().onFocusChanged(async (event) => {
            if (event.payload && strOpenChat) {
                const currentChat = getDMChat(strOpenChat);
                if (currentChat && currentChat.messages.length > 0) {
                    // Find the last message from the contact (not from current user)
                    let lastContactMsg = null;
                    for (let i = currentChat.messages.length - 1; i >= 0; i--) {
                        if (!currentChat.messages[i].mine) {
                            lastContactMsg = currentChat.messages[i];
                            break;
                        }
                    }
                    if (lastContactMsg) {
                        markAsRead(currentChat, lastContactMsg);
                    }
                }
            }
        });
    }

    // Hook up our voice message recorder with Telegram-like UX
    const recorder = new VoiceRecorder(domChatMessageInputVoice, domChatInputContainer);
    
    // Handle state changes for UI updates
    recorder.onStateChange = (newState, oldState) => {
        if (newState === 'idle') {
            // Reset placeholder when returning to idle
            domChatMessageInput.setAttribute('placeholder', 'Enter message...');
        } else if (newState === 'recording' || newState === 'locked') {
            // Clear input and show recording status
            domChatMessageInput.value = '';
            domChatMessageInput.style.height = '';
            domChatMessageInput.style.overflowY = 'hidden';
        }
    };
    
    // Handle cancel callback
    recorder.onCancel = () => {
        domChatMessageInput.setAttribute('placeholder', 'Enter message...');
        cancelReply();
    };

    // Initialize voice transcription with default model
    window.cTranscriber = new VoiceTranscriptionUI();
    window.voiceSettings = new VoiceSettings();

    // Only load whisper models if transcription is supported
    if (platformFeatures.transcription) {
        await window.voiceSettings.loadWhisperModels();
    }
    
    window.voiceSettings.initVoiceSettings();

    // Initialize settings
    await initSettings();

    // Hook up our "Help Prompts" to give users easy feature explainers in ambiguous or complex contexts
    // Note: since some of these overlap with Checkbox Labels: we prevent event bubbling so that clicking the Info Icon doesn't also trigger other events
    domSettingsWhisperModelInfo.onclick = (e) => {
        popupConfirm('Vector Voice AI Model', 'The Vector Voice AI model <b>determines the Quality of your transcriptions.</b><br><br>A larger model will provide more accurate transcriptions & translations, but require more Disk Space, Memory and CPU power to run.', true);
    };
    domSettingsWhisperAutoTranslateInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Vector Voice Translations', 'Vector Voice AI can <b>automatically detect non-English languages and translate them in to English text for you.</b><br><br>You can decide whether Vector Voice transcribes in to their native spoken language, or instead translates in to English on your behalf.', true);
    };
    domSettingsWhisperAutoTranscribeInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Vector Voice Transcriptions', 'Vector Voice AI can <b>automatically transcribe incoming Voice Messages</b> for immediate reading, without needing to listen.<br><br>You can decide whether Vector Voice transcribes automatically, or if you prefer to transcribe each message explicitly.', true);
    };
    domSettingsPrivacyWebPreviewsInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Web Previews', 'When enabled, Vector will <b>automatically fetch and display previews</b> for links shared in messages.<br><br>This may expose your IP address if you do not use a VPN.', true);
    };
    domSettingsPrivacyStripTrackingInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Strip Tracking Markers', 'When enabled, Vector will <b>automatically remove tracking markers</b> from URLs before displaying or sending them.<br><br>This helps reduce your footprint and enhances your privacy with no loss in functionality, only disable if you know what you\'re doing.', true);
    };
    domSettingsPrivacySendTypingInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Send Typing Indicators', 'When enabled, Vector will <b>notify your contacts when you are typing</b> a message to them.<br><br>Disable this if you prefer to type without others knowing you are composing a message.', true);
    };
    domSettingsDisplayImageTypesInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Display Image Types', 'When enabled, images in chat will display a <b>small badge showing the file type</b> (e.g., PNG, GIF, WEBP) in the corner.<br><br>This helps identify image formats at a glance.', true);
    };
    domSettingsNotifMuteInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Mute Notification Sounds', 'When enabled, Vector will <b>not play any notification sounds</b> for incoming messages.<br><br>You will still receive visual notifications and badges.', true);
    };

    domSettingsDeepRescanInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Deep Rescan', 'This will forcefully sync your message history backwards in two‑day sections until 30 days of no events are found. This may take some time.', true);
    };

    domSettingsExportAccountInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Export Account', 'Export Account will display a backup of your encryption keys. Keep it safe to restore your account later.', true);
    };

    // Info button for Refresh KeyPackages
    const domRefreshKeypkgInfo = document.getElementById('refresh-keypkg-info');
    if (domRefreshKeypkgInfo) {
        domRefreshKeypkgInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm(
                'Refresh KeyPackages',
                'Regenerates a fresh device KeyPackage for MLS. This can help you receive Group Invites on this device.',
                true
            );
        };
    }

    domSettingsLogoutInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Logout', 'Logout will erase the local database and remove all stored keys. You will lose access to group chats unless you have a backup.', true);
    };

    // Donors & Contributors info button
    domSettingsDonorsInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Donors & Contributors', 'The organisations listed below have contributed either <b>financially or resourcefully</b> (developers, infrastructure, etc) to Vector\'s development.<br><br>We extend our sincere thanks to these supporters for helping make Vector possible.', true);
    };

    // PIVX donor logo click - opens pivx.org
    domDonorPivx.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        openUrl('https://pivx.org');
    };

    // Restore PIVX Wallet button - restores hidden PIVX app
    if (domRestorePivxBtn) {
        domRestorePivxBtn.onclick = async () => {
            localStorage.removeItem('pivx_hidden');
            // Hide the restore button
            if (domRestorePivxGroup) {
                domRestorePivxGroup.style.display = 'none';
            }
            // Refresh Mini Apps list if it's loaded
            await loadMiniAppsHistory();
            popupConfirm('PIVX Wallet Restored', 'The PIVX Wallet has been restored to your Mini Apps panel.', true);
        };
    }

    // Add npub copy functionality for chat-new section
    document.getElementById('chat-new-npub-copy')?.addEventListener('click', (e) => {
        const npub = document.getElementById('share-npub')?.textContent;
        if (npub) {
            // Copy the full profile URL for easy sharing
            const profileUrl = `https://vectorapp.io/profile/${npub}`;
            navigator.clipboard.writeText(profileUrl).then(() => {
                const copyBtn = e.target.closest('.profile-npub-copy');
                if (copyBtn) {
                    copyBtn.innerHTML = '<span class="icon icon-check"></span>';
                    setTimeout(() => {
                        copyBtn.innerHTML = '<span class="icon icon-copy"></span>';
                    }, 2000);
                }
            });
        }
    });
});

// Listen for app-wide click interations
document.addEventListener('click', (e) => {
    // If we're clicking the emoji search, don't close it!
    if (e.target === emojiSearch) return;

    // If we're clicking an <a> link, handle it with our openUrl function
    if (e.target.tagName === 'A' && e.target.href) {
        e.preventDefault();
        return openUrl(e.target.href);
    }

    // If we're clicking a <summary> to toggle <details>, handle scroll adjustment
    if (e.target.tagName === 'SUMMARY') {
        const details = e.target.parentElement;
        if (details && details.tagName === 'DETAILS') {
            // Add button class if not already present
            if (!e.target.classList.contains('btn')) {
                e.target.classList.add('btn');
            }
            
            const chatMessages = document.getElementById('chat-messages');
            if (chatMessages) {
                // Check scroll position BEFORE toggle
                const wasNearBottom = chatMessages.scrollHeight - chatMessages.scrollTop - chatMessages.clientHeight < 150;
                
                // Wait for the DOM to update after toggle
                requestAnimationFrame(() => {
                    requestAnimationFrame(() => {
                        if (wasNearBottom && details.open) {
                            // Scroll to bottom to reveal expanded content
                            scrollToBottom(chatMessages, true);
                        }
                    });
                });
            }
        }
    }

    // If we're clicking a Reply button, begin a reply
    if (e.target.classList.contains("reply-btn")) return selectReplyingMessage(e);

    // If we're clicking an Edit button, begin editing the message
    if (e.target.classList.contains("edit-btn")) {
        const msgId = e.target.getAttribute('data-msg-id');
        const msgContent = e.target.getAttribute('data-msg-content');
        if (msgId && msgContent) {
            return startEditMessage(msgId, msgContent);
        }
    }

    // If we're clicking an edited indicator, show the edit history
    if (e.target.classList.contains("msg-edited-indicator")) {
        const msgId = e.target.getAttribute('data-msg-id');
        if (msgId) {
            showEditHistory(msgId, e.target);
        }
    }

    // If we're clicking a File Reveal button, reveal the file with the OS File Explorer
    if (e.target.getAttribute('filepath')) {
        return revealItemInDir(e.target.getAttribute('filepath'));
    }

    // If we're clicking a Reply context, center the referenced message in view
    if (e.target.classList.contains('msg-reply') || e.target.parentElement?.classList.contains('msg-reply')  || e.target.parentElement?.parentElement?.classList.contains('msg-reply')) {
        // Note: The `substring(2)` removes the `r-` prefix
        const strID = e.target.id || e.target.parentElement?.id || e.target.parentElement.parentElement.id;
        const targetMsgId = strID.substring(2);
        const domMsg = document.getElementById(targetMsgId);
        
        if (domMsg) {
            // Message is already rendered, just scroll to it
            centerInView(domMsg);

            // Run an animation to bring the user's eye to the message
            const pContainer = domMsg.querySelector('p');
            if (!pContainer.classList.contains('no-background')) {
                domMsg.classList.add('highlight-animation');
                setTimeout(() => domMsg.classList.remove('highlight-animation'), 1500);
            }
        } else {
            // Message not rendered yet, load it and surrounding messages
            loadAndScrollToMessage(targetMsgId);
        }
        return;
    }

    // If we're clicking a Metadata Preview, open it's URL, if one is attached
    if (e.target.classList.contains("msg-preview-container") || e.target.parentElement?.classList.contains("msg-preview-container")) {
        const strURL = e.target.getAttribute('url') || e.target.parentElement.getAttribute('url');
        if (strURL) openUrl(strURL);
        return;
    }

    // If we're clicking a Payment URI, open it's URL
    if (e.target.getAttribute('pay-uri')) {
        return openUrl(e.target.getAttribute('pay-uri'));
    }

    // If we're clicking a Contact in the main chat list (NOT inside the Create Group panel), open the chat
    const cg = document.getElementById('create-group');
    const inCreateGroup = cg && cg.style.display !== 'none' && cg.contains(e.target);
    if (!inCreateGroup && (e.target.classList.contains("chatlist-contact") || e.target.parentElement?.classList.contains("chatlist-contact") ||  e.target.parentElement?.parentElement?.classList.contains("chatlist-contact"))) {
        // Don't open chat if clicking on an invite item
        if (e.target.classList.contains("chatlist-invite") || e.target.closest(".chatlist-invite")) {
            return;
        }
        const strID = e.target.id || e.target.parentElement?.id || e.target.parentElement.parentElement.id;
        // Strip the 'chatlist-' prefix if present
        const chatId = strID.replace('chatlist-', '');
        return openChat(chatId);
    }

    // If we're clicking an Attachment Download button, request the download
    if (e.target.hasAttribute('download')) {
        return invoke('download_attachment', { npub: e.target.getAttribute('npub'), msgId: e.target.getAttribute('msg'), attachmentId: e.target.id });
    }

    // Run the emoji panel open/close logic
    openEmojiPanel(e);

    // Close attachment panel when clicking outside of it
    if (domAttachmentPanel.classList.contains('visible')) {
        const clickedInsidePanel = domAttachmentPanel.contains(e.target);
        const clickedFileButton = domChatMessageInputFile.contains(e.target);
        // Don't close if clicking inside PIVX dialogs, popup prompts, or Mini App launch dialog
        const clickedInsidePivxDialog = e.target.closest('.pivx-dialog-overlay');
        const clickedInsidePopup = e.target.closest('#popup-container');
        const clickedInsideLaunchDialog = e.target.closest('#miniapp-launch-overlay');
        if (!clickedInsidePanel && !clickedFileButton && !clickedInsidePivxDialog && !clickedInsidePopup && !clickedInsideLaunchDialog) {
            closeAttachmentPanel();
        }
    }

    // Close edit history popup when clicking outside of it
    const editHistoryPopup = document.getElementById('edit-history-popup');
    if (editHistoryPopup && editHistoryPopup.style.display !== 'none') {
        if (!editHistoryPopup.contains(e.target) && !e.target.classList.contains('msg-edited-indicator')) {
            hideEditHistory();
        }
    }
});

// Close edit history popup on Escape key
document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
        const editHistoryPopup = document.getElementById('edit-history-popup');
        if (editHistoryPopup && editHistoryPopup.style.display !== 'none') {
            hideEditHistory();
        }
    }
});

/**
 * Resize certain tricky components (i.e: the Chat Box) on window resizes.
 * 
 * This can also be re-called when some components are spawned, since they can
 * affect the height and width of other components, too.
 */
function adjustSize() {
    // Chat List: resize the list to fit within the screen after the upper Account area
    // Note: no idea why the `- 50px` is needed below, magic numbers, I guess.
    const nNewChatBtnHeight = domChatNewDM?.getBoundingClientRect().height || 0;
    const nNavbarHeight = domNavbar.getBoundingClientRect().height;
    domChatList.style.maxHeight = (window.innerHeight - (domChatList.offsetTop + nNewChatBtnHeight + nNavbarHeight)) + 50 + 'px';

    // Re-calculate chat input size on window resize (text may reflow)
    autoResizeChatInput();

    // If the chat is open, and they've not significantly scrolled up: auto-scroll down to correct against container resizes
    softChatScroll();

    // Re-truncate marketplace category tags on resize
    truncateAllCategoryTags();
}

/**
 * Scrolls the chat to the bottom if the user has not already scrolled upwards substantially.
 * 
 * This is used to correct against container resizes, i.e: if an image loads, or a message is received.
 */
function softChatScroll() {
    if (!strOpenChat) return;

    // If the chat is open, and they've not significantly scrolled up: auto-scroll down to correct against container resizes
    const pxFromBottom = domChatMessages.scrollHeight - domChatMessages.scrollTop - domChatMessages.clientHeight;
    if (pxFromBottom < 1000) {
        scrollToBottom(domChatMessages, false);
    }
}

window.onresize = adjustSize;

// ===== Create Group: state and helpers =====
/**
 * Selected members (npubs) for the group being created.
 * Keep this decoupled from arrChats.
 */
let arrSelectedGroupMembers = [];
/**
 * Tracks whether the user attempted to create the group.
 * Used to only show inline validation after an explicit attempt.
 */
let fCreateGroupAttempt = false;


/**
 * Render the filterable, scrollable contact list with checkboxes.
 * Reuses arrProfiles as the source of truth.
 */
function renderCreateGroupList(filterText = '') {
    if (!domCreateGroupList) return;
    domCreateGroupList.innerHTML = '';

    const f = (filterText || '').trim().toLowerCase();

    // Exclude our own profile from selection
    const mine = arrProfiles.find(p => p.mine)?.id;

    // Build a fragment for performance
    const frag = document.createDocumentFragment();

    // Sort profiles: selected members first (by selection order), then unselected by last message time
    const sortedProfiles = [...arrProfiles].sort((a, b) => {
        const aSelectedIndex = arrSelectedGroupMembers.indexOf(a?.id);
        const bSelectedIndex = arrSelectedGroupMembers.indexOf(b?.id);
        const aSelected = aSelectedIndex !== -1;
        const bSelected = bSelectedIndex !== -1;
        
        // Selected members come first
        if (aSelected && !bSelected) return -1;
        if (!aSelected && bSelected) return 1;
        
        // For selected members: sort by selection order (first selected = first in list)
        if (aSelected && bSelected) {
            return aSelectedIndex - bSelectedIndex;
        }
        
        // For unselected members: sort by last message time (most recent first)
        const aChatTimestamp = getChatSortTimestamp(arrChats.find(c => c.id === a?.id) || {});
        const bChatTimestamp = getChatSortTimestamp(arrChats.find(c => c.id === b?.id) || {});
        
        // If both have timestamps, sort by most recent
        if (aChatTimestamp && bChatTimestamp) {
            return bChatTimestamp - aChatTimestamp;
        }
        // Contacts with messages come before those without
        if (aChatTimestamp && !bChatTimestamp) return -1;
        if (!aChatTimestamp && bChatTimestamp) return 1;
        
        // Fallback: sort alphabetically
        const aName = (a?.nickname || a?.name || '').toLowerCase();
        const bName = (b?.nickname || b?.name || '').toLowerCase();
        return aName.localeCompare(bName);
    });

    for (const p of sortedProfiles) {
        if (!p || !p.id) continue;
        if (p.id === mine) continue;

        // Filter by nickname/name/npub
        const name = p.nickname || p.name || '';
        const hay = (name + ' ' + p.id).toLowerCase();
        if (f && !hay.includes(f)) continue;

        // Row container - compact style matching Invite Member list
        const row = document.createElement('div');
        row.id = `cg-${p.id}`;
        row.style.display = 'flex';
        row.style.alignItems = 'center';
        row.style.padding = '5px 10px';
        row.style.borderRadius = '6px';
        row.style.transition = 'background 0.2s ease';
        row.style.isolation = 'isolate';
        row.style.cursor = 'pointer';
        row.style.position = 'relative';

        // Add hover effect with theme-based gradient
        const bgDiv = document.createElement('div');
        bgDiv.style.position = 'absolute';
        bgDiv.style.top = '0';
        bgDiv.style.left = '0';
        bgDiv.style.right = '0';
        bgDiv.style.bottom = '0';
        bgDiv.style.borderRadius = '6px';
        bgDiv.style.opacity = '0';
        bgDiv.style.transition = 'opacity 0.2s ease';
        bgDiv.style.pointerEvents = 'none';
        bgDiv.style.zIndex = '0';
        row.appendChild(bgDiv);

        row.addEventListener('mouseenter', () => {
            const primaryColor = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
            bgDiv.style.background = `linear-gradient(to right, ${primaryColor}40, transparent)`;
            bgDiv.style.opacity = '1';
        });
        row.addEventListener('mouseleave', () => {
            bgDiv.style.opacity = '0';
        });

        // Avatar - compact 25px size
        const listAvatarSrc = getProfileAvatarSrc(p);
        const avatar = createAvatarImg(listAvatarSrc, 25, false);
        avatar.style.marginRight = '10px';
        avatar.style.position = 'relative';
        avatar.style.zIndex = '1';
        row.appendChild(avatar);

        // Name - compact style
        const nameSpan = document.createElement('div');
        nameSpan.className = 'compact-member-name';
        nameSpan.textContent = name || p.id.substring(0, 10) + '...';
        nameSpan.style.color = '#f7f4f4';
        nameSpan.style.fontSize = '14px';
        nameSpan.style.flex = '1';
        nameSpan.style.textAlign = 'left';
        nameSpan.style.position = 'relative';
        nameSpan.style.zIndex = '1';
        if (name) twemojify(nameSpan);
        row.appendChild(nameSpan);

        // Custom checkbox indicator (circular, matching Invite Member style)
        const indicator = document.createElement('div');
        indicator.style.width = '18px';
        indicator.style.height = '18px';
        indicator.style.borderRadius = '50%';
        indicator.style.border = '2px solid var(--icon-color-primary)';
        indicator.style.position = 'relative';
        indicator.style.zIndex = '1';
        indicator.style.flexShrink = '0';
        indicator.style.marginLeft = 'auto';
        indicator.style.transition = 'background-color 0.2s ease';
        
        // Set initial state
        if (arrSelectedGroupMembers.includes(p.id)) {
            indicator.style.backgroundColor = 'var(--icon-color-primary)';
        }
        row.appendChild(indicator);

        // Row click toggles selection
        row.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            
            const isSelected = arrSelectedGroupMembers.includes(p.id);
            if (isSelected) {
                // Deselect
                arrSelectedGroupMembers = arrSelectedGroupMembers.filter(n => n !== p.id);
            } else {
                // Select
                arrSelectedGroupMembers.push(p.id);
            }
            updateCreateGroupValidation(true);
            
            // Re-render to hoist selected members to top
            const currentFilter = domCreateGroupFilter?.value || '';
            renderCreateGroupList(currentFilter);
        });

        frag.appendChild(row);
    }

    // If no matches
    if (!frag.childElementCount) {
        const empty = document.createElement('p');
        empty.style.textAlign = 'center';
        empty.style.opacity = '0.7';
        empty.textContent = f ? 'No matches' : 'No contacts found';
        domCreateGroupList.appendChild(empty);
    } else {
        domCreateGroupList.appendChild(frag);
    }
}

/**
 * Enable/disable Create button and show inline hint
 */
function updateCreateGroupValidation(showInline = false) {
    if (!domCreateGroupCreateBtn) return;
    const nameOk = !!domCreateGroupName?.value.trim();
    const membersOk = arrSelectedGroupMembers.length > 0;

    const enabled = nameOk && membersOk;

    // Toggle both property and attribute to avoid any CSS/UA inconsistencies
    domCreateGroupCreateBtn.disabled = !enabled;
    if (enabled) {
        domCreateGroupCreateBtn.removeAttribute('disabled');
    } else {
        domCreateGroupCreateBtn.setAttribute('disabled', '');
    }

    // Only show status after an explicit attempt, or when forced via parameter
    const shouldShow = showInline || fCreateGroupAttempt;

    if (domCreateGroupStatus) {
        if (shouldShow && (!nameOk || !membersOk)) {
            domCreateGroupStatus.style.display = '';
            domCreateGroupStatus.textContent = !nameOk
                ? 'Group name is required'
                : 'Select at least one contact';
        } else {
            domCreateGroupStatus.style.display = 'none';
            domCreateGroupStatus.textContent = '';
        }
    }
}

/**
 * Open Create Group tab
 */
function openCreateGroup() {
    // Show panel
    domCreateGroup.style.display = '';
    // Hide others
    domChats.style.display = 'none';
    domChat.style.display = 'none';
    domNavbar.style.display = 'none';

    // Reset state
    arrSelectedGroupMembers = [];
    fCreateGroupAttempt = false;
    if (domCreateGroupName) domCreateGroupName.value = '';
    if (domCreateGroupFilter) domCreateGroupFilter.value = '';
    if (domCreateGroupStatus) {
        domCreateGroupStatus.style.display = 'none';
        domCreateGroupStatus.textContent = '';
    }

    // Render list
    renderCreateGroupList('');
    updateCreateGroupValidation(false);

    // Focus name
    domCreateGroupName?.focus();
}

/**
 * Close Create Group tab and go back to Chat list
 */
async function closeCreateGroup() {
    domCreateGroup.style.display = 'none';
    fCreateGroupAttempt = false;

    // Restore navbar to follow the same flow as "Start New Chat" close (see closeChat())
    domNavbar.style.display = '';

    // Navigate back to chat list
    await openChatlist();

    // Adjust layout after UI visibility changes
    adjustSize();
}

/**
 * Wire up Create Group UI events
 */
/*
Create Group UI wiring
- Validation: Create button disabled until non-empty group name and at least one member selected. See updateCreateGroupValidation() for state sync.
- Loading states:
  • Button text toggles to 'Creating...' and disabled during IPC.
  • Inline status text shows 'Preparing devices...' then 'Finalizing...' on success.
- IPC flow:
  • invoke('create_group_chat', { groupName, memberIds })
    - Backend validates inputs and refreshes each member's device KeyPackage.
    - If any member fails refresh/fetch, backend returns Err with a user-facing string. We surface that string directly via popupConfirm and status label.
  • On success:
    - openChat(newGroupId) navigates to the newly created group.
- Error handling:
  • popupConfirm('Group creation failed', errorString, ...) shows a clear toast/modal.
  • domCreateGroupStatus also mirrors the exact error string for inline context.
  • We do not partially create groups: any device refresh failure aborts.
- Notes:
  • Backend emits 'mls_group_initial_sync' on success.
*/
(function wireCreateGroupUI() {
    if (!domCreateGroup) return;

    domCreateGroupBackBtn.onclick = closeCreateGroup;
    domCreateGroupCancelBtn.onclick = closeCreateGroup;

    domCreateGroupName.oninput = () => updateCreateGroupValidation(true);
    domCreateGroupFilter.oninput = (e) => renderCreateGroupList(e.target.value || '');

    domCreateGroupCreateBtn.onclick = async () => {
        const groupName = (domCreateGroupName?.value || '').trim();
        const memberIds = [...arrSelectedGroupMembers];

        // Mark that the user attempted to create a group
        fCreateGroupAttempt = true;

        if (!groupName || memberIds.length === 0) {
            updateCreateGroupValidation(true);
            return;
        }

        // Loading state
        const prevTxt = domCreateGroupCreateBtn.textContent;
        domCreateGroupCreateBtn.textContent = 'Creating...';
        domCreateGroupCreateBtn.disabled = true;

        if (domCreateGroupStatus) {
            domCreateGroupStatus.style.display = '';
            domCreateGroupStatus.textContent = 'Preparing devices...';
        }

        try {
            // Backend orchestration: refresh keypackages per member, create group, persist
            // Note: Tauri expects camelCase arg keys for Rust snake_case params.
            const newGroupId = await invoke('create_group_chat', {
                groupName: groupName,
                memberIds: memberIds
            });

            // On success: refresh groups, open the new group chat, and close panel
            if (domCreateGroupStatus) {
                domCreateGroupStatus.textContent = 'Finalizing...';
            }

            // Navigate to the new group
            openChat(newGroupId);

            // Hide panel
            domCreateGroup.style.display = 'none';
        } catch (e) {
            const raw = (e || '').toString();
            // Map backend "no device keypackages" errors to a friendlier UX message
            let friendly = raw;
            let isHtml = false;
            try {
                const m = raw.match(/no device keypackag(?:e|es) found for (\S+)/i);
                if (m && m[1]) {
                    const npub = m[1];
                    const prof = arrProfiles.find(p => p.id === npub);
                    const display = prof?.nickname || prof?.name || 'This user';
                    friendly = `${display} is using an older Vector version!<br>Please ask them to upgrade before inviting them to a Group Chat.`;
                    isHtml = true;
                }
            } catch (_) {}
            popupConfirm('Group creation failed', friendly, true, '', 'vector_warning.svg');
            if (domCreateGroupStatus) {
                domCreateGroupStatus.style.display = '';
                if (isHtml) domCreateGroupStatus.innerHTML = friendly;
                else domCreateGroupStatus.textContent = friendly;
            }
        } finally {
            domCreateGroupCreateBtn.textContent = prevTxt || 'Create';
            updateCreateGroupValidation();
        }
    };
})();
