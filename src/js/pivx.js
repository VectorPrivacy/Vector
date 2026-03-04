// PIVX DOM constants
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

// ========== PIVX Wallet Functions ==========

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

// Wire up PIVX event listeners (defer guarantees DOM is parsed)
(function initPivxListeners() {
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
})();
