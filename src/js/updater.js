// Updater functionality for Vector
const { check } = window.__TAURI__.updater;
const { relaunch } = window.__TAURI__.process;

// Store update state
let currentUpdate = null;
let updateState = 'idle'; // idle, checking, available, downloading, ready

// Get current version
async function getCurrentVersion() {
    try {
        return await window.__TAURI__.app.getVersion();
    } catch (error) {
        console.error('Error getting version:', error);
        return 'Unknown';
    }
}

// Initialize updater UI elements
function initializeUpdaterUI() {
    const updateSection = document.getElementById('settings-updates');
    if (!updateSection) return;
    
    // Update current version display
    getCurrentVersion().then(version => {
        const versionElement = document.getElementById('current-version');
        if (versionElement) {
            versionElement.textContent = `v${version}`;
        }
    });
    
    // Add click handler for check updates button
    const checkButton = document.getElementById('check-updates-btn');
    if (checkButton) {
        checkButton.addEventListener('click', handleButtonClick);
    }
    
    // Add click handler for restart button
    const restartButton = document.getElementById('restart-update-btn');
    if (restartButton) {
        restartButton.addEventListener('click', () => relaunch());
    }
}

// Handle button click based on current state
function handleButtonClick() {
    if (updateState === 'available') {
        downloadUpdate();
    } else {
        checkForUpdates(false);
    }
}

// Update UI state
function updateUI(state, message = '', progress = 0) {
    updateState = state;
    
    const statusText = document.getElementById('update-status-text');
    const progressContainer = document.getElementById('update-progress-container');
    const progressBar = document.getElementById('update-progress-bar');
    const progressText = document.getElementById('update-progress-text');
    const checkButton = document.getElementById('check-updates-btn');
    const restartButton = document.getElementById('restart-update-btn');
    const newVersionDisplay = document.getElementById('new-version-display');
    const newVersionText = document.getElementById('new-version');
    const changelogContainer = document.getElementById('update-changelog');
    const changelogContent = document.getElementById('changelog-content');
    const updateDot = document.getElementById('settings-update-dot');
    
    // Hide all action buttons by default
    if (restartButton) restartButton.style.display = 'none';
    
    switch (state) {
        case 'idle':
            if (statusText) {
                statusText.textContent = message || 'Click to check for updates';
                statusText.style.display = 'none';
            }
            if (progressContainer) progressContainer.style.display = 'none';
            if (newVersionDisplay) newVersionDisplay.style.display = 'none';
            if (changelogContainer) changelogContainer.style.display = 'none';
            if (checkButton) {
                checkButton.disabled = false;
                checkButton.textContent = 'Check for Updates';
            }
            break;
            
        case 'checking':
            if (statusText) {
                statusText.textContent = 'Checking for updates...';
                statusText.style.display = 'block';
            }
            if (progressContainer) progressContainer.style.display = 'none';
            if (newVersionDisplay) newVersionDisplay.style.display = 'none';
            if (changelogContainer) changelogContainer.style.display = 'none';
            if (checkButton) {
                checkButton.disabled = true;
                checkButton.textContent = 'Checking...';
            }
            break;
            
        case 'available':
            if (statusText) {
                statusText.style.display = 'none';
            }
            if (progressContainer) progressContainer.style.display = 'none';
            if (currentUpdate && newVersionDisplay && newVersionText) {
                newVersionText.textContent = `v${currentUpdate.version}`;
                newVersionDisplay.style.display = 'block';
            }
            if (currentUpdate && currentUpdate.body && changelogContainer && changelogContent) {
                // Convert line breaks to HTML and escape HTML entities
                const escapedBody = currentUpdate.body
                    .replace(/&/g, '&amp;')
                    .replace(/</g, '&lt;')
                    .replace(/>/g, '&gt;')
                    .replace(/"/g, '&quot;')
                    .replace(/'/g, '&#039;')
                    .replace(/\n/g, '<br>');
                changelogContent.innerHTML = escapedBody;
                changelogContainer.style.display = 'block';
            }
            if (checkButton) {
                checkButton.disabled = false;
                checkButton.textContent = 'Download Update';
                checkButton.style.background = '';
            }
            // Show notification dot on settings button
            if (updateDot) updateDot.style.display = 'block';
            break;
            
        case 'downloading':
            if (statusText) {
                statusText.style.display = 'none';
            }
            if (progressContainer) progressContainer.style.display = 'block';
            if (progressBar) progressBar.style.width = `${progress}%`;
            if (progressText) progressText.textContent = `${progress}%`;
            if (checkButton) {
                checkButton.disabled = true;
                checkButton.textContent = 'Downloading...';
            }
            // Hide notification dot when downloading
            if (updateDot) updateDot.style.display = 'none';
            break;
            
        case 'ready':
            if (statusText) {
                statusText.textContent = 'Update ready! Restart to apply.';
                statusText.style.display = 'block';
            }
            if (progressContainer) progressContainer.style.display = 'none';
            if (checkButton) {
                checkButton.disabled = false;
                checkButton.textContent = 'Check for Updates';
                checkButton.style.background = '';
            }
            if (restartButton) restartButton.style.display = 'block';
            // Hide notification dot when ready
            if (updateDot) updateDot.style.display = 'none';
            break;
            
        case 'error':
            if (statusText) {
                statusText.textContent = message;
                statusText.style.display = 'block';
                statusText.style.color = '#ff5252';
            }
            if (progressContainer) progressContainer.style.display = 'none';
            if (newVersionDisplay) newVersionDisplay.style.display = 'none';
            if (changelogContainer) changelogContainer.style.display = 'none';
            if (checkButton) {
                checkButton.disabled = false;
                checkButton.textContent = 'Check for Updates';
                checkButton.style.background = '';
            }
            setTimeout(() => {
                if (statusText) statusText.style.color = '';
                updateUI('idle');
            }, 5000);
            break;
            
        case 'no-updates':
            if (statusText) {
                statusText.textContent = 'You are running the latest version';
                statusText.style.display = 'block';
                statusText.style.color = '#59fcb3';
            }
            if (progressContainer) progressContainer.style.display = 'none';
            if (newVersionDisplay) newVersionDisplay.style.display = 'none';
            if (changelogContainer) changelogContainer.style.display = 'none';
            if (checkButton) {
                checkButton.disabled = false;
                checkButton.textContent = 'Check for Updates';
                checkButton.style.background = '';
            }
            setTimeout(() => {
                if (statusText) statusText.style.color = '';
                updateUI('idle');
            }, 3000);
            break;
    }
}

// Check for updates
async function checkForUpdates(silent = false) {
    if (updateState === 'checking' || updateState === 'downloading') return;
    
    if (!silent) {
        updateUI('checking');
    }
    
    try {
        const update = await check();
        
        if (!update) {
            if (!silent) {
                updateUI('no-updates');
            }
            return false;
        }
        
        // Found an update
        currentUpdate = update;
        console.log(`Updater: Update available: ${update.version} from ${update.date}`);
        
        // Always update UI when an update is found, even in silent mode
        updateUI('available');
        
        return true;
    } catch (error) {
        console.error('Updater: Error checking for updates:', error);
        if (!silent) {
            updateUI('error', 'Failed to check for updates');
        }
        return false;
    }
}

// Download update
async function downloadUpdate() {
    if (!currentUpdate || updateState === 'downloading') return;
    
    updateUI('downloading', '', 0);
    
    try {
        let downloaded = 0;
        let contentLength = 0;
        
        await currentUpdate.downloadAndInstall((event) => {
            switch (event.event) {
                case 'Started':
                    contentLength = event.data.contentLength || 0;
                    console.log(`Updater: Started downloading ${contentLength} bytes`);
                    break;
                    
                case 'Progress':
                    downloaded += event.data.chunkLength;
                    const percentage = contentLength > 0 ? Math.round((downloaded / contentLength) * 100) : 0;
                    updateUI('downloading', '', percentage);
                    break;
                    
                case 'Finished':
                    console.log('Updater: Download finished');
                    break;
            }
        });
        
        console.log('Updater: Update installed successfully');
        updateUI('ready');
        
    } catch (error) {
        console.error('Updater: Error installing update:', error);
        updateUI('error', 'Failed to download update');
    }
}

// Auto-check for updates on app start (silent check)
function initializeUpdater() {
    // Initialize UI when DOM is ready
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', initializeUpdaterUI);
    } else {
        initializeUpdaterUI();
    }

    // Check for updates immediately after app start
    checkForUpdates(true);

    // Check for updates every 4 hours
    setInterval(() => {
        checkForUpdates(true);
    }, 4 * 60 * 60 * 1000);
}