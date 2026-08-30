// Updater functionality for Vector.
// The updater/process plugins are desktop-only, so they are accessed lazily
// inside the desktop paths — a top-level destructure would throw on Android
// and kill this whole module.

// Store update state
let currentUpdate = null;
let updateState = 'idle'; // idle, checking, available, downloading, ready

// Android: where this build updates from — { has_store, label }. Resolved
// once at init from whatever store installed the APK; drives the redirect
// button's label and action. Defaults to sideload (website) until resolved.
let androidInstallSource = { has_store: false, label: '' };

// The running build's identity, resolved once before the first check.
let versionInfo = { raw: '', preview: null, display: '' };

// Where preview builds are downloaded from: previews are never published to a
// store, so there is nothing for a store to hand off to.
const PREVIEW_RELEASES_URL = 'https://github.com/VectorPrivacy/Vector/releases';

// Land on the exact release the check found, falling back to the release list
// when the target isn't known (a manual tap before any check has resolved).
function previewReleaseUrl() {
    const target = currentUpdate && currentUpdate.version;
    return target ? `${PREVIEW_RELEASES_URL}/tag/v${target}` : PREVIEW_RELEASES_URL;
}

// Get current version
async function getCurrentVersion() {
    try {
        return await window.__TAURI__.app.getVersion();
    } catch (error) {
        console.error('Error getting version:', error);
        return 'Unknown';
    }
}

// `0.4.2-1` -> preview 1 of the upcoming 0.4.2; `0.4.2` -> the release itself.
// The identifier is numeric because the Windows MSI bundler rejects anything
// else, so it gets spelled out for display: a bare "v0.4.2-1" reads as though
// it came *after* 0.4.2, which is backwards.
function parseVersion(raw) {
    const match = /^(\d+\.\d+\.\d+)(?:-(\d+))?/.exec(raw);
    if (!match) return { raw, preview: null, display: raw };
    const [, core, pre] = match;
    const preview = pre ? parseInt(pre, 10) : null;
    return {
        raw,
        preview,
        display: preview === null ? `v${core}` : `v${core} Preview ${preview}`,
    };
}

// The Beta Updates preference. Per-install by design (an update applies to
// the machine, not an account), so it lives in localStorage like the
// rich-composer escape hatch. Unset defaults to the build's native channel:
// a preview build follows the preview channel until the user opts out.
function updateChannel() {
    let stored = null;
    try { stored = localStorage.getItem('beta_updates'); } catch (_) {}
    if (stored === 'true') return 'preview';
    if (stored === 'false') return 'stable';
    return versionInfo.preview === null ? 'stable' : 'preview';
}

// The channel to actually follow. An Android store build can only be updated
// by its store (which never carries previews), so it always reads stable.
function followPreviewChannel() {
    if (platformFeatures.os === 'android' && androidInstallSource.has_store) return false;
    return updateChannel() === 'preview';
}

// The toggle renders wherever the choice can work: desktop, or a sideloaded
// Android build — RCs included, where OFF means "sit on this build until the
// official release". Store builds hide it.
function updateBetaRowVisibility() {
    const row = document.getElementById('beta-updates-row');
    if (!row) return;
    const eligible = platformFeatures.os !== 'android' || !androidInstallSource.has_store;
    row.style.display = eligible ? '' : 'none';
}

// Initialize updater UI elements
function initializeUpdaterUI() {
    const updateSection = document.getElementById('settings-updates');
    if (!updateSection) return;
    
    // Update current version display
    const versionElement = document.getElementById('current-version');
    if (versionElement) {
        versionElement.textContent = versionInfo.display;
    }
    const previewNotice = document.getElementById('update-preview-notice');
    if (previewNotice) {
        previewNotice.style.display = versionInfo.preview === null ? 'none' : 'block';
    }

    // Add click handler for check updates button
    const checkButton = document.getElementById('check-updates-btn');
    if (checkButton) {
        checkButton.addEventListener('click', handleButtonClick);
    }
    
    // Add click handler for restart button
    const restartButton = document.getElementById('restart-update-btn');
    if (restartButton) {
        restartButton.addEventListener('click', () => window.__TAURI__.process.relaunch());
    }

    const betaInfo = document.getElementById('beta-updates-info');
    if (betaInfo) {
        betaInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm('Beta Updates', 'Beta builds are <b>release candidates of the next Vector version</b>, offered here before the official release.<br><br>They carry the newest fixes and features with a little less polish, and you\'ll be moved onto the official build the moment it releases.<br><br>Turning this off on a beta parks you on your current build until the next official release, then you ride stable from there.', true);
        };
    }

    // Beta opt-in toggle: flipping it re-checks immediately on the newly
    // chosen channel, and flipping it OFF withdraws an offered RC.
    const betaToggle = document.getElementById('beta-updates-toggle');
    if (betaToggle) {
        betaToggle.checked = updateChannel() === 'preview';
        betaToggle.addEventListener('change', () => {
            try { localStorage.setItem('beta_updates', betaToggle.checked ? 'true' : 'false'); } catch (_) {}
            currentUpdate = null;
            const updateDot = document.getElementById('settings-update-dot');
            if (updateDot) updateDot.style.display = 'none';
            updateUI('idle');
            checkForUpdates(false);
        });
    }
    updateBetaRowVisibility();
}

// Handle button click based on current state
function handleButtonClick() {
    if (updateState === 'available') {
        if (platformFeatures.os === 'android') {
            openAndroidUpdateSource();
        } else {
            downloadUpdate();
        }
    } else {
        checkForUpdates(false);
    }
}

// Where an Android build updates from depends on where it CAME from. A store
// installed it, signed with that store's key, so only that store can update it.
// A sideload was signed by the release key we publish, so Vector can install the
// next release itself. Previews skip stores entirely — a store that never
// carried this build can't offer the next preview.
function androidUpdateButtonLabel() {
    // A store can only update what it installed. Previews are never store
    // builds, so they always take the sideload route.
    if (androidInstallSource.has_store && versionInfo.preview === null) {
        return `Update via ${androidInstallSource.label}`;
    }
    return 'Download & install';
}

async function openAndroidUpdateSource() {
    // A store build hands back to its store — it holds the signing key, so it is
    // the only thing that CAN update this copy. Previews are never store builds,
    // so they skip straight past (a store would 404 or offer the older stable).
    if (androidInstallSource.has_store && versionInfo.preview === null) {
        try {
            const opened = await window.__TAURI__.core.invoke('open_update_source');
            if (opened) return;
        } catch (e) {
            console.warn('Updater: store hand-off failed:', e);
        }
        // The store couldn't take the deep link: the website carries every build
        // and links out to each store.
        return openUrl('https://vectorapp.io');
    }
    // Sideload: this copy carries the release signing key, so Vector can fetch
    // and install the next one itself. The system installer still asks.
    try {
        updateUI('downloading', '', 0);
        const stopProgress = await window.__TAURI__.event.listen('update_download_progress', (evt) => {
            const { received = 0, total = 0 } = evt.payload || {};
            updateUI('downloading', '', total > 0 ? Math.round((received / total) * 100) : 0);
        });
        try {
            const result = await window.__TAURI__.core.invoke('download_and_install_update', { beta: followPreviewChannel() });
            if (result === 'needs-permission') {
                updateUI('available', 'Allow installs from Vector, then tap again');
            } else {
                updateUI('available', 'Confirm the install to finish');
            }
        } finally {
            stopProgress();
        }
    } catch (e) {
        console.warn('Updater: in-app install failed:', e);
        // Anything that stops the in-app route (a signing-key change, no space,
        // a dead network) still has the manual path behind it.
        updateUI('available', String(e?.message || e || 'Update failed'));
        return openUrl('https://vectorapp.io');
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
                checkButton.style.display = 'block';
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
                newVersionText.textContent = parseVersion(currentUpdate.version).display;
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
                checkButton.textContent = (platformFeatures.os === 'android')
                    ? androidUpdateButtonLabel()
                    : 'Download Update';
                checkButton.style.background = '';
                checkButton.style.display = 'block';
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
                checkButton.style.display = 'none';
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
                checkButton.style.display = 'block';
            }
            setTimeout(() => {
                if (statusText) statusText.style.color = '';
                updateUI('idle');
            }, 5000);
            break;
            
        case 'no-updates':
            if (statusText) {
                statusText.textContent = versionInfo.preview === null
                    ? 'You are running the latest version'
                    : "No newer build yet. You'll be offered the official build as soon as it releases.";
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
                checkButton.style.display = 'block';
            }
            setTimeout(() => {
                if (statusText) statusText.style.color = '';
                updateUI('idle');
            }, 3000);
            break;
    }
}

// Resolve the proxy the updater's own HTTP client must use. The updater
// plugin bypasses the backend's Tor-aware client entirely, so with Tor on
// we either hand it the SOCKS proxy or refuse to touch the network at all.
// Returns: { allowed: boolean, proxy?: string }
async function resolveUpdateTransport() {
    try {
        const tor = await window.__TAURI__.core.invoke('tor_get_state');
        if (!tor?.enabled) return { allowed: true };
        if (tor.running && tor.socks_proxy) return { allowed: true, proxy: tor.socks_proxy };
        // Tor wanted but not up (bootstrapping/failed): fail closed.
        return { allowed: false };
    } catch (e) {
        console.warn('Updater: could not read Tor state, skipping update check:', e);
        return { allowed: false };
    }
}

// Check for updates
async function checkForUpdates(silent = false) {
    if (updateState === 'checking' || updateState === 'downloading') return;

    if (!silent) {
        updateUI('checking');
    }

    // Android: no updater plugin — the backend fetches the release manifest
    // through its Tor-aware client and compares versions for us. The Tor
    // gate here is for messaging only; the backend fails closed regardless.
    if (platformFeatures.os === 'android') {
        try {
            const transport = await resolveUpdateTransport();
            if (!transport.allowed) {
                console.log('Updater: skipping update check (Tor enabled but not connected)');
                if (!silent) updateUI('error', 'Update check paused until Tor connects');
                return false;
            }
            const info = await window.__TAURI__.core.invoke('check_app_update', { beta: followPreviewChannel() });
            if (!info.available) {
                if (!silent) updateUI('no-updates');
                return false;
            }
            currentUpdate = { version: info.latest, body: info.notes };
            updateUI('available');
            return true;
        } catch (error) {
            console.error('Updater: Error checking for updates:', error);
            if (!silent) updateUI('error', 'Failed to check for updates');
            return false;
        }
    }

    try {
        const transport = await resolveUpdateTransport();
        if (!transport.allowed) {
            console.log('Updater: skipping update check (Tor enabled but not connected)');
            if (!silent) {
                updateUI('error', 'Update check paused until Tor connects');
            }
            return false;
        }
        // The baked updater config points at the build's native channel. When
        // the user's choice differs (stable build opted INTO beta, or a
        // preview build opted OUT), the check runs through a backend command
        // that builds an updater for the chosen channel at runtime. Same
        // manifest format, same signature verification, different pointer.
        const wantedChannel = followPreviewChannel() ? 'preview' : 'stable';
        const bakedChannel = versionInfo.preview === null ? 'stable' : 'preview';
        if (wantedChannel !== bakedChannel) {
            const info = await window.__TAURI__.core.invoke('check_channel_update', {
                channel: wantedChannel,
                proxy: transport.proxy || null,
            });
            if (!info.available) {
                if (!silent) updateUI('no-updates');
                return false;
            }
            currentUpdate = { version: info.latest, body: info.notes, channelOverride: true };
            updateUI('available');
            return true;
        }

        const update = await window.__TAURI__.updater.check(transport.proxy ? { proxy: transport.proxy } : undefined);
        
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

    // Channel override: the Update object lives backend-side (stashed by the
    // check), so download + install runs there and streams progress back.
    if (currentUpdate.channelOverride) {
        try {
            const stopProgress = await window.__TAURI__.event.listen('update_download_progress', (evt) => {
                const { received = 0, total = 0 } = evt.payload || {};
                updateUI('downloading', '', total > 0 ? Math.round((received / total) * 100) : 0);
            });
            try {
                await window.__TAURI__.core.invoke('install_channel_update');
            } finally {
                stopProgress();
            }
            updateUI('ready');
        } catch (error) {
            console.error('Updater: Error installing update:', error);
            updateUI('error', 'Failed to download update');
        }
        return;
    }

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
async function initializeUpdater() {
    // iOS: no updater plugin and no store hand-off wired up yet.
    if (platformFeatures.os === 'ios') {
        const updatesSection = document.getElementById('settings-updates');
        if (updatesSection) {
            updatesSection.style.display = 'none';
        }
        return;
    }

    // Resolve the channel before anything renders or checks: the endpoint,
    // the button label and the status copy all branch on it.
    versionInfo = parseVersion(await getCurrentVersion());

    // Initialize UI when DOM is ready
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', initializeUpdaterUI);
    } else {
        initializeUpdaterUI();
    }

    // Android: resolve the install source BEFORE the first check so an
    // available-update button renders with the real store label instead of
    // the sideload default. The action self-heals by click time; the label
    // would otherwise stay wrong until the next check.
    if (platformFeatures.os === 'android') {
        try {
            androidInstallSource = await window.__TAURI__.core.invoke('get_install_source');
        } catch (e) { /* keep the sideload default */ }
        updateBetaRowVisibility();
    }

    // Check for updates immediately after app start
    checkForUpdates(true);

    // Check for updates every 4 hours
    setInterval(() => {
        checkForUpdates(true);
    }, 4 * 60 * 60 * 1000);
}
