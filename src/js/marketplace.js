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
 * @property {string|null} icon_cached - Local copy of the icon, once fetched
 * @property {string|null} requested_permissions - Comma-separated permission ids it asks for
 */

// Note: 'invoke' is already declared in main.js, so we use it directly

// Marketplace state
let marketplaceApps = [];
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
            const apps = await invoke('marketplace_fetch_apps', { trustedOnly });
            syncMarketplaceApps(apps);
            return apps;
        } catch (error) {
            console.error('Failed to fetch marketplace apps:', error);
            throw error;
        } finally {
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
    const items = app.requested_permissions.split(',').map(s => s.trim()).filter(Boolean).map(id => {
        const info = availablePermissions.find(p => p.id === id);
        return info ? { id, label: info.label, description: info.description } : null;
    }).filter(Boolean);

    // Return a Promise that resolves when the user makes a choice
    return new Promise((resolve) => {
        const closePrompt = () => {
            popBack('permission-prompt');
            VectorSvelte.closePermissionPrompt();
            // The element outlives the fade-out.
            setTimeout(() => VectorSvelte.unmountPermissionPrompt(), 300);
        };
        VectorSvelte.openPermissionPrompt(app.name, items, {
            // Cancel - don't save anything, just close and cancel the app opening
            deny: () => { closePrompt(); resolve(false); },
            allow: async (permissions) => {
                try {
                    await invoke('miniapp_set_permissions', { fileHash: app.blossom_hash, permissions });
                    // Refresh the App Details permissions list if it's visible
                    loadAppPermissions(app);
                } catch (error) {
                    console.error('Failed to save permissions:', error);
                }
                closePrompt();
                resolve(true);
            },
        });
        setTimeout(() => VectorSvelte.activatePermissionPrompt(), 10);
        pushBack('permission-prompt', () => { closePrompt(); resolve(false); });
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

// The Nexus is a Svelte island over lib/marketplace.svelte.js.
// `marketplaceApps` stays the plain array other modules read; the store mirrors it.
const iconWaiters = new Map();
function syncMarketplaceApps(apps) {
    // Tags are relay data: one spelling of each, so every page and count agrees.
    for (const app of apps) app.categories = [...new Set((app.categories || []).map(c => String(c).trim().toLowerCase()).filter(Boolean))];
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

/**
 * MarketplaceHelpers: the Nexus, its pages, one app in full and the Play with Friends picker.
 * @typedef {Object} MarketplaceHelpers
 * @property {(app: object) => string|null} iconKey   null for a listing without an icon
 * @property {(app: object) => string} actionText   "Play" for a game, "Open" for an app
 * @property {(bytes: number) => string} fileSize
 * @property {(ts: number) => string} publishDate   unix seconds
 * @property {(ms: number) => string} playedAgo   "2d ago", "just now"
 * @property {(url: string) => string} sourceUrl
 * @property {(view: string, category?: string) => void} show   a page: discover, library, updates, all, category
 * @property {(app: object) => void} showDetails
 * @property {() => void} closeDetails
 * @property {() => void} retry
 * @property {(app: object) => void} cardAction   update, play or install, whichever comes next
 * @property {(app: object) => void} uninstall
 * @property {() => void} updateAll
 * @property {(app: object) => void} playWith   pick a chat to send it to
 * @property {() => void} closePicker
 * @property {() => object[]} pickChats   [{ id, name, sub, avatar, group }], newest first
 * @property {(app: object, chatId: string) => void} sendTo
 * @property {(app: object) => void} loadPerms   the open app's permission toggles
 * @property {(app: object, permissionId: string, on: boolean) => Promise<boolean>} setPermission   false when it didn't save
 * @property {(app: object) => void} resetPermissions
 * @property {(npub: string) => { name: string, avatar: string|null, veiled?: boolean }|null} publisher
 * @property {(npub: string) => void} openPublisher
 * @property {(app: object) => boolean} isMine   published by this account, so it can edit the listing
 * @property {(on: boolean) => void} setFitOnly   only what fits this device, remembered on it
 * @property {(app: object) => void} editListing
 * @property {(url: string) => void} openUrl
 * @property {() => void} back
 */
function registerMarketplaceHandlers() {
    const h = {
        iconKey: marketplaceIconKey,
        actionText: getAppActionText,
        fileSize: (n) => formatBytes(n, 1),
        publishDate: formatPublishDate,
        playedAgo: formatPlayedAgo,
        sourceUrl: formatSourceUrl,
        show: (view, category) => { popBack('app-details'); VectorSvelte.mktShow(view, category); },
        showDetails: showAppDetails,
        closeDetails: closeAppDetailsPanel,
        retry: () => initMarketplace(),
        cardAction: handleAppInstallOrPlay,
        uninstall: uninstallFromDetails,
        updateAll: updateAllApps,
        playWith: openPlayWith,
        closePicker: () => { popBack('nexus-picker'); VectorSvelte.mktClosePicker(); },
        pickChats: nexusChatChoices,
        sendTo: sendAppToChat,
        loadPerms: loadAppPermissions,
        setPermission: setAppPermission,
        resetPermissions: resetAppPermissions,
        publisher: publisherProfile,
        openPublisher: openPublisherProfile,
        isMine: (app) => app.publisher === strPubkey,
        setFitOnly: setNexusFitOnly,
        editListing: (app) => showEditListingDialog(current(app)),
        openUrl: (url) => confirmAndOpenUrl(url),   // a listing's source URL is relay data
        back: () => { hideMarketplacePanel(); },
    };
    VectorSvelte.setMarketplaceHandlers(h);
}
// Registered once every script is in: the bag calls helpers from files that load later.
document.addEventListener('DOMContentLoaded', registerMarketplaceHandlers, { once: true });

/** How long ago something was played, said briefly: "just now", "5m ago", "2d ago", "3 Oct". */
function formatPlayedAgo(ms) {
    const s = Math.max(0, (Date.now() - ms) / 1000);
    if (s < 60) return 'just now';
    if (s < 3600) return `${Math.floor(s / 60)}m ago`;
    if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
    if (s < 86400 * 7) return `${Math.floor(s / 86400)}d ago`;
    return new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
}

/** Run one action on an app with its button showing it, and a 2s "Failed" if it throws. */
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

function current(app) { return marketplaceApps.find(a => a.id === app.id) || app; }

/** Mirror a patch onto the plain array other modules read as well as the store. */
function patchMarketplaceApp(id, fields) {
    marketplaceApps = marketplaceApps.map(a => a.id === id ? { ...a, ...fields } : a);
    VectorSvelte.mktPatchApp(id, fields);
}

async function installApp(app) {
    return runAppAction(app, 'installing', 'Installing', 'Failed', async () => {
        const path = await installMarketplaceApp(app.id);
        patchMarketplaceApp(app.id, { installed: true, installed_version: app.version, local_path: path || null, update_available: false });
    });
}

async function updateApp(app) {
    return runAppAction(app, 'updating', 'Updating', 'Failed', async () => {
        const path = await updateMarketplaceApp(app.id);
        patchMarketplaceApp(app.id, { installed_version: app.version, update_available: false, ...(path ? { local_path: path } : {}) });
    });
}

async function launchApp(app) {
    const done = await runAppAction(app, 'launching', getAppLaunchingText(app), getAppActionText(app), () => openMarketplaceApp(app.id, app));
    if (done) loadNexusPlays();
    return done;
}

/** The one-tap action: update if one is waiting, play if installed, otherwise install. */
async function handleAppInstallOrPlay(app) {
    app = current(app);
    if (app.update_available) await updateApp(app);
    else if (app.installed || app.local_path) await launchApp(app);
    else await installApp(app);
}

/** Every waiting update, one at a time: they share the connection and the disk. */
async function updateAllApps() {
    for (const app of marketplaceApps.filter(a => a.update_available)) {
        if (!VectorSvelte.mktActions().get(app.id)) await updateApp(app);
    }
}

function closeAppDetailsPanel() {
    popBack('app-details');
    if (VectorSvelte.mktState().detailsId) VectorSvelte.mktCloseDetails();
}

/** Open one app in full (its page loads its own permissions), fetching its publisher's
 *  profile if this is the first we've seen of them. */
function showAppDetails(app) {
    if (!VectorSvelte.mktState().detailsId) pushBack('app-details', closeAppDetailsPanel);
    VectorSvelte.ensureSignals({ profiles: [app.publisher] });
    VectorSvelte.mktOpenDetails(app.id);
    if (app.publisher && !getProfile(app.publisher) && !strangerProfileRequested.has(app.publisher)) {
        strangerProfileRequested.add(app.publisher);
        invoke('load_profile', { npub: app.publisher }).catch(() => {});
    }
}

/** Permission toggles keyed by the bundle hash: content-based, so a re-upload asks again. */
async function loadAppPermissions(app) {
    const fileHash = app.blossom_hash;
    const shown = () => VectorSvelte.mktState().detailsId === app.id;
    if (!fileHash) { if (shown()) VectorSvelte.mktSetPerms({ error: 'No permissions available' }); return; }
    try {
        const available = await invoke('miniapp_get_available_permissions');
        const grantedStr = await invoke('miniapp_get_granted_permissions', { fileHash });
        const granted = new Set(grantedStr ? grantedStr.split(',').filter(s => s) : []);
        const items = app.requested_permissions.split(',').map(s => s.trim()).filter(Boolean).map(id => {
            const info = available.find(p => p.id === id);
            return info ? { id, label: info.label, description: info.description, granted: granted.has(id) } : null;
        }).filter(Boolean);
        if (shown()) VectorSvelte.mktSetPerms({ items });
    } catch (error) {
        console.error('Failed to load app permissions:', error);
        if (shown()) VectorSvelte.mktSetPerms({ error: 'Failed to load permissions' });
    }
}

async function setAppPermission(app, permission, granted) {
    try {
        await invoke('miniapp_set_permission', { fileHash: app.blossom_hash, permission, granted });
        const perms = VectorSvelte.mktPerms();
        if (perms?.items && VectorSvelte.mktState().detailsId === app.id) {
            VectorSvelte.mktSetPerms({ items: perms.items.map(p => p.id === permission ? { ...p, granted } : p) });
        }
        return true;
    } catch (error) {
        console.error('Failed to update permission:', error);
        loadAppPermissions(app);
        return false;
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

async function uninstallFromDetails(app) {
    const confirmed = await popupConfirm(
        `Uninstall ${app.name}?`,
        'This will delete the app and remove it from your history.',
        false,
        '',
        'vector_warning.svg'
    );
    if (!confirmed) return;
    if (await runAppAction(app, 'uninstalling', 'Uninstalling', 'Failed', () => uninstallMarketplaceApp(app.id, app.name))) {
        patchMarketplaceApp(app.id, { installed: false, local_path: null, installed_version: null, update_available: false });
        if (VectorSvelte.mktState().detailsId === app.id) VectorSvelte.mktSetPerms(null);
        loadNexusPlays();
    }
}

/** What you've played from the Nexus, newest first, one entry per app. */
async function loadNexusPlays() {
    try {
        const history = await invoke('miniapp_get_history', { limit: null });
        const seen = new Map();
        for (const e of history || []) {
            if (!e.marketplace_id) continue;
            const at = e.last_opened_at * 1000;
            const had = seen.get(e.marketplace_id);
            if (!had || had.at < at) seen.set(e.marketplace_id, { id: e.marketplace_id, at, count: e.open_count + (had?.count || 0) });
            else had.count += e.open_count;
        }
        VectorSvelte.mktSetPlayed([...seen.values()].sort((a, b) => b.at - a.at));
    } catch (error) {
        console.error('Failed to load Mini App history:', error);
    }
}

/** Play with Friends: install it if needed, then choose who to play with. */
async function openPlayWith(app) {
    app = current(app);
    if (!(app.installed || app.local_path) && !(await installApp(app))) return;
    // The download outlived the visit: no picker over wherever they went.
    const st = VectorSvelte.mktState();
    if (!st.panelOpen || st.panelClosing) return;
    pushBack('nexus-picker', () => VectorSvelte.mktClosePicker());
    VectorSvelte.mktOpenPicker(app.id);
}

/** The chats a game can go to, newest first: DMs you're talking in, and the channels you can post in. */
function nexusChatChoices() {
    const out = [];
    const icons = new Map();
    for (const chat of arrChats) {
        const communityId = communityIdOfChat(chat);
        if (communityId && chat.metadata?.avatar_cached && !icons.has(communityId)) icons.set(communityId, convertFileSrc(chat.metadata.avatar_cached));
    }
    for (const chat of arrChats) {
        if (chat.id === strPubkey || chat._joining) continue;
        const communityId = communityIdOfChat(chat);
        if (communityId) {
            const channel = communityChannelsCache.get(communityId)?.find(c => c.id === chat.id);
            if (channel?.view === 'voice' || !channelAllowsSend(chat) || channelAwaitsWriterKey(chat)) continue;
            const cf = chat.metadata?.custom_fields || {};
            out.push({ id: chat.id, name: cf.name || 'Community', sub: `#${channel?.name || cf.channel_name || 'general'}`, avatar: icons.get(communityId) || null, group: true });
        } else if (!chatIsGroup(chat) && chat.messages.length && !chatIsRequest(chat) && !getProfile(chat.id)?.is_blocked) {
            const profile = getProfile(chat.id);
            out.push({ id: chat.id, name: getName(profile || chat.id), sub: 'Direct message', avatar: getProfileAvatarSrc(profile), group: false });
        }
    }
    return out;
}

/** Send an installed app to a chat as an invite, open that chat, and start playing. */
async function sendAppToChat(app, chatId) {
    popBack('nexus-picker');
    VectorSvelte.mktClosePicker();
    app = current(app);
    const path = app.local_path || await checkAppInstalled(app.id).catch(() => null);
    if (!path) return;
    await openChat(chatId);
    await inviteMiniAppToChat({ src_url: path, name: app.name, marketplace_id: app.id, categories: (app.categories || []).join(',') }, chatId);
    loadNexusPlays();
}

/** The publisher as the profile list knows them, or null for a bare npub. */
function publisherProfile(npub) {
    if (streamHidden(npub)) return { name: STREAM_DOTS, avatar: getProfileAvatarSrc(getProfile(npub) || { id: npub }), veiled: true };
    try {
        const profile = getProfile(npub);
        if (!profile) return null;
        return { name: getName(profile), avatar: getProfileAvatarSrc(profile) || null };
    } catch (error) {
        console.error('Failed to load publisher profile:', error);
        return null;
    }
}

async function openPublisherProfile(npub) {
    if (!npub) return;
    await hideMarketplacePanel();
    // Reopened while it closed: the Nexus stays, the profile doesn't open over it.
    if (VectorSvelte.mktState().panelOpen) return;
    openProfile((getProfile(npub)) || { id: npub });
}

/** Open the Nexus: cached apps at once, then a fresh fetch with install status. */
async function initMarketplace() {
    if (_marketplaceInitInProgress) return;
    _marketplaceInitInProgress = true;
    detectNexusDevice();
    loadNexusPlays();
    // Loading lasts until the network answers; the cache, when there is one, shows meanwhile.
    VectorSvelte.mktSetLoading(true);
    try {
        await getCachedMarketplaceApps();
        await fetchMarketplaceApps(true);
        await syncInstallStatus();
        await getCachedMarketplaceApps();
        VectorSvelte.mktSetLoading(false);
    } catch (error) {
        console.error('Failed to initialize marketplace:', error);
        VectorSvelte.mktSetError(error.toString());
        if (marketplaceApps.length) showToast("Couldn't refresh the Nexus");
    } finally {
        _marketplaceInitInProgress = false;
    }
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
        strPubkey: strPubkey,
        match: trustedNpub && strPubkey === trustedNpub
    });
    return trustedNpub && strPubkey === trustedNpub;
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
    const st = VectorSvelte.publishState();
    // Generate a default app ID from the name
    const defaultAppId = (miniAppInfo?.name || 'app')
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-|-$/g, '');

    const close = () => {
        VectorSvelte.closePublishDialog();
        // The element outlives the fade-out.
        setTimeout(() => VectorSvelte.unmountPublishDialog(), 300);
    };

    // An App ID we already published prefills the form from that listing, so a new
    // version is the version and changelog fields, not the whole form again.
    const prefillFromExisting = () => {
        const appId = st.form.id.trim().toLowerCase();
        if (!appId) return;
        const existingApp = marketplaceApps.find(app =>
            app.id === appId && app.publisher === strPubkey);
        if (!existingApp) return;
        const f = st.form;
        if (existingApp.description) f.description = existingApp.description;
        if (existingApp.developer) f.developer = existingApp.developer;
        if (existingApp.source_url) f.source = existingApp.source_url;
        if (existingApp.categories?.length) {
            const tags = VectorSvelte.splitControlTags(existingApp.categories.filter(c => c !== 'game' && c !== 'app'));
            Object.assign(f, { categories: tags.rest.join(', '), touch: tags.touch, pad: tags.pad, keys: tags.keys });
            f.isGame = existingApp.categories.includes('game');
        }
        if (existingApp.version) f.version = existingApp.version;
        if (existingApp.changelog) f.changelog = existingApp.changelog;
        if (existingApp.requested_permissions) {
            const requested = existingApp.requested_permissions.split(',').map(p => p.trim());
            for (const perm of st.perms) perm.checked = requested.includes(perm.id);
        }
        VectorSvelte.setPublishHint('✓ Pre-filled from your existing app. Update the version and changelog!', true);
    };
    let prefillTimeout = null;

    return new Promise((resolve) => {
        VectorSvelte.openPublishDialog({
            id: defaultAppId, name: miniAppInfo?.name || '', description: miniAppInfo?.description || '',
            version: miniAppInfo?.version || '1.0.0', isGame: true, categories: '', developer: '',
            source: miniAppInfo?.source_code_url || '', changelog: '', touch: false, pad: false, keys: false,
        }, miniAppIconSrc(miniAppInfo) || '', {
            cancel: () => { close(); resolve(false); },
            idInput: () => {
                VectorSvelte.setPublishHint('Unique identifier (lowercase, no spaces)', false);
                clearTimeout(prefillTimeout);
                prefillTimeout = setTimeout(prefillFromExisting, 500);
            },
            idBlur: prefillFromExisting,
            submit: async () => {
                const f = st.form;
                const appId = f.id.trim();
                const name = f.name.trim();
                if (!appId) { alert('App ID is required'); return; }
                if (!name) { alert('Name is required'); return; }
                const selected = st.perms.filter(p => p.checked).map(p => p.id);
                const permissionsStr = selected.length > 0 ? selected.join(',') : null;
                // Parse categories and add the game or app tag at the beginning.
                const categories = [...new Set([f.isGame ? 'game' : 'app',
                    ...f.categories.toLowerCase().split(',').map(c => c.trim()).filter(Boolean),
                    ...VectorSvelte.controlTags(f)])];
                VectorSvelte.setPublishBusy(true);
                try {
                    const eventId = await publishMarketplaceApp(
                        filePath, appId, name, f.description.trim(), f.version.trim(), categories,
                        f.changelog.trim() || null, f.developer.trim() || null, f.source.trim() || null, permissionsStr,
                    );
                    console.log('Published app with event ID:', eventId);
                    close();
                    popupConfirm('Published!', `${name} has been published to the Nexus.`, true, '', 'vector-check.svg');
                    resolve(true);
                } catch (error) {
                    console.error('Failed to publish:', error);
                    VectorSvelte.setPublishBusy(false);
                    alert('Failed to publish: ' + error.toString());
                }
            },
        });
        setTimeout(() => VectorSvelte.activatePublishDialog(), 10);

        // Load available permissions, then check for pre-fill
        invoke('miniapp_get_available_permissions').then((available) => {
            VectorSvelte.setPublishPerms(available);
        }).catch((error) => {
            console.error('Failed to load permissions:', error);
            VectorSvelte.setPublishPermsError('Failed to load permissions');
        }).then(prefillFromExisting);
    });
}

/**
 * Edit a listing you published, over the package it already has: everything but the App
 * ID and version, which name that package. Resolves true once it's live.
 * @param {MarketplaceApp} app
 */
function showEditListingDialog(app) {
    const st = VectorSvelte.publishState();
    const close = () => {
        VectorSvelte.closePublishDialog();
        setTimeout(() => VectorSvelte.unmountPublishDialog(), 300);
    };
    const requested = (app.requested_permissions || '').split(',').map(p => p.trim()).filter(Boolean);
    const tags = VectorSvelte.splitControlTags((app.categories || []).filter(c => c !== 'game' && c !== 'app'));
    return new Promise((resolve) => {
        VectorSvelte.openPublishDialog({
            id: app.id, name: app.name, description: app.description || '', version: app.version,
            isGame: isGameApp(app), categories: tags.rest.join(', '), touch: tags.touch, pad: tags.pad, keys: tags.keys,
            developer: app.developer || '', source: app.source_url || '', changelog: app.changelog || '',
        }, VectorSvelte.mktIcons().get(marketplaceIconKey(app)) || '', {
            cancel: () => { close(); resolve(false); },
            submit: async () => {
                const f = st.form;
                const name = f.name.trim();
                if (!name) { showToast('A listing needs a name'); return; }
                const categories = [f.isGame ? 'game' : 'app',
                    ...f.categories.toLowerCase().split(',').map(c => c.trim()).filter(Boolean),
                    ...VectorSvelte.controlTags(f)];
                const permissions = st.perms.filter(p => p.checked).map(p => p.id).join(',') || null;
                VectorSvelte.setPublishBusy(true);
                try {
                    await invoke('marketplace_edit_listing', {
                        appId: app.id, name, description: f.description.trim(), categories: [...new Set(categories)],
                        changelog: f.changelog.trim() || null, developer: f.developer.trim() || null,
                        sourceUrl: f.source.trim() || null, permissions,
                    });
                    close();
                    showToast('Listing Updated');
                    resolve(true);
                } catch (error) {
                    console.error('Failed to edit listing:', error);
                    VectorSvelte.setPublishBusy(false);
                    showToast(String(error));
                }
            },
        }, 'edit');
        setTimeout(() => VectorSvelte.activatePublishDialog(), 10);
        invoke('miniapp_get_available_permissions').then((available) => {
            VectorSvelte.setPublishPerms(available);
            for (const perm of st.perms) perm.checked = requested.includes(perm.id);
        }).catch(() => VectorSvelte.setPublishPermsError('Failed to load permissions'));
    });
}

// ============================================================================
// This device's controls
// ============================================================================

const NEXUS_FIT_KEY = 'nexus_fit_only';

/** What this device plays with, as the Nexus fits apps to it: touch, a keyboard and mouse
 *  (a fine pointer stands in for both), and any controller the browser has been shown. */
function detectNexusDevice() {
    const pads = [...(navigator.getGamepads?.() || [])].filter(Boolean);
    const keys = matchMedia('(any-pointer: fine)').matches;
    VectorSvelte.mktSetDevice({
        touch: matchMedia('(any-pointer: coarse)').matches || navigator.maxTouchPoints > 0,
        keys,
        pad: pads.length > 0,
        phone: !keys && matchMedia('(pointer: coarse)').matches,
    });
}

function setNexusFitOnly(on) {
    VectorSvelte.mktSetFitOnly(on);
    try { localStorage.setItem(NEXUS_FIT_KEY, String(on)); } catch (_) { /* this session only */ }
}

document.addEventListener('DOMContentLoaded', () => {
    let fitOnly = true;
    try { fitOnly = localStorage.getItem(NEXUS_FIT_KEY) !== 'false'; } catch (_) { /* the default stands */ }
    VectorSvelte.mktSetFitOnly(fitOnly);
    detectNexusDevice();
}, { once: true });
// A controller shows itself on its first button press; a pointer can come and go (a tablet's keyboard).
window.addEventListener('gamepadconnected', detectNexusDevice);
window.addEventListener('gamepaddisconnected', detectNexusDevice);
for (const q of ['(any-pointer: coarse)', '(any-pointer: fine)']) matchMedia(q).addEventListener?.('change', detectNexusDevice);

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
        VectorSvelte.mktSetProgress(app_id, progress);
        const displayProgress = Math.max(5, progress);

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
    // Update badges for a grid already open; a closed one reads them when it opens.
    if (!hadData && marketplaceApps.length > 0 && miniAppsGridOpen()) {
        loadMiniAppsHistory();
    }
});
