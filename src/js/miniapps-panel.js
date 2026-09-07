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

// The grid is a Svelte island over lib/miniappsgrid.svelte.js; this side reads
// history, resolves icons and owns the gestures.
const miniAppsEditMode = () => VectorSvelte.gridState().editMode;
const miniAppIconCache = new Map();
// Default apps mid-download on a fresh install, kept across history reloads
// until they land: key → grid entry.
const miniAppsPreinstalling = new Map();

let miniAppsGridMounted = false;
function ensureMiniAppsGrid() {
    if (miniAppsGridMounted || !domMiniAppsGrid) return;
    miniAppsGridMounted = true;
    VectorSvelte.mountMiniAppsGrid(domMiniAppsGrid, {
        h: {
            openNexus: () => { closeAttachmentPanel(); showMarketplacePanel(); },
            open: openGridApp,
            showTip: showGlobalTooltip,
            hideTip: hideGlobalTooltip,
            iconFailed: (a) => VectorSvelte.gridPatch(a.key, { icon: null }),
            update: (a) => handleMiniAppPanelUpdate(a.marketplaceId),
            remove: removeGridApp,
        },
    });
}

function miniAppKey(app) { return app.marketplace_id || app.src_url || app.name; }

async function openGridApp(a) {
    if (miniAppsEditMode() || a.downloading) return;
    hideGlobalTooltip();
    if (a.pivx) {
        if (a.hidden) {
            localStorage.removeItem('pivx_hidden');
            await loadMiniAppsHistory();
            popupConfirm('PIVX Wallet Restored', 'The PIVX Wallet has been restored to your Mini Apps panel.', true);
        } else {
            showPivxWalletPanel();
        }
        return;
    }
    if (a.app) await openMiniAppFromHistory(a.app);
}

async function removeGridApp(a) {
    hideGlobalTooltip();
    const displayName = a.pivx ? 'PIVX Wallet' : a.name;
    const confirmed = await popupConfirm(
        'Remove App?',
        `Are you sure you want to remove <b>${escapeHtml(displayName)}</b> from your recent Mini Apps?`,
        false
    );
    if (!confirmed) return;
    if (a.pivx) {
        // Hidden rather than gone: the search reveals it and a click restores it.
        localStorage.setItem('pivx_hidden', 'true');
    } else {
        try {
            await invoke('miniapp_remove_from_history', { name: displayName });
        } catch (err) {
            console.error('Failed to remove Mini App from history:', err);
        }
    }
    deactivateMiniAppsEditMode();
    await loadMiniAppsHistory();
    animateAttachmentPanelItems(domMiniAppsGrid);
}

/**
 * Loads the Mini Apps history into the grid. PIVX is a virtual app positioned
 * by its own last use.
 */
async function loadMiniAppsHistory() {
    ensureMiniAppsGrid();
    try {
        const history = await invoke('miniapp_get_history', { limit: null });

        const preInstallDone = localStorage.getItem('miniapps_preinstall_done') === 'true';
        if (history.length === 0 && !preInstallDone) {
            localStorage.setItem('miniapps_preinstall_done', 'true');
            preinstallDefaultMiniApps();
        }

        const pivxHidden = localStorage.getItem('pivx_hidden') === 'true';
        const pivxLastUsedMs = parseInt(localStorage.getItem('pivx_last_used') || '0', 10);
        const pivxLastOpenedAt = Math.floor(pivxLastUsedMs / 1000);

        const entries = [
            { key: 'pivx', name: 'PIVX', pivx: true, hidden: pivxHidden, lastOpened: pivxLastOpenedAt },
            ...history.map(app => {
                const marketplaceId = app.marketplace_id || null;
                const mktApp = marketplaceId ? marketplaceApps.find(m => m.id === marketplaceId) : null;
                return {
                    key: miniAppKey(app), name: app.name, pivx: false, hidden: false, app, marketplaceId,
                    hasUpdate: !!(mktApp && mktApp.version && mktApp.version !== app.installed_version),
                    downloading: false, icon: miniAppIconCache.get(app.src_url) || null,
                    lastOpened: app.last_opened_at || 0,
                };
            }),
        ];
        entries.sort((a, b) => b.lastOpened - a.lastOpened);
        for (const [key, entry] of miniAppsPreinstalling) {
            if (!entries.some(e => e.key === key)) entries.push(entry);
        }
        const empty = history.length === 0 && !miniAppsPreinstalling.size && (pivxHidden || pivxLastOpenedAt === 0);
        VectorSvelte.gridSetApps(entries, empty);
        VectorSvelte.flushSync();

        for (const app of history) {
            if (!miniAppIconCache.has(app.src_url)) loadMiniAppIcon(app);
        }
    } catch (e) {
        console.error('Failed to load Mini Apps history:', e);
    }
}

/**
 * A fresh install starts with two default apps. Their tiles appear at once as
 * downloads and turn into ordinary apps as each lands; the history reload keys
 * on the marketplace id so the tile is the same node throughout.
 */
async function preinstallDefaultMiniApps() {
    const defaults = [
        { id: 'vectify', name: 'Vectify' },
        { id: 'deadlock', name: 'State of Surveillance' },
    ];
    for (const { id, name } of defaults) {
        miniAppsPreinstalling.set(id, {
            key: id, name, pivx: false, hidden: false, app: null, marketplaceId: id,
            hasUpdate: false, downloading: true, icon: null, lastOpened: 0,
        });
    }
    try {
        await fetchMarketplaceApps(true);
        const installs = [];
        for (const { id } of defaults) {
            const app = marketplaceApps.find(a => a.id === id);
            if (!app) {
                console.warn(`[Mini Apps] Default app "${id}" not found in marketplace`);
                miniAppsPreinstalling.delete(id);
                continue;
            }
            // Cached local icon directly; a remote icon_url via the backend cache
            // (the WebView never fetches remote — Tor).
            let icon = app.icon_cached ? convertFileSrc(app.icon_cached) : null;
            if (!icon && app.icon_url) {
                icon = await invoke('cache_url_image', { url: app.icon_url }).then(p => p ? convertFileSrc(p) : null).catch(() => null);
            }
            const entry = { ...miniAppsPreinstalling.get(id), name: app.name, icon };
            miniAppsPreinstalling.set(id, entry);
            VectorSvelte.gridPatch(id, entry);
            installs.push(installMarketplaceApp(app.id).catch(err => {
                console.error(`[Mini Apps] Failed to pre-install ${app.id}:`, err);
            }).then(() => {
                miniAppsPreinstalling.delete(id);
                return loadMiniAppsHistory();
            }));
        }
        await loadMiniAppsHistory();
        await Promise.all(installs);
    } catch (preInstallErr) {
        console.error('[Mini Apps] Pre-install failed:', preInstallErr);
        miniAppsPreinstalling.clear();
    }
}

/** Filter the grid by name; the Nexus hides while searching. */
function filterMiniApps(query) {
    VectorSvelte.gridSetQuery(query || '');
}

// ========== Mini Apps Edit Mode ==========

let miniAppsHoldTimer = null;
let miniAppsEditModeJustActivated = false;

/** Edit mode: the grid wobbles and every app but the Nexus grows a delete badge. */
function activateMiniAppsEditMode() {
    if (miniAppsEditMode()) return;
    VectorSvelte.gridSetEditMode(true);
    miniAppsEditModeJustActivated = true;
    domMiniAppsGrid.classList.add('edit-mode');

    document.removeEventListener('click', handleEditModeClickOutside, true);
    // A beat later, so the mouseup click from the hold doesn't exit at once.
    setTimeout(() => {
        if (miniAppsEditMode()) document.addEventListener('click', handleEditModeClickOutside, true);
    }, 50);
}

function deactivateMiniAppsEditMode() {
    if (!miniAppsEditMode()) return;
    VectorSvelte.gridSetEditMode(false);
    miniAppsEditModeJustActivated = false;
    domMiniAppsGrid.classList.remove('edit-mode');
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
    if (miniAppsEditMode()) return;
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

/** Resolve an app's icon from its bundle and paint it onto its tile. */
async function loadMiniAppIcon(app) {
    try {
        const info = await invoke('miniapp_load_info', { filePath: app.src_url });
        if (info && info.icon_data) {
            miniAppIconCache.set(app.src_url, info.icon_data);
            VectorSvelte.gridPatch(miniAppKey(app), { icon: info.icon_data });
        }
    } catch (e) {
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
            // DOM construction: app.name comes from the .xdc manifest
            const img = document.createElement('img');
            img.src = info.icon_data;
            img.alt = app.name;
            domMiniAppLaunchIconContainer.replaceChildren(img);
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
        // Open the Mini App directly using the cached file path (openMiniApp
        // runs the Tor IP-exposure consent gate).
        // Use a placeholder chat_id and message_id for solo play
        await openMiniApp(app.src_url, 'solo', `solo_${Date.now()}`, null, null);
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

    // Tor IP-exposure consent BEFORE the file is sent to the chat — declining
    // after the invite already landed would strand a dead lobby message.
    if (!(await confirmMiniAppTorExposure(app.src_url))) {
        closeMiniAppLaunchDialog();
        return; // User declined launching multiplayer over clearnet
    }

    // Fire the send and get out of the user's way: the dialog closes NOW, the
    // upload runs behind the message bubble's own progress UI, and the game
    // opens as soon as the optimistic attachment exists. The realtime topic is
    // minted before the upload starts and rides that pending attachment, so
    // peers who download later already find the host waiting — playing during
    // the upload costs joinability nothing.
    const isGroup = chatIsGroup(getChat(targetChatId));
    // Snapshot BEFORE firing: an older .xdc message of ours in this chat must
    // not be mistaken for the one this send is about to surface.
    const priorXdcId = newestOwnXdcMessage(targetChatId)?.messageId || null;
    const sendPromise = isGroup
        ? invoke('send_community_files', {
            channelId: targetChatId,
            content: '',
            filePaths: [app.src_url],
            nameOverrides: [''],
            useCompression: false,
            keepMetadata: false,
            repliedTo: '',
        })
        : invoke('file_message', {
            receiver: targetChatId,
            repliedTo: '',
            filePath: app.src_url,
            keepMetadata: false,
            nameOverride: '',
        });

    closeMiniAppLaunchDialog();
    closeAttachmentPanel();

    try {
        // The pending bubble (carrying the minted topic) lands in local state
        // before the first uploaded byte — grab it, or the finished result if
        // the send beat us to it (smart-forward reuse completes instantly).
        const found = await waitForOwnXdcMessage(targetChatId, sendPromise, priorXdcId);
        if (!found) {
            throw new Error('send surfaced no Mini App message');
        }

        // DM sends resolve with a pending→final id swap to apply.
        sendPromise.then((result) => {
            if (!isGroup && result && result.pending_id && result.event_id) {
                finalizePendingMessage(targetChatId, result.pending_id, result.event_id);
            }
        }).catch((e) => {
            // The bubble shows the failed state with its own retry — the open
            // game keeps running as a solo session.
            console.error('Play & Invite: send failed after launch:', e);
        });

        // Open the Mini App (consent already given above; the session flag
        // makes openMiniApp's gate a no-op here)
        await openMiniApp(found.filePath, targetChatId, found.messageId, null, found.topicId);
    } catch (e) {
        console.error('Failed to send Mini App to chat:', e);
        // Fallback to solo play if sending fails
        await playMiniAppSoloInternal(app);
    }
}

/**
 * Resolve the just-sent Mini App message from local chat state: the newest
 * own message carrying an .xdc attachment. Polls briefly (the pending bubble
 * lands within milliseconds of the invoke), racing the send's own completion
 * so the instant path (reused blob) needs no poll at all.
 */
async function waitForOwnXdcMessage(chatId, sendPromise, priorXdcId) {
    // Completion beats polling when the send is instant; a rejection just ends
    // the race (the poll below still gets its chance until timeout).
    let settled = false;
    sendPromise.then(() => { settled = true; }).catch(() => { settled = true; });

    const fresh = () => {
        const found = newestOwnXdcMessage(chatId);
        // topicId is required: without it the backend derives a topic from the
        // MESSAGE id, and a pending id derives a different topic than the
        // final id the receiver holds — the one split that breaks joining.
        return found && found.filePath && found.topicId && found.messageId !== priorXdcId ? found : null;
    };
    const deadline = Date.now() + 10_000;
    while (Date.now() < deadline) {
        const found = fresh();
        if (found) return found;
        if (settled) {
            // Send finished (or failed) — one last look, then give up.
            return fresh();
        }
        await new Promise(r => setTimeout(r, 100));
    }
    return null;
}

/** The newest own message in `chatId` carrying an .xdc attachment. */
function newestOwnXdcMessage(chatId) {
    const chat = getChat(chatId);
    if (!chat) return null;
    for (let i = chat.messages.length - 1; i >= 0; i--) {
        const m = chat.messages[i];
        if (!m.mine || !m.attachments) continue;
        const att = m.attachments.find(a =>
            a.extension === 'xdc' || (a.path && a.path.endsWith('.xdc'))
        );
        if (att) {
            return {
                messageId: m.id,
                topicId: att.webxdc_topic || null,
                filePath: att.path || null,
            };
        }
    }
    return null;
}

/**
 * Handle updating a Mini App directly from the panel grid
 * @param {string} marketplaceId - The marketplace app ID to update
 */
async function handleMiniAppPanelUpdate(marketplaceId) {
    // Downloading blocks taps and edit-mode deletion for the tile.
    VectorSvelte.gridPatch(marketplaceId, { downloading: true, hasUpdate: false });
    try {
        await updateMarketplaceApp(marketplaceId);
        await loadMiniAppsHistory();
    } catch (e) {
        console.error('Failed to update Mini App from panel:', e);
        VectorSvelte.gridPatch(marketplaceId, { downloading: false, hasUpdate: true });
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

        // Open the Mini App directly using the cached file path (openMiniApp
        // runs the Tor IP-exposure consent gate).
        await openMiniApp(app.src_url, 'solo', `solo_${Date.now()}`, null, null);
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
