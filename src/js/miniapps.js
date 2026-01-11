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
 * Load information about a Mini App from bytes (in-memory)
 * This is more efficient for preview when the file is already in memory
 * @param {Uint8Array|number[]} bytes - The .xdc file bytes
 * @param {string} fileName - The file name (used as fallback for app name)
 * @returns {Promise<MiniAppInfo>} Information about the Mini App
 */
async function loadMiniAppInfoFromBytes(bytes, fileName) {
    const { invoke } = window.__TAURI__.core;
    // Convert to array if it's a Uint8Array
    const byteArray = bytes instanceof Uint8Array ? Array.from(bytes) : bytes;
    return await invoke('miniapp_load_info_from_bytes', { bytes: byteArray, fileName });
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
    return await invoke('miniapp_open', { filePath, chatId, messageId, href, topicId });
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
 * List all currently open Mini App instances
 * @returns {Promise<MiniAppInfo[]>}
 */
async function listOpenMiniApps() {
    const { invoke } = window.__TAURI__.core;
    return await invoke('miniapp_list_open');
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

/**
 * Check if a file is a Mini App (.xdc file)
 * @param {string} filePath - Path to check
 * @returns {boolean}
 */
function isMiniAppFile(filePath) {
    return filePath.toLowerCase().endsWith('.xdc');
}

// ============================================================================
// Realtime Channel Functions (Iroh P2P)
// These are used by the main window to coordinate peer discovery via Nostr
// ============================================================================

/**
 * Get our Iroh node address for sharing with peers via Nostr
 * This should be called when joining a realtime channel to advertise our presence
 * @returns {Promise<string>} Encoded node address
 */
async function getRealtimeNodeAddr() {
    const { invoke } = window.__TAURI__.core;
    return await invoke('miniapp_get_realtime_node_addr');
}

/**
 * Add a peer to a Mini App's realtime channel
 * Called when receiving a peer advertisement via Nostr
 * @param {string} chatId - The chat ID
 * @param {string} messageId - The message ID
 * @param {string} peerAddr - Encoded peer node address
 * @returns {Promise<void>}
 */
async function addRealtimePeer(chatId, messageId, peerAddr) {
    const { invoke } = window.__TAURI__.core;
    // Note: This needs to be called from the Mini App window context
    // For now, we'll need to emit an event to the Mini App window
    // TODO: Implement cross-window peer addition
    console.log('addRealtimePeer called:', chatId, messageId, peerAddr);
}

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