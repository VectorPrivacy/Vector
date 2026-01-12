/**
 * Encrypt and Save our Nostr Secret Key (bech32)
 * @param {string} pkey - Bech32 Nostr Secret Key
 * @param {string} password - Human Readable Password (pin or password)
 */
async function saveAndEncryptPrivateKey(pkey, password) {
    const encHexPayload = await invoke('encrypt', { input: pkey, password });
    await invoke('set_pkey', { pkey: encHexPayload });
}

/**
 * Load our encrypted Private Key, and attempt to decrypt it with our password
 * @param {string} password - Human Readable Password (pin or password)
 * @returns {Promise<string>} - Decrypted Private Key (or imminent explosion)
 */
async function loadAndDecryptPrivateKey(password) {
    const encPkey = await invoke('get_pkey');
    const pkey = await invoke('decrypt', { ciphertext: encPkey, password });
    return pkey;
}

/**
 * Save the user-selected Whisper Model ID
 * @param {string} name - The model ID
 */
async function saveChosenWhisperModel(name) {
    await invoke('set_sql_setting', { key: 'whisper_model_name', value: name });
}

/**
 * Load the user-selected Whisper Model ID
 * @returns {Promise<string>} - The model ID
 */
async function loadChosenWhisperModel() {
    return (await invoke('get_sql_setting', { key: 'whisper_model_name' }) || '');
}

/**
 * Save the user's theme preference to database
 * @param {string} theme - The theme name (e.g., 'vector', 'chatstr')
 */
async function saveTheme(theme) {
    return await invoke('set_sql_setting', { key: 'theme', value: theme });
}

/**
 * Load the user's Whisper Auto Translate setting
 * @returns {Promise<boolean>}
 */
async function loadWhisperAutoTranslate() {
    const value = await invoke('get_sql_setting', { key: 'whisper_auto_translate' });
    if (value === null || value === undefined) return false; // Default
    return value === 'true' || value === '1';
}

/**
 * Set the user's Whisper Auto Translate setting
 * @param {boolean} bool - `true` to enable, `false` to disable
 */
async function saveWhisperAutoTranslate(bool) {
    return await invoke('set_sql_setting', { key: 'whisper_auto_translate', value: bool ? 'true' : 'false' });
}

/**
 * Load the user's Whisper Auto Transcribe setting
 * @returns {Promise<boolean>}
 */
async function loadWhisperAutoTranscribe() {
    const value = await invoke('get_sql_setting', { key: 'whisper_auto_transcribe' });
    if (value === null || value === undefined) return false; // Default
    return value === 'true' || value === '1';
}

/**
 * Set the user's Whisper Auto Transcribe setting
 * @param {boolean} bool - `true` to enable, `false` to disable
 */
async function saveWhisperAutoTranscribe(bool) {
    return await invoke('set_sql_setting', { key: 'whisper_auto_transcribe', value: bool ? 'true' : 'false' });
}

/**
 * Load the user's Web Previews setting
 * @returns {Promise<boolean>}
 */
async function loadWebPreviews() {
    const value = await invoke('get_sql_setting', { key: 'web_previews' });
    if (value === null || value === undefined) return true; // Default to enabled
    return value === 'true' || value === '1';
}

/**
 * Set the user's Web Previews setting
 * @param {boolean} bool - `true` to enable, `false` to disable
 */
async function saveWebPreviews(bool) {
    return await invoke('set_sql_setting', { key: 'web_previews', value: bool ? 'true' : 'false' });
}

/**
 * Load the user's Strip Tracking setting
 * @returns {Promise<boolean>}
 */
async function loadStripTracking() {
    const value = await invoke('get_sql_setting', { key: 'strip_tracking' });
    if (value === null || value === undefined) return true; // Default to enabled
    return value === 'true' || value === '1';
}

/**
 * Set the user's Strip Tracking setting
 * @param {boolean} bool - `true` to enable, `false` to disable
 */
async function saveStripTracking(bool) {
    return await invoke('set_sql_setting', { key: 'strip_tracking', value: bool ? 'true' : 'false' });
}

/**
 * Load the user's Send Typing Indicators setting
 * @returns {Promise<boolean>}
 */
async function loadSendTypingIndicators() {
    const value = await invoke('get_sql_setting', { key: 'send_typing_indicators' });
    if (value === null || value === undefined) return true; // Default to enabled
    return value === 'true' || value === '1';
}

/**
 * Set the user's Send Typing Indicators setting
 * @param {boolean} bool - `true` to enable, `false` to disable
 */
async function saveSendTypingIndicators(bool) {
    return await invoke('set_sql_setting', { key: 'send_typing_indicators', value: bool ? 'true' : 'false' });
}

// ============================================================================
// Notification Sound Settings
// ============================================================================

/**
 * @typedef {Object} NotificationSettings
 * @property {boolean} global_mute - Whether all notification sounds are muted
 * @property {Object} sound - The notification sound setting
 */

/**
 * Load notification settings from the backend
 * @returns {Promise<NotificationSettings>}
 */
async function loadNotificationSettings() {
    return await invoke('get_notification_settings');
}

/**
 * Save notification settings to the backend
 * @param {NotificationSettings} settings - The notification settings to save
 * @returns {Promise<void>}
 */
async function saveNotificationSettings(settings) {
    return await invoke('set_notification_settings', { settings });
}

/**
 * Preview a notification sound
 * @param {Object} sound - The sound to preview (e.g., { type: 'Default' } or { type: 'Custom', path: '/path/to/file' })
 * @returns {Promise<void>}
 */
async function previewNotificationSound(sound) {
    return await invoke('preview_notification_sound', { sound });
}

/**
 * Open file picker to select a custom notification sound
 * @returns {Promise<string>} - The path to the selected sound file
 */
async function selectCustomNotificationSound() {
    return await invoke('select_custom_notification_sound');
}

/**
 * Check if any account exists.
 * @returns {Promise<boolean>}
 */
async function hasAccount() {
    return await invoke('check_any_account_exists');
}

// ============================================================================
// Custom Relay Management
// ============================================================================

/**
 * @typedef {Object} CustomRelay
 * @property {string} url - The relay WebSocket URL
 * @property {boolean} enabled - Whether the relay is enabled
 */

/**
 * Get the list of user's custom relays
 * @returns {Promise<CustomRelay[]>}
 */
async function loadCustomRelays() {
    return await invoke('get_custom_relays');
}

/**
 * Add a new custom relay
 * @param {string} url - The relay WebSocket URL (must start with wss://)
 * @param {string} [mode='both'] - The relay mode: 'read', 'write', or 'both'
 * @returns {Promise<CustomRelay>} - The newly created relay entry
 * @throws {Error} If the URL is invalid or relay already exists
 */
async function addCustomRelay(url, mode = 'both') {
    return await invoke('add_custom_relay', { url, mode });
}

/**
 * Remove a custom relay
 * @param {string} url - The relay URL to remove
 * @returns {Promise<boolean>} - True if removed, false if not found
 */
async function removeCustomRelay(url) {
    return await invoke('remove_custom_relay', { url });
}

/**
 * Toggle a custom relay's enabled state
 * @param {string} url - The relay URL
 * @param {boolean} enabled - Whether to enable or disable the relay
 * @returns {Promise<boolean>} - True if successful
 */
async function toggleCustomRelay(url, enabled) {
    return await invoke('toggle_custom_relay', { url, enabled });
}

/**
 * Toggle a default relay's enabled state
 * @param {string} url - The relay URL
 * @param {boolean} enabled - Whether to enable or disable the relay
 * @returns {Promise<boolean>} - True if successful
 */
async function toggleDefaultRelay(url, enabled) {
    return await invoke('toggle_default_relay', { url, enabled });
}

/**
 * Update a custom relay's mode
 * @param {string} url - The relay URL
 * @param {string} mode - The new mode: 'read', 'write', or 'both'
 * @returns {Promise<boolean>} - True if successful
 */
async function updateRelayMode(url, mode) {
    return await invoke('update_relay_mode', { url, mode });
}

/**
 * Validate a relay URL format without saving
 * @param {string} url - The relay URL to validate
 * @returns {Promise<string>} - The normalized URL if valid
 * @throws {Error} If the URL is invalid
 */
async function validateRelayUrl(url) {
    return await invoke('validate_relay_url_cmd', { url });
}

/**
 * Get metrics for a specific relay
 * @param {string} url - The relay URL
 * @returns {Promise<RelayMetrics>} - The relay metrics
 */
async function getRelayMetrics(url) {
    return await invoke('get_relay_metrics', { url });
}

/**
 * Get recent logs for a specific relay
 * @param {string} url - The relay URL
 * @returns {Promise<RelayLog[]>} - Array of recent log entries
 */
async function getRelayLogs(url) {
    return await invoke('get_relay_logs', { url });
}