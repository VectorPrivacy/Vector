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
                        popupConfirm('Could not add server', escapeHtml(String(err)), true, '', 'vector_warning.svg');
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
/** Its last fetched activity log (the copy action formats from data, not the DOM). */
let currentRelayLogs = [];
/** Interval for refreshing relay info dialog data */
let relayInfoRefreshInterval = null;

function openAddRelayDialog() {
    VectorSvelte.addRelayDialog.open({ url: '', mode: 'both' });
}

function closeAddRelayDialog() {
    VectorSvelte.addRelayDialog.close();
}

async function handleAddRelay({ url, mode }) {
    url = url.trim();
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
    const dialog = VectorSvelte.relayInfoDialog;

    // Fetch fresh relay data
    try {
        const relays = await invoke('get_relays');
        const freshRelay = relays.find(r => r.url.toLowerCase() === url.toLowerCase());
        if (freshRelay) {
            currentRelayInfo = freshRelay;
            dialog.patch({ status: freshRelay.status, enabled: freshRelay.enabled });
        }
    } catch (err) {
        console.error('Failed to refresh relay data:', err);
    }

    // Refresh metrics
    try {
        const metrics = await invoke('get_relay_metrics', { url });
        const ping = metrics.ping_ms ? `${metrics.ping_ms}ms` : '--';
        const pingColor = !metrics.ping_ms ? ''
            : metrics.ping_ms < 200 ? 'var(--status-excellent)'
            : metrics.ping_ms < 500 ? 'var(--status-good)'
            : metrics.ping_ms < 1000 ? 'var(--status-fair)'
            : 'var(--status-poor)';
        let lastCheck = '--';
        if (metrics.last_check) {
            const checked = new Date(metrics.last_check * 1000);
            const diffSecs = Math.floor((Date.now() - checked) / 1000);
            lastCheck = diffSecs < 60 ? `${diffSecs}s ago`
                : diffSecs < 3600 ? `${Math.floor(diffSecs / 60)}m ago`
                : checked.toLocaleTimeString();
        }
        dialog.patch({ ping, pingColor, lastCheck });
    } catch (err) {
        console.error('Failed to load relay metrics:', err);
    }

    // Refresh logs
    try {
        const logs = await invoke('get_relay_logs', { url });
        currentRelayLogs = logs || [];
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
    currentRelayLogs = [];
    VectorSvelte.setRelayLogs([]);
    VectorSvelte.relayInfoDialog.patch({
        url: relay.url.replace(/^wss?:\/\//, ''), status: relay.status || '',
        isDefault: !!relay.is_default, enabled: relay.enabled !== false, mode: relay.mode || 'both',
        ping: '--', pingColor: '', lastCheck: '--', copied: false,
    });

    // Initial data load
    await refreshRelayInfoDialog();

    // Start refresh interval (every 1 second)
    relayInfoRefreshInterval = setInterval(refreshRelayInfoDialog, 1000);

    VectorSvelte.relayInfoDialog.open({});
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

    VectorSvelte.relayInfoDialog.close();
    currentRelayInfo = null;
}

/**
 * Handles mode change from the info dialog
 */
async function handleRelayModeChange(newMode) {
    if (!currentRelayInfo || currentRelayInfo.is_default) return;

    try {
        await invoke('update_relay_mode', { url: currentRelayInfo.url, mode: newMode });
        currentRelayInfo.mode = newMode;
        VectorSvelte.relayInfoDialog.patch({ mode: newMode });
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
    VectorSvelte.blossomInfoDialog.open({
        url: server.url.replace(/^https?:\/\//, ''), enabled: !!server.enabled, isCustom: !!server.is_custom,
        status: server.status || null,
    });
    // Reset synchronously so stale data doesn't flash mid-fetch.
    VectorSvelte.setBlossomCaps('loading', []);
    VectorSvelte.setBlossomInfo('loading', null);
    VectorSvelte.setBlossomStats(null);
    const token = ++_blossomCapsToken;
    renderBlossomCapabilities(server.url, token);
    renderBlossomInfo(server.url, token);
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

async function renderBlossomInfo(url, token) {
    const enabled = !!currentBlossomInfo?.enabled;
    // What is already known paints first, so the dialog's first frame is its
    // final shape; the network then changes numbers, never layout.
    try {
        const snap = await invoke('get_blossom_server_snapshot', { url, enabled });
        if (token !== _blossomCapsToken) return;
        VectorSvelte.setBlossomStats(snap.stats);
        VectorSvelte.blossomInfoDialog.patch({ status: snap.status });
        if (snap.info) VectorSvelte.setBlossomInfo('ok', snap.info);
    } catch (err) {
        console.warn('Failed to load blossom server snapshot:', err);
    }
    try {
        const info = await invoke('get_blossom_server_info', { url });
        if (token !== _blossomCapsToken) return;
        VectorSvelte.setBlossomInfo('ok', info);
    } catch (err) {
        console.warn('Failed to load blossom server info:', err);
        if (token !== _blossomCapsToken) return;
        VectorSvelte.setBlossomInfo('error', null);
    }
    // After the document, so the status reflects what it just said.
    try {
        const { stats, status } = await invoke('get_blossom_server_stats', { url, enabled });
        if (token !== _blossomCapsToken) return;
        VectorSvelte.setBlossomStats(stats);
        VectorSvelte.blossomInfoDialog.patch({ status });
    } catch (err) {
        console.warn('Failed to load blossom server stats:', err);
    }
}

function closeBlossomServerInfoDialog() {
    VectorSvelte.blossomInfoDialog.close();
    currentBlossomInfo = null;
    renderRelayList();
}

/** Coalesce a burst of status changes into one list render. */
let _relayListTimer = null;
function renderRelayListSoon() {
    clearTimeout(_relayListTimer);
    _relayListTimer = setTimeout(() => { _relayListTimer = null; renderRelayList(); }, 250);
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
        popupConfirm('Error', escapeHtml(String(err)), true, '', 'vector_warning.svg');
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

/** The Network section's dialogs; their handlers are fixed for the app's life. */
function initRelayDialogs() {
    VectorSvelte.setScreen('network', { h: {
        addRelay: { close: closeAddRelayDialog, confirm: handleAddRelay },
        relayInfo: { close: closeRelayInfoDialog, disable: handleRelayDisable, setMode: handleRelayModeChange, copy: copyRelayLogs },
        blossom: { close: closeBlossomServerInfoDialog, action: handleBlossomAction, formatBytes },
    } });
}

/**
 * Copies relay logs to clipboard in a formatted way
 */
function copyRelayLogs() {
    if (!currentRelayInfo) return;

    let text;
    if (currentRelayLogs.length === 0) {
        text = 'No activity recorded yet';
    } else {
        const header = `Relay Logs: ${currentRelayInfo.url.replace(/^wss?:\/\//, '')}\n${'='.repeat(50)}\n`;
        const logs = currentRelayLogs.map(log => {
            const time = new Date(log.timestamp * 1000).toLocaleTimeString();
            const level = log.level === 'error' ? 'ERROR' : log.level === 'warn' ? 'WARN' : 'INFO';
            return `[${time}] [${level}] ${log.message}`;
        }).join('\n');
        text = header + logs;
    }

    navigator.clipboard.writeText(text).then(() => {
        VectorSvelte.relayInfoDialog.patch({ copied: true });
        setTimeout(() => VectorSvelte.relayInfoDialog.patch({ copied: false }), 1500);
    }).catch(err => {
        console.error('Failed to copy relay logs:', err);
    });
}
