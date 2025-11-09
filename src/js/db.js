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

/**
 * Check if any account exists (SQL or Store-based).
 * Checks both SQL accounts and Store-based accounts (for migration).
 * @returns {Promise<boolean>}
 */
async function hasAccount() {
    return await invoke('check_any_account_exists');
}