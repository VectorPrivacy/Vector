/**
 * Mini Apps Marketplace Module
 *
 * This module provides the frontend interface for the decentralized Mini Apps marketplace.
 * Apps are stored on Blossom (decentralized storage) and metadata is published via Nostr.
 */

/**
 * @typedef {Object} MarketplaceApp
 * @property {string} id - Unique identifier
 * @property {string} name - Display name
 * @property {string} description - App description
 * @property {string} version - Version string (marketplace/latest version)
 * @property {string} blossom_hash - SHA-256 hash of the .xdc file
 * @property {string} download_url - Blossom download URL
 * @property {number} size - File size in bytes
 * @property {string|null} icon_url - Optional icon URL (Blossom)
 * @property {string|null} icon_mime - Optional icon MIME type (e.g., "image/png", "image/svg+xml")
 * @property {string[]} categories - Category tags
 * @property {string|null} changelog - Extended description or changelog
 * @property {string|null} developer - Developer/creator name
 * @property {string|null} source_url - Source code or website URL
 * @property {string} publisher - Publisher's npub
 * @property {number} published_at - Unix timestamp
 * @property {boolean} installed - Whether the app is installed locally
 * @property {string|null} local_path - Local file path if installed
 * @property {string|null} installed_version - Version currently installed (if installed)
 * @property {boolean} update_available - Whether an update is available
 */

/**
 * @typedef {Object} InstallStatus
 * @property {string} type - 'NotInstalled' | 'Downloading' | 'Installed' | 'Failed'
 * @property {number} [progress] - Download progress (0-100) for Downloading status
 * @property {string} [path] - Local path for Installed status
 * @property {string} [error] - Error message for Failed status
 */

// Note: 'invoke' is already declared in main.js, so we use it directly

// Marketplace state
let marketplaceApps = [];
let isMarketplaceLoading = false;
let marketplaceError = null;
let marketplaceSearchQuery = '';
let marketplaceActiveFilters = []; // Array of category strings
let marketplaceShouldAnimate = false; // Only animate after initial load from loading state
let _marketplaceFetchPromise = null; // Dedup guard: shared promise for concurrent fetch calls
let _marketplaceInitInProgress = false; // Dedup guard: prevent concurrent initMarketplace calls

/**
 * Fetch apps from the marketplace
 * @param {boolean} trustedOnly - Only fetch from trusted publishers
 * @returns {Promise<MarketplaceApp[]>}
 */
async function fetchMarketplaceApps(trustedOnly = true) {
    // Deduplicate concurrent fetches — return the in-flight promise if one exists
    if (_marketplaceFetchPromise) return _marketplaceFetchPromise;

    _marketplaceFetchPromise = (async () => {
        try {
            isMarketplaceLoading = true;
            marketplaceError = null;
            const apps = await invoke('marketplace_fetch_apps', { trustedOnly });
            syncMarketplaceApps(apps);
            return apps;
        } catch (error) {
            console.error('Failed to fetch marketplace apps:', error);
            marketplaceError = error.toString();
            throw error;
        } finally {
            isMarketplaceLoading = false;
            _marketplaceFetchPromise = null;
        }
    })();

    return _marketplaceFetchPromise;
}

/**
 * Get cached marketplace apps (without network fetch)
 * @returns {Promise<MarketplaceApp[]>}
 */
async function getCachedMarketplaceApps() {
    try {
        const apps = await invoke('marketplace_get_cached_apps');
        syncMarketplaceApps(apps);
        return apps;
    } catch (error) {
        console.error('Failed to get cached marketplace apps:', error);
        throw error;
    }
}

/**
 * Get a specific marketplace app by ID
 * @param {string} appId - The app ID
 * @returns {Promise<MarketplaceApp|null>}
 */
async function getMarketplaceApp(appId) {
    try {
        return await invoke('marketplace_get_app', { appId });
    } catch (error) {
        console.error('Failed to get marketplace app:', error);
        throw error;
    }
}

/**
 * Get the installation status of an app
 * @param {string} appId - The app ID
 * @returns {Promise<InstallStatus>}
 */
async function getInstallStatus(appId) {
    try {
        return await invoke('marketplace_get_install_status', { appId });
    } catch (error) {
        console.error('Failed to get install status:', error);
        throw error;
    }
}

/**
 * Install a marketplace app
 * @param {string} appId - The app ID to install
 * @returns {Promise<string>} The local file path
 */
async function installMarketplaceApp(appId) {
    try {
        return await invoke('marketplace_install_app', { appId });
    } catch (error) {
        console.error('Failed to install marketplace app:', error);
        throw error;
    }
}

/**
 * Check if an app is installed
 * @param {string} appId - The app ID
 * @returns {Promise<string|null>} The local path if installed, null otherwise
 */
async function checkAppInstalled(appId) {
    try {
        return await invoke('marketplace_check_installed', { appId });
    } catch (error) {
        console.error('Failed to check if app is installed:', error);
        throw error;
    }
}

/**
 * Sync installation status for all cached apps
 * @returns {Promise<void>}
 */
async function syncInstallStatus() {
    try {
        await invoke('marketplace_sync_install_status');
    } catch (error) {
        console.error('Failed to sync install status:', error);
        throw error;
    }
}

/**
 * Open a marketplace app (install if needed, then launch)
 * Checks for permission prompt if the app requests permissions and hasn't been prompted yet.
 * @param {string} appId - The app ID
 * @param {MarketplaceApp} [app] - Optional app object with requested_permissions
 * @returns {Promise<void>}
 */
async function openMarketplaceApp(appId, app) {
    try {
        // If app object provided and has requested permissions, check if we need to prompt
        // Use blossom_hash as the permission identifier (content-based security)
        if (app && app.requested_permissions && app.requested_permissions.length > 0 && app.blossom_hash) {
            const hasBeenPrompted = await invoke('miniapp_has_permission_prompt', { fileHash: app.blossom_hash });

            if (!hasBeenPrompted) {
                // Show permission prompt before opening
                const userGranted = await showPermissionPrompt(app);
                if (!userGranted) {
                    // User cancelled - don't open the app
                    return;
                }
            }
        }

        await invoke('marketplace_open_app', { appId });
    } catch (error) {
        console.error('Failed to open marketplace app:', error);
        throw error;
    }
}

/**
 * Show a permission request prompt for an app
 * @param {MarketplaceApp} app - The app requesting permissions
 * @returns {Promise<boolean>} True if user confirmed (grant or deny), false if cancelled
 */
async function showPermissionPrompt(app) {
    // Get available permissions metadata first (outside Promise to properly throw on error)
    let availablePermissions;
    try {
        availablePermissions = await invoke('miniapp_get_available_permissions');
    } catch (error) {
        console.error('Failed to get available permissions:', error);
        return true; // Continue anyway
    }

    // Parse requested permissions
    const requestedPermissions = app.requested_permissions.split(',').filter(s => s.trim());

    // Build permissions list HTML - using same toggle style as App Details
    const permissionsHtml = requestedPermissions.map(permId => {
        const permInfo = availablePermissions.find(p => p.id === permId.trim());
        if (!permInfo) return '';

        return `
            <div class="permission-prompt-item">
                <div class="permission-prompt-info">
                    <span class="permission-prompt-label">${escapeHtml(permInfo.label)}</span>
                    <span class="permission-prompt-desc">${escapeHtml(permInfo.description)}</span>
                </div>
                <label class="toggle-container permission-prompt-toggle">
                    <input type="checkbox" name="permission" value="${escapeHtml(permId.trim())}">
                    <span class="neon-toggle"></span>
                </label>
            </div>
        `;
    }).filter(h => h).join('');

    // Create overlay
    let overlay = document.getElementById('permission-prompt-overlay');
    if (!overlay) {
        overlay = document.createElement('div');
        overlay.id = 'permission-prompt-overlay';
        overlay.className = 'permission-prompt-overlay';
        document.body.appendChild(overlay);
    }

    overlay.innerHTML = `
        <div class="permission-prompt-container">
            <div class="permission-prompt-header">
                <h2>Permission Request</h2>
                <p class="permission-prompt-subtitle">${escapeHtml(app.name)} is requesting the following permissions:</p>
            </div>
            <div class="permission-prompt-content">
                <p class="permission-prompt-hint">Select which permissions you want to grant. The app may have reduced functionality without certain permissions.</p>
                <div class="permission-prompt-list">
                    ${permissionsHtml}
                </div>
            </div>
            <div class="permission-prompt-buttons">
                <button class="file-preview-btn file-preview-btn-cancel" id="permission-prompt-deny">Cancel</button>
                <button class="file-preview-btn file-preview-btn-send" id="permission-prompt-continue">Continue</button>
            </div>
        </div>
    `;

    overlay.style.display = 'flex';
    setTimeout(() => overlay.classList.add('active'), 10);

    // Return a Promise that resolves when the user makes a choice
    return new Promise((resolve) => {
        const denyBtn = document.getElementById('permission-prompt-deny');
        const continueBtn = document.getElementById('permission-prompt-continue');

        const closePrompt = () => {
            popBack('permission-prompt');
            // Clean up event handlers to prevent memory leaks
            denyBtn.onclick = null;
            continueBtn.onclick = null;
            overlay.onclick = null;
            overlay.classList.remove('active');
            setTimeout(() => overlay.style.display = 'none', 300);
        };

        denyBtn.onclick = () => {
            // Cancel - don't save anything, just close and cancel the app opening
            closePrompt();
            resolve(false);
        };

        continueBtn.onclick = async () => {
            // Collect selected permissions
            const checkboxes = overlay.querySelectorAll('input[name="permission"]');
            const permissions = Array.from(checkboxes).map(cb => [cb.value, cb.checked]);

            try {
                await invoke('miniapp_set_permissions', {
                    fileHash: app.blossom_hash,
                    permissions: permissions
                });
                // Refresh the App Details permissions list if it's visible
                loadAppPermissions(app);
            } catch (error) {
                console.error('Failed to save permissions:', error);
            }
            closePrompt();
            resolve(true);
        };

        // Allow clicking outside to cancel
        overlay.onclick = (e) => {
            if (e.target === overlay) {
                closePrompt();
                resolve(false);
            }
        };

        pushBack('permission-prompt', () => {
            closePrompt();
            resolve(false);
        });
    });
}

async function uninstallMarketplaceApp(appId, appName) {
    try {
        await invoke('marketplace_uninstall_app', { appId, appName });
    } catch (error) {
        console.error('Failed to uninstall marketplace app:', error);
        throw error;
    }
}

/**
 * Update a marketplace app to the latest version
 * Downloads to temp file first, verifies hash, then replaces old version
 * @param {string} appId - The app ID to update
 * @returns {Promise<string>} The local file path
 */
async function updateMarketplaceApp(appId) {
    try {
        return await invoke('marketplace_update_app', { appId });
    } catch (error) {
        console.error('Failed to update marketplace app:', error);
        throw error;
    }
}

/**
 * Format file size for display
 * @param {number} bytes - Size in bytes
 * @returns {string} Formatted size string
 */
function formatFileSize(bytes) {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
}

/**
 * Format timestamp for display
 * @param {number} timestamp - Unix timestamp
 * @returns {string} Formatted date string
 */
function formatPublishDate(timestamp) {
    const date = new Date(timestamp * 1000);
    return date.toLocaleDateString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric'
    });
}

/**
 * Format a source URL for display (show domain + path)
 * @param {string} url - The full URL
 * @returns {string} Formatted URL for display
 */
function formatSourceUrl(url) {
    try {
        const parsed = new URL(url);
        // Show domain + truncated path
        let display = parsed.hostname;
        if (parsed.pathname && parsed.pathname !== '/') {
            const path = parsed.pathname.length > 20
                ? parsed.pathname.substring(0, 20) + '...'
                : parsed.pathname;
            display += path;
        }
        return display;
    } catch {
        // If URL parsing fails, just truncate
        return url.length > 30 ? url.substring(0, 30) + '...' : url;
    }
}

/**
 * Create a marketplace app card element
 * @param {MarketplaceApp} app - The app data
 * @returns {HTMLElement}
 */
/**
 * Check if an app is a game (has "game" category but not "app" category)
 * @param {MarketplaceApp} app - The app to check
 * @returns {boolean} True if the app is a game
 */
function isGameApp(app) {
    if (!app.categories || !Array.isArray(app.categories)) {
        return true; // Default to game if no categories
    }
    const lowerCategories = app.categories.map(c => c.toLowerCase());
    // It's a game if it has "game" tag OR doesn't have "app" tag
    return lowerCategories.includes('game') || !lowerCategories.includes('app');
}

/**
 * Get the action text for an app (Play for games, Open for apps)
 * @param {MarketplaceApp} app - The app
 * @returns {string} "Play" or "Open"
 */
function getAppActionText(app) {
    return isGameApp(app) ? 'Play' : 'Open';
}

/**
 * Get the launching text for an app
 * @param {MarketplaceApp} app - The app
 * @returns {string} "Launching..." or "Opening..."
 */
function getAppLaunchingText(app) {
    return isGameApp(app) ? 'Launching...' : 'Opening...';
}

// The catalogue and the details panel are Svelte islands over lib/marketplace.svelte.js.
// `marketplaceApps` stays the plain array other modules read; the store mirrors it.
const iconWaiters = new Map();
function syncMarketplaceApps(apps) {
    marketplaceApps = apps;
    VectorSvelte.mktSetApps(apps);
    for (const app of apps) resolveMarketplaceIcon(app);
}

function marketplaceIconKey(app) {
    return app.icon_cached ? 'file:' + app.icon_cached : (app.icon_url || null);
}

/** Cached local icon directly; a remote icon_url via the backend cache (the WebView never fetches remote: Tor). */
function resolveMarketplaceIcon(app) {
    const key = marketplaceIconKey(app);
    if (!key || VectorSvelte.mktIcons().has(key) || iconWaiters.has(key)) return;
    if (app.icon_cached) { VectorSvelte.mktSetIcon(key, convertFileSrc(app.icon_cached)); return; }
    iconWaiters.set(key, true);
    invoke('cache_url_image', { url: app.icon_url }).then(path => {
        // null = a download already in flight; inline_image_cached fills it in.
        if (path) { iconWaiters.delete(key); VectorSvelte.mktSetIcon(key, convertFileSrc(path)); }
    }).catch(() => { iconWaiters.delete(key); VectorSvelte.mktSetIcon(key, false); });
}

window.__TAURI__.event.listen('inline_image_cached', (event) => {
    const { url, path } = event.payload;
    if (!iconWaiters.has(url)) return;
    iconWaiters.delete(url);
    VectorSvelte.mktSetIcon(url, path ? convertFileSrc(path) : false);
});

let marketplaceMounted = false;
function ensureMarketplaceIslands() {
    if (marketplaceMounted) return;
    marketplaceMounted = true;
    const h = {
        iconKey: marketplaceIconKey,
        actionText: getAppActionText,
        fileSize: formatFileSize,
        publishDate: formatPublishDate,
        sourceUrl: formatSourceUrl,
        showTip: showGlobalTooltip,
        hideTip: hideGlobalTooltip,
        showDetails: showAppDetails,
        closeDetails: closeAppDetailsPanel,
        retry: () => initMarketplace(),
        cardAction: handleAppInstallOrPlay,
        install: installFromDetails,
        update: updateFromDetails,
        play: playFromDetails,
        uninstall: uninstallFromDetails,
        setPermission: setAppPermission,
        resetPermissions: resetAppPermissions,
        publisher: publisherProfile,
        openPublisher: openPublisherProfile,
        openUrl: (url) => {
            if (typeof openUrl === 'function') openUrl(url);
            else invoke('open_url', { url }).catch(console.error);
        },
    };
    document.getElementById('app-details-back-btn').onclick = () => { closeAppDetailsPanel(); };
    VectorSvelte.mountMarketplace({
        body: document.querySelector('.marketplace-scroll-container'),
        filters: document.getElementById('marketplace-filters'),
        details: document.getElementById('app-details-content'),
        h,
    });
}

/** Run one action on an app with the button showing it, and a 2s "Failed" if it throws. */
async function runAppAction(app, kind, label, failedLabel, fn) {
    VectorSvelte.mktSetAction(app.id, { kind, label });
    try {
        await fn();
        VectorSvelte.mktSetAction(app.id, null);
        return true;
    } catch (error) {
        console.error(`Failed to ${kind} app:`, error);
        VectorSvelte.mktSetAction(app.id, { kind: 'failed', label: failedLabel });
        setTimeout(() => {
            if (VectorSvelte.mktActions().get(app.id)?.kind === 'failed') VectorSvelte.mktSetAction(app.id, null);
        }, 2000);
        return false;
    }
}

async function installApp(app) {
    return runAppAction(app, 'installing', 'Installing', 'Failed', async () => {
        await installMarketplaceApp(app.id);
        VectorSvelte.mktPatchApp(app.id, { installed: true, installed_version: app.version });
    });
}

async function updateApp(app) {
    return runAppAction(app, 'updating', 'Updating', 'Failed', async () => {
        await updateMarketplaceApp(app.id);
        VectorSvelte.mktPatchApp(app.id, { installed_version: app.version, update_available: false });
    });
}

async function launchApp(app) {
    return runAppAction(app, 'launching', getAppLaunchingText(app), getAppActionText(app), () => openMarketplaceApp(app.id, app));
}

/** The card button: update if one is waiting, play if installed, otherwise install. */
async function handleAppInstallOrPlay(app) {
    if (app.update_available) await updateApp(app);
    else if (app.installed || app.local_path) await launchApp(app);
    else await installApp(app);
}

/**
 * Close the app details panel with fade out animation
 * @returns {Promise<void>} Resolves when the animation completes
 */
function closeAppDetailsPanel() {
    return new Promise((resolve) => {
        const panel = document.getElementById('app-details-panel');
        if (panel && panel.style.display !== 'none') {
            popBack('app-details');
            panel.classList.add('closing');
            panel.addEventListener('animationend', function handler() {
                panel.removeEventListener('animationend', handler);
                panel.style.display = 'none';
                panel.classList.remove('closing');
                VectorSvelte.mktCloseDetails();
                resolve();
            });
        } else {
            resolve();
        }
    });
}

/** Open the details panel on one app. */
function showAppDetails(app) {
    const panel = document.getElementById('app-details-panel');
    if (!panel) return;
    ensureMarketplaceIslands();
    if (panel.style.display === 'none') pushBack('app-details', () => { closeAppDetailsPanel(); });
    panel.dataset.appId = app.id;
    VectorSvelte.mktOpenDetails(app.id);
    panel.style.display = 'flex';
    if (app.requested_permissions && app.requested_permissions.length > 0 && (app.installed || app.local_path)) {
        loadAppPermissions(app);
    }
}

/** Permission toggles keyed by the bundle hash: content-based, so a re-upload asks again. */
async function loadAppPermissions(app) {
    const fileHash = app.blossom_hash;
    if (!fileHash) { VectorSvelte.mktSetPerms({ error: 'No permissions available' }); return; }
    try {
        const available = await invoke('miniapp_get_available_permissions');
        const grantedStr = await invoke('miniapp_get_granted_permissions', { fileHash });
        const granted = new Set(grantedStr ? grantedStr.split(',').filter(s => s) : []);
        const items = app.requested_permissions.split(',').map(s => s.trim()).filter(Boolean).map(id => {
            const info = available.find(p => p.id === id);
            return info ? { id, label: info.label, description: info.description, granted: granted.has(id) } : null;
        }).filter(Boolean);
        if (VectorSvelte.mktState().detailsId === app.id) VectorSvelte.mktSetPerms({ items });
    } catch (error) {
        console.error('Failed to load app permissions:', error);
        VectorSvelte.mktSetPerms({ error: 'Failed to load permissions' });
    }
}

async function setAppPermission(app, permission, granted) {
    try {
        await invoke('miniapp_set_permission', { fileHash: app.blossom_hash, permission, granted });
        const perms = VectorSvelte.mktPerms();
        if (perms?.items) VectorSvelte.mktSetPerms({ items: perms.items.map(p => p.id === permission ? { ...p, granted } : p) });
    } catch (error) {
        console.error('Failed to update permission:', error);
        loadAppPermissions(app);
    }
}

async function resetAppPermissions(app) {
    try {
        await invoke('miniapp_revoke_all_permissions', { fileHash: app.blossom_hash });
        loadAppPermissions(app);
    } catch (error) {
        console.error('Failed to reset permissions:', error);
    }
}

function current(app) { return marketplaceApps.find(a => a.id === app.id) || app; }

async function installFromDetails(app) {
    if (await installApp(app)) loadAppPermissions(current(app));
}
async function updateFromDetails(app) { await updateApp(app); }
async function playFromDetails(app) { await launchApp(app); }

async function uninstallFromDetails(app) {
    const confirmed = await popupConfirm(
        `Uninstall ${app.name}?`,
        'This will delete the app and remove it from your history.',
        false,
        '',
        'vector_warning.svg'
    );
    if (!confirmed) return;
    await runAppAction(app, 'uninstalling', 'Uninstalling...', 'Failed', async () => {
        await uninstallMarketplaceApp(app.id, app.name);
        VectorSvelte.mktPatchApp(app.id, { installed: false, local_path: null });
    });
}

/** The publisher as the profile list knows them, or null for a bare npub. */
function publisherProfile(npub) {
    try {
        const profile = typeof getProfile === 'function' ? getProfile(npub) : null;
        if (!profile) return null;
        return { name: getName(profile), avatar: getProfileAvatarSrc(profile) || null };
    } catch (error) {
        console.error('Failed to load publisher profile:', error);
        return null;
    }
}

async function openPublisherProfile(npub) {
    if (!npub || typeof openProfile !== 'function') return;
    await Promise.all([closeAppDetailsPanel(), hideMarketplacePanel()]);
    openProfile((typeof getProfile === 'function' ? getProfile(npub) : null) || { id: npub });
}

function addMarketplaceFilter(category) { VectorSvelte.mktAddFilter(category); }
function clearMarketplaceFilters() {
    VectorSvelte.mktClearFilters();
    const searchInput = document.getElementById('marketplace-search-input');
    if (searchInput) searchInput.value = '';
}

/** Open the Nexus: cached apps at once, then a fresh fetch with install status. */
async function initMarketplace() {
    if (_marketplaceInitInProgress) return;
    _marketplaceInitInProgress = true;
    ensureMarketplaceIslands();
    clearMarketplaceFilters();
    VectorSvelte.mktSetAnimate(false);

    const searchInput = document.getElementById('marketplace-search-input');
    if (searchInput) {
        searchInput.removeEventListener('input', handleMarketplaceSearch);
        searchInput.addEventListener('input', handleMarketplaceSearch);
    }

    VectorSvelte.mktSetLoading(true);
    try {
        const cachedApps = await getCachedMarketplaceApps();
        if (cachedApps.length > 0) {
            VectorSvelte.mktSetLoading(false);
        } else {
            // Nothing cached: the first paint comes from the network, so it fades in.
            VectorSvelte.mktSetAnimate(true);
        }
        await fetchMarketplaceApps(true);
        await syncInstallStatus();
        await getCachedMarketplaceApps();
        VectorSvelte.mktSetLoading(false);
        VectorSvelte.flushSync();
        VectorSvelte.mktSetAnimate(false);
    } catch (error) {
        console.error('Failed to initialize marketplace:', error);
        VectorSvelte.mktSetError(error.toString());
    } finally {
        _marketplaceInitInProgress = false;
    }
}

function handleMarketplaceSearch(e) {
    VectorSvelte.mktSetQuery(e.target.value);
}

/**
 * Refresh the marketplace (fetch new data)
 * @param {HTMLElement} container - The marketplace container element
 */
async function refreshMarketplace() {
    await initMarketplace();
}

// ============================================================================
// Marketplace Publishing (for trusted publishers only)
// ============================================================================

// Cached trusted publisher npub
let trustedPublisherNpub = null;

/**
 * Get the trusted publisher npub
 * @returns {Promise<string>}
 */
async function getTrustedPublisher() {
    if (trustedPublisherNpub) {
        return trustedPublisherNpub;
    }
    try {
        trustedPublisherNpub = await invoke('marketplace_get_trusted_publisher');
        return trustedPublisherNpub;
    } catch (error) {
        console.error('Failed to get trusted publisher:', error);
        return null;
    }
}

/**
 * Check if the current user is a trusted publisher
 * @returns {Promise<boolean>}
 */
async function isCurrentUserTrustedPublisher() {
    const trustedNpub = await getTrustedPublisher();
    // strPubkey is defined in main.js
    console.log('[Marketplace] Checking trusted publisher:', {
        trustedNpub,
        strPubkey: typeof strPubkey !== 'undefined' ? strPubkey : 'undefined',
        match: trustedNpub && typeof strPubkey !== 'undefined' && strPubkey === trustedNpub
    });
    return trustedNpub && typeof strPubkey !== 'undefined' && strPubkey === trustedNpub;
}

/**
 * Publish a Mini App to the marketplace
 * @param {string} filePath - Path to the .xdc file
 * @param {string} appId - Unique app identifier
 * @param {string} name - App name
 * @param {string} description - App description
 * @param {string} version - Version string
 * @param {string[]} categories - Category tags
 * @param {string|null} changelog - Optional changelog
 * @param {string|null} developer - Optional developer name
 * @param {string|null} sourceUrl - Optional source URL
 * @param {string|null} permissions - Optional comma-separated permissions string
 * @returns {Promise<string>} Event ID of the published event
 */
async function publishMarketplaceApp(filePath, appId, name, description, version, categories, changelog, developer, sourceUrl, permissions) {
    try {
        const eventId = await invoke('marketplace_publish_app', {
            filePath,
            appId,
            name,
            description,
            version,
            categories,
            changelog,
            developer,
            sourceUrl,
            permissions,
        });
        return eventId;
    } catch (error) {
        console.error('Failed to publish marketplace app:', error);
        throw error;
    }
}

/**
 * Show the publish app dialog
 * @param {string} filePath - Path to the .xdc file
 * @param {object} miniAppInfo - Mini App info from loadMiniAppInfo
 */
async function showPublishAppDialog(filePath, miniAppInfo) {
    // Create overlay if it doesn't exist
    let overlay = document.getElementById('publish-app-overlay');
    if (!overlay) {
        overlay = document.createElement('div');
        overlay.id = 'publish-app-overlay';
        overlay.className = 'publish-app-overlay';
        document.body.appendChild(overlay);
    }

    // Generate a default app ID from the name
    const defaultAppId = (miniAppInfo?.name || 'app')
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-|-$/g, '');

    overlay.innerHTML = `
        <div class="publish-app-container">
            <div class="publish-app-header">
                <h2>Publish to Nexus</h2>
            </div>
            <div class="publish-app-content">
                <div class="publish-app-icon-container">
                    ${miniAppInfo?.icon_data
                        ? `<img src="${escapeHtml(miniAppInfo.icon_data)}" alt="App Icon" class="publish-app-icon">`
                        : '<span class="icon icon-play publish-app-icon-placeholder"></span>'
                    }
                </div>
                <div class="publish-app-form">
                    <div class="publish-app-field">
                        <label for="publish-app-id">App ID</label>
                        <input type="text" id="publish-app-id" value="${escapeHtml(defaultAppId)}" placeholder="my-awesome-game">
                        <span class="publish-app-hint">Unique identifier (lowercase, no spaces)</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-name">Name</label>
                        <input type="text" id="publish-app-name" value="${escapeHtml(miniAppInfo?.name || '')}" placeholder="My Awesome Game">
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-description">Description</label>
                        <textarea id="publish-app-description" placeholder="A brief description of your app...">${escapeHtml(miniAppInfo?.description || '')}</textarea>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-version">Version</label>
                        <input type="text" id="publish-app-version" value="${escapeHtml(miniAppInfo?.version || '1.0.0')}" placeholder="1.0.0">
                    </div>
                    <div class="publish-app-field publish-app-toggle-field">
                        <label class="toggle-container">
                            <span>Is this app a Game?</span>
                            <input type="checkbox" id="publish-app-is-game" checked>
                            <span class="neon-toggle"></span>
                        </label>
                        <span class="publish-app-hint">This allows Vector to present your app correctly the users.</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-categories">Categories</label>
                        <input type="text" id="publish-app-categories" placeholder="shooter, art, multiplayer, arcade">
                        <span class="publish-app-hint">Comma-separated tags</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-developer">Developer (optional)</label>
                        <input type="text" id="publish-app-developer" placeholder="Developer or studio name">
                        <span class="publish-app-hint">The original creator of this app</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-source">Source / Website (optional)</label>
                        <input type="text" id="publish-app-source" value="${escapeHtml(miniAppInfo?.source_code_url || '')}" placeholder="https://github.com/...">
                        <span class="publish-app-hint">Link to source code or website</span>
                    </div>
                    <div class="publish-app-field">
                        <label for="publish-app-changelog">Changelog (optional)</label>
                        <textarea id="publish-app-changelog" placeholder="What's new in this version..."></textarea>
                    </div>
                    <div class="publish-app-field">
                        <label>Requested Permissions (optional)</label>
                        <span class="publish-app-hint">Users must explicitly grant these permissions. Leave all unchecked if your app doesn't need special access.</span>
                        <div class="publish-app-permissions" id="publish-app-permissions">
                            <!-- Permissions will be loaded dynamically -->
                        </div>
                    </div>
                </div>
            </div>
            <div class="publish-app-buttons">
                <button class="file-preview-btn file-preview-btn-cancel" id="publish-app-cancel">Cancel</button>
                <button class="file-preview-btn file-preview-btn-send" id="publish-app-submit">
                    <span class="icon icon-star"></span> Publish
                </button>
            </div>
        </div>
    `;

    // Show overlay
    overlay.style.display = 'flex';
    setTimeout(() => overlay.classList.add('active'), 10);

    // Add App ID change listener to pre-fill from existing published app
    const appIdInput = document.getElementById('publish-app-id');
    let prefillTimeout = null;
    let permissionsLoaded = false;

    const checkAndPrefillFromExisting = () => {
        const appId = appIdInput.value.trim().toLowerCase();
        if (!appId) return;

        // Find existing app with this ID that we published
        const existingApp = marketplaceApps.find(app =>
            app.id === appId &&
            typeof strPubkey !== 'undefined' &&
            app.publisher === strPubkey
        );

        if (existingApp) {
            console.log('[Marketplace] Found existing app to pre-fill:', existingApp);

            // Pre-fill description
            const descEl = document.getElementById('publish-app-description');
            if (descEl && existingApp.description) {
                descEl.value = existingApp.description;
            }

            // Pre-fill developer
            const devEl = document.getElementById('publish-app-developer');
            if (devEl && existingApp.developer) {
                devEl.value = existingApp.developer;
            }

            // Pre-fill source URL
            const sourceEl = document.getElementById('publish-app-source');
            if (sourceEl && existingApp.source_url) {
                sourceEl.value = existingApp.source_url;
            }

            // Pre-fill categories (excluding 'game' and 'app' tags)
            const catEl = document.getElementById('publish-app-categories');
            if (catEl && existingApp.categories && existingApp.categories.length > 0) {
                const filteredCategories = existingApp.categories.filter(c => c !== 'game' && c !== 'app');
                catEl.value = filteredCategories.join(', ');
            }

            // Pre-fill is-game toggle based on categories
            const isGameEl = document.getElementById('publish-app-is-game');
            if (isGameEl && existingApp.categories) {
                isGameEl.checked = existingApp.categories.includes('game');
            }

            // Pre-fill version
            const versionEl = document.getElementById('publish-app-version');
            if (versionEl && existingApp.version) {
                versionEl.value = existingApp.version;
            }

            // Pre-fill changelog
            const changelogEl = document.getElementById('publish-app-changelog');
            if (changelogEl && existingApp.changelog) {
                changelogEl.value = existingApp.changelog;
            }

            // Pre-fill permissions
            if (existingApp.requested_permissions) {
                const requestedPerms = existingApp.requested_permissions.split(',').map(p => p.trim());
                const permItems = document.querySelectorAll('#publish-app-permissions .publish-app-permission-item');
                permItems.forEach(item => {
                    const cb = item.querySelector('input[name="permissions"]');
                    if (cb) {
                        const shouldCheck = requestedPerms.includes(cb.value);
                        cb.checked = shouldCheck;
                        item.classList.toggle('checked', shouldCheck);
                    }
                });
            }

            // Show a subtle indicator that we pre-filled from existing
            const hintEl = appIdInput.nextElementSibling;
            if (hintEl && hintEl.classList.contains('publish-app-hint')) {
                hintEl.innerHTML = '✓ Pre-filled from your existing app. Update the version and changelog!';
                hintEl.style.color = 'var(--accent-color)';
            }
        }
    };

    // Check on input change (debounced)
    appIdInput.addEventListener('input', () => {
        // Reset hint
        const hintEl = appIdInput.nextElementSibling;
        if (hintEl && hintEl.classList.contains('publish-app-hint')) {
            hintEl.innerHTML = 'Unique identifier (lowercase, no spaces)';
            hintEl.style.color = '';
        }

        clearTimeout(prefillTimeout);
        prefillTimeout = setTimeout(checkAndPrefillFromExisting, 500);
    });

    // Also check on blur (when user tabs/clicks away)
    appIdInput.addEventListener('blur', checkAndPrefillFromExisting);

    // Load available permissions, then check for pre-fill
    try {
        const availablePermissions = await invoke('miniapp_get_available_permissions');
        const permissionsContainer = document.getElementById('publish-app-permissions');
        permissionsContainer.innerHTML = availablePermissions.map(perm => `
            <div class="publish-app-permission-item" data-permission="${escapeHtml(perm.id)}">
                <input type="checkbox" name="permissions" value="${escapeHtml(perm.id)}">
                <span class="publish-app-permission-check"></span>
                <div class="publish-app-permission-text">
                    <span class="publish-app-permission-label">${escapeHtml(perm.label)}</span>
                    <span class="publish-app-permission-desc">${escapeHtml(perm.description)}</span>
                </div>
            </div>
        `).join('');
        // Add click handler to toggle checkbox and visual state
        permissionsContainer.querySelectorAll('.publish-app-permission-item').forEach(item => {
            const checkbox = item.querySelector('input[type="checkbox"]');
            item.addEventListener('click', () => {
                checkbox.checked = !checkbox.checked;
                item.classList.toggle('checked', checkbox.checked);
            });
        });
        permissionsLoaded = true;
        // Now that permissions are loaded, check for pre-fill
        checkAndPrefillFromExisting();
    } catch (error) {
        console.error('Failed to load permissions:', error);
        document.getElementById('publish-app-permissions').innerHTML = '<span class="publish-app-hint">Failed to load permissions</span>';
        // Still try pre-fill for other fields even if permissions failed
        checkAndPrefillFromExisting();
    }

    // Event handlers
    const cancelBtn = document.getElementById('publish-app-cancel');
    const submitBtn = document.getElementById('publish-app-submit');

    cancelBtn.onclick = () => {
        overlay.classList.remove('active');
        setTimeout(() => overlay.style.display = 'none', 300);
    };

    overlay.onclick = (e) => {
        if (e.target === overlay) {
            overlay.classList.remove('active');
            setTimeout(() => overlay.style.display = 'none', 300);
        }
    };

    submitBtn.onclick = async () => {
        const appId = document.getElementById('publish-app-id').value.trim();
        const name = document.getElementById('publish-app-name').value.trim();
        const description = document.getElementById('publish-app-description').value.trim();
        const version = document.getElementById('publish-app-version').value.trim();
        const isGame = document.getElementById('publish-app-is-game').checked;
        const categoriesStr = document.getElementById('publish-app-categories').value.trim();
        const developer = document.getElementById('publish-app-developer').value.trim() || null;
        const sourceUrl = document.getElementById('publish-app-source').value.trim() || null;
        const changelog = document.getElementById('publish-app-changelog').value.trim() || null;

        // Collect selected permissions
        const permissionCheckboxes = document.querySelectorAll('#publish-app-permissions input[name="permissions"]:checked');
        const selectedPermissions = Array.from(permissionCheckboxes).map(cb => cb.value);
        const permissionsStr = selectedPermissions.length > 0 ? selectedPermissions.join(',') : null;

        // Validate
        if (!appId) {
            alert('App ID is required');
            return;
        }
        if (!name) {
            alert('Name is required');
            return;
        }

        // Parse categories and add game/app tag based on toggle
        const categories = categoriesStr
            ? categoriesStr.toLowerCase().split(',').map(c => c.trim()).filter(c => c)
            : [];

        // Add the game or app tag at the beginning
        categories.unshift(isGame ? 'game' : 'app');

        // Show loading state
        submitBtn.disabled = true;
        submitBtn.innerHTML = '<span class="icon icon-loading"></span> Publishing...';

        try {
            const eventId = await publishMarketplaceApp(
                filePath,
                appId,
                name,
                description,
                version,
                categories,
                changelog,
                developer,
                sourceUrl,
                permissionsStr
            );

            console.log('Published app with event ID:', eventId);

            // Success - close dialog
            overlay.classList.remove('active');
            setTimeout(() => overlay.style.display = 'none', 300);

            // Show success message
            popupConfirm('Published!', `${name} has been published to the Nexus.`, true, '', 'vector-check.svg');
        } catch (error) {
            console.error('Failed to publish:', error);
            submitBtn.disabled = false;
            submitBtn.innerHTML = '<span class="icon icon-loading"></span> Publish';
            alert('Failed to publish: ' + error.toString());
        }
    };
}

// ============================================================================
// Install Progress Listener
// ============================================================================

/**
 * Set up listener for marketplace install progress events
 * Updates progress spinners in real-time during downloads
 */
function setupMarketplaceProgressListener() {
    window.__TAURI__.event.listen('marketplace_install_progress', (event) => {
        const { app_id, progress } = event.payload;

        // Find all progress spinners for this app
        const spinners = document.querySelectorAll(`.marketplace-progress-spinner[data-app-id="${CSS.escape(app_id)}"]`);
        const displayProgress = Math.max(5, progress);

        for (const spinner of spinners) {
            spinner.style.setProperty('--progress', `${displayProgress}%`);
        }

        // Also update any Mini Apps panel downloading spinners
        const miniappSpinners = document.querySelectorAll(`.miniapp-downloading-spinner[data-app-id="${CSS.escape(app_id)}"]`);
        for (const spinner of miniappSpinners) {
            spinner.style.setProperty('--progress', `${displayProgress}%`);
        }
    });
}

// Initialize the progress listener
setupMarketplaceProgressListener();

// Listen for backend-pushed marketplace data (emitted on preload + network refresh)
window.__TAURI__.event.listen('marketplace_apps_updated', (event) => {
    const hadData = marketplaceApps.length > 0;
    syncMarketplaceApps(event.payload);
    // Refresh mini apps grid to pick up update badges if this is the first load
    if (!hadData && marketplaceApps.length > 0) {
        loadMiniAppsHistory();
    }
});
