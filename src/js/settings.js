/**
 * A GUI wrapper to ask the user for a username, and apply it both
 * in-app and on the Nostr network.
 */
async function askForUsername() {
    const strUsername = await popupConfirm('Choose a Username', `This lets Chatstr users identify you easier!`, false, 'New Username');
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
    const strURL = await popupConfirm('Choose an Avatar', `Use an image URL as your avatar.<br><i style="opacity: 0.6">In the future, in-app avatar uploading will be supported, hang tight!</i>`, false, 'An Image URL');
    if (!strURL) return;

    // Display the change immediately
    const cProfile = arrChats.find(a => a.mine);
    cProfile.avatar = strURL;
    renderCurrentProfile(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_profile", { name: "", avatar: strURL });
    } catch (e) {
        await popupConfirm('Avatar Update Failed!', 'An error occurred while updating your Avatar, the change may not have committed to the network, you can re-try any time.', true);
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