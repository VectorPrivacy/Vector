const { open } = window.__TAURI__.dialog;

let AUTO_DOWNLOAD_ENABLED = true;
let MAX_AUTO_DOWNLOAD_BYTES = 10_485_760;
/** Smallest selectable auto-download limit; also the value a migrated-off account lands on. */
const AUTO_DOWNLOAD_MIN_BYTES = 1_048_576;

/** Set of attachment IDs currently being downloaded — prevents duplicate download requests */
const downloadingAttachmentIds = new Set();


/**
 * Platform features retrieved from the backend
 * @typedef {Object} PlatformFeatures
 * @property {boolean} transcription - Whether transcriptions are enabled
 * @property {"android" | "ios" | "macos" | "windows" | "linux" | "unknown"} os - The operating system
 * @property {boolean} is_mobile - Whether the platform is mobile (Android or iOS)
 * @property {boolean} debug_mode - Whether the app is running in debug/development mode
 * @property {string|null} media_url - Localhost media server URL prefix (Android only)
 */

/** @type {PlatformFeatures} */
let platformFeatures = null;

/**
 * Returns a URL suitable for media element `src` attributes.
 * On Android, uses the localhost media server (HTTP Range support for seeking/streaming).
 * On other platforms, uses the standard Tauri asset protocol.
 * @param {string} filePath - Absolute file path on disk
 * @returns {string} URL for use in media element src
 */
/**
 * Format a TorState object (from the `tor_get_state` Tauri command) as a short
 * human-readable status line. Kept to a single line, even at the narrower
 * widths Vector uses on mobile; full detail (error messages etc.) lands in
 * console + the (i) popup.
 */
function formatTorStatus(state) {
    if (!state) return '';
    if (!state.supported) return 'Not in this build.';
    if (state.running) return 'Connected.';
    if (state.status && state.status.startsWith('bootstrapping')) {
        const pct = Number.isFinite(state.bootstrap_progress) ? state.bootstrap_progress : null;
        return pct != null ? `Bootstrapping ${pct}%…` : 'Bootstrapping…';
    }
    if (state.status && state.status.startsWith('failed')) return 'Failed to start.';
    if (state.enabled) return 'Starting…';
    return 'Disabled.';
}

/**
 * Pick the right state class for the Tor card glyph based on a TorState.
 *   tor-state-connected     → fully bootstrapped, SOCKS listener accepting
 *   tor-state-bootstrapping → service starting (or user just toggled on,
 *                             waiting for await tor_set_enabled to return)
 *   tor-state-failed        → bootstrap returned an error
 *   tor-state-disabled      → service off / not requested
 */
function torStateClass(state) {
    if (!state || !state.supported) return 'tor-state-disabled';
    if (state.running) return 'tor-state-connected';
    if (state.status && state.status.startsWith('failed')) return 'tor-state-failed';
    if (state.enabled || (state.status && state.status.startsWith('bootstrapping'))) {
        return 'tor-state-bootstrapping';
    }
    return 'tor-state-disabled';
}

/** The card derives from the last TorState; `statusOverride` is a handler's own line. */
function torApply(state, statusOverride = '') {
    // A disconnect drops the cached circuit so the next connect re-fetches fresh.
    if (!state || !state.running) _torCircuitsLoaded = false;
    VectorSvelte.setTorState(state, statusOverride);
}

let _torPollHandle = null;

/**
 * Is Tor currently in a transitional state (bootstrap in flight, or "starting"
 * between user click and the service spawning)? In these windows the toggle is
 * locked so the user can't spam it into a confused state — start/stop ops
 * aren't reentrancy-safe across rapid clicks.
 */
function isTorTransitional(state) {
    if (!state || !state.supported) return false;
    const status = state.status || '';
    if (status.startsWith('bootstrapping')) return true;
    // enabled-but-not-running with no failure = the service is mid-spawn.
    if (state.enabled && !state.running && !status.startsWith('failed')) return true;
    return false;
}

/** Cached so re-expanding the disclosure doesn't re-build a circuit unless the
 *  user explicitly hits Refresh (or Tor reconnects, which clears this flag). */
let _torCircuitsLoaded = false;
let _torCircuitsLoading = false;

/**
 * Fetch the current circuit's hops. First call (or after Refresh) actually builds
 * a circuit through Arti, which can take a few seconds — the list shows a loading
 * state meanwhile.
 */
async function loadTorCircuits(forceRefresh = false, forceNewCircuit = false) {
    if (_torCircuitsLoading) return;
    if (_torCircuitsLoaded && !forceRefresh) return;
    _torCircuitsLoading = true;
    VectorSvelte.setTorCircuits({ phase: 'loading', hops: [], error: '' });
    try {
        const hops = await invoke('tor_get_circuits', { forceNew: forceNewCircuit });
        VectorSvelte.setTorCircuits({ phase: 'ok', hops: Array.isArray(hops) ? hops : [], error: '' });
        _torCircuitsLoaded = true;
    } catch (err) {
        console.warn('[Tor] tor_get_circuits failed:', err);
        VectorSvelte.setTorCircuits({ phase: 'error', hops: [], error: String(err) });
    } finally {
        _torCircuitsLoading = false;
    }
}

/** Hydrate the bridges editor from the backend. */
async function loadTorBridges() {
    try {
        const cur = await invoke('tor_get_bridges');
        const lines = (cur && cur.lines) || '';
        VectorSvelte.setSettingsScreen({ bridges: { enabled: !!(cur && cur.enabled), lines, saved: lines, status: '', statusClass: '' } });
        refreshObfs4Banner(lines);
    } catch (e) {
        console.warn('[Tor] tor_get_bridges failed:', e);
    }
}

function _bridgesStatus(status, statusClass = '') {
    VectorSvelte.setSettingsScreen({ bridges: { status, statusClass } });
}

/**
 * The toggle is itself the apply for the on/off state: flipping it persists and
 * reconfigures at once, so Tor never keeps using bridges the user switched off.
 * Content edits still need Apply.
 */
async function setTorBridgesEnabled(enabled) {
    const b = VectorSvelte.settingsScreen().bridges;
    VectorSvelte.setSettingsScreen({ bridges: { enabled } });
    refreshObfs4Banner(b.lines);
    // On with nothing typed yet: expand and wait for Apply, skipping a wasted reconfigure.
    if (enabled && !b.lines.trim()) { _bridgesStatus('Add bridge lines, then Apply.'); return; }

    VectorSvelte.setSettingsScreen({ bridges: { busy: true } });
    VectorSvelte.setTorLocked(true);
    _bridgesStatus(enabled ? 'Enabling bridges, reconnecting…' : 'Disabling bridges, reconnecting…');
    try {
        await invoke('tor_set_bridges', { enabled, lines: b.lines });
        // The toggle persisted the text as a side effect, so Apply has nothing left to do.
        VectorSvelte.setSettingsScreen({ bridges: { saved: b.lines } });
        // tor_set_bridges already cycled relays; a forced new circuit would cycle them twice.
        try { await loadTorCircuits(true, false); } catch (_) {}
        _bridgesStatus(enabled ? 'Bridges enabled. Tor reconnected.' : 'Bridges disabled. Tor reconnected directly.', 'is-ok');
    } catch (err) {
        console.error('[Tor] tor_set_bridges (toggle) failed:', err);
        // Roll back to the backend's unchanged state; the body stays open to show the error.
        VectorSvelte.setSettingsScreen({ bridges: { enabled: !enabled } });
        _bridgesStatus(`Failed: ${err}`, 'is-error');
    } finally {
        VectorSvelte.setSettingsScreen({ bridges: { busy: false } });
        VectorSvelte.setTorLocked(false);
    }
}

/** The editor's text changed: clear a stale result line and re-check the obfs4 hint. */
function onTorBridgesInput() {
    _bridgesStatus('');
    refreshObfs4Banner(VectorSvelte.settingsScreen().bridges.lines);
}

/** Persist the editor and restart Tor on the new bridges. The main toggle locks meanwhile. */
async function applyTorBridges() {
    const b = VectorSvelte.settingsScreen().bridges;
    VectorSvelte.setSettingsScreen({ bridges: { busy: true } });
    VectorSvelte.setTorLocked(true);
    _bridgesStatus('Applying & reconnecting…');
    try {
        const res = await invoke('tor_set_bridges', { enabled: b.enabled, lines: b.lines });
        VectorSvelte.setSettingsScreen({ bridges: { saved: b.lines } });
        try { await loadTorCircuits(true, false); } catch (_) {}
        _bridgesStatus(res && res.enabled ? 'Bridges applied. Tor reconnected.' : 'Bridges saved. Tor will use them when next enabled.', 'is-ok');
    } catch (err) {
        console.error('[Tor] tor_set_bridges failed:', err);
        _bridgesStatus(`Failed: ${err}`, 'is-error');
    } finally {
        VectorSvelte.setSettingsScreen({ bridges: { busy: false } });
        VectorSvelte.setTorLocked(false);
    }
}

/**
 * The obfs4 install hint shows when the bridge lines include an obfs4 entry and
 * obfs4proxy is not on the system. Only the latest check may write: fast typing
 * fires several, and an older "missing" result must not land on newer text.
 */
let _obfs4BannerGen = 0;
async function refreshObfs4Banner(text) {
    const myGen = ++_obfs4BannerGen;
    const hasObfs4 = (text || '').split(/\r?\n/).some(l => l.trim().toLowerCase().startsWith('obfs4 '));
    const setHint = (hint) => VectorSvelte.setSettingsScreen({ bridges: { obfs4Hint: hint } });
    if (!hasObfs4) { setHint(''); return; }
    let status;
    try {
        status = await invoke('tor_check_obfs4_proxy');
    } catch (_) {
        if (myGen === _obfs4BannerGen) setHint('');
        return;
    }
    if (myGen !== _obfs4BannerGen) return;
    if (status && status.installed) { setHint(''); return; }
    const os = (platformFeatures && platformFeatures.os) || 'unknown';
    switch (os) {
        case 'macos': setHint('<code>brew install obfs4proxy</code>'); break;
        case 'linux': setHint('<code>apt install obfs4proxy</code> (or your distro\'s package manager)'); break;
        case 'windows': setHint('download from torproject.org and add to PATH'); break;
        default: setHint('install <code>obfs4proxy</code> for your platform'); break;
    }
}

/**
 * Poll `tor_get_state` every 1.5s and refresh the card until we hit a stable
 * state (running OR disabled-and-not-bootstrapping OR failed). Avoids the
 * "stuck on Starting Tor…" UX where the panel rendered before the
 * auto-start at login finished and never re-fetched. Idempotent — calling
 * twice keeps a single timer alive.
 */
function ensureTorStatePolling() {
    if (_torPollHandle) return;
    _torPollHandle = setInterval(async () => {
        try {
            const state = await invoke('tor_get_state');
            torApply(state);
            const stable = state.running
                || (!state.enabled && !(state.status || '').startsWith('bootstrapping'))
                || (state.status || '').startsWith('failed');
            if (stable) {
                clearInterval(_torPollHandle);
                _torPollHandle = null;
            }
        } catch (e) {
            console.warn('[Tor] poll failed:', e);
            clearInterval(_torPollHandle);
            _torPollHandle = null;
        }
    }, 1500);
}

function mediaUrl(filePath) {
    if (platformFeatures && platformFeatures.media_url) {
        return `${platformFeatures.media_url}/${encodeURIComponent(filePath)}`;
    }
    return convertFileSrc(filePath);
}

/**
 * Fetch platform features from the backend
 */
async function fetchPlatformFeatures() {
    platformFeatures = await invoke("get_platform_features");
    // Touch surfaces key off `.mobile` for gesture-driven affordances
    // (long-press menus, swipe-to-reply, bigger hit targets).
    document.body.classList.toggle('mobile', !!platformFeatures.is_mobile);
}

/**
 * @type {VoiceTranscriptionUI}
 */
let cTranscriber = null;

class VoiceSettings {
    constructor() {
        this.models = [];
        this.selectedModel = 'small';
    }

    get autoTranslate() { return VectorSvelte.voiceState().autoTranslate; }
    get autoTranscribe() { return VectorSvelte.voiceState().autoTranscribe; }

    async initVoiceSettings() {
        if (!platformFeatures.transcription) return;
        VectorSvelte.setSettingsHandlers('voice', {
            formatBytes,
            explain: (kind) => popupConfirm(...VOICE_EXPLAINERS[kind], true),
            setTranslate: async (on) => {
                VectorSvelte.setVoice({ autoTranslate: on });
                await saveWhisperAutoTranslate(on);
            },
            setTranscribe: async (on) => {
                VectorSvelte.setVoice({ autoTranscribe: on });
                await saveWhisperAutoTranscribe(on);
            },
            selectModel: (name) => this.setSelectedModel(name),
            download: () => this.downloadModel(this.selectedModel),
            deleteModel: () => this.deleteSelectedModel(),
            cancelDownload: () => invoke('cancel_whisper_download'),
        });
        VectorSvelte.setSettingsScreen({ platform: { voice: true } });

        // A saved model that no longer exists keeps the pick loadWhisperModels made.
        const strModelID = await loadChosenWhisperModel() || this.selectedModel;
        if (this.models.some(m => m.model.name === strModelID)) {
            this.selectedModel = strModelID;
        } else {
            await saveChosenWhisperModel(this.selectedModel);
        }
        VectorSvelte.setVoice({
            supported: true,
            selected: this.selectedModel,
            autoTranslate: await loadWhisperAutoTranslate(),
            autoTranscribe: await loadWhisperAutoTranscribe(),
        });
    }

    async loadWhisperModels() {
        VectorSvelte.setVoice({ loading: true, error: '' });
        try {
            const [deviceMemory, models] = await Promise.all([
                invoke('get_device_memory'),
                invoke('list_models'),
            ]);
            this.models = models;
            const deviceMemoryMB = deviceMemory > 0 ? deviceMemory / (1024 * 1024) : Infinity;
            const modelHierarchy = this.models
                .slice()
                .sort((a, b) => a.model.size - b.model.size)
                .map(m => m.model.name);
            const canRun = (m) => deviceMemoryMB >= (m.model.ram_required || 0);

            // Keep the selection while it is downloaded and runnable; otherwise fall back.
            const current = this.models.find(m => m.model.name === this.selectedModel);
            if (!(current && current.downloaded && canRun(current))) {
                this.selectedModel = this.findBestFallbackModel(this.selectedModel, modelHierarchy);
            }
            VectorSvelte.setVoice({
                models: this.models,
                memoryMB: deviceMemoryMB,
                recommended: this.findBestModelForDevice(deviceMemoryMB),
                selected: this.selectedModel,
                loading: false,
            });
        } catch (error) {
            console.error('Failed to load models:', error);
            VectorSvelte.setVoice({ loading: false, error: `Error: ${error.message}` });
        }
    }

    /** The best model for the device's RAM: small > base > tiny, else the smallest listed. */
    findBestModelForDevice(deviceMemoryMB) {
        const preferred = ['small', 'base', 'tiny'];
        for (const name of preferred) {
            const model = this.models.find(m => m.model.name === name);
            if (model && deviceMemoryMB >= (model.model.ram_required || 0)) {
                return name;
            }
        }
        return this.models.length > 0 ? this.models[0].model.name : 'small';
    }

    async deleteSelectedModel() {
        const modelName = this.selectedModel;
        if (!modelName) return;
        const confirmDelete = await popupConfirm(
            'Delete Model?',
            `Are you sure you want to delete the "${modelName}" model? This will free up disk space but you'll need to download it again to use it.`,
            false,
            '',
            'vector_warning.svg'
        );
        if (!confirmDelete) return;
        try {
            await invoke('delete_whisper_model', { modelName });
            await this.loadWhisperModels();
            showToast('Model Deleted');
        } catch (error) {
            console.error('Failed to Delete Model:', error);
            await popupConfirm('Deletion Failed', `Could not Delete Model: ${escapeHtml(String(error.message))}`, true, '', 'vector_warning.svg');
        }
    }

    async downloadModel(modelName) {
        const model = this.models.find(m => m.model.name === modelName);
        if (!model || model.downloaded || model.downloading) return;
        model.downloading = true;
        VectorSvelte.setVoice({ error: '', download: { progress: '0%' } });
        try {
            await invoke('download_whisper_model', { modelName });
            model.downloaded = true;
            model.downloading = false;
            VectorSvelte.setVoice({ download: null });
            await this.loadWhisperModels();
        } catch (error) {
            model.downloading = false;
            const isCancelled = String(error).includes('cancelled');
            if (!isCancelled) console.error('Download failed:', error);
            VectorSvelte.setVoice({ download: null, error: isCancelled ? '' : `Download failed: ${String(error)}` });
        }
    }

    async setSelectedModel(modelName) {
        this.selectedModel = modelName;
        VectorSvelte.setVoice({ selected: modelName });
        await saveChosenWhisperModel(modelName);
    }

    /**
     * The best fallback when the current selection is gone: the next larger downloaded
     * model, else the next smaller, else small, else the largest downloaded.
     * @param {string} deletedModel
     * @param {string[]} modelHierarchy - model names, smallest to largest
     */
    findBestFallbackModel(deletedModel, modelHierarchy) {
        const downloadedModels = this.models.filter(m => m.downloaded);
        if (downloadedModels.length === 0) return 'small';
        const has = (name) => downloadedModels.some(m => m.model.name === name);
        if (deletedModel && modelHierarchy.includes(deletedModel)) {
            const deletedIndex = modelHierarchy.indexOf(deletedModel);
            for (let i = deletedIndex + 1; i < modelHierarchy.length; i++) if (has(modelHierarchy[i])) return modelHierarchy[i];
            for (let i = deletedIndex - 1; i >= 0; i--) if (has(modelHierarchy[i])) return modelHierarchy[i];
        }
        if (has('small')) return 'small';
        for (let i = modelHierarchy.length - 1; i >= 0; i--) if (has(modelHierarchy[i])) return modelHierarchy[i];
        return 'small';
    }
}

const VOICE_EXPLAINERS = {
    model: ['Vector Voice AI Model', 'The Vector Voice AI model <b>determines the Quality of your transcriptions.</b><br><br>A larger model will provide more accurate transcriptions & translations, but require more Disk Space, Memory and CPU power to run.'],
    translate: ['Vector Voice Translations', 'Vector Voice AI can <b>automatically detect non-English languages and translate them in to English text for you.</b><br><br>You can decide whether Vector Voice transcribes in to their native spoken language, or instead translates in to English on your behalf.'],
    transcribe: ['Vector Voice Transcriptions', 'Vector Voice AI can <b>automatically transcribe incoming Voice Messages</b> for immediate reading, without needing to listen.<br><br>You can decide whether Vector Voice transcribes automatically, or if you prefer to transcribe each message explicitly.'],
};

/**
 * A GUI wrapper to ask the user for a username, and apply it both
 * in-app and on the Nostr network.
 */
async function askForUsername() {
    const strUsername = await popupConfirm('Choose a Username', 'This lets Vector users identify you easier!', false, 'New Username');
    if (!strUsername) return;

    // Display the change immediately
    const cProfile = arrProfiles.find(a => a.mine);
    const oldName = cProfile.name;
    cProfile.name = strUsername;
    renderCurrentProfile(cProfile);
    if (VectorSvelte.paneShown('profile')) renderProfileTab(cProfile);

    // Send out the metadata update
    try {
        const success = await invoke("update_profile", { name: strUsername, avatar: "", banner: "", about: "" });
        if (!success) {
            cProfile.name = oldName;
            renderCurrentProfile(cProfile);
            if (VectorSvelte.paneShown('profile')) renderProfileTab(cProfile);
            await popupConfirm('Username Update Failed!', 'Failed to broadcast profile update to the network.', true, '', 'vector_warning.svg');
        }
    } catch (e) {
        cProfile.name = oldName;
        renderCurrentProfile(cProfile);
        if (VectorSvelte.paneShown('profile')) renderProfileTab(cProfile);
        await popupConfirm('Username Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

/**
 * Set the user's "About Me" field on the Nostr network.
 * @param {string} about - The new 'About Me' text to be set for the user
 */
async function setAboutMe(about) {
    // Send out the metadata update
    try {
        await invoke("update_profile", { name: "", avatar: "", banner: "", about: about });
    } catch (e) {
        await popupConfirm('About Me Update Failed!', 'An error occurred while updating your "About Me", the change may not have committed to the network, you can re-try any time.', true, '', 'vector_warning.svg');
    }
}

/**
 * A GUI wrapper to ask the user for a status, and apply it both
 * in-app and on the Nostr network.
 */
async function askForStatus() {
    openStatusDialog(arrProfiles.find(a => a.mine));
}

/** The Status field's mini composer: the chat composer module with the
 *  emoji-only grammar (inline emoji, no markdown/mentions). Lazy singleton in
 *  the host the dialog hands over: the dialog is permanent, so the instance is too. */
let _statusComposer = null;
let _statusHost = null;
function _ensureStatusComposer() {
    if (_statusComposer) return _statusComposer;
    _statusComposer = createRichComposer(_statusHost, {
        placeholder: "What's happening?",
        emojiOnly: true,
        resolveEmoji: cmpResolvePackEmoji,
        bindEmojiImg: cmpBindEmojiImg,
    });
    return _statusComposer;
}

// The dialog's handlers belong to one open at a time; the component routes through here.
let _statusSession = null;
let _statusMounted = false;
function _ensureStatusDialog() {
    if (_statusMounted) return;
    _statusMounted = true;
    VectorSvelte.mountStatusDialog({
        h: {
            composerHost: (el) => { _statusHost = el; },
            renderPreview: (node, text) => {
                node.textContent = text || 'No status';
                if (text) {
                    twemojify(node);
                    renderCustomEmojiShortcodes(node, equippedEmojiTags());
                }
            },
            emoji: (e) => _statusSession?.emoji(e),
            save: () => _statusSession?.save(),
            clear: () => _statusSession?.clear(),
            close: () => _statusSession?.close(),
            backdrop: () => _statusSession?.backdrop(),
            key: (e) => _statusSession?.key(e),
        },
    });
    VectorSvelte.flushSync();
}

/** Open the Status dialog prefilled with the current status. Emoji come from
 *  the shared Emoji Panel in status mode (GIFs hidden); the live row renders
 *  your avatar + the exact pill other users will see. While the panel is
 *  open the card glides to the upper third so both stay fully visible. */
function openStatusDialog(cProfile) {
    const strCurrent = cProfile?.status?.title || '';
    _ensureStatusDialog();
    const input = _ensureStatusComposer();
    const dialog = VectorSvelte.statusDialog;

    const updatePreview = () => {
        // Statuses are one line, 120 chars: the composer itself has no
        // maxlength, so sanitize the model on every edit.
        const clean = input.value.replace(/\n/g, ' ').slice(0, 120);
        if (clean !== input.value) input.value = clean;
        const txt = clean.trim();
        const remaining = 120 - clean.length;
        dialog.patch({
            text: txt, empty: !txt,
            count: remaining <= 30 ? String(remaining) : '', low: remaining <= 10,
        });
    };

    const insertIntoStatus = (text) => {
        const start = input.selectionStart ?? input.value.length;
        const end = input.selectionEnd ?? start;
        const before = input.value.slice(0, start);
        const after = input.value.slice(end);
        const space = before && !/\s$/.test(before) ? ' ' : '';
        input.value = (before + space + text + after).slice(0, 120);
        const pos = Math.min((before + space + text).length, input.value.length);
        input.setSelectionRange(pos, pos);
        input.focus();
        updatePreview();
    };

    // Card position tracks the panel: glide up while it's open, recentre when
    // it closes, whichever path closed it (close, outside tap, select).
    const unwatchPanel = VectorSvelte.onPickerVisibility((open) => dialog.patch({ panelOpen: open }));

    const close = () => {
        if (dialog.closing()) return;
        unwatchPanel();
        _emojiPanelTarget = null;
        closeEmojiPanel();
        popBack('status-dialog');
        _statusSession = null;
        dialog.patch({ panelOpen: false });
        dialog.close();
    };

    _statusSession = {
        close,
        key: (e) => {
            if (e.key === 'Escape') {
                if (VectorSvelte.pickerVisible()) closeEmojiPanel();
                else close();
            }
        },
        emoji: (e) => {
            // stopPropagation: the document-level click delegate would otherwise
            // run the panel's open/close toggle against this same click.
            e.stopPropagation();
            if (VectorSvelte.pickerVisible()) closeEmojiPanel();
            else openEmojiPanelForStatus(insertIntoStatus);
        },
        save: () => { const v = input.value.trim(); close(); saveStatus(v); },
        clear: () => { close(); saveStatus(''); },
        backdrop: () => {
            // First outside tap dismisses the emoji panel (the document delegate
            // handles it); the next one dismisses the dialog.
            if (VectorSvelte.pickerVisible()) return;
            close();
        },
    };

    input.value = strCurrent;
    input.oninput = updatePreview;
    // Enter saves: statuses have no second line to go to.
    input.onkeydown = (e) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            _statusSession?.save();
        }
    };

    dialog.open({ avatarSrc: getProfileAvatarSrc(cProfile), clearHidden: !strCurrent, panelOpen: false });
    updatePreview();
    pushBack('status-dialog', close);
    // The composer sits inside the overlay; it can take focus only once that has rendered.
    VectorSvelte.flushSync();
    if (!platformFeatures.is_mobile) input.focus();
}

/** Publish a status (optimistic local echo, rollback + alert on failure). */
async function saveStatus(strStatus) {
    const cProfile = arrProfiles.find(a => a.mine);
    const oldStatus = cProfile.status.title;
    const oldTags = cProfile.status.emoji_tags;
    cProfile.status.title = strStatus;
    // Optimistic tags: the equipped-pack superset renders any :shortcode:
    // immediately; the backend's profile_update follows with the exact set.
    cProfile.status.emoji_tags = strStatus ? equippedEmojiTags() : [];
    renderCurrentProfile(cProfile);
    // Every row showing me (rosters, DM list) re-derives off the signal.
    VectorSvelte.touchProfile(cProfile.id);
    if (VectorSvelte.paneShown('profile')) renderProfileTab(cProfile);

    const rollback = () => {
        cProfile.status.title = oldStatus;
        cProfile.status.emoji_tags = oldTags;
        renderCurrentProfile(cProfile);
        VectorSvelte.touchProfile(cProfile.id);
        if (VectorSvelte.paneShown('profile')) renderProfileTab(cProfile);
    };

    try {
        const success = await invoke("update_status", { status: strStatus });
        if (!success) {
            rollback();
            await popupConfirm('Status Update Failed!', 'Failed to broadcast status update to the network.', true, '', 'vector_warning.svg');
        }
    } catch (e) {
        rollback();
        await popupConfirm('Status Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

/**
 * A GUI wrapper to ask the user for a file path.
 */
async function selectFile() {
    const file = await open({
        multiple: false,
        directory: false
        // No filters = allow all file types
    });
    return file || "";
}

/**
 * A GUI wrapper to ask the user for a folder path (desktop only).
 */
async function selectFolder() {
    const folder = await open({
        multiple: false,
        directory: true
    });
    return folder || "";
}

/**
 * Apply the theme visually by hot-swapping theme CSS files
 * @param {string} theme - The theme name, i.e: vector, chatstr
 * @param {string} mode - The theme mode, i.e: light, dark
 */
function applyTheme(theme = 'vector', mode = 'dark') {
  document.body.classList.remove('vector-theme', 'satoshi-theme', 'chatstr-theme', 'gifverse-theme', 'pivx-theme', 'cyberpunk-theme', 'monero-theme',);
  document.body.classList.add(`${theme}-theme`);
  
  domTheme.href = `/themes/${theme}/${mode}.css`;
  VectorSvelte.setSettingsScreen({ theme });
}

/**
 * Set and save the theme
 * @param {string} theme - The theme name, i.e: vector, chatstr
 * @param {string} mode - The theme mode, i.e: light, dark
 */
async function setTheme(theme = 'vector', mode = 'dark') {
  applyTheme(theme, mode);
  await saveTheme(theme);
  // Swap the pinned theme emoji pack to match the new theme.
  refreshEmojiPacksForTheme();
}

/** Confirm, then erase the local database and keys. */
async function logoutAccount() {
    // Prompt for confirmation
    const fConfirm = await popupConfirm('Going Incognito?', 'Logging out of Vector will fully erase the database, <b>ensure you have a backup of your keys before logging out!</b><br><br><b>You will permanently lose access to your Group Chats after logging out!</b><br><br>That said, would you like to continue?', false, '', 'vector_warning.svg');
    if (!fConfirm) return;

    // Begin the logout sequence. The backend now returns `Result<(), String>`
    // and refuses while an encryption migration is in flight — surface that
    // refusal as a popup instead of an uncaught rejection.
    try {
        await invoke('logout');
    } catch (e) {
        await popupConfirm('Logout failed', String(e), true);
    }
}

/** Show the account's keys in a popup with copy buttons. */
async function exportAccount() {
    try {
        // Call the backend to export keys
        const keys = await invoke('export_keys');
        
        // Create the export content with security warnings
        // Escape values to prevent XSS from malicious DB content
        const safeSeed = keys.seed_phrase ? escapeHtml(keys.seed_phrase) : '';
        const safeNsec = escapeHtml(keys.nsec);

        let exportContent = `
        <div style="text-align: center; padding: 0 8px;">
            <p style="color: var(--danger-pink); font-weight: bold; font-size: 15px; margin: 0 0 10px 0;">
            <p style="opacity: 0.75; font-size: 13px; margin: 0 0 16px 0; word-break: break-word;">These keys are your identity on Vector. There are no recovery options! If lost, your account cannot be restored. Never share them.</p>
        `;

        // Both the seed phrase and the nsec are long single-line strings.
        // We render each in its own horizontally-scrollable container with
        // `min-width: 0` on the flex child so the popup doesn't get pushed
        // wider than the viewport. The content stays on one line and the
        // user scrolls/swipes horizontally to read it.
        if (keys.seed_phrase) {
        exportContent += `
            <div style="text-align: left; padding: 0 8px; margin-bottom: 12px;">
            <p style="font-weight: bold; margin: 0 0 4px 0; text-align: center;">Seed Phrase</p>
            <div style="display: flex; align-items: center; gap: 6px; min-width: 0;">
                <p id="export-seed-value" style="overflow-x: auto; overflow-y: hidden; white-space: nowrap; background: #1a1a1a; padding: 8px 10px; border-radius: 5px; font-family: monospace; font-size: 12px; flex: 1; min-width: 0; margin: 0;">${safeSeed}</p>
                <button data-action="copy-seed" style="flex-shrink: 0; padding: 6px 10px; border-radius: 5px; cursor: pointer;">Copy</button>
            </div>
            </div>
        `;
        }

        exportContent += `
        <div style="text-align: left; padding: 0 8px;">
            <p style="font-weight: bold; margin: 0 0 4px 0; text-align: center;">Private Key (nsec)</p>
            <div style="display: flex; align-items: center; gap: 6px; min-width: 0;">
            <p id="export-nsec-value" style="overflow-x: auto; overflow-y: hidden; white-space: nowrap; background: #1a1a1a; padding: 8px 10px; border-radius: 5px; font-family: monospace; font-size: 12px; flex: 1; min-width: 0; margin: 0;">${safeNsec}</p>
            <button data-action="copy-nsec" style="flex-shrink: 0; padding: 6px 10px; border-radius: 5px; cursor: pointer;">Copy</button>
            </div>
            <p style="color: #4de0a0; font-size: 12px; margin: 8px 0 -10px 0; text-align: center;">Do Not Store on Device. Backup Offline.</p>
        </div>
        `;

        await popupConfirm('Export Account', exportContent, true, '', 'vector_warning.svg', '', null, false, {
            'copy-seed': () => navigator.clipboard.writeText(keys.seed_phrase),
            'copy-nsec': () => navigator.clipboard.writeText(keys.nsec),
        });
    } catch (error) {
        console.error('Export failed:', error);
        await popupConfirm('Export Failed', escapeHtml(error.toString()), true, '', 'vector_warning.svg');
    }
}

// Privacy Settings - Simple global variables
let fWebPreviewsEnabled = true;
let fStripTrackingEnabled = true;
let fSendTypingIndicators = true;

// Display Settings - Simple global variables
let fDisplayImageTypes = false;
// Emoticon suggestions (`:)` → 🙂, …). Read by the `:` shortcode selector; default on. Loaded
// from the DB in initSettings (runs at boot) so the value is live before Settings is ever opened.
let emoticonSuggestionsEnabled = true;
// OS autocorrect in the chat box. Default on; macOS users who find the system's
// substitutions too eager can switch it off (which is also what mobile keyboards
// key their correction behavior off).
let fAutocorrectEnabled = true;

/** Reflect the Autocorrect setting onto the chat box (the edit flow reuses the
 *  same textarea). Spellcheck underlines are left alone: the setting governs
 *  the OS *rewriting* text, not marking it. */
function applyAutocorrectSetting() {
    domChatMessageInput.setAttribute('autocorrect', fAutocorrectEnabled ? 'on' : 'off');
}

// Security Settings - Encryption state
let fEncryptionEnabled = true;
let fSecurityType = 'pin';
let fMigrationInProgress = false;
let fMigrationEncrypting = false;
let fMigrationRekeying = false;
let unlistenMigrationProgress = null;
let unlistenMigrationComplete = null;

/**
 * Get storage information from the backend
 */
async function getStorageInfo() {
    try {
        const storageData = await invoke('get_storage_info');
        return storageData;
    } catch (error) {
        console.error('Failed to get storage info:', error);
        return null;
    }
}

/**
 * Clear storage by deleting all files in the Vector directory
 */
async function clearStorage() {
    if (VectorSvelte.settingsScreen().storage.clearing) return;

    const confirmClear = await popupConfirm(
        'Clear Storage?',
        'This will delete all downloaded and sent files from Vector. This action cannot be undone.',
        false,
        '',
        'vector_warning.svg'
    );
    
    if (!confirmClear) return;
    
    VectorSvelte.setSettingsScreen({ storage: { clearing: true } });
    try {
        await invoke('clear_storage');
        // Full clear nukes the image cache too; drop the emoji memos so
        // rendered emojis re-download instead of pointing at deleted files
        reloadCachedEmojiImgs();
        return true;
    } catch (error) {
        console.error('Failed to clear storage:', error);
        await popupConfirm('Clear Failed', `Could not clear storage: ${escapeHtml(String(error.message))}`, true, '', 'vector_warning.svg');
        return false;
    } finally {
        VectorSvelte.setSettingsScreen({ storage: { clearing: false } });
    }
}

/**
 * Load the auto-download settings into their globals, migrating pre-split accounts.
 *
 * Before v0.4.1 there was a single "Auto-Download Limit" where "Off" (0 bytes) doubled as the
 * disable switch — which users didn't discover. It's now a toggle + a limit. Migration: a stored
 * limit of 0 (was Off) becomes toggle OFF with the limit reset to the minimum; any positive limit
 * becomes toggle ON keeping that value; a fresh account gets the defaults (on, 10 MB). Runs at
 * boot so the toggle is honored before Settings is ever opened.
 */
async function initAutoDownloadSettings() {
    const enabledRaw = await loadAutoDownloadEnabledRaw();
    const limitRaw = await invoke('get_sql_setting', { key: 'max_auto_download_bytes' });
    const storedLimit = (limitRaw !== null && limitRaw !== undefined) ? parseInt(limitRaw, 10) : null;

    if (enabledRaw === 'true' || enabledRaw === 'false') {
        // Already split.
        AUTO_DOWNLOAD_ENABLED = enabledRaw === 'true';
        MAX_AUTO_DOWNLOAD_BYTES = (storedLimit && storedLimit > 0) ? storedLimit : 10_485_760;
        return;
    }

    // Not yet split → migrate from the legacy single setting.
    if (storedLimit === 0) {
        AUTO_DOWNLOAD_ENABLED = false;
        MAX_AUTO_DOWNLOAD_BYTES = AUTO_DOWNLOAD_MIN_BYTES;
    } else if (storedLimit && storedLimit > 0) {
        AUTO_DOWNLOAD_ENABLED = true;
        MAX_AUTO_DOWNLOAD_BYTES = storedLimit;
    } else {
        AUTO_DOWNLOAD_ENABLED = true;
        MAX_AUTO_DOWNLOAD_BYTES = 10_485_760;
    }
    await saveAutoDownloadEnabled(AUTO_DOWNLOAD_ENABLED);
    await saveMaxAutoDownloadBytes(MAX_AUTO_DOWNLOAD_BYTES);
}

/** Refresh the Storage breakdown and reflect the auto-download and gallery values. */
async function initStorageSection() {
    const storageInfo = await getStorageInfo();
    if (storageInfo) VectorSvelte.setStorageDistribution(storageInfo.type_distribution);
    VectorSvelte.setSettingsScreen({ storage: { autoDownload: AUTO_DOWNLOAD_ENABLED, limit: MAX_AUTO_DOWNLOAD_BYTES } });

    // Hide Media from Gallery is Android only: the backend command is a no-op on
    // desktop, and the gallery concept does not apply there.
    if (platformFeatures.is_mobile) {
        let hidden = false;
        try { hidden = await invoke('get_gallery_hidden'); } catch (_) {}
        VectorSvelte.setSettingsScreen({ storage: { galleryShown: true, galleryHidden: !!hidden } });
    }
}

/** Category-aware confirmation for a storage delete; returns the user's choice. */
async function confirmStorageDelete(cat, sizeText) {
    let body;
    if (cat.key === '/ai_models') {
        body = `This will delete ${sizeText} of downloaded AI models from this device.<br><br>Voice transcription will download a model again when needed.`;
    } else if (cat.key === '/cache') {
        body = `This will delete ${sizeText} of cached avatars, banners and images.<br><br>Vector rebuilds this cache automatically over time.`;
    } else {
        body = `This will delete ${sizeText} of ${cat.noun} from this device.<br><br>You can download them again from their chats later, if they are still available.`;
    }
    return popupConfirm(cat.title, body, false, '', 'vector_warning.svg');
}

// ============================================================================
// Notification Sound Settings
// ============================================================================

const NOTIF_EXPLAINERS = {
    mute: ['Mute Notification Sounds', 'When enabled, Vector will <b>not play any notification sounds</b> for incoming messages.<br><br>You will still receive visual notifications and badges.'],
    everyone: ['Mute @everyone Pings', 'When enabled, <b>@everyone</b> mentions from group admins will <b>not bypass</b> your group mute setting.<br><br>By default, @everyone pings from admins will notify you even if the group is muted.'],
    privacy: ['Notification Content Privacy', 'Controls how much of a message shows in OS notifications (lock screen, banners).<br><br><b>Show sender and message</b>: full preview.<br><b>Hide message</b>: shows who messaged you, not what.<br><b>Hide sender and message</b>: a generic "You received a message", revealing nothing.'],
};

const DISPLAY_EXPLAINERS = {
    imageTypes: ['Display Image Types', 'When enabled, images in chat will display a <b>small badge showing the file type</b> (e.g., PNG, GIF, WEBP) in the corner.<br><br>This helps identify image formats at a glance.'],
    chatBg: ['Background Wallpaper', 'This feature enables and disables background images inside of Chats (Private & Group Chats).<br><br>Only applies to certain themes.'],
    richComposer: ['Rich Composer', 'The chat box formats <b>bold</b>, <i>italics</i>, code and links as you type.<br><br>Turn it off to use a plain text box instead. The change applies on the next app start.'],
    emoticons: ['Emoticon Suggestions', 'When enabled, text emoticons suggest the matching emoji as you type:<br><br><b>:)</b> → 🙂&nbsp;&nbsp; <b>:D</b> → 😄&nbsp;&nbsp; <b>:P</b> → 😛&nbsp;&nbsp; <b>:3</b> → 😺<br><br>Turn it off to type emoticons as plain text (e.g. <b>:3</b>) without the emoji selector getting in the way.'],
    autocorrect: ['Autocorrect', 'When enabled, your device corrects typos as you type in the chat box, using your system\'s autocorrect.<br><br>Turn it off if your system keeps "fixing" words you meant to type.'],
};

/**
 * Mount the Display section and load its toggles. Rich Composer lives in
 * localStorage: the composer is built at module scope before the settings load,
 * and it is a per-device compatibility choice.
 */
const DISPLAY_HANDLERS = {
            change: async (key, on) => {
                switch (key) {
                    case 'imageTypes':
                        fDisplayImageTypes = on;
                        await saveDisplayImageTypes(on);
                        break;
                    case 'chatBg':
                        document.body.classList.toggle('chat-bg-disabled', !on);
                        await saveChatBgEnabled(on);
                        refreshChatWallpaper();
                        break;
                    case 'richComposer':
                        localStorage.setItem('rich_composer', on ? 'true' : 'false');
                        // The input is built once at startup, so the swap needs a fresh load.
                        popupConfirm('Restart required', 'The composer changes on the next app start.', true);
                        break;
                    case 'emoticons':
                        emoticonSuggestionsEnabled = on;
                        await saveEmoticonSuggestions(on);
                        break;
                    case 'autocorrect':
                        fAutocorrectEnabled = on;
                        applyAutocorrectSetting();
                        await saveAutocorrect(on);
                        break;
                }
            },
            explain: (key) => popupConfirm(...DISPLAY_EXPLAINERS[key], true),
};

async function initDisplaySettings() {
    fDisplayImageTypes = await loadDisplayImageTypes();
    const chatBg = await loadChatBgEnabled();
    if (!chatBg) document.body.classList.add('chat-bg-disabled');
    emoticonSuggestionsEnabled = await loadEmoticonSuggestions();
    fAutocorrectEnabled = await loadAutocorrect();
    applyAutocorrectSetting();
    VectorSvelte.setDisplaySettings({
        imageTypes: fDisplayImageTypes,
        chatBg,
        richComposer: localStorage.getItem('rich_composer') !== 'false',
        emoticons: emoticonSuggestionsEnabled,
        autocorrect: fAutocorrectEnabled,
    });
}

// The enum is adjacently tagged: only Custom carries a path on the wire.
function soundWire(sound) {
    return sound.type === 'Custom' ? { type: 'Custom', path: sound.path } : { type: sound.type };
}

/**
 * Mount the Notifications section and load its state. Sound preferences live in the
 * desktop-only settings blob; the @everyone mute and content privacy are per-key
 * settings the backend reads at notify time (values: full | hide_content | hide_all).
 */
const NOTIF_HANDLERS = {
            saveSounds: ({ globalMute, muteEveryone, sound }) =>
                saveNotificationSettings({ global_mute: globalMute, mute_everyone: muteEveryone, sound: soundWire(sound) })
                    .catch((e) => console.error('Failed to save notification settings:', e)),
            saveMuteEveryone: (on) => invoke('set_sql_setting', { key: 'notif_mute_everyone', value: on ? 'true' : 'false' }),
            savePrivacy: (value) => invoke('set_sql_setting', { key: 'notif_content_privacy', value }),
            pickCustom: async () => {
                try {
                    return await selectCustomNotificationSound();
                } catch (e) {
                    if (e === 'FILE_TOO_LARGE') {
                        popupConfirm('File Too Large', 'Notification sounds must be under 1MB. Please choose a shorter audio clip.', true);
                    } else if (e === 'AUDIO_TOO_LONG') {
                        popupConfirm('Audio Too Long', 'Notification sounds must be 10 seconds or less.', true);
                    } else if (e !== 'No file selected') {
                        console.error('Failed to select custom sound:', e);
                    }
                    return null;
                }
            },
            preview: (sound) => previewNotificationSound(soundWire(sound)).catch((e) => console.error('Failed to preview sound:', e)),
            explain: (kind) => popupConfirm(...NOTIF_EXPLAINERS[kind], true),
};

async function initNotificationSettings() {
    const sounds = !!platformFeatures.notification_sounds;
    let blob = { global_mute: false, sound: { type: 'Default' }, mute_everyone: false };
    if (sounds) {
        try {
            blob = await loadNotificationSettings();
        } catch (e) {
            console.error('Failed to load notification settings:', e);
        }
    } else {
        try {
            blob.mute_everyone = (await invoke('get_sql_setting', { key: 'notif_mute_everyone' })) === 'true';
        } catch (_) { /* default off */ }
    }
    let privacy = 'full';
    try {
        const val = await invoke('get_sql_setting', { key: 'notif_content_privacy' });
        if (val === 'hide_content' || val === 'hide_all') privacy = val;
    } catch (_) { /* default full */ }
    VectorSvelte.setNotifSettings({
        sounds,
        globalMute: blob.global_mute,
        muteEveryone: blob.mute_everyone,
        sound: { type: blob.sound?.type || 'Default', path: blob.sound?.path || null },
        privacy,
    });
}

/**
 * Load and render the blocked users list in Privacy settings
 */
function loadBlockedUsersList() {
    VectorSvelte.reloadBlockedUsers();
}

/** The Tor toggle: persist the preference and start or stop the embedded service. */
async function setTorEnabled(desired) {
    VectorSvelte.setTorLocked(true);
    torApply(
        { supported: true, enabled: desired, running: false, status: desired ? 'bootstrapping' : 'disabled', bootstrap_progress: desired ? 0 : null },
        desired ? 'Bootstrapping…' : 'Disabling…',
    );
    // tor_set_enabled returns only once bootstrap completes: poll for live progress meanwhile.
    if (desired) ensureTorStatePolling();
    try {
        const state = await invoke('tor_set_enabled', { enabled: desired });
        torApply(state);
        if (state.enabled && !state.running) ensureTorStatePolling();
    } catch (err) {
        console.error('[Tor] tor_set_enabled failed:', err);
        try {
            const state = await invoke('tor_get_state');
            torApply({ ...state, status: 'failed: ' + err }, `Failed: ${err}`);
        } catch (_) { /* nothing else we can do */ }
    } finally {
        try { torApply(await invoke('tor_get_state')); } catch (_) {}
        VectorSvelte.setTorLocked(false);
    }
}

/** The Privacy toggles: keep the global the renderers read, then persist. */
async function setPrivacySetting(key, on) {
    switch (key) {
        case 'webPreviews': fWebPreviewsEnabled = on; await saveWebPreviews(on); break;
        case 'stripTracking': fStripTrackingEnabled = on; await saveStripTracking(on); break;
        case 'sendTyping': fSendTypingIndicators = on; await saveSendTypingIndicators(on); break;
    }
}

/** Copy the pre-fetched logs (clipboard writes must run inside the click). */
function copyLogs() {
    if (!window._cachedLogs) {
        showToast('No logs to copy!');
        return;
    }
    const lines = window._cachedLogs.split('\n').filter(l => l.trim()).length;
    navigator.clipboard.writeText(window._cachedLogs).then(() => {
        showToast('Copied ' + lines + ' log entries to clipboard');
    });
}

/**
 * Initialize settings on app start
 */
async function initSettings() {
    // Load privacy settings from DB (default to true)
    fWebPreviewsEnabled = await loadWebPreviews();
    fStripTrackingEnabled = await loadStripTracking();
    fSendTypingIndicators = await loadSendTypingIndicators();
    VectorSvelte.setSettingsScreen({ privacy: { webPreviews: fWebPreviewsEnabled, stripTracking: fStripTrackingEnabled, sendTyping: fSendTypingIndicators } });

    // Auto-download toggle + limit (migrates pre-split accounts). At boot so the
    // gate in message-row.js is correct before Settings is opened.
    await initAutoDownloadSettings();

    // Tor: the backend knows whether the build carries it. A transient state
    // (bootstrap mid-flight when Settings opened) polls until it settles.
    try {
        const state = await invoke('tor_get_state');
        torApply(state);
        if (state.enabled && !state.running) ensureTorStatePolling();
    } catch (e) {
        console.warn('[Tor] tor_get_state failed:', e);
    }
    await loadTorBridges();

    await initDisplaySettings();

    await initNotificationSettings();

    // Pre-fetch logs so clipboard.writeText runs synchronously on click (user gesture required)
    window._cachedLogs = '';
    invoke('get_logs').then((log) => { window._cachedLogs = log || ''; });

    // Initialize battery / background service settings (mobile only)
    if (platformFeatures.is_mobile) {
        try {
            await initBatterySettings();
        } catch (e) {
            console.error('[Battery] initBatterySettings failed:', e);
        }
    }

    // Initialize encryption settings
    await initEncryptionSettings();
}

// ============================================================================
// Encryption Settings
// ============================================================================

/**
 * Initialize encryption settings UI and event listeners
 */
async function initEncryptionSettings() {
    try {
        const status = await invoke('get_encryption_status', { npub: null });
        fEncryptionEnabled = status.enabled;
        fSecurityType = status.security_type || 'pin';
    } catch (e) {
        console.error('Failed to get encryption status:', e);
        fEncryptionEnabled = true;
    }
    syncSecurityState();

    setupMigrationEventListeners();
    await initUnlockMethodRow();
}

/** Push the flows' globals into the Security reconciler's state. */
function syncSecurityState() {
    VectorSvelte.setSecurity({ enabled: fEncryptionEnabled, type: fSecurityType });
}

/**
 * The one warning biometric-only mode ships with: removing the device's
 * screen lock destroys the hardware key that guards this device's data.
 * Everything else (new fingerprints, changed PIN) is safe and silent.
 */
async function confirmBiometricOnlyWarning() {
    return await popupConfirm(
        'Before you continue',
        'Vector will be protected by your device\'s screen lock: fingerprint, face, or device PIN.<br><br>' +
        '<b>Do not remove your phone\'s screen lock without first disabling Local Encryption in Vector</b>, ' +
        'or this device loses access to its encrypted data and you will need to sign in with your keys again.<br><br>' +
        'Changing your PIN or adding fingerprints is always safe.',
        false, '', 'vector_warning.svg'
    );
}

/** Re-sync the encryption toggle from backend truth after a state-desync refusal. */
async function resyncEncryptionToggle() {
    try {
        const st = await invoke('get_encryption_status', { npub: null });
        fEncryptionEnabled = st.enabled;
        fSecurityType = st.security_type || 'pin';
        syncSecurityState();
    } catch (e) { /* keep last known state */ }
}

/**
 * Enable Local Encryption in biometric-only mode (generated credential,
 * hardware-wrapped). Shared by the Local Encryption toggle's offer and a
 * direct tap on the Biometric Unlock row. Returns true on success.
 */
async function enableBiometricOnlyEncryption() {
    if (!(await confirmBiometricOnlyWarning())) return false;
    // Set BEFORE the invoke: encryption_migration_complete fires from inside
    // the command, and its refresh must read the new type, not the stale one.
    const prevSecurityType = fSecurityType;
    fSecurityType = 'biometric';
    showMigrationModal(true);
    try {
        await invoke('enable_encryption_biometric');
        syncSecurityState();
        return true;
    } catch (e) {
        fSecurityType = prevSecurityType;
        hideMigrationModal();
        const msg = String(e);
        if (msg.includes('already enabled') || msg.includes('already in progress')) {
            await resyncEncryptionToggle();
        } else if (!msg.includes('BIOMETRIC_CANCELLED')) {
            await popupConfirm('Encryption Failed', escapeHtml(msg), true);
        }
        return false;
    }
}

/**
 * Unlock-method row: shows which credential guards this account and switches
 * between the two MUTUALLY EXCLUSIVE modes — OS-backed (biometrics/device
 * credential) or Vector-backed (PIN/password). Never both: one credential
 * means one login path. Switching re-keys the store in memory (the same
 * engine as Change PIN), so plaintext never touches disk.
 */
async function initUnlockMethodRow() {
    let bioSupported = false;
    if (platformFeatures.os === 'android') {
        try {
            const st = await invoke('biometric_status');
            bioSupported = !!st.supported;
        } catch (e) { /* leave unsupported */ }
    }
    VectorSvelte.setSecurity({ enabled: fEncryptionEnabled, type: fSecurityType, bioSupported });
}

/** The Unlock row's button: flip between the two mutually exclusive modes. */
async function switchUnlockMethod() {
    if (fMigrationInProgress) return;
    if (fSecurityType === 'biometric') await switchToCredentialMode();
    else await switchToBiometricMode();
}

/** Vector-backed to OS-backed. The prompt happens before anything commits. */
async function switchToBiometricMode() {
    if (!(await confirmBiometricOnlyWarning())) return;
    const prev = fSecurityType;
    fSecurityType = 'biometric';
    fMigrationRekeying = true;
    showMigrationModal(true);
    try {
        await invoke('switch_to_biometric');
        syncSecurityState();
    } catch (e) {
        fSecurityType = prev;
        fMigrationRekeying = false;
        hideMigrationModal();
        const msg = String(e);
        if (!msg.includes('BIOMETRIC_CANCELLED')) {
            await popupConfirm('Could not switch', escapeHtml(msg), true);
        }
        syncSecurityState();
    }
}

/** OS-backed to Vector-backed. Needs a NEW credential, never the old one. */
async function switchToCredentialMode() {
    const result = await promptSecurityCredential(
        'Switch to PIN or Password',
        'Choose the credential that will unlock Vector from now on. There is no recovery if you forget it!'
    );
    if (!result) return;
    const prev = fSecurityType;
    fSecurityType = result.securityType;
    fMigrationRekeying = true;
    showMigrationModal(true);
    try {
        await invoke('switch_to_credential', {
            credential: result.credential,
            securityType: result.securityType,
        });
        syncSecurityState();
    } catch (e) {
        fSecurityType = prev;
        fMigrationRekeying = false;
        hideMigrationModal();
        await popupConfirm('Could not switch', escapeHtml(String(e)), true);
        syncSecurityState();
    }
}

/**
 * Handle encryption toggle change
 */
async function handleEncryptionToggleChange(desired) {
    // Block if migration running or a credential modal is already open
    if (fMigrationInProgress || VectorSvelte.credentialState().open) {
        syncSecurityState();
        return;
    }
    if (desired) await handleEnableEncryption();
    else await handleDisableEncryption();
}

/** Enable encryption; any exit before the backend commits snaps the toggle back. */
async function handleEnableEncryption() {
    // Android with capable hardware: offer biometric-only mode first — a
    // generated credential nobody knows, unlocked solely by the OS.
    if (platformFeatures.os === 'android') {
        let bs = null;
        try { bs = await invoke('biometric_status'); } catch (e) {}
        if (bs && bs.supported) {
            const useBio = await popupConfirm(
                'Protect with Biometrics?',
                'Unlock Vector with your fingerprint, face, or device PIN. No Vector PIN to remember.<br><br>Choose Cancel to set a PIN or Password instead.',
                false, '', 'vector-check.svg', '', 'Use Biometrics'
            );
            if (useBio) {
                const ok = await enableBiometricOnlyEncryption();
                if (!ok) syncSecurityState();
                return;
            }
        }
    }

    // Ask user to choose security type
    const result = await promptSecurityCredential('Set Up Encryption', 'Choose how to protect your local data. There is no recovery if you forget!');

    if (!result) {
        syncSecurityState();
        return;
    }

    // Show migration modal and start encryption
    showMigrationModal(true);

    try {
        fSecurityType = result.securityType;
        await invoke('enable_encryption', { credential: result.credential, securityType: result.securityType });
        syncSecurityState();
    } catch (e) {
        hideMigrationModal();
        await popupConfirm(
            'Encryption Failed',
            `Failed to enable encryption: ${escapeHtml(String(e))}`,
            true,
            '',
            'vector_warning.svg'
        );
        syncSecurityState();
    }
}

// ==========================================================================
// Credential Modal API
// ==========================================================================
// A reusable modal matching the migration modal design, with proper PIN row
// and password input. Modes: 'pin', 'password', 'type-select'.

/**
 * Show the credential modal in a specific mode.
 * @param {Object} opts
 * @param {'pin'|'password'|'type-select'} opts.mode - Which input to show
 * @param {string} opts.title - Modal title
 * @param {string} opts.subtitle - Subtitle / description text
 * @param {string} [opts.confirmText='Confirm'] - Text for the action button
 * @returns {Promise<string|null>} - credential string, type string, or null if cancelled
 */
function showCredentialModal({ mode, title, subtitle, confirmText = 'Confirm' }) {
    return new Promise((resolve) => {
        let done = false;
        const finish = (value) => {
            if (done) return;
            done = true;
            VectorSvelte.closeCredentialDialog();
            resolve(value);
        };
        VectorSvelte.openCredentialDialog(
            { mode, title, subtitle, confirmText: mode === 'type-select' ? (confirmText || 'Continue') : confirmText },
            { cancel: () => finish(null), submit: (value) => finish(value) },
        );
    });
}

/**
 * Prompt user to choose a security type and enter + confirm a credential.
 * Uses the custom credential modal for all phases.
 * @param {string} title - Overall flow title
 * @param {string} message - Description for the type selection phase
 * @returns {Promise<{credential: string, securityType: string}|null>}
 */
async function promptSecurityCredential(title, message) {
    // Phase 1: Choose security type
    const secType = await showCredentialModal({
        mode: 'type-select',
        title,
        subtitle: message,
        confirmText: 'Continue',
    });
    if (!secType) return null;

    const label = secType === 'pin' ? 'PIN' : 'Password';
    const defaultSubtitle = secType === 'pin' ? 'Enter a 6-digit PIN.' : 'Enter a password (4+ characters).';
    let entrySubtitle = defaultSubtitle;

    // Loop: enter + confirm, retry inline on mismatch or too-short
    while (true) {
        // Phase 2: Enter credential
        const credential = await showCredentialModal({
            mode: secType,
            title: `Create ${label}`,
            subtitle: entrySubtitle,
        });
        if (!credential) return null;

        // Password length validation
        if (secType === 'password' && credential.length < 4) {
            entrySubtitle = 'Too short! Must be at least 4 characters.';
            continue;
        }

        // Phase 3: Confirm credential
        const confirmed = await showCredentialModal({
            mode: secType,
            title: `Confirm ${label}`,
            subtitle: `Re-enter your ${label.toLowerCase()}.`,
        });
        if (!confirmed) return null;

        if (confirmed !== credential) {
            entrySubtitle = `${label}s didn't match. Try again.`;
            continue;
        }

        return { credential, securityType: secType };
    }
}

/**
 * Handle changing PIN/Password (re-keying)
 */
async function handleChangeCredential() {
    if (fMigrationInProgress || VectorSvelte.credentialState().open) return;

    // Step 1: Ask for current credential and verify it
    const currentLabel = fSecurityType === 'password' ? 'Password' : 'PIN';
    let oldCredential = null;
    let subtitle = `Please enter your current ${currentLabel.toLowerCase()} to continue.`;
    while (true) {
        const entered = await showCredentialModal({
            mode: fSecurityType,
            title: `Enter Current ${currentLabel}`,
            subtitle,
        });
        if (!entered) return;

        // The modal holds, controls gone, while Argon2 hashes.
        VectorSvelte.openCredentialDialog(
            { mode: 'validating', title: `Validating ${currentLabel}...`, subtitle: 'Please wait', subtitleGradient: true },
            { cancel: () => {}, submit: () => {} },
        );

        // Verify the credential without exposing key material over IPC
        try {
            await invoke('verify_credential', { credential: entered });
            oldCredential = entered;
            VectorSvelte.closeCredentialDialog();
            break;
        } catch (e) {
            VectorSvelte.closeCredentialDialog();
            subtitle = `Incorrect ${currentLabel.toLowerCase()}, try again.`;
        }
    }

    // Step 2: Choose new security type + credential
    const result = await promptSecurityCredential(
        `Change ${currentLabel}`,
        `Choose a new security type and credential.`
    );
    if (!result) return;

    // Step 3: Re-key
    fMigrationRekeying = true;
    showMigrationModal(true);

    try {
        await invoke('rekey_encryption', {
            oldCredential,
            newCredential: result.credential,
            securityType: result.securityType,
        });
        fSecurityType = result.securityType;
        syncSecurityState();
    } catch (e) {
        hideMigrationModal();
        fMigrationRekeying = false;
        await popupConfirm(
            'Re-keying Failed',
            `Failed to change credential: ${escapeHtml(String(e))}`,
            true,
            '',
            'vector_warning.svg'
        );
    }
}

/** Disable encryption; a cancel or failure snaps the toggle back. */
async function handleDisableEncryption() {
    // Confirm with user
    const confirmed = await popupConfirm(
        'Disable Encryption?',
        'This will decrypt all your local data and remove PIN protection.<br><br>' +
        '<b>Your messages and keys will be stored in plain text.</b><br><br>' +
        'This is useful for faster app startup on personal devices, but reduces security if your device is lost or stolen.',
        false,
        '',
        'vector_warning.svg'
    );

    if (!confirmed) {
        syncSecurityState();
        return;
    }

    // Show migration modal and start decryption
    showMigrationModal(false);

    try {
        await invoke('disable_encryption');
        // Success - migration complete event will hide modal
    } catch (e) {
        hideMigrationModal();
        await popupConfirm(
            'Decryption Failed',
            `Failed to disable encryption: ${escapeHtml(String(e))}`,
            true,
            '',
            'vector_warning.svg'
        );
        syncSecurityState();
    }
}

/**
 * Set up Tauri event listeners for migration progress
 */
async function setupMigrationEventListeners() {
    const { listen } = window.__TAURI__.event;

    // Clean up previous listeners to prevent stacking on re-init
    if (unlistenMigrationProgress) { unlistenMigrationProgress(); unlistenMigrationProgress = null; }
    if (unlistenMigrationComplete) { unlistenMigrationComplete(); unlistenMigrationComplete = null; }

    // Listen for migration progress updates
    unlistenMigrationProgress = await listen('encryption_migration_progress', (event) => {
        const { total, completed, phase } = event.payload;
        updateMigrationProgress(total, completed, phase);
    });

    // Listen for migration completion
    unlistenMigrationComplete = await listen('encryption_migration_complete', () => {
        const wasEncrypting = fMigrationEncrypting;
        const wasRekeying = fMigrationRekeying;
        hideMigrationModal();
        fMigrationRekeying = false;
        // A re-key keeps encryption on; otherwise the migration's direction is the new state.
        if (!wasRekeying) fEncryptionEnabled = wasEncrypting;
        syncSecurityState();
        showToast(wasRekeying
            ? (fSecurityType === 'biometric' ? 'Now unlocking with biometrics' : 'Unlock method updated')
            : wasEncrypting ? 'Encryption enabled' : 'Encryption disabled');
    });
}

/**
 * Show the migration progress modal
 * @param {boolean} encrypting - True if encrypting, false if decrypting
 */
function showMigrationModal(encrypting) {
    fMigrationInProgress = true;
    fMigrationEncrypting = encrypting;
    VectorSvelte.showMigration(fMigrationRekeying ? 'Changing Credential' : encrypting ? 'Enabling Encryption' : 'Disabling Encryption');
}

/**
 * Hide the migration progress modal
 */
function hideMigrationModal() {
    fMigrationInProgress = false;
    VectorSvelte.hideMigration();
}

/**
 * Update the migration progress display
 * @param {number} total - Total items to process
 * @param {number} completed - Items completed
 * @param {string} phase - Current phase description
 */
// ============================================================================
// Battery & Background Service Settings (mobile only)
// ============================================================================

/** Reflect the background service and battery exemption into the Battery section. */
async function refreshBatterySection() {
    const enabled = await invoke('get_background_service_enabled');
    const exempt = enabled ? await invoke('check_battery_optimized') : true;
    VectorSvelte.setSettingsScreen({ battery: { shown: true, enabled, warning: enabled && !exempt } });
}

async function initBatterySettings() {
    await refreshBatterySection();
}

/** Tapping the warning opens the system dialog; re-check once the app is back. */
async function batteryWarningTap() {
    await invoke('request_battery_optimization');
    await waitForVisibility();
    const nowExempt = await invoke('check_battery_optimized');
    VectorSvelte.setSettingsScreen({ battery: { warning: !nowExempt } });
}

/** The Run in Background toggle. Turning on needs the battery exemption first. */
async function setBackgroundService(on) {
    if (on) {
        const exempt = await invoke('check_battery_optimized');
        if (!exempt) {
            await invoke('request_battery_optimization');
            await waitForVisibility();
            const nowExempt = await invoke('check_battery_optimized');
            if (!nowExempt) {
                VectorSvelte.setSettingsScreen({ battery: { enabled: false, warning: false } });
                popupConfirm('Battery Optimization', 'Battery optimization must be disabled for reliable background notifications.', true, '', 'vector_warning.svg');
                return;
            }
        }
        await invoke('set_background_service_enabled', { enabled: true });
        VectorSvelte.setSettingsScreen({ battery: { enabled: true, warning: false } });
    } else {
        await invoke('set_background_service_enabled', { enabled: false });
        VectorSvelte.setSettingsScreen({ battery: { enabled: false, warning: false } });
    }
}

/**
 * Show a one-time prompt asking the user to enable the background service
 * and battery optimization. Called once after first login.
 */
async function showBackgroundServicePrompt() {
    const confirmed = await popupConfirm(
        'Background Notifications',
        'Vector can run in the background to deliver <b>instant notifications</b>.<br><br>This requires disabling battery optimization for Vector so Android doesn\'t kill the service.<br><br>You can change this later in Settings.',
        false, '', 'vector_warning.svg'
    );

    if (confirmed) {
        // User chose "Enable" — explicitly persist enabled=true and request battery exemption
        await invoke('set_background_service_enabled', { enabled: true });
        await invoke('request_battery_optimization');
        // Best-effort wait for user to return from system dialog (non-blocking)
        await waitForVisibility();
    } else {
        // User chose "Not Now" — disable the background service
        await invoke('set_background_service_enabled', { enabled: false });
    }

    await refreshBatterySection();
}

/**
 * Returns a promise that resolves when the page becomes visible again
 * after being hidden (e.g. user returns from system settings dialog).
 * If the page never becomes hidden within 1s, resolves immediately
 * (dialog may have been suppressed or instant-dismissed).
 */
function waitForVisibility() {
    return new Promise(resolve => {
        let hidden = document.hidden;
        const handler = () => {
            if (document.hidden) {
                hidden = true;
            } else if (hidden) {
                // Page was hidden and is now visible again
                document.removeEventListener('visibilitychange', handler);
                setTimeout(resolve, 300);
            }
        };
        document.addEventListener('visibilitychange', handler);
        // Timeout: if the dialog never hid us, resolve after 1s
        setTimeout(() => {
            document.removeEventListener('visibilitychange', handler);
            resolve();
        }, 1000);
    });
}

function updateMigrationProgress(total, completed, phase) {
    const percentage = total > 0 ? Math.round((completed / total) * 100) : 0;
    let phaseText = phase;
    if (phase === 'decrypting') {
        phaseText = `Decrypting messages... ${completed.toLocaleString()} / ${total.toLocaleString()}`;
    } else if (phase === 'encrypting') {
        phaseText = `Encrypting messages... ${completed.toLocaleString()} / ${total.toLocaleString()}`;
    } else if (phase === 'rekeying') {
        phaseText = `Re-encrypting messages... ${completed.toLocaleString()} / ${total.toLocaleString()}`;
    } else if (phase === 'finalizing') {
        phaseText = 'Finalizing...';
    }
    VectorSvelte.setMigrationProgress(phaseText, percentage);
}

// Help prompts: one explainer per info icon. A function body resolves at click
// time (the change-PIN wording follows the security type).
const SETTINGS_HELP = {
    stripTracking: ['Strip Tracking Markers', 'When enabled, Vector will <b>automatically remove tracking markers</b> from URLs before displaying or sending them.<br><br>This helps reduce your footprint and enhances your privacy with no loss in functionality, only disable if you know what you\'re doing.'],
    sendTyping: ['Send Typing Indicators', 'When enabled, Vector will <b>notify your contacts when you are typing</b> a message to them.<br><br>Disable this if you prefer to type without others knowing you are composing a message.'],
    // Trademark notice + non-endorsement disclaimer included per the
    // Tor Project's trademark policy (https://www.torproject.org/about/trademark/).
    tor: ['Route traffic through Tor',
        'When enabled, Vector routes <b>all TCP traffic</b> (Nostr relays, Blossom uploads, link previews, image fetches) through the Tor network using an embedded Arti client.<br><br>'
        + 'This hides your IP address from relays and remote servers, at the cost of slower connections (Tor circuits add latency).<br><br>'
        + '<small style="opacity: 0.6;">Tor and the Tor logo are trademarks of The Tor Project; all rights reserved. More information at <b>torproject.org</b>. Vector is not endorsed or sponsored by, or affiliated with, The Tor Project.</small>'],
    battery: ['Run in Background', 'When enabled, Vector runs a <b>background service</b> to keep your connection alive and deliver <b>instant notifications</b>.<br><br>This requires disabling Android\'s battery optimization for Vector, otherwise the system may kill the service and delay or prevent notifications.'],
    gallery: ['Hide Media from Gallery', 'By default, photos and videos you receive in Vector appear in your phone\'s Gallery app.<br><br>When enabled, Vector hides its media from the Gallery (and other apps). Existing media is removed from the Gallery too. Your files stay on the device and remain visible inside Vector.'],
    autoDownload: ['Auto-Download Media', 'When enabled, Vector automatically downloads incoming photos, videos, voice messages and files (up to the size limit below).<br><br>Turn this off to keep attachments as previews and download them by hand, one at a time.'],
    autoDownloadLimit: ['Auto-Download Limit', 'The largest attachment size Vector will fetch automatically.<br><br>Anything above this waits for you to tap Download. Only applies while Auto-Download Media is on.'],
    clearStorage: ['Clear Storage', 'Deletes the downloaded and sent files Vector has cached on this device, to free up space.<br><br>Your messages stay. Attachments can be downloaded again later if they are still available from their sender.'],
    encryption: ['Local Encryption', 'Protects your messages and keys if your device is lost or stolen.<br><br>Disabling speeds up app launch but stores data in plain text.', 'vector-check.svg'],
    unlockMethod: ['Unlock Method',
        'Your local data is always encrypted. This chooses what unlocks it:<br><br>'
        + '<b>Biometrics</b> uses your device security (fingerprint, face, or device PIN) with a key held in hardware.<br><br>'
        + '<b>PIN or Password</b> uses a credential you type and remember.', 'vector-check.svg'],
    exportAccount: ['Export Account', 'Export Account will display a backup of your encryption keys. Keep it safe to restore your account later.'],
    changePin: () => fSecurityType === 'password'
        ? ['Change Password', 'Your password encrypts all local data including messages, keys, and secrets stored on your device. Resetting it will re-encrypt everything with your new password.']
        : ['Change PIN', 'Your PIN encrypts all local data including messages, keys, and secrets stored on your device. Resetting it will re-encrypt everything with your new PIN.'],
    crashLog: ['Logs', 'Copies error logs and crash details to your clipboard.<br><br>Share with developers when reporting bugs to help diagnose issues.'],
    logout: ['Logout', 'Logout will erase the local database and remove all stored keys. You will lose access to group chats unless you have a backup.'],
    // Rendered against the Tor preference: with Tor on, every preview fetch is forced
    // through Tor (or blackholes during bootstrap), so there is no clearnet leak path.
    webPreviews: async () => {
        let torEnabled = false;
        try { torEnabled = !!(await invoke('tor_get_state'))?.enabled; } catch (_) { /* default warning */ }
        return ['Web Previews', torEnabled
            ? 'When enabled, Vector will <b>automatically fetch and display previews</b> for links shared in messages.<br><br>You have <b>Tor enabled</b>, so preview fetches route through the Tor network. Your IP address stays hidden from the linked sites.'
            : 'When enabled, Vector will <b>automatically fetch and display previews</b> for links shared in messages.<br><br>This may expose your IP address to the linked sites. <b>Use Tor</b> (Privacy, Route traffic through Tor) <b>or a VPN</b> if that\'s a concern.'];
    },
};

// Footer and attribution links. obfs4 is the realistic anti-censorship transport;
// vanilla bridges are essentially abandoned by The Tor Project.
const SETTINGS_LINKS = {
    torAttribution: 'https://torproject.org',
    bridges: 'https://bridges.torproject.org/bridges/en?transport=obfs4',
    donate: 'https://vector-privacy.gitbook.io/vector-privacy/vector-messenger/more/donations',
    gitbook: 'https://docs.vectorapp.io',
    privacy: 'https://vectorapp.io/privacy-policy',
};

async function showSettingsHelp(key) {
    const entry = SETTINGS_HELP[key];
    const [title, body, icon] = typeof entry === 'function' ? await entry() : entry;
    popupConfirm(title, body, true, '', icon || '');
}

/** Re-authorize the external signer: a direct Amber intent for NIP-55, the bunker form otherwise. */
async function reauthorizeSigner() {
    const nip55 = await invoke('get_nip55_status').catch(() => null);
    if (nip55) {
        try {
            await invoke('reauthorize_nip55');
            showToast('Signer re-authorized.');
            refreshRemoteSignerCard();
        } catch (err) {
            popupConfirm(String(err), '', true, '', 'vector_warning.svg');
        }
        return;
    }
    if (typeof window.showBunkerForm === 'function') window.showBunkerForm('reauth');
}

// The Settings screen's helpers. Section owners that come up later (the updater,
// the relay list, voice) register their own bags on the store.
const SETTINGS_HELPERS = {
    help: showSettingsHelp,
    openLink: (key) => openUrl(SETTINGS_LINKS[key]),
    setTheme: async (theme) => {
        await setTheme(theme);
        // The breakdown's slice colours follow the theme.
        initStorageSection();
    },
    setPrivacy: setPrivacySetting,
    tor: {
        stateClass: torStateClass, formatStatus: formatTorStatus, isTransitional: isTorTransitional,
        setTorEnabled,
        injectGlyph: injectTorGlyph,
        help: showSettingsHelp,
        openLink: (key) => openUrl(SETTINGS_LINKS[key]),
        toggleAdvanced: () => {
            const willOpen = !VectorSvelte.torState().advancedOpen;
            VectorSvelte.setTorAdvancedOpen(willOpen);
            if (willOpen) loadTorCircuits(false);
        },
        // A new circuit rotates the isolation token AND cycles relay sockets; the
        // bridges flows only refresh the display.
        newCircuit: () => loadTorCircuits(true, true),
        setBridgesEnabled: setTorBridgesEnabled,
        bridgesInput: onTorBridgesInput,
        applyBridges: applyTorBridges,
    },
    blocked: {
        load: () => invoke('get_blocked_users'),
        getProfile: (npub) => getProfile(npub),
        getProfileAvatarSrc: (p) => getProfileAvatarSrc(p),
        confirmUnblock: (p) => popupConfirm('Unblock User', `Are you sure you want to unblock ${escapeHtml(getName(p))}?`),
        unblock: async (npub) => {
            await invoke('unblock_user', { npub });
            showToast('User unblocked');
            profileChanged(npub);
        },
        reload: () => VectorSvelte.reloadBlockedUsers(),
    },
    display: DISPLAY_HANDLERS,
    notif: NOTIF_HANDLERS,
    storageDonut: {
        // misc.js loads after this file: resolve at call time.
        formatBytes: (n) => formatBytes(n),
        confirmDelete: confirmStorageDelete,
        deleteCategory: (category, exts) => invoke('clear_storage_category', { category, exts }),
        refresh: () => initStorageSection(),
        // The emoji memos and any rendered <img>s point at the deleted cache files
        onCacheCleared: () => reloadCachedEmojiImgs(),
        toast: (msg) => showToast(msg),
        deleteFailed: (e) => popupConfirm('Delete Failed', `Could not delete: ${escapeHtml(String(e))}`, true, '', 'vector_warning.svg'),
    },
    setGalleryHidden: async (hidden) => {
        VectorSvelte.setSettingsScreen({ storage: { galleryHidden: hidden } });
        try {
            await invoke('set_gallery_hidden', { hidden });
        } catch (err) {
            console.error('set_gallery_hidden failed:', err);
        }
    },
    setAutoDownload: async (on) => {
        AUTO_DOWNLOAD_ENABLED = on;
        await saveAutoDownloadEnabled(on);
    },
    setAutoDownloadLimit: async (bytes) => {
        MAX_AUTO_DOWNLOAD_BYTES = bytes;
        VectorSvelte.setSettingsScreen({ storage: { limit: bytes } });
        await saveMaxAutoDownloadBytes(bytes);
    },
    clearStorage: async () => {
        if (await clearStorage()) initStorageSection();
    },
    setBackgroundService,
    batteryWarningTap,
    security: {
        toggleEncryption: handleEncryptionToggleChange,
        changeCredential: handleChangeCredential,
        switchUnlock: switchUnlockMethod,
        reauthorize: reauthorizeSigner,
        exportAccount,
        help: showSettingsHelp,
    },
    copyLogs,
    logout: logoutAccount,
};

// Mounted once every script is in: the sections call helpers from files that load later.
document.addEventListener('DOMContentLoaded', () => {
    VectorSvelte.setScreen('settings', { h: SETTINGS_HELPERS });
    VectorSvelte.mountCredentialModals();
}, { once: true });
