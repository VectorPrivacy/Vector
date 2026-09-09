// Relays and media servers: the network list and its dialogs.
// One global scope: loads after main.js and shares its globals.

let fNetworkHandlersSet = false;

/** Fetch the relay and media server lists into the Network section's state. */
async function renderRelayList() {
    if (!fNetworkHandlersSet) {
        fNetworkHandlersSet = true;
        VectorSvelte.setSettingsHandlers('network', {
                explain: (kind) => kind === 'relays'
                    ? popupConfirm('Nostr Relays', 'Nostr Relays are <b>decentralized servers that store and relay your messages</b> across the Nostr network.<br><br>Vector connects to multiple relays simultaneously to ensure your messages are delivered reliably and are censorship-resistant.', true)
                    : popupConfirm('Media Servers', 'Media Servers are <b>Blossom-compatible servers that store your files</b> (images, videos, documents) for sharing in messages and for storage in an encrypted cloud.<br><br>Your server list syncs automatically across your devices.', true),
                addRelay: () => openAddRelayDialog(),
                addServer: async () => {
                    const url = await popupConfirm(
                        'Add Media Server',
                        'Enter the address of a Blossom-compatible server. A bare domain like <b>blossom.primal.net</b> works. Vector adds <b>https://</b> automatically.',
                        false,
                        'blossom.primal.net',
                    );
                    if (!url) return;
                    try {
                        await addCustomBlossomServer(url.trim());
                        renderRelayList();
                    } catch (err) {
                        popupConfirm('Could not add server', String(err), true, '', 'vector_warning.svg');
                    }
                },
                openRelay: (relay) => openRelayInfoDialog(relay),
                openServer: (server) => openBlossomServerInfoDialog(server),
                toggleRelay: async (relay, enabled) => {
                    try {
                        if (relay.is_default) {
                            if (!enabled) {
                                const confirmed = await popupConfirm(
                                    'Disable Default Relay?',
                                    'This is a <b>default relay</b>. Disabling it may affect message delivery and sync reliability.<br><br>Are you sure you want to disable it?',
                                    false
                                );
                                if (!confirmed) return false;
                            }
                            await invoke('toggle_default_relay', { url: relay.url, enabled });
                        } else {
                            await invoke('toggle_custom_relay', { url: relay.url, enabled });
                        }
                        renderRelayList();
                        return true;
                    } catch (err) {
                        console.error('Failed to toggle relay:', err);
                        return false;
                    }
                },
        });
    }
    try {
        const [relays, servers] = await Promise.all([invoke('get_relays'), invoke('get_blossom_servers_config')]);
        VectorSvelte.setNetwork({ relays, servers });
    } catch (error) {
        console.error('Failed to fetch network info:', error);
    }
}

// =============================================================================
// Relay Dialog Management
// =============================================================================

/** Currently selected relay for info dialog */
let currentRelayInfo = null;
/** Interval for refreshing relay info dialog data */
let relayInfoRefreshInterval = null;

/**
 * Opens the Add Relay dialog
 */
function openAddRelayDialog() {
    const overlay = document.getElementById('add-relay-overlay');
    const urlInput = document.getElementById('add-relay-url');
    const modeSelect = document.getElementById('add-relay-mode');

    // Reset form
    urlInput.value = '';
    modeSelect.value = 'both';

    // Show dialog
    overlay.classList.add('active');
    urlInput.focus();
}

/**
 * Closes the Add Relay dialog
 */
function closeAddRelayDialog() {
    const overlay = document.getElementById('add-relay-overlay');
    overlay.classList.remove('active');
}

/**
 * Handles adding a new relay from the dialog
 */
async function handleAddRelay() {
    const urlInput = document.getElementById('add-relay-url');
    const modeSelect = document.getElementById('add-relay-mode');
    let url = urlInput.value.trim();
    const mode = modeSelect.value;

    if (!url) {
        popupConfirm('Invalid URL', 'Please enter a relay URL.', true);
        return;
    }

    // Normalize URL: strip protocol if present and add wss://
    url = url.replace(/^wss?:\/\//i, '');
    url = 'wss://' + url;

    try {
        await invoke('add_custom_relay', { url, mode });
        closeAddRelayDialog();
        renderRelayList();
    } catch (err) {
        popupConfirm('Failed to Add Relay', escapeHtml(err.toString()), true);
    }
}

/**
 * Refreshes the data displayed in the Relay Info dialog
 */
async function refreshRelayInfoDialog() {
    if (!currentRelayInfo) return;

    const url = currentRelayInfo.url;

    // Fetch fresh relay data
    try {
        const relays = await invoke('get_relays');
        const freshRelay = relays.find(r => r.url.toLowerCase() === url.toLowerCase());
        if (freshRelay) {
            currentRelayInfo = freshRelay;

            // Update status
            const statusEl = document.getElementById('relay-info-status');
            statusEl.textContent = freshRelay.status;
            statusEl.className = `relay-status ${freshRelay.status}`;

            // Update disable button text
        const disableBtn = document.getElementById('relay-info-disable');
        if (freshRelay.is_default) {
            disableBtn.innerHTML = freshRelay.enabled 
                ? '<span class="icon icon-disable"></span> Disable'
                : '<span class="icon icon-disable"></span> Enable';
        }
        }
    } catch (err) {
        console.error('Failed to refresh relay data:', err);
    }

    // Refresh metrics
    try {
        const metrics = await invoke('get_relay_metrics', { url });
        const pingEl = document.getElementById('relay-info-ping');
        if (metrics.ping_ms) {
            pingEl.textContent = `${metrics.ping_ms}ms`;
            pingEl.style.color = metrics.ping_ms < 200 ? 'var(--status-excellent)'
                : metrics.ping_ms < 500 ? 'var(--status-good)'
                : metrics.ping_ms < 1000 ? 'var(--status-fair)'
                : 'var(--status-poor)';
        } else {
            pingEl.textContent = '--';
            pingEl.style.color = '';
        }
        if (metrics.last_check) {
            const lastCheck = new Date(metrics.last_check * 1000);
            const now = new Date();
            const diffSecs = Math.floor((now - lastCheck) / 1000);
            let lastCheckText;
            if (diffSecs < 60) {
                lastCheckText = `${diffSecs}s ago`;
            } else if (diffSecs < 3600) {
                lastCheckText = `${Math.floor(diffSecs / 60)}m ago`;
            } else {
                lastCheckText = lastCheck.toLocaleTimeString();
            }
            document.getElementById('relay-info-last-check').textContent = lastCheckText;
        } else {
            document.getElementById('relay-info-last-check').textContent = '--';
        }
    } catch (err) {
        console.error('Failed to load relay metrics:', err);
    }

    // Refresh logs
    try {
        const logs = await invoke('get_relay_logs', { url });
        VectorSvelte.setRelayLogs(logs);
    } catch (err) {
        console.error('Failed to load relay logs:', err);
    }
}

/**
 * Opens the Relay Info dialog
 * @param {Object} relay - The relay object
 */
async function openRelayInfoDialog(relay) {
    // Clear any existing interval
    if (relayInfoRefreshInterval) {
        clearInterval(relayInfoRefreshInterval);
        relayInfoRefreshInterval = null;
    }

    currentRelayInfo = relay;
    const overlay = document.getElementById('relay-info-overlay');
    const urlEl = document.getElementById('relay-info-url');
    const modeSelect = document.getElementById('relay-info-mode');

    // Set static info (URL doesn't change)
    urlEl.textContent = relay.url.replace(/^wss?:\/\//, '');

    // Set mode (only editable for custom relays)
    modeSelect.value = relay.mode || 'both';
    modeSelect.disabled = relay.is_default;

    // Initial data load
    await refreshRelayInfoDialog();

    // Start refresh interval (every 1 second)
    relayInfoRefreshInterval = setInterval(refreshRelayInfoDialog, 1000);

    // Show dialog
    overlay.classList.add('active');
}

/**
 * Closes the Relay Info dialog
 */
function closeRelayInfoDialog() {
    // Clear the refresh interval
    if (relayInfoRefreshInterval) {
        clearInterval(relayInfoRefreshInterval);
        relayInfoRefreshInterval = null;
    }

    const overlay = document.getElementById('relay-info-overlay');
    overlay.classList.remove('active');
    currentRelayInfo = null;
}

/**
 * Handles mode change from the info dialog
 */
async function handleRelayModeChange() {
    if (!currentRelayInfo || currentRelayInfo.is_default) return;

    const modeSelect = document.getElementById('relay-info-mode');
    const newMode = modeSelect.value;

    try {
        await invoke('update_relay_mode', { url: currentRelayInfo.url, mode: newMode });
        currentRelayInfo.mode = newMode;
        renderRelayList();
    } catch (err) {
        console.error('Failed to update relay mode:', err);
        popupConfirm('Error', 'Failed to update relay mode: ' + err.toString(), true);
    }
}

// =============================================================================
// Blossom Media Server Info Dialog
// =============================================================================

/** Currently-open blossom server (info dialog). */
let currentBlossomInfo = null;

function openBlossomServerInfoDialog(server) {
    currentBlossomInfo = server;
    const overlay = document.getElementById('blossom-info-overlay');
    document.getElementById('blossom-info-url').textContent = server.url.replace(/^https?:\/\//, '');

    const statusEl = document.getElementById('blossom-info-status');
    statusEl.className = `relay-status relay-status-small ${server.enabled ? 'connected' : 'disabled'}`;
    statusEl.textContent = server.enabled ? 'enabled' : 'disabled';

    const actionBtn = document.getElementById('blossom-info-action');
    if (server.is_custom) {
        actionBtn.textContent = 'Remove Server';
    } else {
        actionBtn.textContent = server.enabled ? 'Disable Server' : 'Enable Server';
    }

    overlay.classList.add('active');
    // Reset synchronously so stale data doesn't flash mid-fetch.
    VectorSvelte.setBlossomCaps('loading', []);
    const token = ++_blossomCapsToken;
    renderBlossomCapabilities(server.url, token);
}

/** Monotonic token — rapid open(A) → open(B) races resolve in B's favour. */
let _blossomCapsToken = 0;

async function renderBlossomCapabilities(url, token) {
    try {
        const caps = await getBlossomServerCapabilities(url);
        if (token !== _blossomCapsToken) return;
        VectorSvelte.setBlossomCaps('ok', caps || []);
    } catch (err) {
        console.error('Failed to load blossom capabilities:', err);
        if (token !== _blossomCapsToken) return;
        VectorSvelte.setBlossomCaps('error', []);
    }
}

function closeBlossomServerInfoDialog() {
    document.getElementById('blossom-info-overlay').classList.remove('active');
    currentBlossomInfo = null;
}

async function handleBlossomAction() {
    if (!currentBlossomInfo) return;
    const server = currentBlossomInfo;
    try {
        if (server.is_custom) {
            const ok = await popupConfirm(
                'Remove media server?',
                `<b>${server.url}</b> will be removed from your list. Existing uploads on that server remain accessible.`,
                false,
            );
            if (!ok) return;
            await removeCustomBlossomServer(server.url);
            closeBlossomServerInfoDialog();
            renderRelayList();
        } else {
            const newEnabled = !server.enabled;
            await toggleDefaultBlossomServer(server.url, newEnabled);
            currentBlossomInfo = { ...server, enabled: newEnabled };
            openBlossomServerInfoDialog(currentBlossomInfo);
            renderRelayList();
        }
    } catch (err) {
        popupConfirm('Error', String(err), true, '', 'vector_warning.svg');
    }
}

/**
 * Handles disable/remove button from info dialog
 */
async function handleRelayDisable() {
    if (!currentRelayInfo) return;

    const relay = currentRelayInfo;

    if (relay.is_default) {
        // Toggle default relay
        const newEnabled = !relay.enabled;
        if (!newEnabled) {
            // Show warning before disabling default relay
            const confirmed = await popupConfirm(
                'Disable Default Relay?',
                'This is a <b>default relay</b>. Disabling it may affect message delivery and sync reliability.<br><br>Are you sure you want to disable it?',
                false
            );
            if (confirmed) {
                try {
                    await invoke('toggle_default_relay', { url: relay.url, enabled: false });
                    closeRelayInfoDialog();
                    renderRelayList();
                } catch (err) {
                    popupConfirm('Error', 'Failed to disable relay: ' + err.toString(), true);
                }
            }
        } else {
            // Re-enable without warning
            try {
                await invoke('toggle_default_relay', { url: relay.url, enabled: true });
                closeRelayInfoDialog();
                renderRelayList();
            } catch (err) {
                popupConfirm('Error', 'Failed to enable relay: ' + err.toString(), true);
            }
        }
    } else {
        // Remove custom relay
        const confirmed = await popupConfirm(
            'Remove Relay?',
            `Are you sure you want to remove <b>${relay.url.replace(/^wss?:\/\//, '')}</b>?`,
            false
        );
        if (confirmed) {
            try {
                await invoke('remove_custom_relay', { url: relay.url });
                closeRelayInfoDialog();
                renderRelayList();
            } catch (err) {
                popupConfirm('Error', 'Failed to remove relay: ' + err.toString(), true);
            }
        }
    }
}

/**
 * Initialize relay dialog event listeners
 */
function initRelayDialogs() {
    VectorSvelte.mountRelayLogs(document.getElementById('relay-info-logs'));
    VectorSvelte.mountBlossomCaps(document.getElementById('blossom-info-capabilities'), { h: { formatBytes } });
    // Add Relay Dialog
    document.getElementById('add-relay-close').onclick = closeAddRelayDialog;
    document.getElementById('add-relay-cancel').onclick = closeAddRelayDialog;
    document.getElementById('add-relay-confirm').onclick = handleAddRelay;
    document.getElementById('add-relay-overlay').onclick = (e) => {
        if (e.target.id === 'add-relay-overlay') closeAddRelayDialog();
    };

    // Allow Enter key to submit
    document.getElementById('add-relay-url').onkeydown = (e) => {
        if (e.key === 'Enter') handleAddRelay();
    };

    // Relay Info Dialog
    document.getElementById('relay-info-close').onclick = closeRelayInfoDialog;
    document.getElementById('relay-info-done').onclick = closeRelayInfoDialog;
    document.getElementById('relay-info-disable').onclick = handleRelayDisable;
    document.getElementById('relay-info-mode').onchange = handleRelayModeChange;
    document.getElementById('relay-info-overlay').onclick = (e) => {
        if (e.target.id === 'relay-info-overlay') closeRelayInfoDialog();
    };

    // Copy logs button
    document.getElementById('relay-logs-copy').onclick = copyRelayLogs;

    // Blossom server info dialog
    document.getElementById('blossom-info-close').onclick = closeBlossomServerInfoDialog;
    document.getElementById('blossom-info-done').onclick = closeBlossomServerInfoDialog;
    document.getElementById('blossom-info-action').onclick = handleBlossomAction;
    document.getElementById('blossom-info-overlay').onclick = (e) => {
        if (e.target.id === 'blossom-info-overlay') closeBlossomServerInfoDialog();
    };
}

/**
 * Copies relay logs to clipboard in a formatted way
 */
function copyRelayLogs() {
    if (!currentRelayInfo) return;

    // Read logs from the displayed DOM to avoid async clipboard permission issues
    const logsList = document.getElementById('relay-info-logs');
    const logItems = logsList.querySelectorAll('li:not(.relay-log-empty)');

    let text;
    if (logItems.length === 0) {
        text = 'No activity recorded yet';
    } else {
        const header = `Relay Logs: ${currentRelayInfo.url.replace(/^wss?:\/\//, '')}\n${'='.repeat(50)}\n`;
        const logs = Array.from(logItems).map(li => {
            const time = li.querySelector('.relay-log-time')?.textContent || '';
            const msg = li.querySelector('.relay-log-message')?.textContent || '';
            const level = li.querySelector('.relay-log-message')?.classList.contains('error') ? 'ERROR' :
                          li.querySelector('.relay-log-message')?.classList.contains('warn') ? 'WARN' : 'INFO';
            return `[${time}] [${level}] ${msg}`;
        }).join('\n');
        text = header + logs;
    }

    navigator.clipboard.writeText(text).then(() => {
        // Visual feedback - change icon briefly
        const copyBtn = document.getElementById('relay-logs-copy');
        const icon = copyBtn.querySelector('.icon');
        icon.classList.remove('icon-copy');
        icon.classList.add('icon-check');
        setTimeout(() => {
            icon.classList.remove('icon-check');
            icon.classList.add('icon-copy');
        }, 1500);
    }).catch(err => {
        console.error('Failed to copy relay logs:', err);
    });
}

// =============================================================================
