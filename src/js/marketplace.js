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
 * @property {string} version - Version string
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

/**
 * Fetch apps from the marketplace
 * @param {boolean} trustedOnly - Only fetch from trusted publishers
 * @returns {Promise<MarketplaceApp[]>}
 */
async function fetchMarketplaceApps(trustedOnly = true) {
    try {
        isMarketplaceLoading = true;
        marketplaceError = null;
        const apps = await invoke('marketplace_fetch_apps', { trustedOnly });
        marketplaceApps = apps;
        return apps;
    } catch (error) {
        console.error('Failed to fetch marketplace apps:', error);
        marketplaceError = error.toString();
        throw error;
    } finally {
        isMarketplaceLoading = false;
    }
}

/**
 * Get cached marketplace apps (without network fetch)
 * @returns {Promise<MarketplaceApp[]>}
 */
async function getCachedMarketplaceApps() {
    try {
        const apps = await invoke('marketplace_get_cached_apps');
        marketplaceApps = apps;
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
 * @param {string} appId - The app ID
 * @returns {Promise<void>}
 */
async function openMarketplaceApp(appId) {
    try {
        await invoke('marketplace_open_app', { appId });
    } catch (error) {
        console.error('Failed to open marketplace app:', error);
        throw error;
    }
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

/**
 * Dynamically truncate category tags based on available container width
 * @param {HTMLElement} card - The app card element
 * @param {string[]} allCategories - All category names for the app
 */
function truncateCategoryTags(card, allCategories) {
    const container = card.querySelector('.marketplace-app-categories');
    if (!container || allCategories.length === 0) return;

    const tags = Array.from(container.querySelectorAll('.marketplace-app-category'));
    if (tags.length === 0) return;

    // Get container width
    const containerWidth = container.offsetWidth;
    if (containerWidth === 0) return; // Not yet rendered

    // Calculate how many tags fit
    let usedWidth = 0;
    let visibleCount = 0;
    const gap = 6; // Gap between tags in pixels
    const overflowTagWidth = 35; // Approximate width for "+N" tag

    for (let i = 0; i < tags.length; i++) {
        const tagWidth = tags[i].offsetWidth;
        const remainingTags = tags.length - i - 1;
        const needsOverflow = remainingTags > 0;
        const reservedWidth = needsOverflow ? overflowTagWidth + gap : 0;

        if (usedWidth + tagWidth + (i > 0 ? gap : 0) + reservedWidth <= containerWidth) {
            usedWidth += tagWidth + (i > 0 ? gap : 0);
            visibleCount++;
        } else {
            break;
        }
    }

    // Ensure at least 1 tag is visible if there are any
    if (visibleCount === 0 && tags.length > 0) {
        visibleCount = 1;
    }

    // Hide overflow tags and add overflow indicator
    const hiddenCount = tags.length - visibleCount;
    if (hiddenCount > 0) {
        const hiddenCategories = allCategories.slice(visibleCount);
        
        // Hide tags that don't fit
        for (let i = visibleCount; i < tags.length; i++) {
            tags[i].style.display = 'none';
        }

        // Add overflow indicator with global tooltip
        const overflowSpan = document.createElement('span');
        overflowSpan.className = 'marketplace-app-category-overflow';
        overflowSpan.textContent = `+${hiddenCount}`;
        overflowSpan.dataset.hiddenTags = hiddenCategories.join(', ');
        
        // Use global tooltip system on hover
        overflowSpan.addEventListener('mouseenter', (e) => {
            e.stopPropagation();
            showGlobalTooltip(hiddenCategories.join(', '), overflowSpan);
        });
        overflowSpan.addEventListener('mouseleave', (e) => {
            e.stopPropagation();
            hideGlobalTooltip();
        });
        
        container.appendChild(overflowSpan);
    }
}

/**
 * Truncate all category tags in the marketplace content
 */
function truncateAllCategoryTags() {
    const cards = document.querySelectorAll('.marketplace-app-card');
    cards.forEach(card => {
        const appId = card.dataset.appId;
        const app = marketplaceApps.find(a => a.id === appId);
        if (app && app.categories) {
            // Remove existing overflow indicators first
            const existingOverflow = card.querySelector('.marketplace-app-category-overflow');
            if (existingOverflow) {
                existingOverflow.remove();
            }
            // Show all tags first
            const tags = card.querySelectorAll('.marketplace-app-category');
            tags.forEach(tag => tag.style.display = '');
            // Then truncate
            truncateCategoryTags(card, app.categories);
        }
    });
}

function createMarketplaceAppCard(app) {
    const card = document.createElement('div');
    card.className = 'marketplace-app-card';
    card.dataset.appId = app.id;

    // Determine install button state
    let installBtnClass = 'marketplace-install-btn';
    let installBtnText = 'Install';
    let installBtnDisabled = false;

    if (app.installed) {
        installBtnClass += ' installed';
        installBtnText = getAppActionText(app);
    }

    // Create icon element
    let iconHtml = '<span class="icon icon-play marketplace-app-icon-placeholder"></span>';
    if (app.icon_url) {
        iconHtml = `<img src="${escapeHtml(app.icon_url)}" alt="${escapeHtml(app.name)}" class="marketplace-app-icon" onerror="this.outerHTML='<span class=\\'icon icon-play marketplace-app-icon-placeholder\\'></span>'">`;
    }

    // Categories/tags - render all, will be dynamically truncated after mount
    const categoriesHtml = app.categories.length > 0
        ? `<div class="marketplace-app-categories">${app.categories.map(c => `<span class="marketplace-app-category" data-category="${escapeHtml(c)}">${escapeHtml(c)}</span>`).join('')}</div>`
        : '';

    card.innerHTML = `
        <div class="marketplace-app-icon-container">
            ${iconHtml}
        </div>
        <div class="marketplace-app-info">
            <div class="marketplace-app-header">
                <span class="marketplace-app-name">${escapeHtml(app.name)}</span>
                <span class="marketplace-app-version">v${escapeHtml(app.version)}</span>
            </div>
            <div class="marketplace-app-description">${escapeHtml(app.description)}</div>
            ${categoriesHtml}
            <div class="marketplace-app-meta">
                <span class="marketplace-app-size">${formatFileSize(app.size)}</span>
                <span class="marketplace-app-date">${formatPublishDate(app.published_at)}</span>
            </div>
        </div>
        <button class="${installBtnClass}" ${installBtnDisabled ? 'disabled' : ''}>
            ${installBtnText}
        </button>
    `;

    // Add click handler for install/play button
    const installBtn = card.querySelector('.marketplace-install-btn');
    installBtn.addEventListener('click', async (e) => {
        e.stopPropagation();
        await handleAppInstallOrPlay(app, installBtn);
    });

    // Add click handlers for category tags
    const categoryTags = card.querySelectorAll('.marketplace-app-category');
    categoryTags.forEach(tag => {
        tag.addEventListener('click', (e) => {
            e.stopPropagation();
            const category = tag.dataset.category;
            if (category) {
                addMarketplaceFilter(category);
            }
        });
    });

    // Add click handler for the card (show details)
    card.addEventListener('click', () => {
        showAppDetails(app);
    });

    return card;
}

/**
 * Handle install or play button click
 * @param {MarketplaceApp} app - The app
 * @param {HTMLElement} btn - The button element
 */
async function handleAppInstallOrPlay(app, btn) {
    const actionText = getAppActionText(app);
    const launchingText = getAppLaunchingText(app);
    
    if (app.installed || app.local_path) {
        // Already installed, just play/open
        btn.textContent = launchingText;
        btn.disabled = true;
        try {
            await openMarketplaceApp(app.id);
            // Reset button state after successful launch
            btn.textContent = actionText;
            btn.disabled = false;
        } catch (error) {
            console.error('Failed to open app:', error);
            btn.textContent = actionText;
            btn.disabled = false;
        }
    } else {
        // Need to install first
        btn.textContent = 'Installing...';
        btn.disabled = true;
        btn.classList.add('installing');

        try {
            await installMarketplaceApp(app.id);
            
            // Update the app state in the global marketplaceApps array
            const globalApp = marketplaceApps.find(a => a.id === app.id);
            if (globalApp) {
                globalApp.installed = true;
            }
            
            // Update the passed app object
            app.installed = true;
            
            btn.textContent = actionText;
            btn.classList.remove('installing');
            btn.classList.add('installed');
            btn.disabled = false;

            // Optionally auto-launch after install
            // await openMarketplaceApp(app.id);
        } catch (error) {
            console.error('Failed to install app:', error);
            btn.textContent = 'Failed';
            btn.classList.remove('installing');
            btn.classList.add('failed');
            
            // Reset after a delay
            setTimeout(() => {
                btn.textContent = 'Install';
                btn.classList.remove('failed');
                btn.disabled = false;
            }, 2000);
        }
    }
}

/**
 * Close the app details panel with fade out animation
 * @returns {Promise<void>} Resolves when the animation completes
 */
function closeAppDetailsPanel() {
    return new Promise((resolve) => {
        const panel = document.getElementById('app-details-panel');
        if (panel && panel.style.display !== 'none') {
            panel.classList.add('closing');
            panel.addEventListener('animationend', function handler() {
                panel.removeEventListener('animationend', handler);
                panel.style.display = 'none';
                panel.classList.remove('closing');
                resolve();
            });
        } else {
            resolve();
        }
    });
}

/**
 * Show app details in a full-screen panel
 * @param {MarketplaceApp} app - The app
 */
async function showAppDetails(app) {
    const panel = document.getElementById('app-details-panel');
    const content = document.getElementById('app-details-content');
    
    if (!panel || !content) {
        console.error('App details panel not found');
        return;
    }

    // Create icon HTML
    let iconHtml = '<span class="icon icon-play app-details-icon-placeholder"></span>';
    if (app.icon_url) {
        iconHtml = `<img src="${escapeHtml(app.icon_url)}" alt="${escapeHtml(app.name)}" class="app-details-icon" onerror="this.outerHTML='<span class=\\'icon icon-play app-details-icon-placeholder\\'></span>'">`;
    }

    // Create categories HTML - make them clickable for filtering
    const categoriesHtml = app.categories.length > 0
        ? `<div class="app-details-categories">${app.categories.map(c => `<span class="app-details-category" data-category="${escapeHtml(c)}">${escapeHtml(c)}</span>`).join('')}</div>`
        : '';

    // Determine action buttons based on install status
    const actionText = getAppActionText(app);
    let actionButtonsHtml = '';
    if (app.installed || app.local_path) {
        // Show both Play/Open and Uninstall buttons for installed apps
        actionButtonsHtml = `
            <div class="app-details-actions">
                <button class="app-details-action-btn app-details-play-btn" id="app-details-play-btn">
                    ${actionText}
                </button>
                <button class="app-details-action-btn app-details-uninstall-btn" id="app-details-uninstall-btn">
                    Uninstall
                </button>
            </div>
        `;
    } else {
        // Show only Install button for uninstalled apps
        actionButtonsHtml = `
            <button class="app-details-action-btn" id="app-details-action-btn">
                Install
            </button>
        `;
    }

    // Format publisher npub for display (truncated)
    const publisherNpubShort = app.publisher.length > 20
        ? app.publisher.substring(0, 12) + '...' + app.publisher.substring(app.publisher.length - 8)
        : app.publisher;

    // Build the content HTML
    content.innerHTML = `
        <!-- Hero Section -->
        <div class="app-details-hero">
            <div class="app-details-icon-container">
                ${iconHtml}
            </div>
            <h1 class="app-details-name">${escapeHtml(app.name)}</h1>
            <span class="app-details-version">Version ${escapeHtml(app.version)}</span>
            ${categoriesHtml}
            ${actionButtonsHtml}
        </div>

        <!-- Description Section -->
        ${app.description ? `
        <div class="app-details-section">
            <h3 class="app-details-section-title">Description</h3>
            <p class="app-details-description">${escapeHtml(app.description)}</p>
        </div>
        ` : ''}

        <!-- Changelog Section -->
        ${app.changelog ? `
        <div class="app-details-section">
            <h3 class="app-details-section-title">What's New</h3>
            <p class="app-details-changelog">${escapeHtml(app.changelog)}</p>
        </div>
        ` : ''}

        <!-- Developer Section (if available) -->
        ${app.developer ? `
        <div class="app-details-section">
            <h3 class="app-details-section-title">Developer</h3>
            <div class="app-details-developer">
                <span class="app-details-developer-name">${escapeHtml(app.developer)}</span>
            </div>
        </div>
        ` : ''}

        <!-- Publisher Section -->
        <div class="app-details-section">
            <h3 class="app-details-section-title">Publisher</h3>
            <div class="app-details-publisher" id="app-details-publisher" data-npub="${escapeHtml(app.publisher)}">
                <div class="app-details-publisher-avatar">
                    <span class="icon icon-user-circle"></span>
                </div>
                <div class="app-details-publisher-info">
                    <p class="app-details-publisher-name" id="app-details-publisher-name">Loading...</p>
                    <span class="app-details-publisher-npub">${escapeHtml(publisherNpubShort)}</span>
                </div>
                <div class="app-details-publisher-arrow-container">
                    <span class="icon icon-chevron-double-left app-details-publisher-arrow"></span>
                </div>
            </div>
        </div>

        <!-- Meta Info Section -->
        <div class="app-details-section">
            <h3 class="app-details-section-title">Information</h3>
            <div class="app-details-meta">
                <div class="app-details-meta-row">
                    <span class="app-details-meta-label">Size</span>
                    <span class="app-details-meta-value">${formatFileSize(app.size)}</span>
                </div>
                <div class="app-details-meta-row">
                    <span class="app-details-meta-label">Published</span>
                    <span class="app-details-meta-value">${formatPublishDate(app.published_at)}</span>
                </div>
                ${app.source_url ? `
                <div class="app-details-meta-row">
                    <span class="app-details-meta-label">Source</span>
                    <a href="#" class="app-details-meta-link" id="app-details-source-link" data-url="${escapeHtml(app.source_url)}">${escapeHtml(formatSourceUrl(app.source_url))}</a>
                </div>
                ` : ''}
                <div class="app-details-meta-row">
                    <span class="app-details-meta-label">App ID</span>
                    <span class="app-details-meta-value" style="font-family: monospace; font-size: 12px;">${escapeHtml(app.id)}</span>
                </div>
            </div>
        </div>
    `;

    // Show the panel
    panel.style.display = 'flex';

    // Set up event handlers
    const backBtn = document.getElementById('app-details-back-btn');
    const actionBtn = document.getElementById('app-details-action-btn');
    const playBtn = document.getElementById('app-details-play-btn');
    const uninstallBtn = document.getElementById('app-details-uninstall-btn');
    const publisherEl = document.getElementById('app-details-publisher');

    // Back button handler
    backBtn.onclick = () => {
        closeAppDetailsPanel();
    };

    // Action button handler (Install only - for uninstalled apps)
    if (actionBtn) {
        actionBtn.onclick = async () => {
            await handleAppDetailsAction(app, actionBtn);
        };
    }

    // Play button handler (for installed apps)
    if (playBtn) {
        playBtn.onclick = async () => {
            await handleAppDetailsPlay(app, playBtn);
        };
    }

    // Uninstall button handler (for installed apps)
    if (uninstallBtn) {
        uninstallBtn.onclick = async () => {
            await handleAppDetailsUninstall(app, uninstallBtn);
        };
    }

    // Publisher click handler - open profile
    publisherEl.onclick = async () => {
        const npub = publisherEl.dataset.npub;
        if (npub && typeof openProfile === 'function') {
            // Close both panels simultaneously for faster transition
            const closePromises = [closeAppDetailsPanel()];
            if (typeof hideMarketplacePanel === 'function') {
                closePromises.push(hideMarketplacePanel());
            }
            await Promise.all(closePromises);
            // openProfile expects a profile object with at least an 'id' field
            const profile = (typeof getProfile === 'function' ? getProfile(npub) : null) || { id: npub };
            openProfile(profile);
        }
    };

    // Category tag click handlers - add filter and close details
    const categoryTags = content.querySelectorAll('.app-details-category');
    categoryTags.forEach(tag => {
        tag.addEventListener('click', () => {
            const category = tag.dataset.category;
            if (category) {
                closeAppDetailsPanel();
                addMarketplaceFilter(category);
            }
        });
    });

    // Source link click handler - open in browser
    const sourceLink = document.getElementById('app-details-source-link');
    if (sourceLink) {
        sourceLink.onclick = (e) => {
            e.preventDefault();
            const url = sourceLink.dataset.url;
            if (url && typeof openUrl === 'function') {
                openUrl(url);
            } else if (url) {
                // Fallback: try to open via shell
                invoke('open_url', { url }).catch(console.error);
            }
        };
    }

    // Try to load publisher profile info
    loadPublisherProfile(app.publisher);
}

/**
 * Handle the Install/Play action from the details panel
 * @param {MarketplaceApp} app - The app
 * @param {HTMLElement} btn - The action button
 */
async function handleAppDetailsAction(app, btn) {
    const actionText = getAppActionText(app);
    const launchingText = getAppLaunchingText(app);
    
    if (app.installed || app.local_path) {
        // Already installed, just play/open
        btn.textContent = launchingText;
        btn.disabled = true;
        try {
            await openMarketplaceApp(app.id);
            // Reset button state after successful launch
            btn.textContent = actionText;
            btn.disabled = false;
            // Hide the details panel after launching
            document.getElementById('app-details-panel').style.display = 'none';
        } catch (error) {
            console.error('Failed to open app:', error);
            btn.textContent = actionText;
            btn.disabled = false;
        }
    } else {
        // Need to install first
        btn.textContent = 'Installing...';
        btn.disabled = true;
        btn.classList.add('installing');

        try {
            await installMarketplaceApp(app.id);
            
            // Update the app state in the global marketplaceApps array
            const globalApp = marketplaceApps.find(a => a.id === app.id);
            if (globalApp) {
                globalApp.installed = true;
            }
            
            // Update the passed app object
            app.installed = true;
            
            // Refresh the details panel to show Play/Open/Uninstall buttons
            showAppDetails(app);
            
            // Also update the card in the marketplace list if visible
            const card = document.querySelector(`.marketplace-app-card[data-app-id="${app.id}"]`);
            if (card) {
                const cardBtn = card.querySelector('.marketplace-install-btn');
                if (cardBtn) {
                    cardBtn.textContent = actionText;
                    cardBtn.classList.add('installed');
                }
            }
        } catch (error) {
            console.error('Failed to install app:', error);
            btn.textContent = 'Failed';
            btn.classList.remove('installing');
            
            // Reset after a delay
            setTimeout(() => {
                btn.textContent = 'Install';
                btn.disabled = false;
            }, 2000);
        }
    }
}

/**
 * Handle the Play/Open action from the details panel (for installed apps)
 * @param {MarketplaceApp} app - The app
 * @param {HTMLElement} btn - The play button
 */
async function handleAppDetailsPlay(app, btn) {
    const actionText = getAppActionText(app);
    const launchingText = getAppLaunchingText(app);
    
    btn.textContent = launchingText;
    btn.disabled = true;
    try {
        await openMarketplaceApp(app.id);
        // Reset button state after successful launch
        btn.textContent = actionText;
        btn.disabled = false;
    } catch (error) {
        console.error('Failed to open app:', error);
        btn.textContent = actionText;
        btn.disabled = false;
    }
}

/**
 * Handle the Uninstall action from the details panel (for installed apps)
 * @param {MarketplaceApp} app - The app
 * @param {HTMLElement} btn - The uninstall button
 */
async function handleAppDetailsUninstall(app, btn) {
    // Confirm uninstall using popupConfirm
    const confirmed = await popupConfirm(
        `Uninstall ${app.name}?`,
        'This will delete the app and remove it from your history.',
        false,
        '',
        'vector_warning.svg'
    );
    if (!confirmed) {
        return;
    }

    btn.textContent = 'Uninstalling...';
    btn.disabled = true;

    try {
        await uninstallMarketplaceApp(app.id, app.name);
        
        // Update the app state in the global marketplaceApps array
        const globalApp = marketplaceApps.find(a => a.id === app.id);
        if (globalApp) {
            globalApp.installed = false;
            globalApp.local_path = null;
        }
        
        // Also update the passed app object
        app.installed = false;
        app.local_path = null;
        
        // Refresh the details panel to show Install button instead
        showAppDetails(app);
        
        // Also update the card in the marketplace list if visible
        const card = document.querySelector(`.marketplace-app-card[data-app-id="${app.id}"]`);
        if (card) {
            const cardBtn = card.querySelector('.marketplace-install-btn');
            if (cardBtn) {
                cardBtn.textContent = 'Install';
                cardBtn.classList.remove('installed');
            }
        }
    } catch (error) {
        console.error('Failed to uninstall app:', error);
        btn.textContent = 'Failed';
        
        // Reset after a delay
        setTimeout(() => {
            btn.textContent = 'Uninstall';
            btn.disabled = false;
        }, 2000);
    }
}

/**
 * Load publisher profile information
 * @param {string} npub - Publisher's npub
 */
function loadPublisherProfile(npub) {
    const nameEl = document.getElementById('app-details-publisher-name');
    const avatarContainer = document.querySelector('.app-details-publisher-avatar');
    
    if (!nameEl) return;

    try {
        // Use the getProfile function from main.js (looks up from arrProfiles)
        const profile = typeof getProfile === 'function' ? getProfile(npub) : null;
        
        if (profile) {
            // Update name (prefer nickname, then name, then display_name)
            if (profile.nickname) {
                nameEl.textContent = profile.nickname;
            } else if (profile.name) {
                nameEl.textContent = profile.name;
            } else if (profile.display_name) {
                nameEl.textContent = profile.display_name;
            } else {
                nameEl.textContent = npub.substring(0, 12) + '...';
            }

            // Update avatar if available
            if (profile.avatar && avatarContainer) {
                avatarContainer.innerHTML = `<img src="${escapeHtml(profile.avatar)}" alt="Avatar" onerror="this.outerHTML='<span class=\\'icon icon-user-circle\\'></span>'">`;
            }
        } else {
            // No profile found, show truncated npub
            nameEl.textContent = npub.substring(0, 12) + '...';
        }
    } catch (error) {
        console.error('Failed to load publisher profile:', error);
        nameEl.textContent = npub.substring(0, 12) + '...';
    }
}

/**
 * Get popular categories sorted by app count
 * @param {MarketplaceApp[]} apps - The apps to analyze
 * @param {number} limit - Maximum number of categories to return
 * @returns {Array<{name: string, count: number}>} Popular categories
 */
function getPopularCategories(apps, limit = 6) {
    const categoryCount = {};
    
    for (const app of apps) {
        for (const category of app.categories) {
            const normalized = category.toLowerCase();
            categoryCount[normalized] = (categoryCount[normalized] || 0) + 1;
        }
    }
    
    return Object.entries(categoryCount)
        .map(([name, count]) => ({ name, count }))
        .sort((a, b) => b.count - a.count)
        .slice(0, limit);
}

/**
 * Get apps for a specific category
 * @param {MarketplaceApp[]} apps - The apps to filter
 * @param {string} category - The category to filter by
 * @param {number} limit - Maximum number of apps to return
 * @returns {MarketplaceApp[]} Apps in the category
 */
function getAppsForCategory(apps, category, limit = 4) {
    const normalized = category.toLowerCase();
    return apps
        .filter(app => app.categories.some(c => c.toLowerCase() === normalized))
        .sort((a, b) => b.published_at - a.published_at)
        .slice(0, limit);
}

/**
 * Render the featured categories section
 * @param {MarketplaceApp[]} apps - All marketplace apps
 */
function renderFeaturedCategories(apps) {
    const container = document.getElementById('marketplace-featured');
    if (!container) return;
    
    // Don't show featured section when searching or filtering
    if (marketplaceSearchQuery.trim() || marketplaceActiveFilters.length > 0) {
        container.innerHTML = '';
        return;
    }
    
    container.innerHTML = '';
    
    // Featured category: Multiplayer (highlighted)
    const multiplayerApps = getAppsForCategory(apps, 'multiplayer', 4);
    if (multiplayerApps.length > 0) {
        const multiplayerCount = apps.filter(app =>
            app.categories.some(c => c.toLowerCase() === 'multiplayer')
        ).length;
        
        const multiplayerDescription = "Team up with friends and dive into the Vectorverse together";
        const featuredCard = createFeaturedCategoryCard('Multiplayer', multiplayerDescription, multiplayerApps, multiplayerCount, true);
        container.appendChild(featuredCard);
    }
    
    // Popular categories pills
    const popularCategories = getPopularCategories(apps, 8)
        .filter(cat => cat.name !== 'multiplayer' && cat.name !== 'game' && cat.name !== 'app'); // Exclude generic ones
    
    if (popularCategories.length > 0) {
        const pillsContainer = document.createElement('div');
        pillsContainer.className = 'marketplace-popular-categories';
        
        for (const category of popularCategories) {
            const pill = document.createElement('div');
            pill.className = 'marketplace-popular-category';
            pill.textContent = category.name;
            pill.onclick = () => addMarketplaceFilter(category.name);
            pillsContainer.appendChild(pill);
        }
        
        container.appendChild(pillsContainer);
    }
}

/**
 * Create a featured category card with card-spread preview
 * @param {string} categoryName - The category name
 * @param {string} description - A brief description for the category
 * @param {MarketplaceApp[]} apps - Top apps in this category
 * @param {number} totalCount - Total number of apps in this category
 * @param {boolean} highlighted - Whether this is a highlighted/featured category
 * @returns {HTMLElement} The featured category card element
 */
function createFeaturedCategoryCard(categoryName, description, apps, totalCount, highlighted = false) {
    const card = document.createElement('div');
    card.className = 'marketplace-featured-category';
    if (highlighted) {
        card.classList.add('highlighted');
    }
    
    // Create card spread preview
    let cardSpreadHtml = '<div class="marketplace-card-spread">';
    for (const app of apps) {
        if (app.icon_url) {
            cardSpreadHtml += `<div class="marketplace-card-spread-item"><img src="${escapeHtml(app.icon_url)}" alt="${escapeHtml(app.name)}" onerror="this.outerHTML='<span class=\\'icon icon-play\\'></span>'"></div>`;
        } else {
            cardSpreadHtml += `<div class="marketplace-card-spread-item"><span class="icon icon-play"></span></div>`;
        }
    }
    cardSpreadHtml += '</div>';
    
    card.innerHTML = `
        <div class="marketplace-featured-header">
            <div class="marketplace-featured-title">
                ${escapeHtml(categoryName)}
                <span class="marketplace-featured-count">${totalCount} ${totalCount === 1 ? 'app' : 'apps'}</span>
            </div>
        </div>
        <p class="marketplace-featured-description">${escapeHtml(description)}</p>
        ${cardSpreadHtml}
    `;
    
    card.onclick = () => addMarketplaceFilter(categoryName);
    
    return card;
}

/**
 * Filter apps based on search query and active category filters
 * @param {MarketplaceApp[]} apps - The apps to filter
 * @returns {MarketplaceApp[]} Filtered apps
 */
function filterMarketplaceApps(apps) {
    let filtered = apps;

    // Filter by search query
    if (marketplaceSearchQuery.trim()) {
        const query = marketplaceSearchQuery.toLowerCase().trim();
        filtered = filtered.filter(app =>
            app.name.toLowerCase().includes(query) ||
            app.description.toLowerCase().includes(query) ||
            app.categories.some(c => c.toLowerCase().includes(query))
        );
    }

    // Filter by active category filters
    if (marketplaceActiveFilters.length > 0) {
        filtered = filtered.filter(app =>
            marketplaceActiveFilters.every(filter =>
                app.categories.some(c => c.toLowerCase() === filter.toLowerCase())
            )
        );
    }

    return filtered;
}

/**
 * Add a category filter
 * @param {string} category - The category to filter by
 */
function addMarketplaceFilter(category) {
    const normalizedCategory = category.toLowerCase();
    if (!marketplaceActiveFilters.includes(normalizedCategory)) {
        marketplaceActiveFilters.push(normalizedCategory);
        updateMarketplaceFiltersUI();
        refreshMarketplaceDisplay();
    }
}

/**
 * Remove a category filter
 * @param {string} category - The category to remove
 */
function removeMarketplaceFilter(category) {
    const normalizedCategory = category.toLowerCase();
    const index = marketplaceActiveFilters.indexOf(normalizedCategory);
    if (index > -1) {
        marketplaceActiveFilters.splice(index, 1);
        updateMarketplaceFiltersUI();
        refreshMarketplaceDisplay();
    }
}

/**
 * Clear all filters
 */
function clearMarketplaceFilters() {
    marketplaceActiveFilters = [];
    marketplaceSearchQuery = '';
    const searchInput = document.getElementById('marketplace-search-input');
    if (searchInput) searchInput.value = '';
    updateMarketplaceFiltersUI();
    refreshMarketplaceDisplay();
}

/**
 * Update the filters UI to show active filters
 */
function updateMarketplaceFiltersUI() {
    const filtersContainer = document.getElementById('marketplace-filters');
    const activeFiltersContainer = document.getElementById('marketplace-active-filters');
    
    if (!filtersContainer || !activeFiltersContainer) return;

    if (marketplaceActiveFilters.length === 0) {
        filtersContainer.style.display = 'none';
        return;
    }

    filtersContainer.style.display = 'block';
    activeFiltersContainer.innerHTML = '';

    for (const filter of marketplaceActiveFilters) {
        const tag = document.createElement('div');
        tag.className = 'marketplace-filter-tag';
        tag.innerHTML = `
            <span>${escapeHtml(filter)}</span>
            <span class="icon icon-x"></span>
        `;
        tag.onclick = () => removeMarketplaceFilter(filter);
        activeFiltersContainer.appendChild(tag);
    }
}

/**
 * Refresh the marketplace display with current filters
 */
function refreshMarketplaceDisplay() {
    const container = document.getElementById('marketplace-content');
    if (!container) return;
    
    // Render featured categories (hidden when searching/filtering)
    renderFeaturedCategories(marketplaceApps);
    
    const filteredApps = filterMarketplaceApps(marketplaceApps);
    renderMarketplaceApps(container, filteredApps);
}

/**
 * Render the marketplace apps list
 * @param {HTMLElement} container - The container element
 * @param {MarketplaceApp[]} apps - The apps to render
 */
function renderMarketplaceApps(container, apps) {
    container.innerHTML = '';

    // Check if we have filters active but no results
    const hasFilters = marketplaceSearchQuery.trim() || marketplaceActiveFilters.length > 0;

    if (apps.length === 0) {
        const emptyMsg = document.createElement('div');
        emptyMsg.className = 'marketplace-empty';
        
        if (hasFilters) {
            emptyMsg.innerHTML = `
                <span class="icon icon-search marketplace-empty-icon"></span>
                <p>No apps found</p>
                <p class="marketplace-empty-hint">Try adjusting your search or filters</p>
            `;
        } else {
            emptyMsg.innerHTML = `
                <span class="icon icon-gift marketplace-empty-icon"></span>
                <p>No apps available yet</p>
                <p class="marketplace-empty-hint">Check back later for new Mini Apps!</p>
            `;
        }
        container.appendChild(emptyMsg);
        return;
    }

    // Sort apps by publish date (newest first)
    const sortedApps = [...apps].sort((a, b) => {
        return b.published_at - a.published_at;
    });

    // Add section title based on current view
    const sectionTitle = document.createElement('div');
    sectionTitle.className = 'marketplace-section-title';
    if (marketplaceSearchQuery.trim()) {
        sectionTitle.innerHTML = `<span class="icon icon-search"></span> Search Results`;
    } else if (marketplaceActiveFilters.length > 0) {
        const filterText = marketplaceActiveFilters.map(f => f.charAt(0).toUpperCase() + f.slice(1)).join(', ');
        sectionTitle.innerHTML = `<span class="icon icon-bookmark"></span> ${escapeHtml(filterText)}`;
    } else {
        sectionTitle.innerHTML = `<span class="icon icon-clock"></span> New Arrivals`;
    }
    container.appendChild(sectionTitle);

    for (const app of sortedApps) {
        const card = createMarketplaceAppCard(app);
        container.appendChild(card);
    }

    // Truncate category tags after cards are rendered
    requestAnimationFrame(() => {
        truncateAllCategoryTags();
    });
}

/**
 * Initialize the marketplace panel
 * @param {HTMLElement} container - The marketplace container element
 */
async function initMarketplace(container) {
    // Reset filters and search
    marketplaceSearchQuery = '';
    marketplaceActiveFilters = [];
    
    // Set up search input handler
    const searchInput = document.getElementById('marketplace-search-input');
    if (searchInput) {
        searchInput.value = '';
        // Remove old listener if any
        searchInput.removeEventListener('input', handleMarketplaceSearch);
        searchInput.addEventListener('input', handleMarketplaceSearch);
    }
    
    // Update filters UI (hide it since no filters active)
    updateMarketplaceFiltersUI();
    
    // Show loading state
    container.innerHTML = `
        <div class="marketplace-loading">
            <span class="icon icon-loading marketplace-loading-icon"></span>
            <p>Loading marketplace...</p>
        </div>
    `;

    try {
        // First try to show cached apps (filtered)
        const cachedApps = await getCachedMarketplaceApps();
        if (cachedApps.length > 0) {
            marketplaceApps = cachedApps;
            renderFeaturedCategories(cachedApps);
            const filteredCached = filterMarketplaceApps(cachedApps);
            renderMarketplaceApps(container, filteredCached);
        }

        // Then fetch fresh data
        const apps = await fetchMarketplaceApps(true);
        
        // Sync install status (this updates the Rust state)
        await syncInstallStatus();
        
        // Re-fetch to get updated install status from Rust
        const updatedApps = await getCachedMarketplaceApps();
        // Update the global marketplaceApps array with the updated data
        marketplaceApps = updatedApps;
        // Render featured categories and filtered apps
        renderFeaturedCategories(updatedApps);
        const filteredApps = filterMarketplaceApps(updatedApps);
        renderMarketplaceApps(container, filteredApps);
    } catch (error) {
        console.error('Failed to initialize marketplace:', error);
        container.innerHTML = `
            <div class="marketplace-error">
                <span class="icon icon-warning marketplace-error-icon"></span>
                <p>Failed to load marketplace</p>
                <p class="marketplace-error-hint">${escapeHtml(error.toString())}</p>
                <button class="marketplace-retry-btn" onclick="initMarketplace(this.parentElement.parentElement)">
                    Retry
                </button>
            </div>
        `;
    }
}

/**
 * Handle search input changes
 * @param {Event} e - Input event
 */
function handleMarketplaceSearch(e) {
    marketplaceSearchQuery = e.target.value;
    refreshMarketplaceDisplay();
}

/**
 * Refresh the marketplace (fetch new data)
 * @param {HTMLElement} container - The marketplace container element
 */
async function refreshMarketplace(container) {
    await initMarketplace(container);
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
 * @returns {Promise<string>} Event ID of the published event
 */
async function publishMarketplaceApp(filePath, appId, name, description, version, categories, changelog, developer, sourceUrl) {
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
                <h2>Publish to Marketplace</h2>
            </div>
            <div class="publish-app-content">
                <div class="publish-app-icon-container">
                    ${miniAppInfo?.icon_data
                        ? `<img src="${miniAppInfo.icon_data}" alt="App Icon" class="publish-app-icon">`
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
                sourceUrl
            );

            console.log('Published app with event ID:', eventId);

            // Success - close dialog
            overlay.classList.remove('active');
            setTimeout(() => overlay.style.display = 'none', 300);

            // Show success message
            popupConfirm('Published!', `${name} has been published to the marketplace.`, true, '', 'vector-check.svg');
        } catch (error) {
            console.error('Failed to publish:', error);
            submitBtn.disabled = false;
            submitBtn.innerHTML = '<span class="icon icon-star"></span> Publish';
            alert('Failed to publish: ' + error.toString());
        }
    };
}
