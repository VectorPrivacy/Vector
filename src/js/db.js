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
    await invoke('set_whisper_model_name', { name: name });
}

/**
 * Load the user-selected Whisper Model ID
 * @returns {Promise<string>} - The model ID
 */
async function loadChosenWhisperModel() {
    return (await invoke('get_whisper_model_name') || '');
}

/**
 * Load the user's Whisper Auto Translate setting
 * @returns {Promise<boolean>}
 */
async function loadWhisperAutoTranslate() {
    return (await invoke('get_whisper_auto_translate') || false);
}

/**
 * Set the user's Whisper Auto Translate setting
 * @param {boolean} bool - `true` to enable, `false` to disable
 */
async function saveWhisperAutoTranslate(bool) {
    return await invoke('set_whisper_auto_translate', { to: bool });
}

/**
 * Load the user's Whisper Auto Transcribe setting
 * @returns {Promise<boolean>}
 */
async function loadWhisperAutoTranscribe() {
    return (await invoke('get_whisper_auto_transcribe') || false);
}

/**
 * Set the user's Whisper Auto Transcribe setting
 * @param {boolean} bool - `true` to enable, `false` to disable
 */
async function saveWhisperAutoTranscribe(bool) {
    return await invoke('set_whisper_auto_transcribe', { to: bool });
}

/**
 * `true` if a local encrypted key exists, `false` otherwise.
 * @returns {Promise<boolean>}
 */
async function hasKey() {
    return await invoke('get_pkey') !== null;
}