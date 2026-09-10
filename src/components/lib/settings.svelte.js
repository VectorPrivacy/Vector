// Settings-screen state. The Tor card derives from the last TorState the
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

const blocked = $state({ seq: 0 });
export function blockedVersion() { return blocked.seq; }
export function reloadBlockedUsers() { blocked.seq++; }

// The Storage breakdown: the per-extension byte map the backend last reported.
// `seq` moves on every report so the chart re-plays its slice-in animation.
const storage = $state({ distribution: null, seq: 0 });
export function storageState() { return storage; }
export function setStorageDistribution(distribution) {
    storage.distribution = distribution || {};
    storage.seq++;
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
    seq: 0,                // moves on every sync so a cancelled flip snaps the toggle back
});
export function securityState() { return security; }
export function setSecurity({ enabled, type, bioSupported }) {
    security.enabled = !!enabled;
    security.type = type || 'pin';
    if (bioSupported !== undefined) security.bioSupported = !!bioSupported;
    security.seq++;
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
/** A relay's live status, by URL (case-insensitive). */
export function patchRelayStatus(url, status) {
    const u = (url || '').toLowerCase();
    network.relays = network.relays.map(r => r.url.toLowerCase() === u ? { ...r, status } : r);
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


// The Settings screen itself: the plain toggles and platform visibility
// the sections render from. Nested keys merge one level deep so a caller patches
// `{ storage: { clearing: true } }` without restating the rest.
const screen = $state({
    theme: 'vector',
    privacy: { webPreviews: true, stripTracking: true, sendTyping: true },
    battery: { shown: false, enabled: false, warning: false },
    storage: { galleryShown: false, galleryHidden: false, autoDownload: true, limit: 10485760, clearing: false },
    platform: { tor: true, voice: false, updates: true },
    bridges: {
        enabled: false, lines: '', saved: '',   // `saved` is the persisted text: Apply gates on a diff
        status: '', statusClass: '',            // a handler's own line and its is-ok / is-error class
        busy: false, obfs4Hint: '',             // obfs4Hint: the install hint when obfs4proxy is missing
    },
    scroll: { target: '', seq: 0 },               // a section to scroll into view on open
});
export function settingsScreen() { return screen; }
export function setSettingsScreen(patch) {
    for (const [k, v] of Object.entries(patch)) {
        if (v && typeof v === 'object' && !Array.isArray(v) && screen[k] && typeof screen[k] === 'object') Object.assign(screen[k], v);
        else screen[k] = v;
    }
}
export function requestSettingsScroll(target) { screen.scroll = { target, seq: screen.scroll.seq + 1 }; }

// Section handler bags that other modules register when they are ready (the
// updater, the relay list, voice). A section renders once its bag exists.
const handlers = $state({});
export function settingsHandlers() { return handlers; }
export function setSettingsHandlers(key, h) { Object.assign(handlers, { [key]: h }); }
