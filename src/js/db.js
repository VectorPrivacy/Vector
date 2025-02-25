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
 * `true` if a local encrypted key exists, `false` otherwise.
 * @returns {Promise<boolean>}
 */
async function hasKey() {
    return await invoke('get_pkey') !== null;
}

/**
 * Nuke our Private Key, particularly as a "log out" feature
 */
async function deleteKey() {
    await await invoke('delete_setting', { key: 'pkey' });
}