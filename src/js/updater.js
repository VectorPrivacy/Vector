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
    VectorSvelte.setUpdates({ betaRow: platformFeatures.os !== 'android' || !androidInstallSource.has_store });
}

// Initialize updater UI elements
function initializeUpdaterUI() {
    VectorSvelte.setSettingsHandlers('updates', {
            check: handleButtonClick,
            restart: () => window.__TAURI__.process.relaunch(),
            explainBeta: () => popupConfirm('Beta Updates', 'Beta builds are <b>release candidates of the next Vector version</b>, offered here before the official release.<br><br>They carry the newest fixes and features with a little less polish, and you\'ll be moved onto the official build the moment it releases.<br><br>Turning this off on a beta parks you on your current build until the next official release, then you ride stable from there.', true),
            // Flipping re-checks on the new channel; OFF withdraws an offered RC.
            setBeta: (on) => {
                try { localStorage.setItem('beta_updates', on ? 'true' : 'false'); } catch (_) {}
                VectorSvelte.setUpdates({ beta: on });
                currentUpdate = null;
                VectorSvelte.setShellFlag('updateDot', false);
                updateUI('idle');
                checkForUpdates(false);
            },
    });
    VectorSvelte.setUpdates({
        version: versionInfo.display,
        preview: versionInfo.preview !== null,
        beta: updateChannel() === 'preview',
    });
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

// Update UI state. Transient outcomes (error, no-updates) fall back to idle on a timer.
let updateUiTimer = null;
function updateUI(state, message = '', progress = 0) {
    updateState = state;
    clearTimeout(updateUiTimer);
    const found = state === 'available' && currentUpdate;
    VectorSvelte.setUpdates({
        phase: state,
        message,
        progress,
        newVersion: found ? parseVersion(currentUpdate.version).display : '',
        changelog: found ? (currentUpdate.body || '') : '',
        downloadLabel: platformFeatures.os === 'android' ? androidUpdateButtonLabel() : 'Download Update',
    });
    if (state === 'available') VectorSvelte.setShellFlag('updateDot', true);
    else if (state === 'downloading' || state === 'ready') VectorSvelte.setShellFlag('updateDot', false);
    if (state === 'error' || state === 'no-updates') {
        updateUiTimer = setTimeout(() => updateUI('idle'), state === 'error' ? 5000 : 3000);
    }
}

// Check for updates. Every check runs in the backend, through the account's network: the page
// never picks a proxy and never talks to the updater plugin itself.
async function checkForUpdates(silent = false) {
    if (updateState === 'checking' || updateState === 'downloading') return;

    if (!silent) {
        updateUI('checking');
    }

    try {
        let info;
        if (platformFeatures.os === 'android') {
            info = await window.__TAURI__.core.invoke('check_app_update', { beta: followPreviewChannel() });
        } else {
            info = await window.__TAURI__.core.invoke('check_channel_update', {
                channel: followPreviewChannel() ? 'preview' : 'stable',
            });
        }
        if (!info.available) {
            if (!silent) updateUI('no-updates');
            return false;
        }
        currentUpdate = { version: info.latest, body: info.notes };
        updateUI('available');
        return true;
    } catch (error) {
        console.error('Updater: Error checking for updates:', error);
        if (!silent) updateUI('error', typeof error === 'string' ? error : 'Failed to check for updates');
        return false;
    }
}

// Download update: the update found by the last check lives backend-side, so the download and
// install run there too and stream their progress back.
async function downloadUpdate() {
    if (!currentUpdate || updateState === 'downloading') return;

    updateUI('downloading', '', 0);
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
        updateUI('error', typeof error === 'string' ? error : 'Failed to download update');
    }
}

// Auto-check for updates on app start (silent check)
async function initializeUpdater() {
    // iOS: no updater plugin and no store hand-off wired up yet.
    if (platformFeatures.os === 'ios') {
        VectorSvelte.setSettingsScreen({ platform: { updates: false } });
        return;
    }

    // A store flavour (F-Droid) has no updater at all: the store notifies and
    // installs, so the section only says where updates come from. The web build
    // is served fresh, so its updates are simply the next load.
    if (platformFeatures.self_update === false) {
        versionInfo = parseVersion(await getCurrentVersion());
        const render = async () => {
            if (platformFeatures.os === 'web') {
                VectorSvelte.setSettingsHandlers('updates', {});
                VectorSvelte.setUpdates({ version: versionInfo.display, preview: versionInfo.preview !== null, phase: 'store', message: 'Updates arrive when you reload Vector' });
                return;
            }
            let label = 'F-Droid';
            try {
                const source = await window.__TAURI__.core.invoke('get_install_source');
                if (source.has_store) label = source.label;
            } catch (_) { /* keep the flavour's own store */ }
            VectorSvelte.setSettingsHandlers('updates', {});
            VectorSvelte.setUpdates({ version: versionInfo.display, preview: versionInfo.preview !== null, phase: 'store', message: `Updates arrive through ${label}` });
        };
        if (document.readyState === 'loading') {
            document.addEventListener('DOMContentLoaded', render);
        } else {
            render();
        }
        return;
    }

    // Resolve the channel before anything renders or checks: the endpoint,
    // the button label and the status copy all branch on it.
    versionInfo = parseVersion(await getCurrentVersion());

    initializeUpdaterUI();

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
