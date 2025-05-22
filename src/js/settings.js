const { open } = window.__TAURI__.dialog;

let MAX_AUTO_DOWNLOAD_BYTES = 10_485_760;

/**
 * @type {VoiceSettings}
 */
let cTranscriber = null;

class VoiceSettings {
    constructor() {
        this.models = [];
        this.autoTranslate = false;
        this.autoTranscript = false;
        this.initVoiceSettings();
    }

    async initVoiceSettings() {
        const voiceSection = document.getElementById('settings-voice');
        if (voiceSection) {
            // Show the section immediately (remove display:none)
            voiceSection.style.display = 'block';
            
            // Load models right away since we're showing the section
            await this.loadWhisperModels();

            // Load saved toggle states from localStorage
            this.autoTranslate = localStorage.getItem('autoTranslate') === 'true';
            this.autoTranscript = localStorage.getItem('autoTranscript') === 'true';
            
            // Set initial toggle states
            document.getElementById('auto-translate-toggle').checked = this.autoTranslate;
            document.getElementById('auto-transcript-toggle').checked = this.autoTranscript;

            // Add event listeners
            document.getElementById('whisper-model').addEventListener('change', (e) => {
                cTranscriber.selectedModel = e.target.value;
                this.updateModelStatus();
            });
            
            document.getElementById('download-model').addEventListener('click', async () => {
                const modelSelect = document.getElementById('whisper-model');
                const modelName = modelSelect.value;
                
                if (!modelName) {
                    alert('Please select a model first');
                    return;
                }

                await this.downloadModel(modelName);

               // Add toggle event listeners
                document.getElementById('auto-translate-toggle').addEventListener('change', (e) => {
                this.autoTranslate = e.target.checked;
                localStorage.setItem('autoTranslate', this.autoTranslate);
                });

                document.getElementById('auto-transcript-toggle').addEventListener('change', (e) => {
                this.autoTranscript = e.target.checked;
                localStorage.setItem('autoTranscript', this.autoTranscript);
                });

            });
        }
    }

    async loadWhisperModels() {
        const modelSelect = document.getElementById('whisper-model');
        const modelStatus = document.getElementById('model-status');
        
        try {
            modelSelect.innerHTML = '<option value="" disabled selected>Loading models...</option>';
            
            this.models = await invoke('list_models');
            modelSelect.innerHTML = ''; // Clear loading message
            
            this.models.forEach(modelState => {
                const option = document.createElement('option');
                option.value = modelState.model.name;
                option.textContent = `${modelState.model.display_name}`;
                                if (modelState.downloaded) {
                    option.selected = true;
                    // Set the transcriber's selected model
                    if (cTranscriber) {
                        cTranscriber.selectedModel = modelState.name;
                    }
                }
                modelSelect.appendChild(option);
            });
            
            modelStatus.textContent = ``;
        } catch (error) {
            modelSelect.innerHTML = '<option value="" disabled>Error loading models</option>';
            modelStatus.textContent = `Error: ${error.message}`;
            console.error('Failed to load models:', error);
        }
    }

    updateModelStatus() {
        const statusElement = document.getElementById('model-status');
        if (!statusElement) return;
        
        const model = this.models.find(m => m.model.name === cTranscriber?.selectedModel);
        if (!model) return;
        
        if (model.downloading) {
            statusElement.innerHTML = `<div class="alert alert-info">Downloading ${model.model.name} model... <span id="voice-model-download-progression">(0%)</span></div>`;
            return;
        }
        
        if (model.downloaded) {
            statusElement.innerHTML = `<div class="alert alert-success">Vector AI is ready</div>`;
            document.getElementById('download-model').style.display = 'none';
        } else {
            statusElement.innerHTML = `<div class="alert alert-warning">AI model is not downloaded</div>`;
            document.getElementById('download-model').style.display = '';
        }
    }

    async downloadModel(modelName) {
    const model = this.models.find(m => m.model.name === modelName);
    if (!model || model.downloaded) return;

    const modelStatus = document.getElementById('model-status');
    const progressContainer = document.querySelector('.download-progress-container');
    const progressFill = document.querySelector('.progress-bar-fill');
    const progressText = document.querySelector('.progress-text');

    // Initialize UI
    modelStatus.textContent = `Downloading AI model...`;
    progressContainer.style.display = 'block';
    progressFill.style.width = '0%';
    progressText.textContent = '0%';

    try {
        model.downloading = true;
        this.updateModelStatus();

        // Set up progress listener
        const unlisten = await window.__TAURI__.event.listen(
            'whisper_download_progress', 
            (event) => {
                const progress = event.payload.progress;
                progressFill.style.width = `${progress}%`;
                progressText.textContent = `${progress}%`;
            }
        );

        await invoke('download_whisper_model', { modelName });

        // Clean up listener
        unlisten();

        model.downloaded = true;
        model.downloading = false;
        modelStatus.textContent = `Successfully downloaded AI model!`;
        
        // Add completion animation
        progressFill.style.background = 'linear-gradient(90deg, #59fcb3 0%, #2b976c 100%)';
        progressContainer.style.animation = 'none';
        void progressContainer.offsetWidth; // Trigger reflow
        progressContainer.style.animation = 'fadeIn 0.3s ease-out';
        
        await this.loadWhisperModels();
    } catch (error) {
        model.downloading = false;
        modelStatus.textContent = `Error downloading model: ${error}`;
        console.error('Download failed:', error);
        
        // Error state styling
        progressFill.style.background = 'linear-gradient(90deg, #ff5e5e 0%, #d40000 100%)';
        progressText.textContent = 'Failed';
    } finally {
        // Hide progress bar after delay
        setTimeout(() => {
            progressContainer.style.display = 'none';
            // Reset progress bar for next use
            progressFill.style.width = '0%';
            progressFill.style.background = 'linear-gradient(90deg, #59fcb3 0%, #00d4ff 100%)';
            progressText.textContent = '0%';
        }, 3000);
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
    const strUsername = await popupConfirm('Choose a Username', 'This lets Vector users identify you easier!', false, 'New Username');
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
    const strStatus = await popupConfirm('Status', 'Set a public status for everyone to see', false, 'Custom Status');
    if (!strStatus) return;

    // Display the change immediately
    const cProfile = arrChats.find(a => a.mine);
    cProfile.status.title = strStatus;
    renderCurrentProfile(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_status", { status: strStatus });
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
 * @param {string} theme - The theme name, i.e: vector, chatstr
 * @param {string} mode - The theme mode, i.e: light, dark
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
};