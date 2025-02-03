const { appDataDir } = window.__TAURI__.path;
const { load } = window.__TAURI__.store;

let store;

/**
 * Encrypt and Save our Nostr Secret Key (bech32)
 * @param {string} pkey - Bech32 Nostr Secret Key
 * @param {string} password - Human Readable Password (pin or password)
 */
async function saveAndEncryptPrivateKey(pkey, password) {
    const encHexPayload = await invoke('encrypt', { input: pkey, password });
    await store.set('pkey', encHexPayload);
}

/**
 * Load our encrypted Private Key, and attempt to decrypt it with our password
 * @param {string} password - Human Readable Password (pin or password)
 * @returns {Promise<string>} - Decrypted Private Key (or imminent explosion)
 */
async function loadAndDecryptPrivateKey(password) {
    const encPkey = await store.get('pkey');
    const pkey = await invoke('decrypt', { ciphertext: encPkey, password });
    return pkey;
}

/**
 * `true` if a local encrypted key exists, `false` otherwise.
 * @returns {Promise<boolean>}
 */
async function hasKey() {
    return await store.has('pkey');
}

/**
 * Nuke our Private Key, particularly as a "log out" feature
 */
async function deleteKey() {
    await store.delete('pkey');
}

/**
 * Get a string value via DB key
 * @param {string} key - The key to fetch from the DB
 */
async function getKey(key) {
    return await store.get(key);
}

/**
 * Set a string value via DB key
 * @param {string} key - The key we're setting
 * @param {string} value - The value to set
 */
async function setKey(key, value) {
    await store.set(key, value);
}