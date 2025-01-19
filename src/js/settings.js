/**
 * A GUI wrapper to ask the user for a username, and apply it both
 * in-app and on the Nostr network.
 */
async function askForUsername() {
    const strUsername = await popupConfirm('Choose a Username', `This lets Chatstr users identify you easier!`, false, 'New Username');
    if (!strUsername) return;

    // Display the change immediately
    const cProfile = arrProfiles.find(a => a.mine);
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
    const cProfile = arrProfiles.find(a => a.mine);
    cProfile.avatar = strURL;
    renderCurrentProfile(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_profile", { name: "", avatar: strURL });
    } catch (e) {
        await popupConfirm('Avatar Update Failed!', 'An error occurred while updating your Avatar, the change may not have committed to the network, you can re-try any time.', true);
    }
}