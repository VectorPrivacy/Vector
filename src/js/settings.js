const { open } = window.__TAURI__.dialog;

let MAX_AUTO_DOWNLOAD_BYTES = 10_485_760;

/**
 * Platform features retrieved from the backend
 * @typedef {Object} PlatformFeatures
 * @property {boolean} transcription - Whether transcriptions are enabled
 * @property {"android" | "ios" | "macos" | "windows" | "linux" | "unknown"} os - The operating system
 */

/** @type {PlatformFeatures} */
let platformFeatures = null;

/**
 * Fetch platform features from the backend
 */
async function fetchPlatformFeatures() {
    platformFeatures = await invoke("get_platform_features");
}

/**
 * @type {VoiceTranscriptionUI}
 */
let cTranscriber = null;

class VoiceSettings {
    constructor() {
        this.models = [];
        this.autoTranslate = false;
        this.autoTranscribe = false;
        this.selectedModel = 'small'; // Default model
    }

    async initVoiceSettings() {
        const voiceSection = document.getElementById('settings-voice');
        if (!voiceSection) return;

        // Only show voice settings if transcription is supported
        if (!platformFeatures.transcription) {
            voiceSection.style.display = 'none';
            return;
        }

        voiceSection.style.display = 'block';

        // Load our Settings from disk (or use a default value)
        const strModelID = await loadChosenWhisperModel() || this.selectedModel;
        this.autoTranslate = await loadWhisperAutoTranslate();
        this.autoTranscribe = await loadWhisperAutoTranscribe();

        // Set initial toggle states (will be loaded from backend DB in future)
        document.getElementById('auto-translate-toggle').checked = this.autoTranslate;
        document.getElementById('auto-transcribe-toggle').checked = this.autoTranscribe;
        
        // Update selectedModel to match the loaded model ID
        this.selectedModel = strModelID;
        document.getElementById('whisper-model').value = strModelID;

        this.updateModelStatus();
        this.setupEventListeners();
    }

    setupEventListeners() {
        // Model selection change
        document.getElementById('whisper-model').addEventListener('change', async (e) => {
            this.selectedModel = e.target.value;
            this.updateModelStatus();
            this.updateDeleteButton();
            await this.setSelectedModel(e.target.value);
        });
        
        // Model download
        document.getElementById('download-model').addEventListener('click', async () => {
            const modelName = document.getElementById('whisper-model').value;
            await this.downloadModel(modelName);
        });

        // Toggle event listeners
        document.getElementById('auto-translate-toggle').addEventListener('change', async (e) => {
            this.autoTranslate = e.target.checked;
            await this.setAutoTranslate(e.target.checked);
        });

        document.getElementById('auto-transcribe-toggle').addEventListener('change', async (e) => {
            this.autoTranscribe = e.target.checked;
            await this.setAutoTranscribe(e.target.checked);
        });

        // Model deletion
        const deleteBtn = document.getElementById('delete-model');
        if (deleteBtn) {
            deleteBtn.addEventListener('click', () => this.deleteSelectedModel());
        }
    }

    async loadWhisperModels() {
        const modelSelect = document.getElementById('whisper-model');
        const modelStatus = document.getElementById('model-status');
        
        try {
            // Store the currently selected model before rebuilding the dropdown
            const currentSelection = modelSelect.value;
            
            // Show loading state while fetching models from backend
            modelSelect.innerHTML = '<option value="" disabled selected>Loading models...</option>';
            
            // Fetch available models from Tauri backend
            this.models = await invoke('list_models');
            modelSelect.innerHTML = ''; // Clear loading message
            
            // Create model hierarchy dynamically from model sizes (lowest to highest quality)
            const modelHierarchy = this.models
                .slice() // Create a copy to avoid mutating original
                .sort((a, b) => a.model.size - b.model.size) // Sort by size (smaller = lower quality)
                .map(m => m.model.name);
            
            // Track if we need to select a fallback model
            let foundCurrentSelection = false;
            let selectedModel = null;
            
            // Populate dropdown with all available models
            this.models.forEach(modelState => {
                const option = document.createElement('option');
                option.value = modelState.model.name;
                option.textContent = modelState.model.display_name;
                
                // If this was the previously selected model and it's still downloaded, keep it selected
                if (currentSelection === modelState.model.name && modelState.downloaded) {
                    foundCurrentSelection = true;
                    option.selected = true;
                    selectedModel = modelState.model.name;
                }
                
                modelSelect.appendChild(option);
            });
            
            // If the previously selected model is no longer available, find the best fallback
            if (!foundCurrentSelection) {
                selectedModel = this.findBestFallbackModel(currentSelection, modelHierarchy);
                
                const fallbackOption = Array.from(modelSelect.options).find(opt => opt.value === selectedModel);
                if (fallbackOption) {
                    fallbackOption.selected = true;
                }
            }
            
            // Update this.selectedModel to match the UI selection
            this.selectedModel = selectedModel || this.selectedModel;
            
            // Update UI elements based on the selected model
            this.updateDeleteButton();
            modelStatus.textContent = '';
        } catch (error) {
            // Handle errors by showing error state in UI
            modelSelect.innerHTML = '<option value="" disabled>Error loading models</option>';
            modelStatus.textContent = `Error: ${error.message}`;
            console.error('Failed to load models:', error);
        }
    }

    updateDeleteButton() {
        const modelSelect = document.getElementById('whisper-model');
        const deleteBtn = document.getElementById('delete-model');
        const selectedModel = modelSelect.value;
        
        // Hide delete button if no model is selected
        if (!selectedModel) {
            deleteBtn.style.display = 'none';
            return;
        }
        
        // Find the model data for the selected model
        const model = this.models.find(m => m.model.name === selectedModel);
        
        // Only show delete button for downloaded models (can't delete what isn't there)
        if (model?.downloaded) {
            deleteBtn.style.display = 'block';
            deleteBtn.classList.add('downloaded');
            deleteBtn.title = `Delete ${model.model.display_name}`;
        } else {
            deleteBtn.style.display = 'none';
            deleteBtn.classList.remove('downloaded');
        }
    }

    async deleteSelectedModel() {
        const modelSelect = document.getElementById('whisper-model');
        const modelName = modelSelect.value;
        
        if (!modelName) {
            return;
        }
        
        // Confirm deletion
        const confirmDelete = await popupConfirm(
            'Delete Model?', 
            `Are you sure you want to delete the "${modelName}" model? This will free up disk space but you'll need to download it again to use it.`,
            false,
            '',
            'vector_warning.svg'
        );
        
        if (!confirmDelete) return;
        
        try {
            await invoke('delete_whisper_model', { modelName });
            await this.loadWhisperModels();
            this.updateModelStatus();
        } catch (error) {
            console.error('Failed to delete model:', error);
            await popupConfirm('Deletion Failed', `Could not delete model: ${error.message}`, true, '', 'vector_warning.svg');
        }
    }

    updateModelStatus() {
        const statusElement = document.getElementById('model-status');
        if (!statusElement) return;
        
        const model = this.models.find(m => m.model.name === this.selectedModel);
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
            const downloadBtn = document.getElementById('download-model');
            downloadBtn.style.display = '';
            
            // Update button text to include model size
            const sizeInBytes = model.model.size * 1024 * 1024;
            const formattedSize = formatBytes(sizeInBytes);
            downloadBtn.textContent = `Download Selected Model (${formattedSize})`;
        }
    }

    async downloadModel(modelName) {
        // Find the model in our cached list and validate it's not already downloaded
        const model = this.models.find(m => m.model.name === modelName);
        if (!model || model.downloaded) return;

        // Get UI elements for progress display
        const modelStatus = document.getElementById('model-status');
        const progressContainer = document.querySelector('.download-progress-container');
        const progressFill = document.querySelector('.progress-bar-fill');
        const progressText = document.querySelector('.progress-text');

        // Initialize download UI state
        modelStatus.textContent = `Downloading AI model...`;
        progressContainer.style.display = 'block';
        progressFill.style.width = '0%';
        progressText.textContent = '0%';
        
        // Disable UI during download to prevent user interference
        document.getElementById('download-model').style.display = 'none';
        document.getElementById('whisper-model').disabled = true;

        try {
            // Mark model as downloading and update status display
            model.downloading = true;
            this.updateModelStatus();

            // Set up Tauri event listener for download progress updates
            const unlisten = await window.__TAURI__.event.listen(
                'whisper_download_progress', 
                (event) => {
                    const progress = event.payload.progress;
                    progressFill.style.width = `${progress}%`;
                    progressText.textContent = `${progress}%`;
                }
            );

            // Start the actual download via Tauri backend
            await invoke('download_whisper_model', { modelName });

            // Clean up event listener to prevent memory leaks
            unlisten();

            // Update model state to reflect successful download
            model.downloaded = true;
            model.downloading = false;
            
            modelStatus.textContent = `Successfully downloaded AI model!`;
            
            // Show success animation with green gradient
            progressFill.style.width = `100%`;
            progressText.textContent = `100%`;
            progressFill.style.background = 'linear-gradient(90deg, #59fcb3 0%, #2b976c 100%)';
            progressContainer.style.animation = 'none';
            void progressContainer.offsetWidth; // Force reflow to reset animation
            progressContainer.style.animation = 'fadeIn 0.3s ease-out';
            
            // Refresh the models dropdown and update UI state
            await this.loadWhisperModels();
            this.updateModelStatus();
        } catch (error) {
            // Handle download failures
            model.downloading = false;
            modelStatus.textContent = `Error downloading model: ${error}`;
            console.error('Download failed:', error);
            
            // Show error state with red gradient
            progressFill.style.background = 'linear-gradient(90deg, #ff5e5e 0%, #d40000 100%)';
            progressText.textContent = 'Failed';
        } finally {
            // Re-enable model selector after download completes
            document.getElementById('whisper-model').disabled = false;
            
            // Clean up progress bar after a delay, regardless of success/failure
            setTimeout(() => {
                progressContainer.style.display = 'none';
                // Reset progress bar styling for next download
                progressFill.style.width = '0%';
                progressFill.style.background = 'linear-gradient(90deg, #59fcb3 0%, #00d4ff 100%)';
                progressText.textContent = '0%';
            }, 3000);
        }
    }

    async setAutoTranslate(enabled) {
        this.autoTranslate = enabled;
        
        // Update UI toggle
        const toggle = document.getElementById('auto-translate-toggle');
        if (toggle) {
            toggle.checked = enabled;
        }
        
        // Save to DB
        await saveWhisperAutoTranslate(enabled);
        
        console.log(`Auto-translate ${enabled ? 'enabled' : 'disabled'}`);
    }

    async setAutoTranscribe(enabled) {
        this.autoTranscribe = enabled;
        
        // Update UI toggle
        const toggle = document.getElementById('auto-transcribe-toggle');
        if (toggle) {
            toggle.checked = enabled;
        }
        
        // Save to DB
        await saveWhisperAutoTranscribe(enabled);
        
        console.log(`Auto-transcribe ${enabled ? 'enabled' : 'disabled'}`);
    }

    async setSelectedModel(modelName) {
        this.selectedModel = modelName;
        
        // Update UI dropdown
        const modelSelect = document.getElementById('whisper-model');
        if (modelSelect) {
            modelSelect.value = modelName;
        }
        
        // Save to DB
        await saveChosenWhisperModel(modelName);
        console.log(`Selected model set to: ${modelName}`);
    }

    /**
     * Find the best fallback model when the current selection is no longer available
     * @param {string} deletedModel - The model that was deleted or is no longer available
     * @param {string[]} modelHierarchy - Array of model names ordered from lowest to highest quality
     * @returns {string} The best fallback model name
     */
    findBestFallbackModel(deletedModel, modelHierarchy) {
        // Get all downloaded models
        const downloadedModels = this.models.filter(m => m.downloaded);
        
        if (downloadedModels.length === 0) {
            // No downloaded models, fallback to default 'small'
            return 'small';
        }
        
        // If we have a deleted model, find the next highest downloaded model
        if (deletedModel && modelHierarchy.includes(deletedModel)) {
            const deletedIndex = modelHierarchy.indexOf(deletedModel);
            
            // Look for next higher models first
            for (let i = deletedIndex + 1; i < modelHierarchy.length; i++) {
                const candidate = modelHierarchy[i];
                if (downloadedModels.some(m => m.model.name === candidate)) {
                    return candidate;
                }
            }
            
            // If no higher model found, look for lower models
            for (let i = deletedIndex - 1; i >= 0; i--) {
                const candidate = modelHierarchy[i];
                if (downloadedModels.some(m => m.model.name === candidate)) {
                    return candidate;
                }
            }
        }
        
        // If 'small' is downloaded, prefer it as default
        if (downloadedModels.some(m => m.model.name === 'small')) {
            return 'small';
        }
        
        // Otherwise, return the highest quality downloaded model
        for (let i = modelHierarchy.length - 1; i >= 0; i--) {
            const candidate = modelHierarchy[i];
            if (downloadedModels.some(m => m.model.name === candidate)) {
                return candidate;
            }
        }
        
        // Final fallback to 'small' if nothing else works
        return 'small';
    }
}

/**
 * A GUI wrapper to ask the user for a username, and apply it both
 * in-app and on the Nostr network.
 */
async function askForUsername() {
    const strUsername = await popupConfirm('Choose a Username', 'This lets Vector users identify you easier!', false, 'New Username');
    if (!strUsername) return;

    // Display the change immediately
    const cProfile = arrProfiles.find(a => a.mine);
    cProfile.name = strUsername;
    renderCurrentProfile(cProfile);
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_profile", { name: strUsername, avatar: "", banner: "", about: "" });
    } catch (e) {
        await popupConfirm('Username Update Failed!', 'An error occurred while updating your Username, the change may not have committed to the network, you can re-try any time.', true, '', 'vector_warning.svg');
    }
}

/**
 * Set the user's "About Me" field on the Nostr network.
 * @param {string} about - The new 'About Me' text to be set for the user
 */
async function setAboutMe(about) {
    // Send out the metadata update
    try {
        await invoke("update_profile", { name: "", avatar: "", banner: "", about: about });
    } catch (e) {
        await popupConfirm('About Me Update Failed!', 'An error occurred while updating your "About Me", the change may not have committed to the network, you can re-try any time.', true, '', 'vector_warning.svg');
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
        return await popupConfirm('Avatar Upload Failed!', e, true, '', 'vector_warning.svg');
    }

    // Display the change immediately
    const cProfile = arrProfiles.find(a => a.mine);
    cProfile.avatar = strUploadURL;
    renderCurrentProfile(cProfile);
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_profile", { name: "", avatar: strUploadURL, banner: "", about: "" });
    } catch (e) {
        return await popupConfirm('Avatar Update Failed!', e, true, '', 'vector_warning.svg');
    }
}

/**
 * A GUI wrapper to ask the user for a banner URL, and apply it both
 * in-app and on the Nostr network.
 */
async function askForBanner() {
    // Prompt the user to select an image file
    const file = await open({
        title: 'Choose a Banner',
        multiple: false,
        directory: false,
        filters: [{
            name: 'Image',
            extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp']
        }]
    });
    if (!file) return;

    // Upload the banner to a NIP-96 server
    let strUploadURL = '';
    try {
        strUploadURL = await invoke("upload_avatar", { filepath: file });
    } catch (e) {
        return await popupConfirm('Banner Upload Failed!', e, true, '', 'vector_warning.svg');
    }

    // Display the change immediately
    const cProfile = arrProfiles.find(a => a.mine);
    cProfile.banner = strUploadURL;
    renderCurrentProfile(cProfile);
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_profile", { name: "", avatar: "", banner: strUploadURL, about: "" });
    } catch (e) {
        return await popupConfirm('Banner Update Failed!', e, true, '', 'vector_warning.svg');
    }
}

/**
 * A GUI wrapper to ask the user for a status, and apply it both
 * in-app and on the Nostr network.
 */
async function askForStatus() {
    const strStatus = await popupConfirm('Status', 'Set a public status for everyone to see', false, 'Custom Status');
    if (strStatus === false) return;

    // Display the change immediately
    const cProfile = arrProfiles.find(a => a.mine);
    cProfile.status.title = strStatus;
    renderCurrentProfile(cProfile);
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    // Send out the metadata update
    try {
        await invoke("update_status", { status: strStatus });
    } catch (e) {
        await popupConfirm('Status Update Failed!', 'An error occurred while updating your status, the change may not have committed to the network, you can re-try any time.', true, '', 'vector_warning.svg');
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
  document.body.classList.remove('vector-theme', 'chatstr-theme');
  document.body.classList.add(`${theme}-theme`);
  
  domTheme.href = `/themes/${theme}/${mode}.css`;
  domSettingsThemeSelect.value = theme;
  await invoke('set_theme', { theme: theme });
}

// Apply Theme changes in real-time
domSettingsThemeSelect.onchange = (evt) => {
    setTheme(evt.target.value);
};

// Listen for Logout clicks
domSettingsLogout.onclick = async (evt) => {
    // Prompt for confirmation
    const fConfirm = await popupConfirm('Going Incognito?', 'Logging out of Vector will fully erase the database, <b>ensure you have a backup of your keys before logging out!</b><br><br>That said, would you like to continue?', false, '', 'vector_warning.svg');
    if (!fConfirm) return;

    // Begin the logout sequence
    await invoke('logout');
};

// Listen for Export Account clicks
domSettingsExport.onclick = async (evt) => {
    try {
        // Call the backend to export keys
        const keys = await invoke('export_keys');
        
        // Create the export content with security warnings
        let exportContent = `<h3>Account Export</h3>
            <p style="color: #ff2ea9; font-weight: bold;">SECURITY WARNING</p>
            <p>These are your private keys. Anyone with access to them can access your account.</p>
            <p>Store them securely and never share them with anyone.</p><br>`;

        // Add seed phrase first if available (prioritized for users)
        if (keys.seed_phrase) {
            exportContent += `<p><strong>Seed Phrase:</strong></p>
                <p style="word-break: break-all; background: #1a1a1a; padding: 10px; border-radius: 5px; font-family: 'Courier New', monospace;">${keys.seed_phrase}</p><br>`;
        }

        // Always add the private key (nsec)
        exportContent += `<p><strong>Private Key (nsec):</strong></p>
            <p style="word-break: break-all; background: #1a1a1a; padding: 10px; border-radius: 5px; font-family: 'Courier New', monospace;">${keys.nsec}</p>`;

        // Show the export information in a popup
        await popupConfirm('', exportContent, true, '', 'vector_warning.svg');
    } catch (error) {
        console.error('Export failed:', error);
        await popupConfirm('Export Failed', error.toString(), true, '', 'vector_warning.svg');
    }
};
