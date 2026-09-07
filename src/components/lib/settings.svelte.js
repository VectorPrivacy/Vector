// Settings-screen state (Phase 4). The Tor card derives from the last TorState the
// backend reported (or an optimistic one the toggle handler set); the blocked-users
// list re-fetches when its version moves.
const tor = $state({
    state: null,          // TorState from the backend, or the handler's optimistic one
    statusOverride: '',   // handler-supplied status text ("Bootstrapping…", "Failed: …")
    locked: false,        // an operation is in flight: the toggle stays disabled
    advancedOpen: false,  // the Advanced disclosure is expanded
    circuits: { phase: 'idle', hops: [], error: '' },   // idle | loading | ok | error
});

export function torState() { return tor; }
export function setTorState(state, statusOverride = '') {
    tor.state = state || null;
    tor.statusOverride = statusOverride || '';
    // Pre-connect there is nothing to inspect; a disconnect collapses the disclosure.
    if (!state || !state.running) tor.advancedOpen = false;
}
export function setTorLocked(locked) { tor.locked = !!locked; }
export function setTorAdvancedOpen(open) { tor.advancedOpen = !!open; }
export function setTorCircuits(circuits) { tor.circuits = circuits; }

const blocked = $state({ v: 0 });
export function blockedVersion() { return blocked.v; }
export function reloadBlockedUsers() { blocked.v++; }

// The Storage breakdown: the per-extension byte map the backend last reported.
// `v` moves on every report so the chart re-plays its slice-in animation.
const storage = $state({ distribution: null, v: 0 });
export function storageState() { return storage; }
export function setStorageDistribution(distribution) {
    storage.distribution = distribution || {};
    storage.v++;
}

// Notifications: the account's sound preferences plus the cross-platform toggles.
// `sounds` is false on mobile, where only the @everyone mute and content privacy apply.
const notif = $state({
    loaded: false, sounds: true,
    globalMute: false, muteEveryone: false,
    sound: { type: 'Default', path: null },
    privacy: 'full',
});
export function notifState() { return notif; }
export function setNotifSettings({ sounds, globalMute, muteEveryone, sound, privacy }) {
    notif.sounds = !!sounds;
    notif.globalMute = !!globalMute;
    notif.muteEveryone = !!muteEveryone;
    notif.sound = sound || { type: 'Default', path: null };
    notif.privacy = privacy || 'full';
    notif.loaded = true;
}

// Security: the encryption status the app last confirmed (the flows write their
// globals, then sync here) and the external-signer card's content.
const security = $state({
    enabled: true, type: 'pin', bioSupported: false,
    signer: null,          // null (local key) | { label, hint, npub }
    dot: '',               // '' | online | offline | connecting
});
export function securityState() { return security; }
export function setSecurity({ enabled, type, bioSupported }) {
    security.enabled = !!enabled;
    security.type = type || 'pin';
    if (bioSupported !== undefined) security.bioSupported = !!bioSupported;
}
export function setSigner(signer) { security.signer = signer || null; }
export function setSignerDot(dot) { security.dot = dot || ''; }

// Display: five toggles. The rows render from here; the app applies each one.
const display = $state({ loaded: false, imageTypes: false, chatBg: true, richComposer: true, emoticons: true, autocorrect: true });
export function displayState() { return display; }
export function setDisplaySettings(values) {
    Object.assign(display, values);
    display.loaded = true;
}

// Updates: the running build, the updater's phase and what it found. The section
// renders from here; updater.js drives the phase and owns the checks.
const updates = $state({
    version: '',           // display form of the running build
    preview: false,        // a preview build: shows the notice, changes the copy
    phase: 'idle',         // idle | checking | available | downloading | ready | error | no-updates | store
    message: '',           // status line for error / store / an available-with-hint
    progress: 0,
    newVersion: '',
    changelog: '',
    downloadLabel: 'Download Update',
    betaRow: false,        // the Beta Updates toggle is offered on this install
    beta: false,
});
export function updatesState() { return updates; }
export function setUpdates(values) { Object.assign(updates, values); }

// Network: the relay and media server lists the backend last reported.
const network = $state({ relays: [], servers: [] });
export function networkState() { return network; }
export function setNetwork({ relays, servers }) {
    if (relays) network.relays = relays;
    if (servers) network.servers = servers;
}

// Voice: the Whisper models the backend lists, the chosen one and the download in
// flight. VoiceSettings (settings.js) owns the fetches and writes here.
const voice = $state({
    supported: false,
    models: [],            // [{ model: { name, display_name, size, ram_required, supports_translate }, downloaded, downloading }]
    memoryMB: Infinity,
    recommended: '',
    selected: 'small',
    autoTranslate: false,
    autoTranscribe: false,
    loading: false,        // the model list is being fetched
    error: '',             // list fetch or download failure text
    download: null,        // { progress: '0%' } while a download runs
});
export function voiceState() { return voice; }
export function setVoice(values) { Object.assign(voice, values); }
export function setVoiceDownloadProgress(text) {
    if (voice.download) voice.download = { progress: text };
}

// The media server info dialog's learned capabilities and the relay info dialog's log.
const blossomCaps = $state({ status: 'loading', caps: [] });
export function blossomCapsState() { return blossomCaps; }
export function setBlossomCaps(status, caps) { blossomCaps.status = status; blossomCaps.caps = caps || []; }
const relayLogs = $state({ logs: [] });
export function relayLogsState() { return relayLogs; }
export function setRelayLogs(logs) { relayLogs.logs = logs || []; }
