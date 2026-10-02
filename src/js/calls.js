// Native calls: the overlay's helper bag, the backend's events, and the reload hydrate.
// The engine lives in Rust; this side only asks and renders.

/** The voice processing switches and volumes: applied by the backend mid-call, then saved. */
async function setCallAudioSettings(patch) {
    const a = VectorSvelte.callAudio();
    const next = {
        auto_gain: patch.autoGain ?? a.autoGain,
        echo_cancel: patch.echoCancel ?? a.echoCancel,
        noise_suppress: patch.noiseSuppress ?? a.noiseSuppress,
        mic_volume: patch.micVolume ?? a.micVolume,
        speaker_volume: patch.speakerVolume ?? a.speakerVolume,
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
        // Access may have just been granted: the microphones can be listed now.
        loadAudioDevices();
    } catch (e) {
        if (String(e) === 'MIC_DENIED') promptMicAccess();
        else VectorSvelte.showToast(String(e));
    }
}

/**
 * Microphone access was refused. The system won't ask a second time, so this says where it
 * is turned back on, and opens that page where the platform can.
 */
async function promptMicAccess() {
    const os = platformFeatures.os;
    const where = os === 'macos' ? 'System Settings → Privacy & Security → Microphone'
        : os === 'windows' ? 'Settings → Privacy → Microphone' : "your device's app settings";
    const canOpen = os === 'macos' || os === 'windows';
    const text = `Vector doesn't have access to your microphone. Allow it in ${where}, then try again.`;
    const open = await popupConfirm('Microphone Access', text, !canOpen, '', 'vector_warning.svg', '', canOpen ? 'Open Settings' : null);
    if (open && canOpen) invoke('open_mic_settings').catch((e) => VectorSvelte.showToast(String(e)));
}

let _micOffToldFor = null;
/** Once per call: it went ahead without a microphone, so say why it is muted. */
function callMicOnState(s) {
    if (!s || !s.mic_off || s.phase !== 'active' || _micOffToldFor === s.id) return;
    _micOffToldFor = s.id;
    VectorSvelte.showToast("You're muted: Vector doesn't have access to your microphone.");
}

async function micTestStop() {
    VectorSvelte.setMicTest(false, 0);
    await invoke('call_mic_test_stop').catch(() => {});
}

let _audioDevicesShown = false;
async function loadAudioDevices() {
    _audioDevicesShown = true;
    try {
        VectorSvelte.setAudioDevices(await invoke('audio_devices_list'));
    } catch (_) {}
}

/** A device came or went: refresh the pickers, only if they were ever shown. */
function reloadAudioDevices() {
    if (_audioDevicesShown) loadAudioDevices();
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
const CALL_EXPLAINERS = {
    autoGain: ['Automatic Gain', 'Lifts a quiet microphone so nobody has to shout, and eases off a loud one.'],
    echoCancel: ['Echo Cancellation', 'Keeps your speakers out of your microphone, so the other side never hears themselves back.'],
    noiseSuppress: ['Noise Suppression', 'Takes down fans, keyboards and hum while you talk.'],
};

SETTINGS_HELPERS.calls = {
    setAudio: (patch) => setCallAudioSettings(patch),
    explain: (kind) => popupConfirm(...CALL_EXPLAINERS[kind], true),
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
            micAccess: () => promptMicAccess(),
            setVolume: (volume) => invoke('call_set_volume', { volume }).catch(() => {}),
            setVideo: (kind, on) => startVideo(kind, on),
            changeScreen: () => changeScreenSource(),
            setVideoPrefs: (kind, rung, fps) => setVideoPrefs(kind, rung, fps),
            setShareAudio: (on) => setShareAudio(on),
            setShareVolume: (volume) => invoke('call_set_share_volume', { volume }).catch(() => {}),
            peerCanvas: (kind, el, gone) => attachPeerCanvas(kind, el, gone),
            selfPreview: (kind, el) => attachSelfPreview(kind, el),
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
}

/** A held quality rung or frame rate for one of our pictures; null lets the ladder decide. */
async function setVideoPrefs(kind, rung, fps) {
    VectorSvelte.setVideoPrefs(kind, { rung, fps });
    await invoke('call_video_prefs', { kind, rung, fps }).catch((e) => VectorSvelte.showToast(String(e)));
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
