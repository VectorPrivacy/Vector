// The Network section's dialogs and the data behind them: the Add Relay form, the relay
// and media server info dialogs, a media server's capabilities and a relay's log lines.
import { fadeDialog, popOverlay } from './dialog-lifecycle.svelte.js';

export const addRelayDialog = fadeDialog({ url: '', mode: 'both' });

export const relayInfoDialog = popOverlay({
    url: '', status: '', isDefault: false, enabled: true, mode: 'both',
    ping: '--', pingColor: '', lastCheck: '--', copied: false,
});

// Pops like the QR overlay: the content is ready in the first frame, so the motion is
// the only cue that something opened.
export const blossomInfoDialog = popOverlay({ url: '', enabled: true, isCustom: false, status: null });


const blossomCaps = $state({ status: 'loading', caps: [] });
export function blossomCapsState() { return blossomCaps; }
export function setBlossomCaps(status, caps) { blossomCaps.status = status; blossomCaps.caps = caps || []; }
// The server's own information document, or null when it publishes none.
const blossomInfo = $state({ status: 'loading', info: null });
export function blossomInfoState() { return blossomInfo; }
export function setBlossomInfo(status, info) { blossomInfo.status = status; blossomInfo.info = info || null; }
// This device's latency / speed / reachability history with the open server.
const blossomStats = $state({ stats: null });
export function blossomStatsState() { return blossomStats; }
export function setBlossomStats(stats) { blossomStats.stats = stats || null; }
const relayLogs = $state({ logs: [] });
export function relayLogsState() { return relayLogs; }
export function setRelayLogs(logs) { relayLogs.logs = logs || []; }
