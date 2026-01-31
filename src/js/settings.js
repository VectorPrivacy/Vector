const { open } = window.__TAURI__.dialog;

let MAX_AUTO_DOWNLOAD_BYTES = 10_485_760;

/**
 * Platform features retrieved from the backend
 * @typedef {Object} PlatformFeatures
 * @property {boolean} transcription - Whether transcriptions are enabled
 * @property {"android" | "ios" | "macos" | "windows" | "linux" | "unknown"} os - The operating system
 * @property {boolean} is_mobile - Whether the platform is mobile (Android or iOS)
 * @property {boolean} debug_mode - Whether the app is running in debug/development mode
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
    const oldName = cProfile.name;
    cProfile.name = strUsername;
    renderCurrentProfile(cProfile);
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    // Send out the metadata update
    try {
        const success = await invoke("update_profile", { name: strUsername, avatar: "", banner: "", about: "" });
        if (!success) {
            cProfile.name = oldName;
            renderCurrentProfile(cProfile);
            if (domProfile.style.display === '') renderProfileTab(cProfile);
            await popupConfirm('Username Update Failed!', 'Failed to broadcast profile update to the network.', true, '', 'vector_warning.svg');
        }
    } catch (e) {
        cProfile.name = oldName;
        renderCurrentProfile(cProfile);
        if (domProfile.style.display === '') renderProfileTab(cProfile);
        await popupConfirm('Username Update Failed!', e, true, '', 'vector_warning.svg');
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
            extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico']
        }]
    });
    if (!file) return;

    // Show upload progress spinner
    const avatarEditBtn = document.querySelector('.profile-avatar-edit');
    const avatarIcon = avatarEditBtn?.querySelector('.icon');
    let unlisten = null;

    if (avatarIcon) {
        // Replace icon with progress spinner
        avatarIcon.className = 'profile-upload-spinner';
        avatarIcon.style.setProperty('--progress', '5%');

        // Listen for progress events
        unlisten = await window.__TAURI__.event.listen('profile_upload_progress', (event) => {
            if (event.payload.type === 'avatar') {
                const progress = Math.max(5, event.payload.progress);
                avatarIcon.style.setProperty('--progress', `${progress}%`);
            }
        });
    }

    // Upload the avatar to a NIP-96 server
    let strUploadURL = '';
    try {
        strUploadURL = await invoke("upload_avatar", { filepath: file, uploadType: "avatar" });
    } catch (e) {
        // Restore icon on failure
        if (avatarIcon) avatarIcon.className = 'icon icon-plus-circle';
        if (unlisten) unlisten();
        return await popupConfirm('Avatar Upload Failed!', e, true, '', 'vector_warning.svg');
    }

    // Restore icon on success
    if (avatarIcon) avatarIcon.className = 'icon icon-plus-circle';
    if (unlisten) unlisten();

    // Display the change immediately
    const cProfile = arrProfiles.find(a => a.mine);
    const oldAvatar = cProfile.avatar;
    const oldAvatarCached = cProfile.avatar_cached;
    cProfile.avatar = strUploadURL;
    cProfile.avatar_cached = ''; // Clear stale cached image so new URL is used
    renderCurrentProfile(cProfile);
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    // Send out the metadata update
    try {
        const success = await invoke("update_profile", { name: "", avatar: strUploadURL, banner: "", about: "" });
        if (!success) {
            // Revert local change since network update failed
            cProfile.avatar = oldAvatar;
            cProfile.avatar_cached = oldAvatarCached;
            renderCurrentProfile(cProfile);
            if (domProfile.style.display === '') renderProfileTab(cProfile);
            return await popupConfirm('Avatar Update Failed!', 'Failed to broadcast profile update to the network.', true, '', 'vector_warning.svg');
        }
    } catch (e) {
        // Revert local change on error
        cProfile.avatar = oldAvatar;
        cProfile.avatar_cached = oldAvatarCached;
        renderCurrentProfile(cProfile);
        if (domProfile.style.display === '') renderProfileTab(cProfile);
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
            extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico']
        }]
    });
    if (!file) return;

    // Show upload progress spinner
    const bannerEditBtn = document.querySelector('.profile-banner-edit');
    const bannerIcon = bannerEditBtn?.querySelector('.icon');
    let unlisten = null;

    if (bannerIcon) {
        // Replace icon with progress spinner
        bannerIcon.className = 'profile-upload-spinner';
        bannerIcon.style.setProperty('--progress', '5%');

        // Listen for progress events
        unlisten = await window.__TAURI__.event.listen('profile_upload_progress', (event) => {
            if (event.payload.type === 'banner') {
                const progress = Math.max(5, event.payload.progress);
                bannerIcon.style.setProperty('--progress', `${progress}%`);
            }
        });
    }

    // Upload the banner to a NIP-96 server
    let strUploadURL = '';
    try {
        strUploadURL = await invoke("upload_avatar", { filepath: file, uploadType: "banner" });
    } catch (e) {
        // Restore icon on failure
        if (bannerIcon) bannerIcon.className = 'icon icon-edit';
        if (unlisten) unlisten();
        return await popupConfirm('Banner Upload Failed!', e, true, '', 'vector_warning.svg');
    }

    // Restore icon on success
    if (bannerIcon) bannerIcon.className = 'icon icon-edit';
    if (unlisten) unlisten();

    // Display the change immediately
    const cProfile = arrProfiles.find(a => a.mine);
    const oldBanner = cProfile.banner;
    const oldBannerCached = cProfile.banner_cached;
    cProfile.banner = strUploadURL;
    cProfile.banner_cached = ''; // Clear stale cached image so new URL is used
    renderCurrentProfile(cProfile);
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    // Send out the metadata update
    try {
        const success = await invoke("update_profile", { name: "", avatar: "", banner: strUploadURL, about: "" });
        if (!success) {
            // Revert local change since network update failed
            cProfile.banner = oldBanner;
            cProfile.banner_cached = oldBannerCached;
            renderCurrentProfile(cProfile);
            if (domProfile.style.display === '') renderProfileTab(cProfile);
            return await popupConfirm('Banner Update Failed!', 'Failed to broadcast profile update to the network.', true, '', 'vector_warning.svg');
        }
    } catch (e) {
        // Revert local change on error
        cProfile.banner = oldBanner;
        cProfile.banner_cached = oldBannerCached;
        renderCurrentProfile(cProfile);
        if (domProfile.style.display === '') renderProfileTab(cProfile);
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
    const oldStatus = cProfile.status.title;
    cProfile.status.title = strStatus;
    renderCurrentProfile(cProfile);
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    // Send out the status update
    try {
        const success = await invoke("update_status", { status: strStatus });
        if (!success) {
            cProfile.status.title = oldStatus;
            renderCurrentProfile(cProfile);
            if (domProfile.style.display === '') renderProfileTab(cProfile);
            await popupConfirm('Status Update Failed!', 'Failed to broadcast status update to the network.', true, '', 'vector_warning.svg');
        }
    } catch (e) {
        cProfile.status.title = oldStatus;
        renderCurrentProfile(cProfile);
        if (domProfile.style.display === '') renderProfileTab(cProfile);
        await popupConfirm('Status Update Failed!', e, true, '', 'vector_warning.svg');
    }
}

/**
 * A GUI wrapper to ask the user for a file path.
 */
async function selectFile() {
    const file = await open({
        multiple: false,
        directory: false
        // No filters = allow all file types
    });
    return file || "";
}

/**
 * Apply the theme visually by hot-swapping theme CSS files
 * @param {string} theme - The theme name, i.e: vector, chatstr
 * @param {string} mode - The theme mode, i.e: light, dark
 */
function applyTheme(theme = 'vector', mode = 'dark') {
  document.body.classList.remove('vector-theme', 'satoshi-theme', 'chatstr-theme', 'gifverse-theme', 'pivx-theme');
  document.body.classList.add(`${theme}-theme`);
  
  domTheme.href = `/themes/${theme}/${mode}.css`;
  domSettingsThemeSelect.value = theme;
}

/**
 * Set and save the theme
 * @param {string} theme - The theme name, i.e: vector, chatstr
 * @param {string} mode - The theme mode, i.e: light, dark
 */
async function setTheme(theme = 'vector', mode = 'dark') {
  applyTheme(theme, mode);
  await saveTheme(theme);
}

// Apply Theme changes in real-time
domSettingsThemeSelect.onchange = async (evt) => {
    await setTheme(evt.target.value);
    // Refresh storage section after theme change to update colors
    initStorageSection();
};

// Listen for Logout clicks
domSettingsLogout.onclick = async (evt) => {
    // Prompt for confirmation
    const fConfirm = await popupConfirm('Going Incognito?', 'Logging out of Vector will fully erase the database, <b>ensure you have a backup of your keys before logging out!</b><br><br><b>You will permanently lose access to your Group Chats after logging out!</b><br><br>That said, would you like to continue?', false, '', 'vector_warning.svg');
    if (!fConfirm) return;

    // Begin the logout sequence
    await invoke('logout');
};

// Check if this device is the primary one (has the latest keypackage)
async function checkPrimaryDeviceStatus() {
    try {
        // Refresh keypackages from the network for the current user
        try {
            await invoke('refresh_keypackages_for_contact', { npub: strPubkey });
        } catch (error) {
            // Continue with local data if network fetch fails
        }

        // Get all keypackages for the current account (now includes fresh network data)
        const keypackages = await invoke('load_mls_keypackages');

        if (!keypackages || keypackages.length === 0) {
            updatePrimaryDeviceDot(false);
            return;
        }

        let userKeypackages = keypackages.filter(kp =>
            kp.owner_pubkey === strPubkey
        );

        // Deduplicate entries with the same keypackage_ref (event ID)
        // Since device_id is purely local, we use keypackage_ref as the common identifier
        const deduped = new Map();
        for (const kp of userKeypackages) {
            const ref = kp.keypackage_ref;
            if (!deduped.has(ref)) {
                deduped.set(ref, kp);
            }
        }
        userKeypackages = Array.from(deduped.values());

        if (userKeypackages.length === 0) {
            updatePrimaryDeviceDot(false);
            return;
        }

        // Get the local device_id first
        let myDeviceId;
        try {
            myDeviceId = await invoke('load_mls_device_id');
        } catch (error) {
            updatePrimaryDeviceDot(false);
            return;
        }

        // Find the latest keypackage (highest created_at timestamp - when it was actually created, not fetched)
        // Falls back to fetched_at for legacy entries without created_at
        const latestKeypackage = userKeypackages.reduce((latest, current) => {
            const currentTime = current.created_at || current.fetched_at;
            const latestTime = latest.created_at || latest.fetched_at;
            return (currentTime > latestTime) ? current : latest;
        });

        // Find keypackages that have our device_id (created locally)
        const myKeypackages = userKeypackages.filter(kp =>
            kp.device_id === myDeviceId
        );

        // Get the most recent keypackage created by this device
        // Uses created_at (when actually created) with fallback to fetched_at for legacy entries
        const myLatestKeypackage = myKeypackages.length > 0
            ? myKeypackages.reduce((latest, current) => {
                const currentTime = current.created_at || current.fetched_at;
                const latestTime = latest.created_at || latest.fetched_at;
                return (currentTime > latestTime) ? current : latest;
              })
            : null;

        const myLatestKeypackageRef = myLatestKeypackage?.keypackage_ref;

        // This device is primary if its latest keypackage matches the overall latest
        const isPrimary = myLatestKeypackageRef && latestKeypackage.keypackage_ref === myLatestKeypackageRef;
        updatePrimaryDeviceDot(isPrimary);

    } catch (error) {
        console.error('Error checking primary device status:', error);
        updatePrimaryDeviceDot(false);
    }
}

// Update the primary device dot UI
function updatePrimaryDeviceDot(isPrimary) {
    const dot = document.getElementById('primary-device-dot');
    if (dot) {
        dot.className = 'device-status-dot ' + (isPrimary ? 'primary' : 'not-primary');
    }
}

// Show info popup about primary device status
async function showPrimaryDeviceInfo() {
    const dot = document.getElementById('primary-device-dot');
    const isPrimary = dot?.classList.contains('primary');
    
    if (isPrimary) {
        await popupConfirm(
            'Primary Device',
            'This device is currently the Primary Device for receiving Group Invites.',
            true,
            '',
            'vector-check.svg'
        );
    } else {
        await popupConfirm(
            'Not Primary Device',
            'This device is NOT currently the Primary Device for receiving Group Invites.',
            true,
            '',
            'vector_warning.svg'
        );
    }
}

// Listen for Refresh KeyPackages clicks
const domRefreshKeypkg = document.getElementById('refresh-keypkg-btn');
if (domRefreshKeypkg) {
    domRefreshKeypkg.onclick = async (evt) => {
        if (domRefreshKeypkg.disabled) return;

        // Disable button to prevent multiple clicks
        domRefreshKeypkg.disabled = true;
        try {
            await invoke('regenerate_device_keypackage', { cache: false });
            // Wait a moment for the database to be updated
            await new Promise(resolve => setTimeout(resolve, 100));
            // Refresh primary device status after keypackage regeneration
            await checkPrimaryDeviceStatus();
            await popupConfirm('KeyPackages Refreshed', 'A new device KeyPackage has been generated.', true, '', 'vector-check.svg');
        } catch (error) {
            console.error('Refresh KeyPackages failed:', error);
            await popupConfirm('Refresh Failed', error.toString(), true, '', 'vector_warning.svg');
        } finally {
            // Re‑enable button regardless of success or failure
            domRefreshKeypkg.disabled = false;
        }
    };
}

// Listen for Deep Rescan clicks
const domSettingsDeepRescan = document.getElementById('deep-rescan-btn');
domSettingsDeepRescan.onclick = async (evt) => {
    try {
        // Prompt for confirmation first
        const fConfirm = await popupConfirm('Deep Rescan', 'This will forcefully sync your message history backwards in two-day sections until 30 days of no events are found. This may take some time. Continue?', false, '', 'vector_warning.svg');
        if (!fConfirm) return;

        // Check if already scanning (only after user confirms)
        const isScanning = await invoke('is_scanning');
        if (isScanning) {
            await popupConfirm('Already Scanning!', 'Please wait for the current scan to finish before starting a deep rescan.', true, '', 'vector_warning.svg');
            return;
        }

        // Start the deep rescan
        await invoke('deep_rescan');
        await popupConfirm('Deep Rescan Started', 'The deep rescan has been initiated. You can continue using the app while it runs in the background.', true, '', 'vector-check.svg');
    } catch (error) {
        console.error('Deep rescan failed:', error);
        await popupConfirm('Deep Rescan Failed', error.toString(), true, '', 'vector_warning.svg');
    }
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

// Privacy Settings - Simple global variables
let fWebPreviewsEnabled = true;
let fStripTrackingEnabled = true;
let fSendTypingIndicators = true;

// Display Settings - Simple global variables
let fDisplayImageTypes = false;

/**
 * Get storage information from the backend
 */
async function getStorageInfo() {
    try {
        const storageData = await invoke('get_storage_info');
        return storageData;
    } catch (error) {
        console.error('Failed to get storage info:', error);
        return null;
    }
}

/**
 * Clear storage by deleting all files in the Vector directory
 */
async function clearStorage() {
    const clearStorageBtn = document.getElementById('clear-storage-btn');
    if (clearStorageBtn.disabled) return;

    const confirmClear = await popupConfirm(
        'Clear Storage?',
        'This will delete all downloaded and sent files from Vector. This action cannot be undone.',
        false,
        '',
        'vector_warning.svg'
    );
    
    if (!confirmClear) return;
    
    let strPrevText = clearStorageBtn.textContent;
    try {
        clearStorageBtn.disabled = true;
        clearStorageBtn.textContent = 'Clearing...';
        await invoke('clear_storage');
        clearStorageBtn.textContent = strPrevText;
        clearStorageBtn.disabled = false;
        return true;
    } catch (error) {
        clearStorageBtn.textContent = strPrevText;
        clearStorageBtn.disabled = false;
        console.error('Failed to clear storage:', error);
        await popupConfirm('Clear Failed', `Could not clear storage: ${error.message}`, true, '', 'vector_warning.svg');
        return false;
    }
}

/**
 * Initialize the Storage section in settings
 */
async function initStorageSection() {
    // Get and display storage info
    const storageInfo = await getStorageInfo();
    if (storageInfo) {
        // Update storage summary with formatted total size
        const storageSummary = document.getElementById('storage-summary');
        if (storageSummary) {
            if (storageInfo.total_bytes === 0) {
                storageSummary.textContent = "A breakdown of Vector's storage use.";
            } else {
                storageSummary.textContent = `A breakdown of Vector's ${storageInfo.total_formatted} in files.`;
            }
        }
        
        // Render file type distribution bar
        renderFileTypeDistribution(storageInfo.type_distribution, storageInfo.file_count);
    }
}

function renderFileTypeDistribution(typeDistribution, totalBytes) {
    const storageBar = document.getElementById('storage-bar');
    if (!storageBar) return;
    
    // Clear existing segments and tooltips
    storageBar.innerHTML = '';
    
    // Remove any existing tooltip if it exists
    const existingTooltip = document.getElementById('storage-tooltip');
    if (existingTooltip) {
        existingTooltip.remove();
    }
    
    // Handle case when there are no files
    if (totalBytes === 0) {
        storageBar.innerHTML = '<div style="width: 100%; height: 100%; background-color: #333; display: flex; align-items: center; justify-content: center; color: #888; font-size: 12px;">No Storage Used</div>';
        return;
    }
    
    // Create tooltip element
    const tooltip = document.createElement('div');
    tooltip.id = 'storage-tooltip';
    tooltip.style.position = 'absolute';
    tooltip.style.backgroundColor = '#333';
    tooltip.style.color = 'white';
    tooltip.style.padding = '8px 12px';
    tooltip.style.borderRadius = '6px';
    tooltip.style.fontSize = '12px';
    tooltip.style.pointerEvents = 'none';
    tooltip.style.opacity = '0';
    tooltip.style.display = 'none';
    tooltip.style.transition = 'opacity 0.2s ease';
    tooltip.style.zIndex = '1000';
    tooltip.style.whiteSpace = 'nowrap';
    tooltip.style.boxShadow = '0 2px 10px rgba(0, 0, 0, 0.3)';
    document.body.appendChild(tooltip);
    
    // Define file type categories with their extensions
    const categories = [
        {
            name: 'Images',
            extensions: ['jpg', 'jpeg', 'png', 'gif', 'bmp', 'webp', 'svg']
        },
        {
            name: 'Video',
            extensions: ['mp4', 'mov', 'avi', 'mkv', 'flv', 'wmv', '3gp', 'ogg', 'webm']
        }
    ];
    
    // Calculate sizes for each category
    const categorySizes = categories.map(category => {
        let size = 0;
        for (const ext of category.extensions) {
            if (typeDistribution[ext]) {
                size += typeDistribution[ext];
            }
        }
        return { name: category.name, size: size };
    });
    
    // Add AI Models category if ai_models exists in type distribution
    if (typeDistribution['ai_models']) {
        categorySizes.push({
            name: 'AI',
            size: typeDistribution['ai_models']
        });
    }

    // Add Cache category if cache exists in type distribution (avatars, banners, icons)
    if (typeDistribution['cache']) {
        categorySizes.push({
            name: 'Cache',
            size: typeDistribution['cache']
        });
    }

    // Calculate size for other files (excluding special categories)
    let otherSize = 0;
    for (const ext in typeDistribution) {
        let isCategorized = false;
        // Skip special categories as they're already handled
        if (ext === 'ai_models' || ext === 'cache') continue;
        
        for (const category of categories) {
            if (category.extensions.includes(ext)) {
                isCategorized = true;
                break;
            }
        }
        if (!isCategorized) {
            otherSize += typeDistribution[ext];
        }
    }
    
    // Create segments array with all categories and sort by size (descending)
    const segments = [];
    for (const category of categorySizes) {
        if (category.size > 0) {
            segments.push({
                name: category.name,
                size: category.size
            });
        }
    }
    
    // Add "Other" if there are any uncategorized files
    if (otherSize > 0) {
        segments.push({
            name: 'Other',
            size: otherSize
        });
    }
    
    // Sort segments by size (largest first)
    segments.sort((a, b) => b.size - a.size);
    
    // Get primary color from theme
    const root = document.documentElement;
    const primaryColor = getComputedStyle(root).getPropertyValue('--icon-color-primary').trim();
    
    // Create segments in the bar
    const largestSize = segments[0].size;
    
    for (const segmentData of segments) {
        const size = segmentData.size;
        // Use sum of all typeDistribution values as total, since totalBytes is incorrect
        const total = Object.values(typeDistribution).reduce((sum, val) => sum + val, 0);
        const percentage = (size / total) * 100;
        // Convert hex to RGB and set opacity
        const rgbColor = hexToRgb(primaryColor);
        
        // Calculate opacity relative to the largest segment
        // Largest segment gets 100% opacity, others get proportionally less
        const relativeOpacity = size / largestSize;
        
        // Round to 2 decimal places to avoid floating point precision issues
        const roundedPercentage = Math.round(percentage * 100) / 100;
        const segment = document.createElement('div');
        segment.style.width = `${roundedPercentage}%`;
        segment.style.flexShrink = '0';
        segment.style.boxSizing = 'border-box';
        // Ensure minimum opacity of 1% for visibility
        const preciseOpacity = Math.max(0.01, relativeOpacity);
        // Set background color using CSS variable and opacity
        // Apply opacity directly to background using existing primaryColor and rgbColor
        const backgroundColor = `rgba(${rgbColor.r}, ${rgbColor.g}, ${rgbColor.b}, ${preciseOpacity})`;
        segment.style.backgroundColor = backgroundColor;
        // Set position relative to enable absolute positioning of child elements
        segment.style.position = 'relative';
        
        // Add text label if percentage is greater than 20%
        if (roundedPercentage > 20) {
            const label = document.createElement('div');
            label.textContent = `${segmentData.name} (${roundedPercentage.toFixed(0)}%)`;
            label.style.position = 'absolute';
            label.style.top = '50%';
            label.style.left = '50%';
            label.style.transform = 'translate(-50%, -50%)';
            // Change text color to black when opacity is 50% or above
            label.style.color = preciseOpacity >= 0.5 ? 'black' : 'white';
            label.style.textAlign = 'center';
            label.style.fontWeight = 'bold';
            label.style.fontSize = '12px';
            label.style.fontFamily = 'Arial, sans-serif';
            label.style.whiteSpace = 'nowrap';
            label.style.cursor = 'default';
            segment.appendChild(label);
        }
        
        segment.dataset.type = segmentData.name;
        segment.dataset.size = segmentData.size;
        
        segment.addEventListener('mouseenter', (e) => {
            const tooltip = document.getElementById('storage-tooltip');
            if (tooltip) {
                // Format size in human-readable format
                const formattedSize = `${segmentData.name} - ${formatBytes(segmentData.size)}`;
                tooltip.textContent = formattedSize;
                tooltip.style.display = 'block';
                tooltip.style.opacity = '1';
                
                // Position tooltip above the cursor with edge detection
                const tooltipWidth = tooltip.offsetWidth;
                const tooltipHeight = tooltip.offsetHeight;
                const viewportWidth = window.innerWidth;
                const viewportHeight = window.innerHeight;
                
                // Calculate tooltip position based on cursor position
                let leftPos = e.clientX + window.scrollX;
                let topPos = e.clientY + window.scrollY - tooltipHeight - 10;
                
                // Check if tooltip would overflow right edge
                if (leftPos + tooltipWidth > viewportWidth) {
                    // Position tooltip to the left of cursor
                    leftPos = e.clientX + window.scrollX - tooltipWidth;
                }
                
                // Check if tooltip would overflow left edge
                if (leftPos < 0) {
                    leftPos = 10;
                }
                
                // Check if tooltip would overflow bottom edge
                if (topPos + tooltipHeight > viewportHeight) {
                    // Position tooltip above cursor
                    topPos = e.clientY + window.scrollY + 10;
                }
                
                // Ensure tooltip doesn't go off top edge
                if (topPos < 0) {
                    topPos = 10;
                }
                
                tooltip.style.left = `${leftPos}px`;
                tooltip.style.top = `${topPos}px`;
            }
        });
        
        segment.addEventListener('mouseleave', () => {
            const tooltip = document.getElementById('storage-tooltip');
            if (tooltip) {
                tooltip.style.opacity = '0';
                setTimeout(() => {
                    if (tooltip.style.opacity === '0') {
                        tooltip.style.display = 'none';
                    }
                }, 200);
            }
        });
        
        storageBar.appendChild(segment);
    }
    
    // If no files or all segments are empty, ensure the bar is filled
    if (totalBytes === 0 || segments.length === 0) {
        const segment = document.createElement('div');
        segment.style.flex = '1';
        // Use primary color with very low opacity for empty state
        const rgbColor = hexToRgb(primaryColor);
        segment.style.backgroundColor = `rgba(${rgbColor.r}, ${rgbColor.g}, ${rgbColor.b}, 0.1)`;
        segment.dataset.type = 'NONE';
        segment.dataset.size = 0;
        
        segment.addEventListener('mouseenter', (e) => {
            const tooltip = document.getElementById('storage-tooltip');
            if (tooltip) {
                tooltip.textContent = 'No files found';
                tooltip.style.opacity = '1';
                
                // Position tooltip above the segment with edge detection
                const rect = e.target.getBoundingClientRect();
                const tooltipWidth = tooltip.offsetWidth;
                const viewportWidth = window.innerWidth;
                
                // Calculate tooltip position
                let leftPos = rect.left + window.scrollX;
                
                // Check if tooltip would overflow right edge
                if (leftPos + tooltipWidth > viewportWidth) {
                    // Position tooltip to the left of the segment
                    leftPos = rect.right + window.scrollX - tooltipWidth;
                }
                
                // Ensure tooltip doesn't go off left edge
                if (leftPos < 0) {
                    leftPos = 0;
                }
                
                tooltip.style.left = `${leftPos}px`;
                tooltip.style.top = `${rect.top + window.scrollY - 30}px`;
            }
        });
        
        segment.addEventListener('mouseleave', () => {
            const tooltip = document.getElementById('storage-tooltip');
            if (tooltip) {
                tooltip.style.opacity = '0';
            }
        });
        
        storageBar.appendChild(segment);
    }
    
    // Helper function to convert hex color to RGB
    function hexToRgb(hex) {
        // Remove # if present
        hex = hex.replace('#', '');
        
        // Parse hex to RGB
        const bigint = parseInt(hex, 16);
        return {
            r: (bigint >> 16) & 255,
            g: (bigint >> 8) & 255,
            b: bigint & 255
        };
    }
}

// ============================================================================
// Notification Sound Settings
// ============================================================================

/** @type {Object|null} Current notification settings */
let currentNotificationSettings = null;

/** @type {string|null} Path to custom sound file */
let customSoundPath = null;

/**
 * Initialize notification sound settings UI
 */
async function initNotificationSettings() {
    // Load current settings
    try {
        currentNotificationSettings = await loadNotificationSettings();
    } catch (e) {
        console.error('Failed to load notification settings:', e);
        currentNotificationSettings = { global_mute: false, sound: { type: 'Default' } };
    }

    const muteToggle = document.getElementById('notif-mute-toggle');
    const soundSelect = document.getElementById('notif-sound-select');
    const customGroup = document.getElementById('notif-custom-group');
    const customFilename = document.getElementById('notif-custom-filename');
    const customSelectBtn = document.getElementById('notif-custom-select-btn');
    const previewBtn = document.getElementById('notif-preview-btn');

    // Set initial mute toggle state
    muteToggle.checked = currentNotificationSettings.global_mute;

    // Determine current sound selection
    const sound = currentNotificationSettings.sound;
    if (sound && sound.type === 'Custom' && sound.path) {
        customSoundPath = sound.path;
        soundSelect.value = 'custom';
        customGroup.style.display = 'block';
        updateCustomFilename(sound.path);
    } else if (sound && sound.type === 'None') {
        soundSelect.value = 'none';
    } else if (sound && sound.type === 'Techno') {
        soundSelect.value = 'techno';
    } else {
        soundSelect.value = 'default';
    }

    // Mute toggle handler
    muteToggle.addEventListener('change', async (e) => {
        currentNotificationSettings.global_mute = e.target.checked;
        await saveCurrentNotificationSettings();
    });

    // Sound selection handler
    soundSelect.addEventListener('change', async (e) => {
        const value = e.target.value;

        if (value === 'custom') {
            customGroup.style.display = 'block';
            if (customSoundPath) {
                updateCustomFilename(customSoundPath);
                currentNotificationSettings.sound = { type: 'Custom', path: customSoundPath };
                await saveCurrentNotificationSettings();
            } else {
                // No custom path yet - show placeholder
                customFilename.textContent = 'No file selected';
            }
        } else {
            customGroup.style.display = 'none';
            if (value === 'none') {
                currentNotificationSettings.sound = { type: 'None' };
            } else if (value === 'techno') {
                currentNotificationSettings.sound = { type: 'Techno' };
            } else {
                currentNotificationSettings.sound = { type: 'Default' };
            }
            await saveCurrentNotificationSettings();
        }
    });

    // Custom sound file selection handler
    customSelectBtn.addEventListener('click', async () => {
        try {
            const path = await selectCustomNotificationSound();
            customSoundPath = path;
            currentNotificationSettings.sound = { type: 'Custom', path: path };
            updateCustomFilename(path);
            await saveCurrentNotificationSettings();
        } catch (e) {
            if (e === 'FILE_TOO_LARGE') {
                popupConfirm('File Too Large', 'Notification sounds must be under 1MB. Please choose a shorter audio clip.', true);
            } else if (e === 'AUDIO_TOO_LONG') {
                popupConfirm('Audio Too Long', 'Notification sounds must be 10 seconds or less.', true);
            } else if (e !== 'No file selected') {
                console.error('Failed to select custom sound:', e);
            }
        }
    });

    // Clear custom sound handler
    const clearBtn = document.getElementById('notif-custom-clear');
    clearBtn.addEventListener('click', async (e) => {
        e.stopPropagation(); // Prevent triggering the chip click (file picker)
        customSoundPath = null;
        currentNotificationSettings.sound = { type: 'Default' };
        soundSelect.value = 'default';
        customGroup.style.display = 'none';
        await saveCurrentNotificationSettings();
    });

    // Preview button handler
    previewBtn.addEventListener('click', async () => {
        try {
            await previewNotificationSound(currentNotificationSettings.sound);
        } catch (e) {
            console.error('Failed to preview sound:', e);
        }
    });
}

/**
 * Update the custom filename display
 * @param {string} path - Full path to the sound file (may be cache format: name_RATE.raw)
 */
function updateCustomFilename(path) {
    const filename = path.split(/[/\\]/).pop() || 'Unknown file';
    // Extract friendly name from cache format (e.g., "discord_ping_48000.raw" -> "discord_ping")
    const friendlyName = filename.replace(/_\d+\.raw$/, '');
    document.getElementById('notif-custom-filename').textContent = friendlyName;
}

/**
 * Save current notification settings to backend
 */
async function saveCurrentNotificationSettings() {
    try {
        await saveNotificationSettings(currentNotificationSettings);
    } catch (e) {
        console.error('Failed to save notification settings:', e);
    }
}

/**
 * Initialize settings on app start
 */
async function initSettings() {
    // Load privacy settings from DB (default to true)
    fWebPreviewsEnabled = await loadWebPreviews();
    fStripTrackingEnabled = await loadStripTracking();
    fSendTypingIndicators = await loadSendTypingIndicators();

    // Set initial toggle states
    const webPreviewsToggle = document.getElementById('privacy-web-previews-toggle');
    const stripTrackingToggle = document.getElementById('privacy-strip-tracking-toggle');
    const sendTypingToggle = document.getElementById('privacy-send-typing-toggle');
    
    webPreviewsToggle.checked = fWebPreviewsEnabled;
    webPreviewsToggle.addEventListener('change', async (e) => {
        fWebPreviewsEnabled = e.target.checked;
        await saveWebPreviews(e.target.checked);
    });
    
    stripTrackingToggle.checked = fStripTrackingEnabled;
    stripTrackingToggle.addEventListener('change', async (e) => {
        fStripTrackingEnabled = e.target.checked;
        await saveStripTracking(e.target.checked);
    });
    
    sendTypingToggle.checked = fSendTypingIndicators;
    sendTypingToggle.addEventListener('change', async (e) => {
        fSendTypingIndicators = e.target.checked;
        await saveSendTypingIndicators(e.target.checked);
    });

    // Load and initialize display settings
    fDisplayImageTypes = await loadDisplayImageTypes();
    const displayImageTypesToggle = document.getElementById('display-image-types-toggle');
    displayImageTypesToggle.checked = fDisplayImageTypes;
    displayImageTypesToggle.addEventListener('change', async (e) => {
        fDisplayImageTypes = e.target.checked;
        await saveDisplayImageTypes(e.target.checked);
    });

    // Background Wallpaper toggle (Chat Background)
    const chatBgToggle = document.getElementById('chat-bg-toggle');
    if (chatBgToggle) {
        // Load saved preference (default: enabled)
        const chatBgEnabled = localStorage.getItem('chatBgEnabled') !== 'false';
        chatBgToggle.checked = chatBgEnabled;
        if (!chatBgEnabled) document.body.classList.add('chat-bg-disabled');

        // Handle toggle changes
        chatBgToggle.addEventListener('change', () => {
            if (chatBgToggle.checked) {
                document.body.classList.remove('chat-bg-disabled');
                localStorage.setItem('chatBgEnabled', 'true');
            } else {
                document.body.classList.add('chat-bg-disabled');
                localStorage.setItem('chatBgEnabled', 'false');
            }
        });
    }

    // Initialize notification sound settings (desktop only)
    if (platformFeatures.notification_sounds) {
        await initNotificationSettings();
    } else {
        // Hide notification sounds section on mobile
        const notifSection = document.getElementById('settings-notifications');
        if (notifSection) notifSection.style.display = 'none';
    }

    // Set up clear storage button
    const clearStorageBtn = document.getElementById('clear-storage-btn');
    clearStorageBtn.addEventListener('click', async () => {
        const success = await clearStorage();
        if (success) initStorageSection();
    });

    // Add click handler for primary device status
    const primaryDeviceStatus = document.getElementById('primary-device-status');
    primaryDeviceStatus.onclick = showPrimaryDeviceInfo;
}
