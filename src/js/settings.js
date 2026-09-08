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

/**
 * Wire the Bridges section under Tor → Advanced. Toggle reveals the textarea;
 * Apply persists via tor_set_bridges (which also restarts Tor if it's
 * currently running so the new bridges take effect immediately).
 */
async function initTorBridgesUI() {
    const toggle = document.getElementById('tor-bridges-toggle');
    const body = document.getElementById('tor-bridges-body');
    const textarea = document.getElementById('tor-bridges-textarea');
    const applyBtn = document.getElementById('tor-bridges-apply');
    const statusEl = document.getElementById('tor-bridges-status');
    const link = document.getElementById('tor-bridges-link');
    if (!toggle || !body || !textarea || !applyBtn || !statusEl) return;

    // Hydrate from backend.
    // Track the persisted lines so Apply can be gated on a real diff.
    let savedLines = '';
    const isDirty = () => textarea.value !== savedLines;
    const refreshApplyEnabled = () => {
        applyBtn.disabled = !isDirty();
    };

    try {
        const cur = await invoke('tor_get_bridges');
        toggle.checked = !!(cur && cur.enabled);
        textarea.value = (cur && cur.lines) || '';
        savedLines = textarea.value;
        body.style.display = toggle.checked ? '' : 'none';
        renderBridgesStatus(statusEl, textarea.value);
        refreshApplyEnabled();
    } catch (e) {
        console.warn('[Tor] tor_get_bridges failed:', e);
        refreshApplyEnabled();
    }

    toggle.addEventListener('change', async () => {
        body.style.display = toggle.checked ? '' : 'none';
        refreshObfs4Banner(textarea.value);

        // Toggling ON with no lines yet: just expand the UI and wait for
        // the user to enter bridges + hit Apply. Skipping persist/reconfigure
        // here avoids a wasted Tor reconfigure cycle (enabled-but-no-lines
        // resolves to direct anyway), and avoids any "empty obfs4" attempt.
        if (toggle.checked && !textarea.value.trim()) {
            statusEl.textContent = 'Add bridge lines, then Apply.';
            statusEl.classList.remove('is-error', 'is-ok');
            refreshApplyEnabled();
            return;
        }

        // The toggle is itself the apply for the on/off state. Without this,
        // flipping the toggle off would leave the saved-and-running config
        // untouched (Tor would keep using bridges until the user found and
        // hit Apply). Persist + reconfigure immediately. The textarea still
        // requires a separate Apply for content edits.
        toggle.disabled = true;
        VectorSvelte.setTorLocked(true);
        statusEl.textContent = toggle.checked
            ? 'Enabling bridges, reconnecting…'
            : 'Disabling bridges, reconnecting…';
        statusEl.classList.remove('is-error', 'is-ok');
        try {
            await invoke('tor_set_bridges', {
                enabled: !!toggle.checked,
                lines: textarea.value,
            });
            // Toggle persists the textarea content as a side effect; the
            // saved baseline now matches the textarea, so Apply has nothing
            // to do. Sync.
            savedLines = textarea.value;
            // Refresh the displayed circuit, but DON'T pass forceNewCircuit:
            // tor_set_bridges already cycled relays once. Passing
            // force_new=true here would rotate the isolation token and
            // cycle sockets a second time.
            try { await loadTorCircuits(true, false); } catch (_) {}
            statusEl.textContent = toggle.checked
                ? 'Bridges enabled. Tor reconnected.'
                : 'Bridges disabled. Tor reconnected directly.';
            statusEl.classList.add('is-ok');
        } catch (err) {
            console.error('[Tor] tor_set_bridges (toggle) failed:', err);
            // Roll the toggle back so the UI matches the actual (unchanged)
            // backend state. KEEP the body visible regardless so the user
            // can see the error message + the obfs4 banner that explains
            // why — the status element lives inside the body, so hiding
            // the body would silence the failure entirely.
            toggle.checked = !toggle.checked;
            body.style.display = '';
            statusEl.textContent = `Failed: ${err}`;
            statusEl.classList.add('is-error');
        } finally {
            toggle.disabled = false;
            VectorSvelte.setTorLocked(false);
            refreshApplyEnabled();
        }
    });
    textarea.addEventListener('input', () => {
        renderBridgesStatus(statusEl, textarea.value);
        refreshObfs4Banner(textarea.value);
        refreshApplyEnabled();
    });
    // Initial banner state.
    refreshObfs4Banner(textarea.value);

    // bridges.torproject.org link → external open via Tauri (matches the
    // Tor logo attribution link's pattern in main.js).
    if (link) {
        link.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            // obfs4 is the realistic anti-censorship transport; vanilla
            // bridges are essentially abandoned by The Tor Project.
            openUrl('https://bridges.torproject.org/bridges/en?transport=obfs4');
        };
    }

    applyBtn.addEventListener('click', async () => {
        applyBtn.disabled = true;
        textarea.disabled = true;
        toggle.disabled = true;
        // Lock the main Tor toggle too so the user can't rip the rug out mid-restart.
        VectorSvelte.setTorLocked(true);
        statusEl.textContent = 'Applying & reconnecting…';
        statusEl.classList.remove('is-error', 'is-ok');
        try {
            const res = await invoke('tor_set_bridges', {
                enabled: !!toggle.checked,
                lines: textarea.value,
            });
            // Update the saved baseline so the Apply button gates back to
            // disabled until the user types another change.
            savedLines = textarea.value;
            // Re-render circuit display only — tor_set_bridges already cycled
            // relays. Don't pass forceNewCircuit or we'd rotate the isolation
            // token + cycle sockets a second time.
            try { await loadTorCircuits(true, false); } catch (_) {}
            statusEl.textContent = res && res.enabled
                ? 'Bridges applied. Tor reconnected.'
                : 'Bridges saved. Tor will use them when next enabled.';
            statusEl.classList.add('is-ok');
        } catch (err) {
            console.error('[Tor] tor_set_bridges failed:', err);
            statusEl.textContent = `Failed: ${err}`;
            statusEl.classList.add('is-error');
        } finally {
            textarea.disabled = false;
            toggle.disabled = false;
            VectorSvelte.setTorLocked(false);
            // Re-evaluate Apply against the (possibly newly-saved) baseline
            // rather than blindly enabling it.
            refreshApplyEnabled();
        }
    });
}

/**
 * Show the obfs4-needs-install banner inline when (a) the user's bridge
 * lines include any obfs4 entry AND (b) `obfs4proxy` isn't detected on the
 * system. Otherwise hide it. Renders a platform-tailored install command so
 * the user can copy-paste-fix instead of guessing.
 */
let _obfs4BannerGen = 0;
async function refreshObfs4Banner(text) {
    const banner = document.getElementById('tor-obfs4-banner');
    const msg = document.getElementById('tor-obfs4-banner-msg');
    if (!banner || !msg) return;

    // Stamp this invocation. Fast typing can fire many parallel checks; only
    // the most recent one is allowed to mutate the DOM. Otherwise an older
    // "obfs4 + missing" check resolving after a newer "no obfs4" check would
    // re-show the banner against an empty textarea.
    const myGen = ++_obfs4BannerGen;

    const lines = (text || '').split(/\r?\n/);
    const hasObfs4 = lines.some(l => l.trim().toLowerCase().startsWith('obfs4 '));
    if (!hasObfs4) {
        banner.style.display = 'none';
        return;
    }
    let status;
    try {
        status = await invoke('tor_check_obfs4_proxy');
    } catch (_) {
        if (myGen !== _obfs4BannerGen) return;
        banner.style.display = 'none';
        return;
    }
    if (myGen !== _obfs4BannerGen) return;
    if (status && status.installed) {
        banner.style.display = 'none';
        return;
    }

    // Platform-specific install hint.
    const os = (platformFeatures && platformFeatures.os) || 'unknown';
    let hint;
    switch (os) {
        case 'macos':
            hint = '<code>brew install obfs4proxy</code>';
            break;
        case 'linux':
            hint = '<code>apt install obfs4proxy</code> (or your distro\'s package manager)';
            break;
        case 'windows':
            hint = 'download from torproject.org and add to PATH';
            break;
        default:
            hint = 'install <code>obfs4proxy</code> for your platform';
            break;
    }
    msg.innerHTML = `obfs4 bridges need <code>obfs4proxy</code> installed: ${hint}. Apply will fail until it\'s available.`;
    banner.style.display = '';
}

/** Show a tiny "N bridges configured" / "0 bridges" line under the textarea. */
function renderBridgesStatus(el, text) {
    const lines = (text || '').split(/\r?\n/).map(l => l.trim()).filter(Boolean);
    el.classList.remove('is-error', 'is-ok');
    if (lines.length === 0) {
        el.textContent = 'No bridges configured.';
    } else {
        el.textContent = `${lines.length} bridge${lines.length === 1 ? '' : 's'} configured`;
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
        const voiceSection = document.getElementById('settings-voice');
        if (!voiceSection) return;
        if (!platformFeatures.transcription) {
            voiceSection.style.display = 'none';
            return;
        }
        voiceSection.style.display = 'block';

        VectorSvelte.mountVoice(document.getElementById('settings-voice-body'), {
            h: {
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
            },
        });

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
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    // Send out the metadata update
    try {
        const success = await invoke("update_profile", { name: strUsername, avatar: "", banner: "", about: "" });
        if (!success) {
            cProfile.name = oldName;
            renderCurrentProfile(cProfile);
            if (domProfile.style.display === '') renderProfileTab(cProfile);
            await popupConfirm('Username Update Failed!', 'Failed to broadcast profile update to the network.', true, '', 'vector_warning.svg');
        }
    } catch (e) {
        cProfile.name = oldName;
        renderCurrentProfile(cProfile);
        if (domProfile.style.display === '') renderProfileTab(cProfile);
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
 * A GUI wrapper to ask the user for a avatar URL, and apply it both
 * in-app and on the Nostr network.
 */
async function askForAvatar() {
    // Prompt the user to select an image file
    const file = await open({
        title: 'Choose an Avatar',
        multiple: false,
        directory: false,
        filters: [{
            name: 'Image',
            extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico']
        }]
    });
    if (!file) return;

    const inEditMode = typeof fProfileEditMode !== 'undefined' && fProfileEditMode;
    const avatarEditBtn = document.querySelector('.profile-avatar-edit');
    const avatarIcon = avatarEditBtn?.querySelector('.icon');
    const avatarContainer = inEditMode ? document.querySelector('.profile-avatar-container') : null;
    let unlisten = null;
    let ringEl = null;

    if (inEditMode && avatarContainer) {
        avatarContainer.classList.add('uploading');
        ringEl = document.createElement('div');
        ringEl.className = 'profile-upload-ring';
        avatarContainer.appendChild(ringEl);
        unlisten = await window.__TAURI__.event.listen('profile_upload_progress', (event) => {
            if (event.payload.type === 'avatar' && ringEl) {
                const progress = Math.max(5, event.payload.progress);
                ringEl.style.setProperty('--progress', `${progress}%`);
            }
        });
    } else if (avatarIcon) {
        avatarIcon.className = 'profile-upload-spinner';
        avatarIcon.style.setProperty('--progress', '5%');
        unlisten = await window.__TAURI__.event.listen('profile_upload_progress', (event) => {
            if (event.payload.type === 'avatar') {
                const progress = Math.max(5, event.payload.progress);
                avatarIcon.style.setProperty('--progress', `${progress}%`);
            }
        });
    }

    const restoreFeedback = () => {
        if (avatarContainer) avatarContainer.classList.remove('uploading');
        if (ringEl && ringEl.parentNode) ringEl.remove();
        if (avatarIcon && !inEditMode) avatarIcon.className = 'icon icon-plus-circle';
        if (unlisten) unlisten();
    };

    // Upload the avatar to a NIP-96 server
    let strUploadURL = '';
    try {
        strUploadURL = await invoke("upload_avatar", { filepath: file, uploadType: "avatar" });
    } catch (e) {
        restoreFeedback();
        return await popupConfirm('Avatar Upload Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
    restoreFeedback();

    const cProfile = arrProfiles.find(a => a.mine);
    const oldAvatar = cProfile.avatar;
    const oldAvatarCached = cProfile.avatar_cached;
    cProfile.avatar = strUploadURL;
    cProfile.avatar_cached = '';

    if (inEditMode && domProfileAvatar) {
        // Preview from the locally-picked file — instant and animation-safe.
        // A fresh Blossom GIF can take >15s to fetch back, and the WebView must
        // never point at a remote URL anyway (Tor bypass). Mirrors the banner
        // branch: an avatar-less profile renders a placeholder <div> and the
        // #profile-avatar id doesn't survive re-renders, so go via the tracked
        // element and swap the placeholder for a real <img>.
        if (domProfileAvatar.tagName === 'IMG') {
            domProfileAvatar.src = convertFileSrc(file);
        } else {
            const img = document.createElement('img');
            img.id = 'profile-avatar';
            img.className = 'profile-avatar';
            img.src = convertFileSrc(file);
            domProfileAvatar.replaceWith(img);
            domProfileAvatar = img;
        }
    } else {
        renderCurrentProfile(cProfile);
        if (domProfile.style.display === '') renderProfileTab(cProfile);
    }

    try {
        const success = await invoke("update_profile", { name: "", avatar: strUploadURL, banner: "", about: "" });
        if (!success) {
            cProfile.avatar = oldAvatar;
            cProfile.avatar_cached = oldAvatarCached;
            if (!inEditMode) {
                renderCurrentProfile(cProfile);
                if (domProfile.style.display === '') renderProfileTab(cProfile);
            }
            return await popupConfirm('Avatar Update Failed!', 'Failed to broadcast profile update to the network.', true, '', 'vector_warning.svg');
        }
    } catch (e) {
        cProfile.avatar = oldAvatar;
        cProfile.avatar_cached = oldAvatarCached;
        if (!inEditMode) {
            renderCurrentProfile(cProfile);
            if (domProfile.style.display === '') renderProfileTab(cProfile);
        }
        return await popupConfirm('Avatar Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

/**
 * A GUI wrapper to ask the user for a banner URL, and apply it both
 * in-app and on the Nostr network.
 */
async function askForBanner() {
    // Prompt the user to select an image file
    const file = await open({
        title: 'Choose a Banner',
        multiple: false,
        directory: false,
        filters: [{
            name: 'Image',
            extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp', 'tiff', 'tif', 'ico']
        }]
    });
    if (!file) return;

    const inEditMode = typeof fProfileEditMode !== 'undefined' && fProfileEditMode;
    const bannerEditBtn = document.querySelector('.profile-banner-edit');
    const bannerIcon = bannerEditBtn?.querySelector('.icon');
    const bannerContainer = inEditMode ? document.getElementById('profile-banner-container') : null;
    let unlisten = null;
    let ringEl = null;

    if (inEditMode && bannerContainer) {
        bannerContainer.classList.add('uploading');
        ringEl = document.createElement('div');
        ringEl.className = 'profile-upload-ring';
        bannerContainer.appendChild(ringEl);
        unlisten = await window.__TAURI__.event.listen('profile_upload_progress', (event) => {
            if (event.payload.type === 'banner' && ringEl) {
                const progress = Math.max(5, event.payload.progress);
                ringEl.style.setProperty('--progress', `${progress}%`);
            }
        });
    } else if (bannerIcon) {
        bannerIcon.className = 'profile-upload-spinner';
        bannerIcon.style.setProperty('--progress', '5%');
        unlisten = await window.__TAURI__.event.listen('profile_upload_progress', (event) => {
            if (event.payload.type === 'banner') {
                const progress = Math.max(5, event.payload.progress);
                bannerIcon.style.setProperty('--progress', `${progress}%`);
            }
        });
    }

    const restoreFeedback = () => {
        if (bannerContainer) bannerContainer.classList.remove('uploading');
        if (ringEl && ringEl.parentNode) ringEl.remove();
        if (bannerIcon && !inEditMode) bannerIcon.className = 'icon icon-edit';
        if (unlisten) unlisten();
    };

    // Upload the banner to a NIP-96 server
    let strUploadURL = '';
    try {
        strUploadURL = await invoke("upload_avatar", { filepath: file, uploadType: "banner" });
    } catch (e) {
        restoreFeedback();
        return await popupConfirm('Banner Upload Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
    restoreFeedback();

    // Update the in-memory profile.
    const cProfile = arrProfiles.find(a => a.mine);
    const oldBanner = cProfile.banner;
    const oldBannerCached = cProfile.banner_cached;
    cProfile.banner = strUploadURL;
    cProfile.banner_cached = ''; // Clear stale cached image so new URL is used

    if (inEditMode && domProfileBanner) {
        // Surgical img.src swap. renderProfileTab would tear down the
        // edit-bar overlay; Save's exit handler paints the final state.
        if (domProfileBanner.tagName === 'IMG') {
            domProfileBanner.src = convertFileSrc(file);
        } else {
            // Currently a placeholder <div>; swap to a real <img>.
            const img = document.createElement('img');
            img.id = 'profile-banner';
            img.className = domProfileBanner.className;
            img.src = convertFileSrc(file);
            domProfileBanner.replaceWith(img);
            domProfileBanner = img;
        }
    } else {
        renderCurrentProfile(cProfile);
        if (domProfile.style.display === '') renderProfileTab(cProfile);
    }

    // Send out the metadata update
    try {
        const success = await invoke("update_profile", { name: "", avatar: "", banner: strUploadURL, about: "" });
        if (!success) {
            // Revert local change since network update failed
            cProfile.banner = oldBanner;
            cProfile.banner_cached = oldBannerCached;
            if (!inEditMode) {
                renderCurrentProfile(cProfile);
                if (domProfile.style.display === '') renderProfileTab(cProfile);
            }
            return await popupConfirm('Banner Update Failed!', 'Failed to broadcast profile update to the network.', true, '', 'vector_warning.svg');
        }
    } catch (e) {
        // Revert local change on error
        cProfile.banner = oldBanner;
        cProfile.banner_cached = oldBannerCached;
        if (!inEditMode) {
            renderCurrentProfile(cProfile);
            if (domProfile.style.display === '') renderProfileTab(cProfile);
        }
        return await popupConfirm('Banner Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

/**
 * A GUI wrapper to ask the user for a status, and apply it both
 * in-app and on the Nostr network.
 */
async function askForStatus() {
    openStatusDialog(arrProfiles.find(a => a.mine));
}

/** Open the Status dialog prefilled with the current status. Emoji come from
 *  the shared Emoji Panel in status mode (GIFs hidden); the live row renders
 *  your avatar + the exact pill other users will see. While the panel is
 *  open the card glides to the upper third so both stay fully visible. */
/** The Status field's mini composer: the chat composer module with the
 *  emoji-only grammar (inline emoji, no markdown/mentions). Lazy singleton —
 *  the dialog's DOM is permanent, so the instance is too. */
let _statusComposer = null;
function _ensureStatusComposer() {
    if (_statusComposer) return _statusComposer;
    _statusComposer = createRichComposer(document.getElementById('status-input-host'), {
        placeholder: "What's happening?",
        emojiOnly: true,
        resolveEmoji: cmpResolvePackEmoji,
        bindEmojiImg: cmpBindEmojiImg,
    });
    return _statusComposer;
}

function openStatusDialog(cProfile) {
    const strCurrent = cProfile?.status?.title || '';
    const overlay = document.getElementById('status-dialog');
    const input = _ensureStatusComposer();
    const btnEmoji = document.getElementById('status-emoji-btn');
    const btnSave = document.getElementById('status-save');
    const btnClose = document.getElementById('status-dialog-close');
    const btnClear = document.getElementById('status-clear');
    const charCount = document.getElementById('status-char-count');
    const preview = document.getElementById('status-preview');
    const previewText = document.getElementById('status-preview-text');
    const avatarWrap = document.getElementById('status-preview-avatar');

    avatarWrap.innerHTML = '';
    avatarWrap.appendChild(createAvatarImg(getProfileAvatarSrc(cProfile), 34, false));

    const updatePreview = () => {
        // Statuses are one line, 120 chars — the composer itself has no
        // maxlength, so sanitize the model on every edit.
        const clean = input.value.replace(/\n/g, ' ').slice(0, 120);
        if (clean !== input.value) input.value = clean;
        const txt = clean.trim();
        preview.classList.toggle('status-preview-empty', !txt);
        previewText.textContent = txt || 'No status';
        if (txt) {
            twemojify(previewText);
            renderCustomEmojiShortcodes(previewText, equippedEmojiTags());
        }
        const remaining = 120 - clean.length;
        charCount.textContent = remaining <= 30 ? String(remaining) : '';
        charCount.classList.toggle('status-char-low', remaining <= 10);
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
    // it closes — regardless of WHICH path closed it (✕, outside tap, select).
    const panelWatcher = new MutationObserver(() => {
        overlay.classList.toggle('panel-open', picker.classList.contains('visible'));
    });
    panelWatcher.observe(picker, { attributes: true, attributeFilter: ['class'] });

    const close = () => {
        if (overlay.classList.contains('closing')) return;
        panelWatcher.disconnect();
        _emojiPanelTarget = null;
        closeEmojiPanel();
        document.removeEventListener('keydown', onKey);
        popBack('status-dialog');
        // Mirror the open pop, then actually hide (matches the 0.15s animation)
        overlay.classList.add('closing');
        overlay._closeTimer = setTimeout(() => {
            overlay.classList.remove('active', 'closing', 'panel-open');
            overlay.querySelector('.status-dialog-card').classList.remove('pop-in');
        }, 160);
    };

    const onKey = (e) => {
        if (e.key === 'Escape') {
            if (picker.classList.contains('visible')) closeEmojiPanel();
            else close();
        }
    };

    input.value = strCurrent;
    btnClear.classList.toggle('hidden', !strCurrent);
    updatePreview();

    input.oninput = updatePreview;
    // Enter saves — statuses have no second line to go to.
    input.onkeydown = (e) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            btnSave.click();
        }
    };
    btnEmoji.onclick = (e) => {
        // stopPropagation: the document-level click delegate would otherwise
        // run the panel's open/close toggle against this same click.
        e.stopPropagation();
        if (picker.classList.contains('visible')) closeEmojiPanel();
        else openEmojiPanelForStatus(insertIntoStatus);
    };
    btnSave.onclick = () => { const v = input.value.trim(); close(); saveStatus(v); };
    btnClear.onclick = () => { close(); saveStatus(''); };
    btnClose.onclick = close;
    overlay.onclick = (e) => {
        if (e.target !== overlay) return;
        // First outside tap dismisses the emoji panel (the document delegate
        // handles it); the next one dismisses the dialog.
        if (picker.classList.contains('visible')) return;
        close();
    };

    // Cancel any in-flight close, then pop in: the animation class lands
    // AFTER the overlay renders, because WebKit won't start one declared on
    // a subtree emerging from display:none.
    clearTimeout(overlay._closeTimer);
    overlay.classList.remove('closing');
    const card = overlay.querySelector('.status-dialog-card');
    card.classList.remove('pop-in');
    overlay.classList.add('active');
    void card.offsetWidth;
    card.classList.add('pop-in');
    document.addEventListener('keydown', onKey);
    pushBack('status-dialog', close);
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
    if (domProfile.style.display === '') renderProfileTab(cProfile);

    const rollback = () => {
        cProfile.status.title = oldStatus;
        cProfile.status.emoji_tags = oldTags;
        renderCurrentProfile(cProfile);
        if (domProfile.style.display === '') renderProfileTab(cProfile);
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
  domSettingsThemeSelect.value = theme;
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

// Apply Theme changes in real-time
domSettingsThemeSelect.onchange = async (evt) => {
    await setTheme(evt.target.value);
    // Refresh storage section after theme change to update colors
    initStorageSection();
};

// Listen for Logout clicks
domSettingsLogout.onclick = async (evt) => {
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
};

// Listen for Export Account clicks
domSettingsExport.onclick = async (evt) => {
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
                <button id="export-seed-copy" style="flex-shrink: 0; padding: 6px 10px; border-radius: 5px; cursor: pointer;">Copy</button>
            </div>
            </div>
        `;
        }

        exportContent += `
        <div style="text-align: left; padding: 0 8px;">
            <p style="font-weight: bold; margin: 0 0 4px 0; text-align: center;">Private Key (nsec)</p>
            <div style="display: flex; align-items: center; gap: 6px; min-width: 0;">
            <p id="export-nsec-value" style="overflow-x: auto; overflow-y: hidden; white-space: nowrap; background: #1a1a1a; padding: 8px 10px; border-radius: 5px; font-family: monospace; font-size: 12px; flex: 1; min-width: 0; margin: 0;">${safeNsec}</p>
            <button id="export-nsec-copy" style="flex-shrink: 0; padding: 6px 10px; border-radius: 5px; cursor: pointer;">Copy</button>
            </div>
            <p style="color: #4de0a0; font-size: 12px; margin: 8px 0 -10px 0; text-align: center;">Do Not Store on Device. Backup Offline.</p>
        </div>
        `;

        // Kick off the popup. `popupConfirm` is async but the DOM mutations
        // it does (assigning the innerHTML for our subtext) run synchronously
        // before it returns its promise, so we can wire the copy buttons up
        // BEFORE awaiting — `await popupConfirm(...)` only resolves on the
        // user's Okay click, by which point the popup is gone.
        const popupPromise = popupConfirm('Export Account', exportContent, true, '', 'vector_warning.svg');

        const seedCopyBtn = document.getElementById('export-seed-copy');
        if (seedCopyBtn) seedCopyBtn.onclick = () => navigator.clipboard.writeText(keys.seed_phrase);
        const nsecCopyBtn = document.getElementById('export-nsec-copy');
        if (nsecCopyBtn) nsecCopyBtn.onclick = () => navigator.clipboard.writeText(keys.nsec);

        await popupPromise;
    } catch (error) {
        console.error('Export failed:', error);
        await popupConfirm('Export Failed', escapeHtml(error.toString()), true, '', 'vector_warning.svg');
    }
};

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
    const clearStorageBtn = document.getElementById('clear-storage-btn');
    if (clearStorageBtn.disabled) return;

    const confirmClear = await popupConfirm(
        'Clear Storage?',
        'This will delete all downloaded and sent files from Vector. This action cannot be undone.',
        false,
        '',
        'vector_warning.svg'
    );
    
    if (!confirmClear) return;
    
    let strPrevText = clearStorageBtn.textContent;
    try {
        clearStorageBtn.disabled = true;
        clearStorageBtn.textContent = 'Clearing...';
        await invoke('clear_storage');
        // Full clear nukes the image cache too; drop the emoji memos so
        // rendered emojis re-download instead of pointing at deleted files
        reloadCachedEmojiImgs();
        clearStorageBtn.textContent = strPrevText;
        clearStorageBtn.disabled = false;
        return true;
    } catch (error) {
        clearStorageBtn.textContent = strPrevText;
        clearStorageBtn.disabled = false;
        console.error('Failed to clear storage:', error);
        await popupConfirm('Clear Failed', `Could not clear storage: ${escapeHtml(String(error.message))}`, true, '', 'vector_warning.svg');
        return false;
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

let fStorageDonutMounted = false;

/**
 * Initialize the Storage section in settings
 */
async function initStorageSection() {
    if (!fStorageDonutMounted) {
        fStorageDonutMounted = true;
        VectorSvelte.mountStorageDonut(document.getElementById('storage-breakdown'), {
            h: {
                formatBytes,
                confirmDelete: confirmStorageDelete,
                deleteCategory: (category, exts) => invoke('clear_storage_category', { category, exts }),
                refresh: () => initStorageSection(),
                // The emoji memos and any rendered <img>s point at the deleted cache files
                onCacheCleared: () => reloadCachedEmojiImgs(),
                toast: (msg) => showToast(msg),
                deleteFailed: (e) => popupConfirm('Delete Failed', `Could not delete: ${escapeHtml(String(e))}`, true, '', 'vector_warning.svg'),
            },
        });
    }
    const storageInfo = await getStorageInfo();
    if (storageInfo) VectorSvelte.setStorageDistribution(storageInfo.type_distribution);

    // Auto-download: an explicit toggle plus a size limit that greys out when the toggle is off.
    // Values + the pre-split migration load at boot (initAutoDownloadSettings); here we only
    // reflect them into the UI and wire the controls. onchange (not addEventListener) since
    // initStorageSection re-runs (theme change / Clear Storage) and must not stack listeners.
    const adToggle = document.getElementById('auto-download-toggle');
    const adLimitGroup = document.getElementById('auto-download-limit-group');
    const adLimit = document.getElementById('auto-download-limit');
    const applyAutoDownloadState = () => {
        if (adLimit) adLimit.disabled = !AUTO_DOWNLOAD_ENABLED;
        if (adLimitGroup) adLimitGroup.classList.toggle('disabled', !AUTO_DOWNLOAD_ENABLED);
    };
    if (adToggle) {
        adToggle.checked = AUTO_DOWNLOAD_ENABLED;
        adToggle.onchange = async () => {
            AUTO_DOWNLOAD_ENABLED = adToggle.checked;
            await saveAutoDownloadEnabled(AUTO_DOWNLOAD_ENABLED);
            applyAutoDownloadState();
        };
    }
    if (adLimit) {
        adLimit.value = String(MAX_AUTO_DOWNLOAD_BYTES);
        adLimit.onchange = async () => {
            MAX_AUTO_DOWNLOAD_BYTES = parseInt(adLimit.value, 10);
            await saveMaxAutoDownloadBytes(MAX_AUTO_DOWNLOAD_BYTES);
        };
    }
    applyAutoDownloadState();

    // Explainer (i) icons. preventDefault so the toggle-row icon doesn't flip the switch.
    const adInfo = document.getElementById('auto-download-info');
    if (adInfo) adInfo.onclick = (e) => {
        e.preventDefault(); e.stopPropagation();
        popupConfirm('Auto-Download Media', 'When enabled, Vector automatically downloads incoming photos, videos, voice messages and files (up to the size limit below).<br><br>Turn this off to keep attachments as previews and download them by hand, one at a time.', true);
    };
    const adLimitInfo = document.getElementById('auto-download-limit-info');
    if (adLimitInfo) adLimitInfo.onclick = (e) => {
        e.preventDefault(); e.stopPropagation();
        popupConfirm('Auto-Download Limit', 'The largest attachment size Vector will fetch automatically.<br><br>Anything above this waits for you to tap Download. Only applies while Auto-Download Media is on.', true);
    };
    const clearInfo = document.getElementById('clear-storage-info');
    if (clearInfo) clearInfo.onclick = (e) => {
        e.preventDefault(); e.stopPropagation();
        popupConfirm('Clear Storage', 'Deletes the downloaded and sent files Vector has cached on this device, to free up space.<br><br>Your messages stay. Attachments can be downloaded again later if they are still available from their sender.', true);
    };

    // Hide Media from Gallery (Android only — the backend command is a no-op on
    // desktop, and the gallery concept doesn't apply there). onchange (not
    // addEventListener) since initStorageSection re-runs after Clear Storage.
    const galleryGroup = document.getElementById('storage-gallery-group');
    const galleryToggle = document.getElementById('storage-gallery-toggle');
    if (galleryGroup && galleryToggle && platformFeatures.is_mobile) {
        galleryGroup.style.display = '';
        try {
            galleryToggle.checked = await invoke('get_gallery_hidden');
        } catch (_) {
            galleryToggle.checked = false;
        }
        galleryToggle.onchange = async (e) => {
            try {
                await invoke('set_gallery_hidden', { hidden: e.target.checked });
            } catch (err) {
                console.error('set_gallery_hidden failed:', err);
            }
        };
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
async function initDisplaySettings() {
    VectorSvelte.mountDisplay(document.getElementById('settings-display-body'), {
        h: {
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
        },
    });

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
async function initNotificationSettings() {
    const sounds = !!platformFeatures.notification_sounds;
    VectorSvelte.mountNotifications(document.getElementById('settings-notifications-body'), {
        h: {
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
        },
    });

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

/**
 * Initialize settings on app start
 */
async function initSettings() {
    // Load privacy settings from DB (default to true)
    fWebPreviewsEnabled = await loadWebPreviews();
    fStripTrackingEnabled = await loadStripTracking();
    fSendTypingIndicators = await loadSendTypingIndicators();

    // Auto-download toggle + limit (migrates pre-split accounts). At boot so the
    // gate in message-row.js is correct before Settings is opened.
    await initAutoDownloadSettings();

    // Set initial toggle states
    const webPreviewsToggle = document.getElementById('privacy-web-previews-toggle');
    const stripTrackingToggle = document.getElementById('privacy-strip-tracking-toggle');
    const sendTypingToggle = document.getElementById('privacy-send-typing-toggle');
    
    webPreviewsToggle.checked = fWebPreviewsEnabled;
    webPreviewsToggle.addEventListener('change', async (e) => {
        fWebPreviewsEnabled = e.target.checked;
        await saveWebPreviews(e.target.checked);
    });
    
    stripTrackingToggle.checked = fStripTrackingEnabled;
    stripTrackingToggle.addEventListener('change', async (e) => {
        fStripTrackingEnabled = e.target.checked;
        await saveStripTracking(e.target.checked);
    });
    
    sendTypingToggle.checked = fSendTypingIndicators;
    sendTypingToggle.addEventListener('change', async (e) => {
        fSendTypingIndicators = e.target.checked;
        await saveSendTypingIndicators(e.target.checked);
    });

    // Tor toggle — reads current state from the backend (which knows whether
    // the build was compiled with `--features tor`), then attaches a change
    // handler that persists the preference and starts/stops the embedded Tor
    // service. While the toggle awaits bootstrap, we show progress text.
    const torToggle = document.getElementById('privacy-tor-toggle');
    const torStatus = document.getElementById('privacy-tor-status');
    if (torToggle && torStatus) {
        VectorSvelte.mountTorCard({
            els: {
                card: document.getElementById('settings-tor-card'),
                toggle: torToggle,
                status: torStatus,
                advanced: document.getElementById('settings-tor-advanced'),
                panel: document.getElementById('tor-advanced-panel'),
                refresh: document.getElementById('tor-circuits-refresh'),
                list: document.getElementById('tor-circuits-list'),
            },
            h: { stateClass: torStateClass, formatStatus: formatTorStatus, isTransitional: isTorTransitional },
        });
        try {
            const state = await invoke('tor_get_state');
            torApply(state);
            // If we landed in a transient state (bootstrap still mid-flight
            // when Settings opened), poll until it settles.
            if (state.enabled && !state.running) ensureTorStatePolling();
        } catch (e) {
            console.warn('[Tor] tor_get_state failed:', e);
        }

        torToggle.addEventListener('change', async (e) => {
            const desired = e.target.checked;
            VectorSvelte.setTorLocked(true);
            torApply(
                { supported: true, enabled: desired, running: false, status: desired ? 'bootstrapping' : 'disabled', bootstrap_progress: desired ? 0 : null },
                desired ? 'Bootstrapping…' : 'Disabling…',
            );
            // tor_set_enabled doesn't return until bootstrap completes (~20-30s
            // first boot) — start polling now so the UI gets live progress.
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
                // The toggle stays locked while Tor is transitional (derived from the state).
                try { torApply(await invoke('tor_get_state')); } catch (_) {}
                VectorSvelte.setTorLocked(false);
            }
        });

        const advToggle = document.getElementById('tor-advanced-toggle');
        const advRefresh = document.getElementById('tor-circuits-refresh');
        if (advToggle) {
            advToggle.addEventListener('click', () => {
                const willOpen = !VectorSvelte.torState().advancedOpen;
                VectorSvelte.setTorAdvancedOpen(willOpen);
                if (willOpen) loadTorCircuits(false);
            });
        }
        if (advRefresh) {
            // "New circuit" intentionally rotates the isolation token AND
            // cycles relay sockets. Bridges flows below only refresh the
            // display (without doubling up on switch_relay_transport).
            advRefresh.addEventListener('click', () => loadTorCircuits(true, true));
        }

        // Bridges: toggle reveals textarea, Apply persists + reconnects Tor.
        await initTorBridgesUI();
    }

    // The blocked-users list island + its disclosure toggle
    VectorSvelte.mountBlockedUsers(document.getElementById('settings-blocked-list'), {
        emptyEl: document.getElementById('settings-blocked-empty'),
        h: {
            load: () => invoke('get_blocked_users'),
            getProfile: (npub) => getProfile(npub),
            getProfileAvatarSrc: (p) => getProfileAvatarSrc(p),
            createAvatarImg: (src, size, group) => createAvatarImg(src, size, group),
            confirmUnblock: (p) => popupConfirm('Unblock User', `Are you sure you want to unblock ${escapeHtml(getName(p))}?`),
            unblock: async (npub) => {
                await invoke('unblock_user', { npub });
                showToast('User unblocked');
                profileChanged(npub);
            },
            reload: () => VectorSvelte.reloadBlockedUsers(),
        },
    });
    const blockedToggle = document.getElementById('settings-blocked-toggle');
    const blockedContent = document.getElementById('settings-blocked-content');
    const blockedChevron = blockedToggle.querySelector('.icon');
    blockedToggle.onclick = () => {
        const isOpen = blockedContent.style.display !== 'none';
        if (isOpen) {
            blockedContent.style.display = 'none';
            blockedChevron.style.transform = '';
        } else {
            blockedContent.style.display = '';
            blockedContent.style.animation = 'blockedFadeIn 0.2s ease';
            blockedChevron.style.transform = 'rotate(180deg)';
        }
    };

    await initDisplaySettings();

    await initNotificationSettings();

    // Set up clear storage button
    const clearStorageBtn = document.getElementById('clear-storage-btn');
    clearStorageBtn.addEventListener('click', async () => {
        const success = await clearStorage();
        if (success) initStorageSection();
    });

    // Pre-fetch logs so clipboard.writeText runs synchronously on click (user gesture required)
    window._cachedLogs = '';
    invoke('get_logs').then((log) => { window._cachedLogs = log || ''; });
    const copyCrashLogBtn = document.getElementById('copy-crash-log-btn');
    copyCrashLogBtn.addEventListener('click', () => {
        if (!window._cachedLogs) {
            showToast('No logs to copy!');
            return;
        }
        const lines = window._cachedLogs.split('\n').filter(l => l.trim()).length;
        navigator.clipboard.writeText(window._cachedLogs).then(() => {
            showToast('Copied ' + lines + ' log entries to clipboard');
        });
    });

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
    const encryptionToggle = document.getElementById('security-encryption-toggle');
    const encryptionInfoBtn = document.getElementById('security-encryption-info');
    const changeCredentialBtn = document.getElementById('security-change-credential');

    if (!encryptionToggle) return;

    VectorSvelte.mountSecurityCard({
        els: {
            toggle: encryptionToggle,
            unlockRow: document.getElementById('unlock-method-container'),
            unlockLabel: document.getElementById('unlock-method-label'),
            unlockBtn: document.getElementById('unlock-method-switch'),
            pinRow: document.getElementById('change-pin-container'),
            pinLabel: document.getElementById('change-pin-label'),
            card: document.getElementById('settings-remote-signer'),
            label: document.getElementById('remote-signer-label'),
            hint: document.getElementById('remote-signer-hint'),
            pubkey: document.getElementById('remote-signer-pubkey'),
            dot: document.getElementById('remote-signer-dot'),
            exportRow: document.getElementById('export-account-row'),
        },
    });

    try {
        const status = await invoke('get_encryption_status', { npub: null });
        fEncryptionEnabled = status.enabled;
        fSecurityType = status.security_type || 'pin';
    } catch (e) {
        console.error('Failed to get encryption status:', e);
        fEncryptionEnabled = true;
    }
    syncSecurityState();

    encryptionToggle.addEventListener('change', handleEncryptionToggleChange);

    if (encryptionInfoBtn) {
        encryptionInfoBtn.addEventListener('click', (e) => {
            e.stopPropagation();
            e.preventDefault();
            showEncryptionInfo();
        });
    }
    if (changeCredentialBtn) {
        changeCredentialBtn.addEventListener('click', handleChangeCredential);
    }

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
        const t = document.getElementById('security-encryption-toggle');
        if (t) t.checked = st.enabled;
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
    const btn = document.getElementById('unlock-method-switch');
    const infoBtn = document.getElementById('unlock-method-info');
    if (!btn) return;

    let bioSupported = false;
    if (platformFeatures.os === 'android') {
        try {
            const st = await invoke('biometric_status');
            bioSupported = !!st.supported;
        } catch (e) { /* leave unsupported */ }
    }
    VectorSvelte.setSecurity({ enabled: fEncryptionEnabled, type: fSecurityType, bioSupported });

    if (!btn.dataset.unlockBound) {
        btn.dataset.unlockBound = '1';
        btn.addEventListener('click', async () => {
            if (fMigrationInProgress) return;
            if (fSecurityType === 'biometric') {
                await switchToCredentialMode();
            } else {
                await switchToBiometricMode();
            }
        });
        if (infoBtn) {
            infoBtn.addEventListener('click', (e) => {
                e.stopPropagation();
                e.preventDefault();
                popupConfirm(
                    'Unlock Method',
                    'Your local data is always encrypted. This chooses what unlocks it:<br><br>' +
                    '<b>Biometrics</b> uses your device security (fingerprint, face, or device PIN) with a key held in hardware.<br><br>' +
                    '<b>PIN or Password</b> uses a credential you type and remember.',
                    true,
                    '',
                    'vector-check.svg'
                );
            });
        }
    }
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
 * Update change credential button visibility and text
 */
/**
 * Show info popup about local encryption
 */
async function showEncryptionInfo() {
    await popupConfirm(
        'Local Encryption',
        'Protects your messages and keys if your device is lost or stolen.<br><br>' +
        'Disabling speeds up app launch but stores data in plain text.',
        true,
        '',
        'vector-check.svg'
    );
}

/**
 * Handle encryption toggle change
 */
async function handleEncryptionToggleChange(e) {
    const newValue = e.target.checked;

    // Block if migration running or a credential modal is already open
    if (fMigrationInProgress || document.getElementById('credential-modal-overlay')?.classList.contains('active')) {
        e.target.checked = fEncryptionEnabled;
        return;
    }

    if (newValue) {
        // Enabling encryption - requires PIN
        await handleEnableEncryption(e.target);
    } else {
        // Disabling encryption - confirm and migrate
        await handleDisableEncryption(e.target);
    }
}

/**
 * Handle enabling encryption
 * @param {HTMLInputElement} toggle - The toggle element
 */
async function handleEnableEncryption(toggle) {
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
                if (!ok) toggle.checked = false;
                return;
            }
        }
    }

    // Ask user to choose security type
    const result = await promptSecurityCredential('Set Up Encryption', 'Choose how to protect your local data. There is no recovery if you forget!');

    if (!result) {
        toggle.checked = false;
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
        toggle.checked = false;
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
        const overlay = document.getElementById('credential-modal-overlay');
        const titleEl = document.getElementById('credential-modal-title');
        const subtitleEl = document.getElementById('credential-modal-subtitle');
        const typeSelect = document.getElementById('credential-modal-type-select');
        const pinRow = document.getElementById('credential-modal-pin-row');
        const passwordDiv = document.getElementById('credential-modal-password');
        const passwordInput = document.getElementById('credential-modal-password-input');
        const confirmBtn = document.getElementById('credential-modal-confirm');
        const cancelBtn = document.getElementById('credential-modal-cancel');

        // Reset state
        titleEl.textContent = title;
        subtitleEl.textContent = subtitle;
        typeSelect.style.display = 'none';
        pinRow.style.display = 'none';
        passwordDiv.style.display = 'none';
        confirmBtn.textContent = confirmText;

        // PIN inputs — fresh query each time
        const pinInputs = pinRow.querySelectorAll('.cred-pin');
        pinInputs.forEach(el => { el.value = ''; });

        passwordInput.value = '';

        let selectedType = 'pin';
        let resolved = false;

        function cleanup() {
            if (resolved) return;
            resolved = true;
            overlay.classList.remove('active');
            document.removeEventListener('keydown', onKeyDown);
            // Remove PIN listeners
            pinInputs.forEach(el => {
                el.removeEventListener('input', onPinInput);
                el.removeEventListener('keydown', onPinKeyDown);
            });
        }

        function finish(value) {
            cleanup();
            resolve(value);
        }

        // --- Cancel ---
        cancelBtn.onclick = () => finish(null);

        function onKeyDown(e) {
            if (e.key === 'Escape') {
                e.preventDefault();
                finish(null);
            }
        }
        document.addEventListener('keydown', onKeyDown);

        // --- PIN input handlers ---
        function onPinKeyDown(e) {
            const idx = Array.from(pinInputs).indexOf(e.target);
            if (e.key === 'Backspace') {
                e.preventDefault();
                e.target.value = '';
                if (idx > 0) pinInputs[idx - 1].focus();
            } else if (e.key.length === 1 && !/^[0-9]$/.test(e.key)) {
                e.preventDefault();
            }
        }

        function onPinInput(e) {
            const idx = Array.from(pinInputs).indexOf(e.target);
            let val = e.target.value.replace(/[^0-9]/g, '');
            if (val.length > 1) val = val.charAt(0);
            e.target.value = val;
            if (val && idx < pinInputs.length - 1) {
                pinInputs[idx + 1].focus();
            }
            // Auto-submit when all 6 digits entered
            const full = Array.from(pinInputs).every(el => /^[0-9]$/.test(el.value));
            if (full) {
                const pin = Array.from(pinInputs).map(el => el.value).join('');
                finish(pin);
            }
        }

        // --- Mode setup ---
        if (mode === 'pin') {
            pinRow.style.display = '';
            // No confirm button for PIN (auto-submits on 6th digit)
            confirmBtn.style.display = 'none';
            pinInputs.forEach(el => {
                el.addEventListener('keydown', onPinKeyDown);
                el.addEventListener('input', onPinInput);
            });
            // Show and focus
            overlay.classList.add('active');
            requestAnimationFrame(() => pinInputs[0].focus());

        } else if (mode === 'password') {
            passwordDiv.style.display = '';
            confirmBtn.style.display = '';
            confirmBtn.onclick = () => {
                const val = passwordInput.value;
                if (val) finish(val);
            };
            // Enter key submits
            passwordInput.onkeydown = (e) => {
                if (e.key === 'Enter') {
                    e.preventDefault();
                    const val = passwordInput.value;
                    if (val) finish(val);
                }
            };
            overlay.classList.add('active');
            requestAnimationFrame(() => passwordInput.focus());

        } else if (mode === 'type-select') {
            typeSelect.style.display = '';
            confirmBtn.style.display = '';
            confirmBtn.textContent = confirmText || 'Continue';

            const btnPin = document.getElementById('credential-modal-type-pin');
            const btnPwd = document.getElementById('credential-modal-type-password');
            const descEl = document.getElementById('credential-modal-type-desc');

            btnPin.classList.add('active');
            btnPwd.classList.remove('active');
            selectedType = 'pin';
            descEl.textContent = 'A 6-digit code. Quick and convenient.';

            btnPin.onclick = () => {
                selectedType = 'pin';
                btnPin.classList.add('active');
                btnPwd.classList.remove('active');
                descEl.textContent = 'A 6-digit code. Quick and convenient.';
            };
            btnPwd.onclick = () => {
                selectedType = 'password';
                btnPwd.classList.add('active');
                btnPin.classList.remove('active');
                descEl.textContent = 'A text password. More secure, but slower to enter.';
            };
            confirmBtn.onclick = () => finish(selectedType);
            overlay.classList.add('active');
        }
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
    if (fMigrationInProgress || document.getElementById('credential-modal-overlay')?.classList.contains('active')) return;

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

        // Show validating state while Argon2 hashes (keep modal visible)
        const overlay = document.getElementById('credential-modal-overlay');
        const titleEl = document.getElementById('credential-modal-title');
        const subtitleEl = document.getElementById('credential-modal-subtitle');
        const pinRow = document.getElementById('credential-modal-pin-row');
        const passwordDiv = document.getElementById('credential-modal-password');
        const buttonsDiv = document.getElementById('credential-modal-buttons');
        titleEl.textContent = `Validating ${currentLabel}...`;
        subtitleEl.textContent = 'Please wait';
        subtitleEl.classList.add('startup-subtext-gradient');
        pinRow.style.display = 'none';
        passwordDiv.style.display = 'none';
        buttonsDiv.style.display = 'none';
        overlay.classList.add('active');

        // Verify the credential without exposing key material over IPC
        try {
            await invoke('verify_credential', { credential: entered });
            oldCredential = entered;
            overlay.classList.remove('active');
            subtitleEl.classList.remove('startup-subtext-gradient');
            buttonsDiv.style.display = '';
            break;
        } catch (e) {
            overlay.classList.remove('active');
            subtitleEl.classList.remove('startup-subtext-gradient');
            buttonsDiv.style.display = '';
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

/**
 * Handle disabling encryption
 * @param {HTMLInputElement} toggle - The toggle element
 */
async function handleDisableEncryption(toggle) {
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
        // User cancelled - revert toggle
        toggle.checked = true;
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
        toggle.checked = true;
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
        // Update local state
        fEncryptionEnabled = document.getElementById('security-encryption-toggle').checked;
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

    const overlay = document.getElementById('encryption-migration-overlay');
    const title = document.getElementById('encryption-migration-title');
    const phase = document.getElementById('encryption-migration-phase');
    const progressFill = document.getElementById('encryption-migration-progress-fill');
    const progressText = document.getElementById('encryption-migration-progress-text');

    // Set title based on operation
    title.textContent = fMigrationRekeying ? 'Changing Credential' : encrypting ? 'Enabling Encryption' : 'Disabling Encryption';
    phase.textContent = 'Preparing...';
    progressFill.style.width = '0%';
    progressText.textContent = '0%';

    // Show the overlay
    overlay.classList.add('active');
}

/**
 * Hide the migration progress modal
 */
function hideMigrationModal() {
    fMigrationInProgress = false;

    const overlay = document.getElementById('encryption-migration-overlay');
    overlay.classList.remove('active');
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

/**
 * Initialize the battery settings section (toggle, warning).
 */
async function initBatterySettings() {
    const section = document.getElementById('settings-battery');
    if (!section) return;

    section.style.display = 'block';

    const toggle = document.getElementById('battery-bg-service-toggle');
    const warning = document.getElementById('battery-warning');

    // Load current state
    const enabled = await invoke('get_background_service_enabled');
    toggle.checked = enabled;

    // Show warning if enabled but battery optimization is active
    if (enabled) {
        const exempt = await invoke('check_battery_optimized');
        warning.style.display = exempt ? 'none' : '';
    } else {
        warning.style.display = 'none';
    }

    // Tap warning to open battery optimization dialog
    warning.style.cursor = 'pointer';
    warning.addEventListener('click', async () => {
        await invoke('request_battery_optimization');
        await waitForVisibility();
        const nowExempt = await invoke('check_battery_optimized');
        warning.style.display = nowExempt ? 'none' : '';
    });

    toggle.addEventListener('change', async () => {
        if (toggle.checked) {
            // Turning ON — check battery optimization first
            const exempt = await invoke('check_battery_optimized');
            if (!exempt) {
                // Request exemption
                await invoke('request_battery_optimization');
                // Wait for user to return from system dialog
                await waitForVisibility();
                const nowExempt = await invoke('check_battery_optimized');
                if (!nowExempt) {
                    // User denied — revert toggle
                    toggle.checked = false;
                    warning.style.display = 'none';
                    popupConfirm('Battery Optimization', 'Battery optimization must be disabled for reliable background notifications.', true, '', 'vector_warning.svg');
                    return;
                }
            }
            await invoke('set_background_service_enabled', { enabled: true });
            warning.style.display = 'none';
        } else {
            // Turning OFF — stop service immediately
            await invoke('set_background_service_enabled', { enabled: false });
            warning.style.display = 'none';
        }
    });
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

    // Refresh the settings UI to reflect current state
    const warning = document.getElementById('battery-warning');
    const toggle = document.getElementById('battery-bg-service-toggle');
    if (warning && toggle) {
        const enabled = await invoke('get_background_service_enabled');
        const exempt = await invoke('check_battery_optimized');
        toggle.checked = enabled;
        warning.style.display = (enabled && !exempt) ? '' : 'none';
    }
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
    const phaseEl = document.getElementById('encryption-migration-phase');
    const progressFill = document.getElementById('encryption-migration-progress-fill');
    const progressText = document.getElementById('encryption-migration-progress-text');

    // Calculate percentage
    const percentage = total > 0 ? Math.round((completed / total) * 100) : 0;

    // Update phase description
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

    phaseEl.textContent = phaseText;
    progressFill.style.width = `${percentage}%`;
    progressText.textContent = `${percentage}%`;
}

/** Wire the settings help prompts and the footer links. */
async function wireSettingsHelp() {
    // Hook up our "Help Prompts" to give users easy feature explainers in ambiguous or complex contexts
    // Note: since some of these overlap with Checkbox Labels: we prevent event bubbling so that clicking the Info Icon doesn't also trigger other events
   domSettingsPrivacyWebPreviewsInfo.onclick = async (e) => {
        e.preventDefault();
        e.stopPropagation();
        // Render contextually based on Tor preference. When Tor is enabled,
        // every preview fetch is forced through Tor (or blackholes during
        // bootstrap) by the network failsafe — no clearnet leak path.
        let torEnabled = false;
        try {
            const torState = await invoke('tor_get_state');
            torEnabled = !!(torState && torState.enabled);
        } catch (_) { /* fall through to default warning */ }
        const message = torEnabled
            ? 'When enabled, Vector will <b>automatically fetch and display previews</b> for links shared in messages.<br><br>You have <b>Tor enabled</b>, so preview fetches route through the Tor network. Your IP address stays hidden from the linked sites.'
            : 'When enabled, Vector will <b>automatically fetch and display previews</b> for links shared in messages.<br><br>This may expose your IP address to the linked sites. <b>Use Tor</b> (Privacy, Route traffic through Tor) <b>or a VPN</b> if that\'s a concern.';
        popupConfirm('Web Previews', message, true);
    };
    domSettingsPrivacyStripTrackingInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Strip Tracking Markers', 'When enabled, Vector will <b>automatically remove tracking markers</b> from URLs before displaying or sending them.<br><br>This helps reduce your footprint and enhances your privacy with no loss in functionality, only disable if you know what you\'re doing.', true);
    };
    domSettingsPrivacySendTypingInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Send Typing Indicators', 'When enabled, Vector will <b>notify your contacts when you are typing</b> a message to them.<br><br>Disable this if you prefer to type without others knowing you are composing a message.', true);
    };
    if (domSettingsPrivacyTorInfo) {
        domSettingsPrivacyTorInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            // Trademark notice + non-endorsement disclaimer included per the
            // Tor Project's trademark policy (https://www.torproject.org/about/trademark/).
            popupConfirm(
                'Route traffic through Tor',
                'When enabled, Vector routes <b>all TCP traffic</b> (Nostr relays, Blossom uploads, link previews, image fetches) through the Tor network using an embedded Arti client.<br><br>'
                + 'This hides your IP address from relays and remote servers, at the cost of slower connections (Tor circuits add latency).<br><br>'
                + '<small style="opacity: 0.6;">Tor and the Tor logo are trademarks of The Tor Project; all rights reserved. More information at <b>torproject.org</b>. Vector is not endorsed or sponsored by, or affiliated with, The Tor Project.</small>',
                true
            );
        };
    }
    // Open torproject.org when the small attribution logo is clicked.
    const torAttributionLink = document.getElementById('tor-attribution-link');
    if (torAttributionLink) {
        torAttributionLink.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            openUrl('https://torproject.org');
        };
    }
    const domSettingsBatteryBgServiceInfo = document.getElementById('battery-bg-service-info');
    if (domSettingsBatteryBgServiceInfo) {
        domSettingsBatteryBgServiceInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm('Run in Background', 'When enabled, Vector runs a <b>background service</b> to keep your connection alive and deliver <b>instant notifications</b>.<br><br>This requires disabling Android\'s battery optimization for Vector, otherwise the system may kill the service and delay or prevent notifications.', true);
        };
    }
    if (domSettingsStorageGalleryInfo) domSettingsStorageGalleryInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Hide Media from Gallery', 'By default, photos and videos you receive in Vector appear in your phone\'s Gallery app.<br><br>When enabled, Vector hides its media from the Gallery (and other apps). Existing media is removed from the Gallery too. Your files stay on the device and remain visible inside Vector.', true);
    };

    domSettingsExportAccountInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Export Account', 'Export Account will display a backup of your encryption keys. Keep it safe to restore your account later.', true);
    };

    if (domSettingsChangePinInfo) {
        domSettingsChangePinInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm(
                fSecurityType === 'password' ? 'Change Password' : 'Change PIN',
                fSecurityType === 'password'
                    ? 'Your password encrypts all local data including messages, keys, and secrets stored on your device. Resetting it will re-encrypt everything with your new password.'
                    : 'Your PIN encrypts all local data including messages, keys, and secrets stored on your device. Resetting it will re-encrypt everything with your new PIN.',
                true
            );
        };
    }

    // Info button for Copy Logs
    const domCrashLogInfo = document.getElementById('crash-log-info');
    if (domCrashLogInfo) {
        domCrashLogInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm(
                'Logs',
                'Copies error logs and crash details to your clipboard.<br><br>Share with developers when reporting bugs to help diagnose issues.',
                true
            );
        };
    }

    domSettingsLogoutInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Logout', 'Logout will erase the local database and remove all stored keys. You will lose access to group chats unless you have a backup.', true);
    };

    if (domRemoteSignerReauthBtn) {
        domRemoteSignerReauthBtn.onclick = async (e) => {
            e.preventDefault();
            e.stopPropagation();
            // NIP-55 re-auth is a direct Amber intent (no QR/paste), so dispatch
            // on the actual account type rather than assuming bunker.
            const nip55 = await invoke('get_nip55_status').catch(() => null);
            if (nip55) {
                try {
                    await invoke('reauthorize_nip55');
                    if (typeof showToast === 'function') showToast('Signer re-authorized.');
                    refreshRemoteSignerCard();
                } catch (err) {
                    popupConfirm(String(err), '', true, '', 'vector_warning.svg');
                }
                return;
            }
            if (typeof window.showBunkerForm === 'function') {
                window.showBunkerForm('reauth');
            }
        };
    }

    // Footer Hyperlinks
    document.getElementById('footer-donate').onclick = (e) => {
        e.preventDefault();
        openUrl('https://vector-privacy.gitbook.io/vector-privacy/vector-messenger/more/donations');
    };
    document.getElementById('footer-gitbook').onclick = (e) => {
        e.preventDefault();
        openUrl('https://docs.vectorapp.io');
    };
    document.getElementById('footer-privacy').onclick = (e) => {
        e.preventDefault();
        openUrl('https://vectorapp.io/privacy-policy');
    };
}
