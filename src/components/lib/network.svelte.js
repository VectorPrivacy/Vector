// The Network section's dialogs and the data behind them: the Add Relay form, the relay
// and media server info dialogs, a media server's capabilities and a relay's log lines.
import { fadeDialog } from './dialog-lifecycle.svelte.js';

export const addRelayDialog = fadeDialog({ url: '', mode: 'both' });

export const relayInfoDialog = fadeDialog({
    url: '', status: '', isDefault: false, enabled: true, mode: 'both',
    ping: '--', pingColor: '', lastCheck: '--', copied: false,
});

export const blossomInfoDialog = fadeDialog({ url: '', enabled: true, isCustom: false });


const blossomCaps = $state({ status: 'loading', caps: [] });
export function blossomCapsState() { return blossomCaps; }
export function setBlossomCaps(status, caps) { blossomCaps.status = status; blossomCaps.caps = caps || []; }
const relayLogs = $state({ logs: [] });
export function relayLogsState() { return relayLogs; }
export function setRelayLogs(logs) { relayLogs.logs = logs || []; }
