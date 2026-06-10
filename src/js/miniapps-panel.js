/**
 * Mini Apps panel — the attachment-panel sub-view that lists installed
 * Mini Apps (WebXDC), the PIVX wallet entry, and a marketplace shortcut.
 *
 * Three regions of behaviour live here:
 *   1. Panel show/hide and item animation
 *   2. History rendering (incl. PIVX-as-virtual-app and pre-install on first run)
 *   3. Hold-to-edit-mode (delete badges) + per-app launch dialog
 *
 * Low-level WebXDC helpers (loadMiniAppInfo, etc.) live in miniapps.js.
 * Marketplace itself (catalog, install, update internals) lives in marketplace.js.
 */

/**
 * Shows the main attachment panel view (File, Mini Apps buttons)
 */
function showAttachmentPanelMain() {
    popBack('attachment-mini-apps');
    domAttachmentPanelMain.style.display = 'flex';
    domAttachmentPanelMiniAppsView.style.display = 'none';
    // Also hide PIVX wallet view if open
    if (domAttachmentPanelPivxView) {
        domAttachmentPanelPivxView.style.display = 'none';
    }
    // Remove PIVX-active border styling
    if (domAttachmentPanel) {
        domAttachmentPanel.classList.remove('pivx-active');
    }

    // Animate items with staggered delay
    animateAttachmentPanelItems(domAttachmentPanelMain);
}

/**
 * Shows the Mini Apps list view
 */
async function showAttachmentPanelMiniApps() {
    pushBack('attachment-mini-apps', showAttachmentPanelMain);
    domAttachmentPanelMain.style.display = 'none';
    domAttachmentPanelMiniAppsView.style.display = 'flex';
    // Also hide PIVX wallet view if open
    if (domAttachmentPanelPivxView) {
        domAttachmentPanelPivxView.style.display = 'none';
    }

    // Clear search input and reset filter
    if (domMiniAppsSearch) {
        domMiniAppsSearch.value = '';
        filterMiniApps('');
    }

    // Load Mini Apps history from backend
    await loadMiniAppsHistory();

    // Animate items with staggered delay
    animateAttachmentPanelItems(domMiniAppsGrid);
}

/**
 * Shows the marketplace panel
 */
function showMarketplacePanel() {
    if (domMarketplacePanel) {
        pushBack('marketplace', () => { hideMarketplacePanel(); });
        domMarketplacePanel.style.display = 'flex';
        // Initialize marketplace on first show
        initMarketplace(domMarketplaceContent);
    }
}

/**
 * Hides the marketplace panel with fade out animation
 * @returns {Promise<void>} Resolves when the animation completes
 */
function hideMarketplacePanel() {
    return new Promise((resolve) => {
        if (domMarketplacePanel && domMarketplacePanel.style.display !== 'none') {
            popBack('marketplace');
            domMarketplacePanel.classList.add('closing');
            domMarketplacePanel.addEventListener('animationend', function handler() {
                domMarketplacePanel.removeEventListener('animationend', handler);
                domMarketplacePanel.style.display = 'none';
                domMarketplacePanel.classList.remove('closing');
                resolve();
            });
        } else {
            resolve();
        }
    });
}

/**
 * Animate attachment panel items with staggered fade-in effect
 */
function animateAttachmentPanelItems(container) {
    const items = container.querySelectorAll('.attachment-panel-item');
    const totalAnimTime = 0.35; // Total stagger duration in seconds
    const maxDelay = 0.08; // Cap at 80ms per item for small counts
    const staggerDelay = items.length > 1 ? Math.min(maxDelay, totalAnimTime / (items.length - 1)) : 0;

    items.forEach((item, index) => {
        // Remove any existing animation
        item.classList.remove('animate-in');
        item.style.animationDelay = '';

        // Force reflow to restart animation
        void item.offsetWidth;

        // Add animation with staggered delay
        item.style.animationDelay = `${index * staggerDelay}s`;
        item.classList.add('animate-in');

        // Remove animate-in class when animation finishes to avoid conflicts with other animations
        item.addEventListener('animationend', () => {
            item.classList.remove('animate-in');
            item.style.animationDelay = '';
        }, { once: true });
    });
}

/**
 * Loads and renders the Mini Apps history in the panel
 * PIVX is treated as a virtual app and positioned based on usage history
 */
async function loadMiniAppsHistory() {
    try {
        let history = await invoke('miniapp_get_history', { limit: null });

        // Pre-install default apps for new users (empty history)
        const preInstallDone = localStorage.getItem('miniapps_preinstall_done') === 'true';
        if (history.length === 0 && !preInstallDone) {
            // Mark as done first to prevent re-triggering
            localStorage.setItem('miniapps_preinstall_done', 'true');

            // Clear any existing items first (except Marketplace button)
            const existingItems = domMiniAppsGrid.querySelectorAll('.attachment-panel-item:not(#attachment-panel-marketplace), .attachment-panel-empty');
            existingItems.forEach(item => item.remove());

            // Default apps to pre-install (by app ID with display names)
            const defaultApps = [
                { id: 'vectify', name: 'Vectify' },
                { id: 'deadlock', name: 'State of Surveillance' }
            ];

            // IMMEDIATELY show placeholder UI before fetching marketplace data
            const staggerDelay = 0.03; // Same delay used for regular mini apps
            let animIndex = 0;

            for (const { id: appId, name: displayName } of defaultApps) {
                const item = document.createElement('button');
                item.className = 'attachment-panel-item attachment-panel-miniapp animate-in';
                item.id = `miniapp-downloading-${appId}`;
                item.draggable = false;
                item.style.position = 'relative';
                item.style.animationDelay = `${animIndex * staggerDelay}s`;

                // Show generic loading state initially
                item.innerHTML = `
                    <div class="attachment-panel-btn attachment-panel-miniapp-btn">
                        <span class="icon icon-play"></span>
                        <div class="miniapp-downloading-overlay">
                            <div class="miniapp-downloading-spinner" data-app-id="${escapeHtml(appId)}"></div>
                        </div>
                    </div>
                    <span class="attachment-panel-label cutoff">${escapeHtml(displayName)}</span>
                `;
                item.dataset.appName = displayName.toLowerCase();
                item.addEventListener('animationend', () => {
                    item.classList.remove('animate-in');
                    item.style.animationDelay = '';
                }, { once: true });
                domMiniAppsGrid.appendChild(item);
                animIndex++;
            }

            // Add PIVX after the downloading apps
            const pivxBtn = document.createElement('button');
            pivxBtn.className = 'attachment-panel-item animate-in';
            pivxBtn.id = 'attachment-panel-pivx';
            pivxBtn.draggable = false;
            pivxBtn.style.animationDelay = `${animIndex * staggerDelay}s`;
            pivxBtn.innerHTML = `
                <div class="attachment-panel-btn attachment-panel-pivx-btn">
                    <span class="icon icon-pivx"></span>
                </div>
                <span class="attachment-panel-label">PIVX</span>
            `;
            pivxBtn.dataset.appName = 'pivx';
            pivxBtn.addEventListener('mouseenter', () => showGlobalTooltip('PIVX Wallet', pivxBtn));
            pivxBtn.addEventListener('mouseleave', () => hideGlobalTooltip());
            pivxBtn.addEventListener('animationend', () => {
                pivxBtn.classList.remove('animate-in');
                pivxBtn.style.animationDelay = '';
            }, { once: true });
            pivxBtn.onclick = () => {
                if (miniAppsEditMode) return;
                hideGlobalTooltip();
                showPivxWalletPanel();
            };
            domMiniAppsGrid.appendChild(pivxBtn);

            // Now fetch marketplace data and start downloads in background
            try {
                await fetchMarketplaceApps(true);

                const appsToInstall = [];

                // Update placeholders with actual app metadata and start downloads
                for (const { id: appId } of defaultApps) {
                    const app = marketplaceApps.find(a => a.id === appId);
                    const placeholder = document.getElementById(`miniapp-downloading-${appId}`);

                    if (app && placeholder) {
                        appsToInstall.push(app);

                        // Update with actual icon and name
                        const iconSrc = app.icon_cached ? convertFileSrc(app.icon_cached) : app.icon_url;
                        const iconHtml = iconSrc
                            ? `<img src="${escapeHtml(iconSrc)}" style="width: 100%; height: 100%; object-fit: cover; border-radius: inherit;" onerror="this.outerHTML='<span class=\\'icon icon-play\\'></span>'">`
                            : '<span class="icon icon-play"></span>';

                        placeholder.innerHTML = `
                            <div class="attachment-panel-btn attachment-panel-miniapp-btn">
                                ${iconHtml}
                                <div class="miniapp-downloading-overlay">
                                    <div class="miniapp-downloading-spinner" data-app-id="${escapeHtml(appId)}"></div>
                                </div>
                            </div>
                            <span class="attachment-panel-label cutoff">${escapeHtml(app.name)}</span>
                        `;
                        placeholder.dataset.appName = app.name.toLowerCase();
                    } else if (!app && placeholder) {
                        console.warn(`[Mini Apps] Default app "${appId}" not found in marketplace`);
                        placeholder.remove();
                    }
                }

                // Start all downloads in parallel - transform placeholders when complete
                const installPromises = appsToInstall.map(async (app) => {
                    try {
                        await installMarketplaceApp(app.id);

                        // Transform the downloading placeholder into a normal clickable app
                        const placeholder = document.getElementById(`miniapp-downloading-${app.id}`);
                        if (placeholder) {
                            // Remove the downloading overlay
                            const overlay = placeholder.querySelector('.miniapp-downloading-overlay');
                            if (overlay) overlay.remove();

                            // Remove the temporary ID
                            placeholder.removeAttribute('id');

                            // Add click handler to open the app
                            placeholder.onclick = async () => {
                                if (miniAppsEditMode) return;
                                hideGlobalTooltip();
                                // Fetch the updated app info from history
                                const historyApps = await invoke('miniapp_get_history', { limit: null });
                                const historyApp = historyApps.find(h => h.marketplace_id === app.id || h.name === app.name);
                                if (historyApp) {
                                    await openMiniAppFromHistory(historyApp);
                                }
                            };

                            // Add tooltip
                            placeholder.addEventListener('mouseenter', () => showGlobalTooltip(app.name, placeholder));
                            placeholder.addEventListener('mouseleave', () => hideGlobalTooltip());
                        }

                        return { success: true, app };
                    } catch (installErr) {
                        console.error(`[Mini Apps] Failed to pre-install ${app.id}:`, installErr);
                        // Remove failed placeholder
                        const placeholder = document.getElementById(`miniapp-downloading-${app.id}`);
                        if (placeholder) placeholder.remove();
                        return { success: false, app, error: installErr };
                    }
                });

                // Wait for all installs to complete (UI updates happen individually above)
                await Promise.all(installPromises);

                // Return early - don't do the normal render since we already set up the UI
                return;
            } catch (preInstallErr) {
                console.error('[Mini Apps] Pre-install failed:', preInstallErr);
            }
        }

        // Clear existing Mini App items (keep only Marketplace)
        const existingItems = domMiniAppsGrid.querySelectorAll('.attachment-panel-item:not(#attachment-panel-marketplace), .attachment-panel-empty');
        existingItems.forEach(item => item.remove());

        // Check if PIVX is hidden by user
        const pivxHidden = localStorage.getItem('pivx_hidden') === 'true';

        // Get PIVX last used timestamp from localStorage (stored in ms, convert to seconds)
        const pivxLastUsedMs = parseInt(localStorage.getItem('pivx_last_used') || '0', 10);
        const pivxLastOpenedAt = Math.floor(pivxLastUsedMs / 1000);

        // Create combined list of apps with PIVX always included (hidden flag controls visibility)
        const allApps = [
            // Add PIVX as a virtual app entry (always present, hidden flag for grayed-out state)
            { name: 'PIVX', isPivx: true, isHidden: pivxHidden, last_opened_at: pivxLastOpenedAt },
            // Add history apps with their timestamps (backend uses last_opened_at in seconds)
            ...history.map(app => ({ ...app, isPivx: false, last_opened_at: app.last_opened_at || 0 }))
        ];

        // Sort by last_opened_at timestamp (most recent first)
        allApps.sort((a, b) => b.last_opened_at - a.last_opened_at);

        // Add all app items in sorted order
        for (const app of allApps) {
            if (app.isPivx) {
                // Create PIVX button
                const pivxBtn = document.createElement('button');
                pivxBtn.className = 'attachment-panel-item' + (app.isHidden ? ' miniapp-disabled' : '');
                pivxBtn.id = 'attachment-panel-pivx';
                pivxBtn.draggable = false;
                pivxBtn.innerHTML = `
                    <div class="attachment-panel-btn attachment-panel-pivx-btn">
                        <span class="icon icon-pivx"></span>
                    </div>
                    <span class="attachment-panel-label">PIVX</span>
                `;
                // Store app name for search filtering
                pivxBtn.dataset.appName = 'pivx';
                // Mark hidden state for search filter logic
                if (app.isHidden) pivxBtn.dataset.appHidden = 'true';

                // Hidden PIVX starts invisible (only revealed via search)
                if (app.isHidden) pivxBtn.style.display = 'none';

                // Add tooltip on hover
                pivxBtn.addEventListener('mouseenter', () => {
                    showGlobalTooltip(app.isHidden ? 'Restore PIVX Wallet' : 'PIVX Wallet', pivxBtn);
                });
                pivxBtn.addEventListener('mouseleave', () => {
                    hideGlobalTooltip();
                });

                pivxBtn.onclick = async () => {
                    // Don't launch if in edit mode
                    if (miniAppsEditMode) return;
                    hideGlobalTooltip();
                    if (app.isHidden) {
                        // Restore hidden PIVX
                        localStorage.removeItem('pivx_hidden');
                        await loadMiniAppsHistory();
                        popupConfirm('PIVX Wallet Restored', 'The PIVX Wallet has been restored to your Mini Apps panel.', true);
                    } else {
                        showPivxWalletPanel();
                    }
                };
                domMiniAppsGrid.appendChild(pivxBtn);
            } else {
                // Create Mini App button
                const item = document.createElement('button');
                item.className = 'attachment-panel-item attachment-panel-miniapp';
                item.draggable = false;

                // Start with a placeholder icon, then load the actual icon
                item.innerHTML = `
                    <div class="attachment-panel-btn attachment-panel-miniapp-btn">
                        <span class="icon icon-play"></span>
                    </div>
                    <span class="attachment-panel-label cutoff">${escapeHtml(app.name)}</span>
                `;

                // Store the app name for search filtering
                item.dataset.appName = app.name.toLowerCase();

                // Add tooltip on hover for the entire item
                item.addEventListener('mouseenter', () => {
                    showGlobalTooltip(app.name, item);
                });
                item.addEventListener('mouseleave', () => {
                    hideGlobalTooltip();
                });

                item.onclick = async () => {
                    // Don't launch if in edit mode or currently updating
                    if (miniAppsEditMode) return;
                    if (item.dataset.updating) return;
                    hideGlobalTooltip();
                    // Open the Mini App using the stored attachment reference
                    await openMiniAppFromHistory(app);
                };
                // Set marketplace ID for update lookup
                if (app.marketplace_id) {
                    item.dataset.marketplaceId = app.marketplace_id;
                }

                // Check for marketplace update
                if (app.marketplace_id) {
                    const mktApp = marketplaceApps.find(m => m.id === app.marketplace_id);
                    if (mktApp && mktApp.version && mktApp.version !== app.installed_version) {
                        item.dataset.hasUpdate = 'true';
                        const badge = document.createElement('div');
                        badge.className = 'miniapp-update-badge';
                        badge.innerHTML = '<span class="icon icon-arrow-up"></span>';
                        badge.onclick = (e) => {
                            e.stopPropagation();
                            handleMiniAppPanelUpdate(app.marketplace_id);
                        };
                        item.appendChild(badge);
                    }
                }

                domMiniAppsGrid.appendChild(item);

                // Load the Mini App icon asynchronously
                loadMiniAppIcon(app, item.querySelector('.attachment-panel-btn'));
            }
        }

        // If no apps at all (only PIVX which is always there), show empty message
        if (history.length === 0 && (pivxHidden || pivxLastOpenedAt === 0)) {
            const emptyMsg = document.createElement('div');
            emptyMsg.className = 'attachment-panel-empty';
            emptyMsg.textContent = 'No recent Mini Apps';
            domMiniAppsGrid.appendChild(emptyMsg);
        }
    } catch (e) {
        console.error('Failed to load Mini Apps history:', e);
    }
}

/**
 * Filters the Mini Apps grid based on search query
 * Hides Marketplace when search is active
 * @param {string} query - The search query
 */
function filterMiniApps(query) {
    const normalizedQuery = query.toLowerCase().trim();
    const isSearching = normalizedQuery.length > 0;

    // Get all items in the grid
    const items = domMiniAppsGrid.querySelectorAll('.attachment-panel-item');
    let visibleCount = 0;

    items.forEach(item => {
        const appName = item.dataset.appName;

        // Always hide Marketplace when searching
        if (item.id === 'attachment-panel-marketplace') {
            item.classList.toggle('hidden-by-search', isSearching);
            if (!isSearching) visibleCount++;
            return;
        }

        // Filter all apps (including PIVX) by name
        if (appName) {
            const isHiddenApp = item.dataset.appHidden === 'true';
            const matches = !isSearching || appName.includes(normalizedQuery);

            if (isHiddenApp) {
                // Hidden apps: only show when searching and name matches
                const show = isSearching && matches;
                item.style.display = show ? '' : 'none';
                item.classList.toggle('hidden-by-search', !show);
                if (show) visibleCount++;
            } else {
                item.classList.toggle('hidden-by-search', !matches);
                if (matches) visibleCount++;
            }
        }
    });

    // Also hide/show empty message based on visible items
    const emptyMsg = domMiniAppsGrid.querySelector('.attachment-panel-empty');
    if (emptyMsg) {
        emptyMsg.classList.toggle('hidden-by-search', isSearching);
    }

    // Show/hide "no results" message
    let noResultsMsg = domMiniAppsGrid.querySelector('.miniapps-no-results');
    if (isSearching && visibleCount === 0) {
        if (!noResultsMsg) {
            noResultsMsg = document.createElement('div');
            noResultsMsg.className = 'miniapps-no-results';
            noResultsMsg.innerHTML = `
                <p>No Mini Apps found</p>
                <p class="miniapps-no-results-hint">Try a different search, or check out the Nexus!</p>
            `;
            domMiniAppsGrid.appendChild(noResultsMsg);
        }
        noResultsMsg.style.display = '';
    } else if (noResultsMsg) {
        noResultsMsg.style.display = 'none';
    }
}

// ========== Mini Apps Edit Mode ==========

let miniAppsEditMode = false;
let miniAppsHoldTimer = null;
let miniAppsEditModeJustActivated = false;

/**
 * Activates edit mode for the Mini Apps grid
 * Shows red X badges on all apps (except Marketplace) for removal
 */
function activateMiniAppsEditMode() {
    if (miniAppsEditMode) return;
    miniAppsEditMode = true;
    miniAppsEditModeJustActivated = true;

    // Add edit-mode class for wobble animation
    domMiniAppsGrid.classList.add('edit-mode');

    // Add delete badges to all apps except Marketplace
    const items = domMiniAppsGrid.querySelectorAll('.attachment-panel-item:not(#attachment-panel-marketplace)');
    items.forEach(item => {
        // Don't add badge if already exists or app is updating
        if (item.querySelector('.miniapp-delete-badge')) return;
        if (item.dataset.updating) return;

        const badge = document.createElement('div');
        badge.className = 'miniapp-delete-badge';
        badge.innerHTML = '<span class="icon icon-x"></span>';

        // Get app info for deletion
        const appName = item.dataset.appName;
        const isPivx = item.id === 'attachment-panel-pivx';

        badge.onclick = async (e) => {
            e.stopPropagation();
            e.preventDefault();

            // Hide tooltip before showing confirm dialog
            hideGlobalTooltip();

            const displayName = isPivx ? 'PIVX Wallet' : item.querySelector('.attachment-panel-label')?.textContent || appName;

            const confirmed = await popupConfirm(
                'Remove App?',
                `Are you sure you want to remove <b>${escapeHtml(displayName)}</b> from your recent Mini Apps?`,
                false
            );

            if (confirmed) {
                if (isPivx) {
                    // For PIVX, set hidden flag (can be restored in Settings)
                    localStorage.setItem('pivx_hidden', 'true');
                } else {
                    // For regular apps, remove from backend history
                    try {
                        await invoke('miniapp_remove_from_history', { name: displayName });
                    } catch (err) {
                        console.error('Failed to remove Mini App from history:', err);
                    }
                }

                // Deactivate edit mode and refresh the list
                deactivateMiniAppsEditMode();
                await loadMiniAppsHistory();
                animateAttachmentPanelItems(domMiniAppsGrid);
            }
        };

        item.appendChild(badge);
    });

    // Add global click listener to exit edit mode
    // Remove any existing listener first to prevent duplicates
    document.removeEventListener('click', handleEditModeClickOutside, true);
    // Add with a small delay so the mouseup click from hold doesn't immediately trigger it
    setTimeout(() => {
        if (miniAppsEditMode) {
            document.addEventListener('click', handleEditModeClickOutside, true);
        }
    }, 50);
}

/**
 * Deactivates edit mode for the Mini Apps grid
 */
function deactivateMiniAppsEditMode() {
    if (!miniAppsEditMode) return;
    miniAppsEditMode = false;
    miniAppsEditModeJustActivated = false;

    // Remove edit-mode class
    domMiniAppsGrid.classList.remove('edit-mode');

    // Remove all delete badges
    const badges = domMiniAppsGrid.querySelectorAll('.miniapp-delete-badge');
    badges.forEach(badge => badge.remove());

    // Remove global click listener
    document.removeEventListener('click', handleEditModeClickOutside, true);
}

/**
 * Handles clicks outside of delete badges to exit edit mode
 */
function handleEditModeClickOutside(e) {
    // If clicking on a delete badge, let it handle itself (check FIRST, before other guards)
    if (e.target.closest('.miniapp-delete-badge')) {
        // Reset the just-activated flag since user is interacting with edit mode
        miniAppsEditModeJustActivated = false;
        return;
    }

    // If clicking on a popup, let it handle itself (don't exit edit mode)
    if (e.target.closest('#popup-container')) return;

    // If we just activated edit mode, ignore this click (it's from the hold release)
    if (miniAppsEditModeJustActivated) {
        miniAppsEditModeJustActivated = false;
        e.preventDefault();
        e.stopPropagation();
        return;
    }

    // Otherwise, deactivate edit mode
    e.preventDefault();
    e.stopPropagation();
    deactivateMiniAppsEditMode();
}

/**
 * Starts the hold timer for entering edit mode
 * @param {Event} e - The mousedown/touchstart event
 */
function startMiniAppHold(e) {
    // Don't start hold on Marketplace or if already in edit mode
    if (miniAppsEditMode) return;
    const item = e.target.closest('.attachment-panel-item');
    if (!item || item.id === 'attachment-panel-marketplace') return;

    // Clear any existing timer
    if (miniAppsHoldTimer) {
        clearTimeout(miniAppsHoldTimer);
    }

    miniAppsHoldTimer = setTimeout(() => {
        miniAppsHoldTimer = null;
        activateMiniAppsEditMode();
    }, 500); // 0.5 second hold
}

/**
 * Cancels the hold timer
 */
function cancelMiniAppHold() {
    if (miniAppsHoldTimer) {
        clearTimeout(miniAppsHoldTimer);
        miniAppsHoldTimer = null;
    }
}

/**
 * Suppresses click events right after edit mode activation
 */
function suppressClickAfterEditMode(e) {
    if (miniAppsEditModeJustActivated) {
        miniAppsEditModeJustActivated = false; // Reset flag so future clicks work
        e.preventDefault();
        e.stopPropagation();
        e.stopImmediatePropagation();
    }
}

/**
 * Sets up hold-to-edit event listeners on the Mini Apps grid
 */
function setupMiniAppsEditMode() {
    if (!domMiniAppsGrid) return;

    // Prevent any drag behavior on the grid items
    domMiniAppsGrid.addEventListener('dragstart', (e) => {
        e.preventDefault();
        return false;
    });

    // Mouse events
    domMiniAppsGrid.addEventListener('mousedown', startMiniAppHold);
    domMiniAppsGrid.addEventListener('mouseup', cancelMiniAppHold);
    domMiniAppsGrid.addEventListener('mouseleave', cancelMiniAppHold);

    // Suppress clicks immediately after edit mode activation
    domMiniAppsGrid.addEventListener('click', suppressClickAfterEditMode, true);

    // Touch events for mobile
    domMiniAppsGrid.addEventListener('touchstart', startMiniAppHold, { passive: false });
    domMiniAppsGrid.addEventListener('touchend', cancelMiniAppHold);
    domMiniAppsGrid.addEventListener('touchcancel', cancelMiniAppHold);
    domMiniAppsGrid.addEventListener('touchmove', cancelMiniAppHold);
}

/**
 * Load Mini App icon asynchronously and update the button
 */
async function loadMiniAppIcon(app, btnElement) {
    try {
        const info = await invoke('miniapp_load_info', { filePath: app.src_url });
        if (info && info.icon_data) {
            // Replace only the placeholder icon, preserving overlays (update badge, etc.)
            const placeholder = btnElement.querySelector('.icon, .attachment-panel-miniapp-icon');
            const img = document.createElement('img');
            img.src = info.icon_data;
            img.alt = app.name;
            img.className = 'attachment-panel-miniapp-icon';
            if (placeholder) {
                placeholder.replaceWith(img);
            } else {
                btnElement.prepend(img);
            }
        }
    } catch (e) {
        // Keep the placeholder icon if loading fails
        console.debug('Failed to load Mini App icon:', e);
    }
}

// Store the pending Mini App for the launch dialog
let pendingMiniAppLaunch = null;

/**
 * Check if a Mini App is a game based on its categories
 * @param {Object} app - The app object with categories field
 * @returns {boolean} True if the app is a game
 */
function isMiniAppGame(app) {
    // If no categories, default to game
    if (!app.categories) {
        return true;
    }

    // Categories can be a comma-separated string or an array
    let cats = app.categories;
    if (typeof cats === 'string') {
        cats = cats.split(',').map(c => c.trim().toLowerCase()).filter(c => c);
    } else if (Array.isArray(cats)) {
        cats = cats.map(c => c.toLowerCase());
    } else {
        return true; // Default to game
    }

    // It's a game if it has "game" tag OR doesn't have "app" tag
    return cats.includes('game') || !cats.includes('app');
}

/**
 * Show the Mini App launch dialog
 */
async function showMiniAppLaunchDialog(app) {
    pendingMiniAppLaunch = app;

    // Set the app name
    domMiniAppLaunchName.textContent = app.name;

    // Determine if this is a game or app and update button text accordingly
    const isGame = isMiniAppGame(app);
    const actionText = isGame ? 'Play' : 'Open';
    domMiniAppLaunchSolo.textContent = actionText;
    domMiniAppLaunchInvite.textContent = `${actionText} & Invite`;

    // Check if this app has a marketplace update available
    const hasUpdate = app.marketplace_id &&
        marketplaceApps.find(m => m.id === app.marketplace_id && m.version && m.version !== app.installed_version);

    if (hasUpdate) {
        domMiniAppLaunchInvite.textContent = 'Update';
        domMiniAppLaunchInvite.dataset.updateMode = 'true';
        domMiniAppLaunchInvite.dataset.marketplaceId = app.marketplace_id;
    } else {
        delete domMiniAppLaunchInvite.dataset.updateMode;
        delete domMiniAppLaunchInvite.dataset.marketplaceId;
    }

    // Try to load the Mini App icon
    try {
        const info = await invoke('miniapp_load_info', { filePath: app.src_url });
        if (info && info.icon_data) {
            domMiniAppLaunchIconContainer.innerHTML = `<img src="${info.icon_data}" alt="${escapeHtml(app.name)}">`;
        } else {
            domMiniAppLaunchIconContainer.innerHTML = '<span class="icon icon-play"></span>';
        }
    } catch (e) {
        // Fallback to generic icon
        domMiniAppLaunchIconContainer.innerHTML = '<span class="icon icon-play"></span>';
    }

    // Show the overlay
    domMiniAppLaunchOverlay.classList.add('active');
    pushBack('miniapp-launch', closeMiniAppLaunchDialog);
}

/**
 * Close the Mini App launch dialog
 */
function closeMiniAppLaunchDialog() {
    popBack('miniapp-launch');
    domMiniAppLaunchOverlay.classList.remove('active');
    pendingMiniAppLaunch = null;
}

/**
 * Play Mini App solo (from original attachment)
 */
async function playMiniAppSolo() {
    if (!pendingMiniAppLaunch) return;

    const app = pendingMiniAppLaunch;
    closeMiniAppLaunchDialog();
    closeAttachmentPanel();

    // Check permissions for marketplace apps
    const shouldContinue = await checkMiniAppPermissions(app);
    if (!shouldContinue) {
        return; // User cancelled
    }

    try {
        // Open the Mini App directly using the cached file path
        // Use a placeholder chat_id and message_id for solo play
        await invoke('miniapp_open', {
            filePath: app.src_url,
            chatId: 'solo',
            messageId: `solo_${Date.now()}`,
            href: null,
            topicId: null,
        });
    } catch (e) {
        console.error('Failed to open Mini App:', e);
    }
}

/**
 * Play Mini App and invite (send to current chat, then open from the new message)
 */
async function playMiniAppAndInvite() {
    if (!pendingMiniAppLaunch) return;

    // Intercept update mode
    if (domMiniAppLaunchInvite.dataset.updateMode === 'true') {
        const marketplaceId = domMiniAppLaunchInvite.dataset.marketplaceId;
        closeMiniAppLaunchDialog();
        await handleMiniAppPanelUpdate(marketplaceId);
        return;
    }

    const app = pendingMiniAppLaunch;
    const targetChatId = strOpenChat;

    // Check if we have an active chat
    if (!targetChatId) {
        console.error('No active chat to send Mini App to');
        closeMiniAppLaunchDialog();
        closeAttachmentPanel();
        // Fallback to solo play
        await playMiniAppSoloInternal(app);
        return;
    }

    // Check permissions for marketplace apps before doing anything
    const shouldContinue = await checkMiniAppPermissions(app);
    if (!shouldContinue) {
        closeMiniAppLaunchDialog();
        return; // User cancelled
    }

    // Show upload progress spinner on the invite button and disable all buttons
    const inviteBtn = domMiniAppLaunchInvite;
    const originalText = inviteBtn.textContent;
    inviteBtn.disabled = true;
    domMiniAppLaunchSolo.disabled = true;
    domMiniAppLaunchCancel.disabled = true;
    domMiniAppLaunchSolo.style.opacity = '0.3';
    domMiniAppLaunchCancel.style.opacity = '0.3';
    inviteBtn.innerHTML = '<div class="miniapp-downloading-spinner" style="width:18px;height:18px;display:inline-block;vertical-align:middle;--progress:2.5%;background:conic-gradient(var(--icon-color-primary) 0% max(2.5%, var(--progress)), rgba(255,255,255,0.5) max(2.5%, var(--progress)) 100%);"></div>';
    const spinnerEl = inviteBtn.querySelector('.miniapp-downloading-spinner');

    // Listen for upload progress to update the spinner
    let progressUnlisten = null;
    try {
        progressUnlisten = await listen('attachment_upload_progress', (evt) => {
            if (spinnerEl && evt.payload.progress != null) {
                spinnerEl.style.setProperty('--progress', `${evt.payload.progress}%`);
            }
        });
    } catch (_) { /* non-critical */ }

    // Helper to reset buttons and close dialog
    const finishAndClose = () => {
        if (progressUnlisten) progressUnlisten();
        inviteBtn.disabled = false;
        inviteBtn.textContent = originalText;
        domMiniAppLaunchSolo.disabled = false;
        domMiniAppLaunchCancel.disabled = false;
        domMiniAppLaunchSolo.style.opacity = '';
        domMiniAppLaunchCancel.style.opacity = '';
        closeMiniAppLaunchDialog();
        closeAttachmentPanel();
    };

    try {
        // Send the Mini App file to the current chat — awaits upload + relay send.
        // Community channels ride their own send pipeline (file_message is DM-only).
        let messageId = null;
        let topicId = null;
        let filePath = app.src_url;
        if (chatIsGroup(getChat(targetChatId))) {
            const result = await invoke('send_community_files', {
                channelId: targetChatId,
                content: '',
                filePaths: [app.src_url],
                nameOverrides: [''],
                useCompression: false,
                repliedTo: '',
            });
            if (!result || !result.message_id) {
                console.error('Play & Invite: send_community_files returned no message_id');
                finishAndClose();
                return;
            }
            messageId = result.message_id;
            topicId = result.webxdc_topic || null;
        } else {
            const result = await invoke('file_message', {
                receiver: targetChatId,
                repliedTo: '',
                filePath: app.src_url,
                nameOverride: '',
            });

            if (!result || !result.event_id) {
                console.error('Play & Invite: file_message returned no event_id');
                finishAndClose();
                return;
            }
            messageId = result.event_id;

            // Finalize the pending message in local state
            finalizePendingMessage(targetChatId, result.pending_id, result.event_id);

            // Find the message in local state to get the topic ID from the attachment
            const chat = getChat(targetChatId);
            if (chat) {
                const msg = chat.messages.find(m => m.id === messageId);
                if (msg && msg.attachments) {
                    const xdcAtt = msg.attachments.find(a =>
                        a.extension === 'xdc' || (a.path && a.path.endsWith('.xdc'))
                    );
                    if (xdcAtt) {
                        topicId = xdcAtt.webxdc_topic || null;
                        if (xdcAtt.path) filePath = xdcAtt.path;
                    }
                }
            }
        }

        // Open the Mini App
        await invoke('miniapp_open', {
            filePath,
            chatId: targetChatId,
            messageId,
            href: null,
            topicId,
        });

        finishAndClose();
    } catch (e) {
        console.error('Failed to send Mini App to chat:', e);
        finishAndClose();
        // Fallback to solo play if sending fails
        await playMiniAppSoloInternal(app);
    }
}

/**
 * Handle updating a Mini App directly from the panel grid
 * @param {string} marketplaceId - The marketplace app ID to update
 */
async function handleMiniAppPanelUpdate(marketplaceId) {
    const item = domMiniAppsGrid.querySelector(
        `[data-marketplace-id="${CSS.escape(marketplaceId)}"]`
    );
    if (!item) return;

    const btn = item.querySelector('.attachment-panel-btn');
    if (!btn) return;

    // Mark as updating (blocks taps and edit-mode deletion)
    item.dataset.updating = 'true';

    // Remove update badge
    const badge = item.querySelector('.miniapp-update-badge');
    if (badge) badge.remove();

    // Add downloading overlay with progress spinner
    const overlay = document.createElement('div');
    overlay.className = 'miniapp-downloading-overlay';
    overlay.innerHTML = `<div class="miniapp-downloading-spinner" data-app-id="${CSS.escape(marketplaceId)}"></div>`;
    btn.appendChild(overlay);

    try {
        await updateMarketplaceApp(marketplaceId);
        overlay.remove();
        delete item.dataset.hasUpdate;
        delete item.dataset.updating;
        await loadMiniAppsHistory();
    } catch (e) {
        console.error('Failed to update Mini App from panel:', e);
        overlay.remove();
        delete item.dataset.updating;
    }
}

/**
 * Check and show permission prompt for a Mini App from history
 * @param {Object} app - The app from history (MiniAppHistoryEntry)
 * @returns {Promise<boolean>} True if we should continue opening, false if cancelled
 */
async function checkMiniAppPermissions(app) {
    // Only marketplace apps have permissions
    if (!app.marketplace_id) {
        return true;
    }

    try {
        // Get the marketplace app info to check for requested permissions and blossom_hash
        const marketplaceApp = await invoke('marketplace_get_app', { appId: app.marketplace_id });

        if (!marketplaceApp || !marketplaceApp.requested_permissions || marketplaceApp.requested_permissions.length === 0) {
            return true; // No permissions requested
        }

        // Use blossom_hash as the permission identifier (content-based security)
        if (!marketplaceApp.blossom_hash) {
            return true; // No hash available, continue
        }

        // Check if we've already prompted for permissions using the file hash
        const hasBeenPrompted = await invoke('miniapp_has_permission_prompt', { fileHash: marketplaceApp.blossom_hash });

        if (hasBeenPrompted) {
            return true; // Already prompted, continue
        }

        // Show the permission prompt (using the function from marketplace.js)
        const userGranted = await showPermissionPrompt(marketplaceApp);
        return userGranted;
    } catch (e) {
        console.error('Failed to check Mini App permissions:', e);
        return true; // Continue on error
    }
}

/**
 * Check and show permission prompt for a Mini App opened from a chat attachment
 * Uses the file hash to look up if there's a matching marketplace app with permissions
 * @param {string} filePath - Path to the .xdc file
 * @returns {Promise<boolean>} True if we should continue opening, false if cancelled
 */
async function checkChatMiniAppPermissions(filePath) {
    try {
        // Load the Mini App info to get the file hash
        const miniAppInfo = await invoke('miniapp_load_info', { filePath });
        if (!miniAppInfo || !miniAppInfo.file_hash) {
            return true; // No hash available, continue
        }

        const fileHash = miniAppInfo.file_hash;

        // Check if we've already prompted for permissions using this file hash
        const hasBeenPrompted = await invoke('miniapp_has_permission_prompt', { fileHash });
        if (hasBeenPrompted) {
            return true; // Already prompted, continue
        }

        // Look up if there's a marketplace app with this hash
        const marketplaceApp = await invoke('marketplace_get_app_by_hash', { fileHash });
        if (!marketplaceApp || !marketplaceApp.requested_permissions || marketplaceApp.requested_permissions.length === 0) {
            return true; // No matching marketplace app or no permissions requested
        }

        // Show the permission prompt (using the function from marketplace.js)
        const userGranted = await showPermissionPrompt(marketplaceApp);
        return userGranted;
    } catch (e) {
        console.error('Failed to check chat Mini App permissions:', e);
        return true; // Continue on error
    }
}

/**
 * Internal function to play Mini App solo
 */
async function playMiniAppSoloInternal(app) {
    try {
        // Check permissions for marketplace apps
        const shouldContinue = await checkMiniAppPermissions(app);
        if (!shouldContinue) {
            return; // User cancelled
        }

        // Open the Mini App directly using the cached file path
        await invoke('miniapp_open', {
            filePath: app.src_url,
            chatId: 'solo',
            messageId: `solo_${Date.now()}`,
            href: null,
            topicId: null,
        });
    } catch (e) {
        console.error('Failed to open Mini App:', e);
    }
}

/**
 * Open Mini App from history - shows the launch dialog
 */
async function openMiniAppFromHistory(app) {
    await showMiniAppLaunchDialog(app);
}
