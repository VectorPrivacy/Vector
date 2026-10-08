/**
 * Mini Apps (WebXDC) support for Vector
 *
 * This module provides functions to interact with Mini Apps (.xdc files)
 * which are isolated web applications that can be shared in chats.
 *
 * Includes support for realtime peer channels using Iroh P2P,
 * compatible with DeltaChat's WebXDC implementation.
 */

/**
 * Load information about a Mini App from a file path
 * @param {string} filePath - Path to the .xdc file
 * @returns {Promise<MiniAppInfo>} Information about the Mini App
 */
async function loadMiniAppInfo(filePath) {
    const { invoke } = window.__TAURI__.core;
    return await invoke('miniapp_load_info', { filePath });
}

/**
 * Load information about the Mini App whose bytes the composer just cached
 * (`cache_file_bytes`), so the archive never crosses IPC a second time.
 * @returns {Promise<MiniAppInfo>} Information about the Mini App
 */
async function loadMiniAppInfoFromCachedFile() {
    const { invoke } = window.__TAURI__.core;
    return await invoke('miniapp_load_info_from_cached_file');
}

// The network and account a Mini App window was last allowed under (macOS and Windows).
let _miniAppWindowsAllowed = '';

/**
 * Before a Mini App launches off Clearnet. On macOS and Windows its window has no network-level
 * block, so it is asked about once per network and account. A realtime app needs the account's
 * consent to connect outside the network, as a call does.
 * @param {string} filePath - Path to the .xdc about to launch
 * @returns {Promise<boolean>} true to proceed with the launch
 */
async function confirmMiniAppNetwork(filePath) {
    const { invoke } = window.__TAURI__.core;
    let view = null;
    try {
        view = await invoke('transport_get_state');
    } catch (_) { /* ask below, as off Clearnet */ }
    if (view && view.kind === 'clearnet') return true;
    const label = view?.label || 'the network in use';

    const unblocked = platformFeatures.os === 'macos' || platformFeatures.os === 'windows';
    const key = `${view?.kind}:${strPubkey}`;
    const askWindow = unblocked && _miniAppWindowsAllowed !== key;
    const windowLine = `On this device, a Mini App could reach the internet outside ${escapeHtml(label)}.`;

    let usesRealtime = false;
    if (!view?.realtime_allowed) {
        try {
            usesRealtime = !!(await loadMiniAppInfo(filePath))?.uses_realtime;
        } catch (e) {
            usesRealtime = true; // can't tell → ask rather than silently expose
        }
    }
    // A multiplayer app asks once: its window and its connection leave the network alike.
    if (usesRealtime) {
        if (!(await askRealtimeConsent(askWindow ? windowLine : ''))) return false;
        if (askWindow) _miniAppWindowsAllowed = key;
        return true;
    }
    if (askWindow) {
        const ok = await popupConfirm(
            'Open this Mini App?',
            `${windowLine}<br>Allowed until you close Vector, switch accounts or change networks.`,
            false, '', '', '', 'Open',
        );
        if (!ok) return false;
        _miniAppWindowsAllowed = key;
    }
    return true;
}

/**
 * Open a Mini App in a new window
 * @param {string} filePath - Path to the .xdc file
 * @param {string} chatId - The chat ID this Mini App is associated with (optional)
 * @param {string} messageId - The message ID containing this Mini App (optional)
 * @param {string} href - Deep link path from update.href (optional) - will be appended to root URL
 * @param {string} topicId - The webxdc-topic from the message (optional) - for realtime channel isolation
 * @returns {Promise<void>}
 */
async function openMiniApp(filePath, chatId = '', messageId = '', href = null, topicId = null) {
    const { invoke } = window.__TAURI__.core;
    if (!(await confirmMiniAppNetwork(filePath))) {
        return false; // the user declined: nothing opened
    }
    messageId = miniAppLaunchIds.get(messageId) || messageId;
    await invoke('miniapp_open', { filePath, chatId, messageId, href, topicId });
    return true;
}

/**
 * The window of an app launched with a DM send is keyed under the send's pending id, and the
 * bubble carries the final id once the send settles; the row's click has to find that window.
 * @type {Map<string, string>} final id -> pending id
 */
const miniAppLaunchIds = new Map();
function noteMiniAppIdSwap(pendingId, eventId) {
    if (pendingId && eventId && pendingId !== eventId) miniAppLaunchIds.set(eventId, pendingId);
}

/**
 * Close a Mini App window
 * @param {string} chatId - The chat ID
 * @param {string} messageId - The message ID
 * @returns {Promise<void>}
 */
async function closeMiniApp(chatId, messageId) {
    const { invoke } = window.__TAURI__.core;
    return await invoke('miniapp_close', { chatId, messageId });
}

/**
 * Listen for Mini App update events
 * @param {function} callback - Called when a Mini App sends an update
 * @returns {Promise<function>} Unsubscribe function
 */
async function onMiniAppUpdate(callback) {
    const { listen } = window.__TAURI__.event;
    return await listen('miniapp_update_sent', (event) => {
        callback(event.payload);
    });
}

// ============================================================================
// Realtime Channel Functions (Iroh P2P)
// These are used by the main window to coordinate peer discovery via Nostr
// ============================================================================

/**
 * Listen for realtime channel events from Mini Apps
 * Used to coordinate peer discovery via Nostr
 * @param {function} callback - Called when a Mini App joins/leaves realtime channel
 * @returns {Promise<function>} Unsubscribe function
 */
async function onRealtimeChannelEvent(callback) {
    const { listen } = window.__TAURI__.event;
    return await listen('miniapp_realtime_event', (event) => {
        callback(event.payload);
    });
}

/**
 * @typedef {Object} MiniAppInfo
 * @property {string} id - Unique identifier
 * @property {string} name - Display name
 * @property {string} description - Description
 * @property {string} version - Version string
 * @property {boolean} hasIcon - Whether the app has an icon
 */

/**
 * @typedef {Object} RealtimeChannelEvent
 * @property {string} type - Event type: 'joined' | 'left' | 'data'
 * @property {string} chatId - The chat ID
 * @property {string} messageId - The message ID
 * @property {string} topicId - The Iroh topic ID
 * @property {string} [nodeAddr] - Our node address (for 'joined' events)
 */