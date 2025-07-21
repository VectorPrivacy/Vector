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
        checkButton.addEventListener('click', () => checkForUpdates(false));
    }
    
    // Add click handler for download button
    const downloadButton = document.getElementById('download-update-btn');
    if (downloadButton) {
        downloadButton.addEventListener('click', downloadUpdate);
    }
    
    // Add click handler for restart button
    const restartButton = document.getElementById('restart-update-btn');
    if (restartButton) {
        restartButton.addEventListener('click', () => relaunch());
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
    const downloadButton = document.getElementById('download-update-btn');
    const restartButton = document.getElementById('restart-update-btn');
    
    // Hide all action buttons by default
    if (downloadButton) downloadButton.style.display = 'none';
    if (restartButton) restartButton.style.display = 'none';
    
    switch (state) {
        case 'idle':
            if (statusText) {
                statusText.textContent = message || 'Click to check for updates';
                statusText.style.display = 'none';
            }
            if (progressContainer) progressContainer.style.display = 'none';
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
            if (checkButton) {
                checkButton.disabled = true;
                checkButton.textContent = 'Checking...';
            }
            break;
            
        case 'available':
            if (statusText) {
                statusText.textContent = message;
                statusText.style.display = 'block';
            }
            if (progressContainer) progressContainer.style.display = 'none';
            if (checkButton) {
                checkButton.disabled = false;
                checkButton.textContent = 'Check for Updates';
            }
            if (downloadButton) downloadButton.style.display = 'block';
            break;
            
        case 'downloading':
            if (statusText) {
                statusText.textContent = 'Downloading update...';
                statusText.style.display = 'block';
            }
            if (progressContainer) progressContainer.style.display = 'block';
            if (progressBar) progressBar.style.width = `${progress}%`;
            if (progressText) progressText.textContent = `${progress}%`;
            if (checkButton) checkButton.disabled = true;
            if (downloadButton) downloadButton.style.display = 'none';
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
            }
            if (restartButton) restartButton.style.display = 'block';
            break;
            
        case 'error':
            if (statusText) {
                statusText.textContent = message;
                statusText.style.display = 'block';
                statusText.style.color = '#ff5252';
            }
            if (progressContainer) progressContainer.style.display = 'none';
            if (checkButton) {
                checkButton.disabled = false;
                checkButton.textContent = 'Check for Updates';
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
            if (checkButton) {
                checkButton.disabled = false;
                checkButton.textContent = 'Check for Updates';
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
        console.log(`Update available: ${update.version} from ${update.date}`);
        
        if (!silent) {
            const message = `Version ${update.version} is available`;
            updateUI('available', message);
        }
        
        return true;
    } catch (error) {
        console.error('Error checking for updates:', error);
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
                    console.log(`Started downloading ${contentLength} bytes`);
                    break;
                    
                case 'Progress':
                    downloaded += event.data.chunkLength;
                    const percentage = contentLength > 0 ? Math.round((downloaded / contentLength) * 100) : 0;
                    console.log(`Downloaded ${downloaded} of ${contentLength} bytes (${percentage}%)`);
                    updateUI('downloading', '', percentage);
                    break;
                    
                case 'Finished':
                    console.log('Download finished');
                    break;
            }
        });
        
        console.log('Update installed successfully');
        updateUI('ready');
        
    } catch (error) {
        console.error('Error installing update:', error);
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
    
    // Check for updates 10 seconds after app start
    setTimeout(() => {
        checkForUpdates(true);
    }, 10000);
    
    // Check for updates every 4 hours
    setInterval(() => {
        checkForUpdates(true);
    }, 4 * 60 * 60 * 1000);
}

// Make initializeUpdater available globally
window.initializeUpdater = initializeUpdater;
