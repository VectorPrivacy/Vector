// Native calls: the overlay's helper bag, the backend's events, and the reload hydrate.
// The engine lives in Rust; this side only asks and renders.

/** The voice processing switches: applied by the backend mid-call, then saved. */
async function setCallAudioSettings(patch) {
    const a = VectorSvelte.callAudio();
    const next = {
        auto_gain: patch.autoGain ?? a.autoGain,
        echo_cancel: patch.echoCancel ?? a.echoCancel,
        noise_suppress: patch.noiseSuppress ?? a.noiseSuppress,
    };
    VectorSvelte.setCallAudio(next);
    try {
        await invoke('call_audio_settings_set', { settings: next });
    } catch (e) {
        VectorSvelte.showToast(String(e));
    }
}

async function micTestStart() {
    try {
        await invoke('call_mic_test_start');
        VectorSvelte.setMicTest(true, 0);
    } catch (e) {
        VectorSvelte.showToast(String(e));
    }
}

async function micTestStop() {
    VectorSvelte.setMicTest(false, 0);
    await invoke('call_mic_test_stop').catch(() => {});
}

async function loadAudioDevices() {
    try {
        VectorSvelte.setAudioDevices(await invoke('audio_devices_list'));
    } catch (_) {}
}

/** `name` null means the system default; the change applies to live streams at once. */
async function setAudioDevice(kind, name) {
    const d = VectorSvelte.callAudio().devices;
    const prefs = { input: kind === 'input' ? name : d.input, output: kind === 'output' ? name : d.output };
    try {
        await invoke('audio_devices_set', { prefs });
    } catch (e) {
        VectorSvelte.showToast(String(e));
    }
    await loadAudioDevices();
}

// The Settings screen's Calls section takes the same helpers; the bag is settings.js's.
SETTINGS_HELPERS.calls = {
    setAudio: (patch) => setCallAudioSettings(patch),
    micTestStart: () => micTestStart(),
    micTestStop: () => micTestStop(),
    loadDevices: () => loadAudioDevices(),
    setDevice: (kind, name) => setAudioDevice(kind, name),
};

function registerCallScreen() {
    VectorSvelte.setScreen('call', {
        // Lazy: the helpers live in scripts that load after this one evaluates.
        h: {
            accept: () => invoke('call_accept').catch((e) => VectorSvelte.showToast(String(e))),
            reject: () => invoke('call_reject').catch(() => {}),
            hangup: () => invoke('call_hangup').catch(() => {}),
            setMuted: (on) => invoke('call_set_muted', { muted: on }).catch(() => {}),
            setVolume: (volume) => invoke('call_set_volume', { volume }).catch(() => {}),
            setVideo: (kind) => startVideo(kind),
            setVideoPause: (on) => invoke('call_video_pause', { on }).catch(() => {}),
            peerCanvas: (el) => attachPeerCanvas(el),
            selfPreview: (el) => attachSelfPreview(el),
            setAudio: (patch) => setCallAudioSettings(patch),
            getProfile: (npub) => getProfile(npub),
            getName: (x) => getName(x),
            getProfileAvatarSrc: (p) => getProfileAvatarSrc(p),
            openChat: (npub) => openChat(npub),
        },
    });
    // A reloaded webview finds the call the backend still holds, and the switches it saved.
    invoke('call_status').then((s) => { if (s) { VectorSvelte.setCallState(s); callVideoOnState(s); } }).catch(() => {});
    invoke('call_audio_settings_get').then((s) => VectorSvelte.setCallAudio(s)).catch(() => {});
    loadAudioDevices();
}

/** Ring a DM contact. The Chat header's call button lands here. */
async function startCall(npub, video = false) {
    try {
        await invoke('call_start', { npub, video });
    } catch (e) {
        VectorSvelte.showToast(String(e));
    }
}

document.addEventListener('DOMContentLoaded', registerCallScreen, { once: true });
