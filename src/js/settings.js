const { open } = window.__TAURI__.dialog;

/**
 * A GUI wrapper to ask the user for a username, and apply it both
 * in-app and on the Nostr network.
 */
async function askForUsername() {
    const strUsername = await popupConfirm('Choose a Username', `This lets Vector users identify you easier!`, false, 'New Username');
    if (!strUsername) return;

    // Display the change immediately
    const cProfile = arrChats.find(a => a.mine);
    cProfile.name = strUsername;
    renderCurrentProfile(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_profile", { name: strUsername, avatar: "" });
    } catch (e) {
        await popupConfirm('Username Update Failed!', 'An error occurred while updating your Username, the change may not have committed to the network, you can re-try any time.', true);
    }
}

/**
 * A GUI wrapper to ask the user for a avatar URL, and apply it both
 * in-app and on the Nostr network.
 */
async function askForAvatar() {
    // Prompt the user to select an image file
    const file = await open({
        title: 'Choose an Avatar',
        multiple: false,
        directory: false,
        filters: [{
            name: 'Image',
            extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp']
        }]
    });
    if (!file) return;

    // Upload the avatar to a NIP-96 server
    let strUploadURL = '';
    try {
        strUploadURL = await invoke("upload_avatar", { filepath: file });
    } catch (e) {
        return await popupConfirm('Avatar Upload Failed!', e, true);
    }

    // Display the change immediately
    const cProfile = arrChats.find(a => a.mine);
    cProfile.avatar = strUploadURL;
    renderCurrentProfile(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_profile", { name: "", avatar: strUploadURL });
    } catch (e) {
        return await popupConfirm('Avatar Update Failed!', e, true);
    }
}

/**
 * A GUI wrapper to ask the user for a status, and apply it both
 * in-app and on the Nostr network.
 */
async function askForStatus() {
    const strStatus = await popupConfirm('Status', `Set a public status for everyone to see`, false, 'Custom Status');
    if (!strStatus) return;

    // Display the change immediately
    const cProfile = arrChats.find(a => a.mine);
    cProfile.status.title = strStatus;
    renderCurrentProfile(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_status", { status: strStatus, });
    } catch (e) {
        await popupConfirm('Status Update Failed!', 'An error occurred while updating your status, the change may not have committed to the network, you can re-try any time.', true);
    }
}

/**
 * A GUI wrapper to ask the user for a file path.
 */
async function selectFile() {
    const file = await open({
        multiple: false,
        directory: false,
    });
    return file || "";
}

/**
 * Set the theme of the app by hot-swapping theme CSS files
 * @param {string} theme - The theme name, i.e: `vector`, `chatstr`
 * @param {string} mode - The theme mode, i.e: `light`, `dark`
 */
function setTheme(theme = 'vector', mode = 'dark') {
    domTheme.href = `/themes/${theme}/${mode}.css`;

    // Ensure the value of the Theme Selector matches (i.e: at bootup during theme load)
    domSettingsThemeSelect.value = theme;

    // Save the selection to DB
    setKey('theme', theme);
}

// Apply Theme changes in real-time
domSettingsThemeSelect.onchange = (evt) => {
    setTheme(evt.target.value);
};