const { open } = window.__TAURI__.dialog;

let MAX_AUTO_DOWNLOAD_BYTES = 10_485_760;

/**
 * @type {VoiceSettings}
 */
let cTranscriber = null;

class VoiceSettings {
    constructor() {
        this.models = [
            { id: 'tiny', name: 'Tiny', description: 'Fastest, least accurate', downloaded: false, downloading: false },
            { id: 'base', name: 'Base', description: 'Fast, decent accuracy', downloaded: false, downloading: false },
            { id: 'small', name: 'Small', description: 'Slower, better accuracy', downloaded: false, downloading: false },
            { id: 'medium', name: 'Medium', description: 'Slow, good accuracy', downloaded: false, downloading: false },
            { id: 'large-v3', name: 'Large', description: 'Very slow, best accuracy', downloaded: false, downloading: false }
        ];
        this.initVoiceSettings();
    }

    async initVoiceSettings() {
        const voiceSection = document.getElementById('settings-voice');
        if (voiceSection) {
            voiceSection.style.display = 'block';
            
            // Add event listeners
            document.getElementById('whisper-model').addEventListener('change', (e) => {
                cTranscriber.selectedModel = e.target.value;
                this.updateModelStatus();
            });
            
            document.getElementById('download-model').addEventListener('click', () => {
                this.downloadModel(cTranscriber.selectedModel);
            });
            
            await this.checkDownloadedModels();
            this.updateModelStatus();
        }
    }

    async checkDownloadedModels() {
        try {
            const downloadedModels = await invoke('list_models');
            this.models.forEach(model => {
                model.downloaded = downloadedModels.find(m => m.name === model.id).downloaded;
            });
            this.updateModelStatus();
        } catch (err) {
            console.error('Error checking downloaded models:', err);
        }
    }

    updateModelStatus() {
        const statusElement = document.getElementById('model-status');
        if (!statusElement) return;
        
        const model = this.models.find(m => m.id === cTranscriber.selectedModel);
        if (!model) return;
        
        if (model.downloading) {
            statusElement.innerHTML = `<div class="alert alert-info">Downloading ${model.name} model...</div>`;
            return;
        }
        
        if (model.downloaded) {
            statusElement.innerHTML = `<div class="alert alert-success">${model.name} model is downloaded and ready</div>`;
            document.getElementById('download-model').style.display = 'none';
        } else {
            statusElement.innerHTML = `<div class="alert alert-warning">${model.name} model is not downloaded</div>`;
            document.getElementById('download-model').style.display = '';
        }
    }

    async downloadModel(modelId) {
        const model = this.models.find(m => m.id === modelId);
        if (!model || model.downloading || model.downloaded) return;
        
        model.downloading = true;
        this.updateModelStatus();
        
        try {
            await invoke('download_model', { modelId });
            model.downloaded = true;
        } catch (err) {
            console.error('Error downloading model:', err);
        } finally {
            model.downloading = false;
            this.updateModelStatus();
        }
    }
}

// Initialize voice settings when DOM is ready
document.addEventListener('DOMContentLoaded', () => {
    window.voiceSettings = new VoiceSettings();
});

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
async function setTheme(theme = 'vector', mode = 'dark') {
    domTheme.href = `/themes/${theme}/${mode}.css`;

    // Ensure the value of the Theme Selector matches (i.e: at bootup during theme load)
    domSettingsThemeSelect.value = theme;

    // Save the selection to DB
    await invoke('set_theme', { theme: theme });
}

// Apply Theme changes in real-time
domSettingsThemeSelect.onchange = (evt) => {
    setTheme(evt.target.value);
};

// Listen for Logout clicks
domSettingsLogout.onclick = async (evt) => {
    // Prompt for confirmation
    const fConfirm = await popupConfirm('Going Incognito?', 'Logging out of Vector will fully erase the database, <b>ensure you have a backup of your keys before logging out!</b><br><br>That said, would you like to continue?');
    if (!fConfirm) return;

    // Begin the logout sequence
    await invoke('logout');
}