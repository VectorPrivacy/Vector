// ========== PIVX Wallet Functions ==========

/** The PIVX wallet card replaces the Mini Apps view; the card slides in on each open. */
function showPivxWalletPanel() {
    // Track PIVX usage for history-based positioning
    localStorage.setItem('pivx_last_used', Date.now().toString());
    VectorSvelte.attachmentSetView('pivx');
    VectorSvelte.attachmentPulse('pivx');
    refreshPivxWallet();
}

/** Back from the wallet card to the Mini Apps view. */
function hidePivxWalletPanel() {
    VectorSvelte.attachmentSetView('miniapps');
    VectorSvelte.attachmentPulse('grid');
}

/** Fetch the balance and price and paint the wallet card. */
async function refreshPivxWallet() {
    VectorSvelte.pivxWalletLoading();
    try {
        const [balance, priceInfo] = await Promise.all([
            invoke('pivx_get_wallet_balance'),
            fetchPivxPrice()
        ]);
        // Store current balance for deposit limit check
        pivxCurrentWalletBalance = balance;
        const fiat = priceInfo && priceInfo.value > 0
            ? formatFiatValue(balance * priceInfo.value, priceInfo.currency.toUpperCase()) : '';
        VectorSvelte.pivxWalletSet({ balance, fiat, depositDisabled: balance >= PIVX_MAX_BALANCE_WARNING });
    } catch (err) {
        console.error('Failed to refresh PIVX wallet:', err);
        VectorSvelte.pivxWalletSet({ balance: 0, fiat: '', depositDisabled: false });
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

    VectorSvelte.pivxWalletPatch({ depositLoading: true });
    try {
        // Create a new promo code for deposit
        const promo = await invoke('pivx_create_promo');
        pivxCurrentDepositAddress = promo.address;
        VectorSvelte.pivxDeposit.open({ address: promo.address, received: 0 });
        // Start polling for incoming deposit
        startDepositPolling(promo.address);
    } catch (err) {
        console.error('Failed to create deposit promo:', err);
        showToast('Failed to create deposit address');
    } finally {
        VectorSvelte.pivxWalletPatch({ depositLoading: false });
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

            VectorSvelte.pivxDeposit.patch({ received: thisPromo.balance_piv });

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
    VectorSvelte.pivxDeposit.close();
}

// Track send dialog state
let pivxSendAvailableBalance = 0;
let pivxSendPromos = [];

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

/** The send dialog for the open chat: quick send lists funded promos, custom takes an amount. */
async function showPivxSendDialog() {
    if (!strOpenChat) {
        showToast('Open a chat first to send PIVX');
        return;
    }
    const d = VectorSvelte.pivxSend;
    pivxSendAvailableBalance = 0;
    d.open({
        recipient: getChatDisplayName(strOpenChat) || 'this chat', mode: 'quick', loading: true,
        promos: [], error: '', selectedCode: '', amount: '', available: 0, busy: false,
    });
    try {
        pivxSendPromos = await invoke('pivx_refresh_balances');
        // Only promos with balance, largest first
        const promosWithBalance = pivxSendPromos
            .filter(p => p.balance_piv > 0)
            .sort((a, b) => b.balance_piv - a.balance_piv);
        pivxSendAvailableBalance = promosWithBalance.reduce((sum, p) => sum + p.balance_piv, 0);
        d.patch({ promos: promosWithBalance, available: pivxSendAvailableBalance, loading: false });
    } catch (err) {
        console.error('Failed to fetch promos for send:', err);
        pivxSendPromos = [];
        d.patch({ promos: [], available: 0, error: 'Failed to load wallet.', loading: false });
    }
}

function showPivxSendCustomMode() { VectorSvelte.pivxSend.patch({ mode: 'custom', selectedCode: '' }); }
function showPivxSendQuickMode() { VectorSvelte.pivxSend.patch({ mode: 'quick', amount: '' }); }
function closePivxSendDialog() { VectorSvelte.pivxSend.close(); }

/** Wallet settings: the auto-withdraw address (local) and the currency list (an API call). */
async function showPivxSettingsDialog() {
    const d = VectorSvelte.pivxSettings;
    d.open({ address: '', currencies: [], currency: '', currenciesLoading: true });

    invoke('pivx_get_wallet_address').then(address => {
        d.patch({ address: address || '' });
    }).catch(err => {
        console.error('Failed to get wallet address:', err);
    });

    Promise.all([
        fetchPivxCurrencies(),
        invoke('pivx_get_preferred_currency').catch(() => null)
    ]).then(([currencies, savedCurrency]) => {
        const currentCurrency = (savedCurrency || pivxPreferredCurrency || detectDefaultCurrency()).toUpperCase();
        pivxPreferredCurrency = currentCurrency;
        const list = currencies.map(c => c.currency.toUpperCase());
        d.patch({ currencies: list, currency: list.includes(currentCurrency) ? currentCurrency : '', currenciesLoading: false });
    }).catch(err => {
        console.error('Failed to load currencies:', err);
        d.patch({ currencies: ['USD'], currency: 'USD', currenciesLoading: false });
    });
}

function closePivxSettingsDialog() { VectorSvelte.pivxSettings.close(); }

// Withdraw dialog state
let pivxWithdrawAvailableBalance = 0;

/** The withdraw dialog, with the wallet balance as the ceiling. */
async function showPivxWithdrawDialog() {
    const d = VectorSvelte.pivxWithdraw;
    d.open({ address: '', amount: '', available: 0, busy: false });
    try {
        pivxWithdrawAvailableBalance = await invoke('pivx_get_wallet_balance');
    } catch (err) {
        console.error('Failed to get balance:', err);
        pivxWithdrawAvailableBalance = 0;
    }
    d.patch({ available: pivxWithdrawAvailableBalance });
}

function closePivxWithdrawDialog() { VectorSvelte.pivxWithdraw.close(); }

/**
 * Executes a PIVX withdrawal
 */
async function executePivxWithdraw() {
    const d = VectorSvelte.pivxWithdraw;
    const address = (d.state().address || '').trim();
    const amount = parseFloat(d.state().amount || '0');

    // Validate address
    if (!address || !address.startsWith('D') || address.length < 30 || address.length > 36) {
        showToast('Invalid PIVX address');
        return;
    }
    if (amount <= 0) {
        showToast('Enter a valid amount');
        return;
    }
    if (amount > pivxWithdrawAvailableBalance) {
        showToast('Insufficient balance');
        return;
    }

    d.patch({ busy: true });
    try {
        const result = await invoke('pivx_withdraw', {
            destAddress: address,
            amountPiv: amount
        });
        closePivxWithdrawDialog();
        showToast(`Withdrawn ${amount.toFixed(2)} PIV`);
        refreshPivxWallet();
        if (result.change_piv > 0) {
            console.log(`Withdrawal change: ${result.change_piv} PIV saved to new promo`);
        }
    } catch (err) {
        console.error('Withdrawal failed:', err);
        showToast('Withdrawal failed: ' + (err.message || err));
    } finally {
        d.patch({ busy: false });
    }
}

/**
 * Sends a PIVX payment to the current chat
 */
async function sendPivxPayment() {
    if (!strOpenChat) {
        showToast('No chat selected');
        return;
    }
    const d = VectorSvelte.pivxSend;
    const st = d.state();
    d.patch({ busy: true });
    try {
        if (st.mode === 'quick') {
            // Quick send: an existing whole promo
            const promo = st.promos.find(p => p.gift_code === st.selectedCode);
            if (!promo) {
                showToast('Select an amount to send');
                return;
            }
            await invoke('pivx_send_existing_promo', {
                receiver: strOpenChat,
                giftCode: promo.gift_code
            });
            closePivxSendDialog();
            showToast(`Sent ${promo.balance_piv.toFixed(2)} PIV`);
            refreshPivxWallet();
        } else {
            const amount = parseFloat(st.amount || '0');
            if (amount <= 0) {
                showToast('Enter a valid amount');
                return;
            }
            if (amount > pivxSendAvailableBalance) {
                showToast(`Insufficient funds (max: ${pivxSendAvailableBalance.toFixed(2)} PIV)`);
                return;
            }
            // Custom amount via coin selection
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
        d.patch({ busy: false });
    }
}

/**
 * Saves the PIVX wallet settings
 */
async function savePivxSettings() {
    const st = VectorSvelte.pivxSettings.state();
    const address = (st.address || '').trim();
    const currency = st.currency || '';

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
    // DM - get profile name
    return getName(chatId);
}

// ========== End PIVX Wallet Functions ==========

// ========== Chat Integration ==========
// PIVX-specific chat-side helpers: handles inbound payment events from the
// backend and merges historical payments into chat messages on chat-open.
// Both depend on globals defined elsewhere in classic-script scope: arrChats,
// strOpenChat, eventCache, getProfile, renderMessage, domChatMessages,
// softChatScroll, renderChatlist, invoke.

/**
 * Tauri event handler for inbound `pivx_payment_received`. Builds a synthetic
 * message object, inserts it into the target chat in timestamp order, and
 * re-renders the chatlist + the open chat (if applicable).
 */
function handlePivxPaymentReceived(evt) {
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
            const profile = getProfile(conversation_id);
            updateChat(chat, [pivxMsg], profile, false);
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

    chatChanged(chat);
}

/**
 * Fetch this chat's PIVX payment history from the backend and merge into the
 * existing message array (mutates `initialMessages` in place via eventCache).
 * Re-sorts by timestamp after the merge. Failures are logged and swallowed —
 * a missing PIVX payment must not block chat-open.
 */
async function mergePivxPaymentsIntoChat(contact, initialMessages) {
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
}

// ========== End Chat Integration ==========

// The dialogs mount once the bundle has run (this script is not deferred).
document.addEventListener('DOMContentLoaded', function initPivxDialogs() {
    VectorSvelte.mountPivxDialogs({ h: {
        deposit: {
            close: closePivxDepositDialog,
            copy: () => {
                const address = VectorSvelte.pivxDeposit.state().address;
                if (address) {
                    navigator.clipboard.writeText(address);
                    showToast('Address copied!');
                }
            },
        },
        send: {
            close: closePivxSendDialog,
            confirm: sendPivxPayment,
            custom: showPivxSendCustomMode,
            quick: showPivxSendQuickMode,
            max: () => { if (pivxSendAvailableBalance > 0) VectorSvelte.pivxSend.patch({ amount: pivxSendAvailableBalance.toFixed(2) }); },
        },
        withdraw: {
            close: closePivxWithdrawDialog,
            confirm: executePivxWithdraw,
            max: () => { if (pivxWithdrawAvailableBalance > 0) VectorSvelte.pivxWithdraw.patch({ amount: pivxWithdrawAvailableBalance.toFixed(2) }); },
        },
        settings: { close: closePivxSettingsDialog, save: savePivxSettings },
    } });
});
