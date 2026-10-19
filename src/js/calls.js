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
    direct: ['Direct Connection', 'Off, calls and Mini Apps reach people through a relay, so they never learn your IP address. On, you connect straight to them for lower latency, and they can see your IP address.'],
};

SETTINGS_HELPERS.calls = {
    setAudio: (patch) => setCallAudioSettings(patch),
    explain: (kind) => popupConfirm(...CALL_EXPLAINERS[kind], true),
    micTestStart: () => micTestStart(),
    micTestStop: () => micTestStop(),
    loadDevices: () => loadAudioDevices(),
    setDevice: (kind, name) => setAudioDevice(kind, name),
    setDirect: (on) => invoke('realtime_direct_set', { on }).then(() => VectorSvelte.setCallDirect(on)).catch((e) => VectorSvelte.showToast(String(e))),
};

function registerCallScreen() {
    VectorSvelte.setScreen('call', {
        // Lazy: the helpers live in scripts that load after this one evaluates.
        h: {
            accept: () => acceptCall(),
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
    invoke('realtime_direct_get').then((on) => VectorSvelte.setCallDirect(on)).catch(() => {});
}

/** A held quality rung or frame rate for one of our pictures; null lets the ladder decide. */
async function setVideoPrefs(kind, rung, fps) {
    VectorSvelte.setVideoPrefs(kind, { rung, fps });
    await invoke('call_video_prefs', { kind, rung, fps }).catch((e) => VectorSvelte.showToast(String(e)));
}

/** Ring a DM contact. The Chat header's call button lands here. */
async function startCall(npub, video = false, asked = false) {
    try {
        await invoke('call_start', { npub, video });
    } catch (e) {
        if (!asked && String(e) === 'realtime_consent_required') {
            if (await askRealtimeConsent()) return startCall(npub, video, true);
            return;
        }
        VectorSvelte.showToast(String(e));
    }
}

/** Answer the ringing call; off Clearnet the account agrees first. */
async function acceptCall(asked = false) {
    try {
        await invoke('call_accept');
    } catch (e) {
        if (!asked && String(e) === 'realtime_consent_required') {
            if (await askRealtimeConsent()) return acceptCall(true);
            return;
        }
        VectorSvelte.showToast(String(e));
    }
}

// ── Voice channels ──

/** Join a voice channel's call; off Clearnet the account agrees first. */
async function joinVoiceChannel(communityId, channel, asked = false) {
    if (VectorSvelte.voiceSession()?.channelId === channel.id || VectorSvelte.voiceJoining()) return;
    VectorSvelte.setVoiceJoining(channel.id);
    try {
        voiceOnState(await invoke('voice_join', { communityId, channelId: channel.id }));
    } catch (e) {
        if (!asked && String(e) === 'realtime_consent_required') {
            VectorSvelte.setVoiceJoining(null);
            if (await askRealtimeConsent()) return joinVoiceChannel(communityId, channel, true);
            return;
        }
        VectorSvelte.showToast(String(e));
    } finally {
        VectorSvelte.setVoiceJoining(null);
    }
}

let _voiceChannel = null;
/** The backend's voice state: the bar mirrors it, and the channel we joined or left re-reads who is in it. */
function voiceOnState(s) {
    VectorSvelte.setVoiceSession(s);
    voiceVideoOnState(s);
    const now = s?.channel_id || null;
    if (_voiceChannel && _voiceChannel !== now) refreshVoicePresent(_voiceChannel);
    if (now) refreshVoicePresent(now);
    _voiceChannel = now;
}

function refreshVoicePresent(channelId) {
    if (!channelId) return;
    invoke('voice_present', { channelId }).then((npubs) => VectorSvelte.setVoicePresent(channelId, npubs)).catch(() => {});
}

const voiceChannelOf = (communityId, channelId) => (getCommunityChannels(communityId) || []).find((c) => c.id === channelId) || null;

/** A member of a voice call: how loud they play here, and what a moderator may do to them. */
async function openVoiceMemberMenu(communityId, channelId, npub, x, y) {
    if (npub === strPubkey) return;
    const live = VectorSvelte.voiceSession();
    const peer = live?.channelId === channelId ? live.peers.find((p) => p.npub === npub) : null;
    const present = await invoke('voice_present', { channelId }).catch(() => []);
    const them = present.find((p) => p.npub === npub);
    const powers = await invoke('voice_powers', { communityId, npub }).catch(() => ({}));
    const volume = peer ? peer.volume : 1;
    const setVolume = (v) => invoke('voice_set_volume', { npub, volume: v }).catch((e) => VectorSvelte.showToast(String(e)));
    const items = [
        { label: volume === 0 ? 'Unmute for Me' : 'Mute for Me', icon: volume === 0 ? 'volume-max' : 'volume-mute', onClick: () => setVolume(volume === 0 ? 1 : 0) },
        ...[0.5, 1, 1.5, 2].map((v) => ({ label: `Volume ${Math.round(v * 100)}%`, icon: Math.abs(volume - v) < 0.01 ? 'check' : undefined, onClick: () => setVolume(v) })),
    ];
    const moderate = (action, to = null) => invoke('voice_moderate', { communityId, channelId, npub, action, to }).catch((e) => VectorSvelte.showToast(String(e)));
    const mods = [];
    if (them && powers.mute) mods.push(them.server_muted ? { label: 'Server Unmute', icon: 'mic-on', onClick: () => moderate('unmute') } : { label: 'Server Mute', icon: 'mic-off', onClick: () => moderate('mute') });
    if (them && powers.deafen) mods.push(them.server_deafened ? { label: 'Server Undeafen', icon: 'volume-max', onClick: () => moderate('undeafen') } : { label: 'Server Deafen', icon: 'volume-mute', onClick: () => moderate('deafen') });
    if (them && powers.move) {
        for (const ch of (getCommunityChannels(communityId) || []).filter((c) => c.view === 'voice' && c.id !== channelId)) {
            mods.push({ label: `Move to ${ch.name}`, icon: 'volume-max', onClick: () => moderate('move', ch.id) });
        }
        mods.push({ label: 'Disconnect', icon: 'x', danger: true, onClick: () => moderate('disconnect') });
    }
    if (mods.length) items.push({ divider: true }, ...mods);
    items.push({ divider: true }, { label: 'View Profile', icon: 'user-circle', onClick: () => openProfile(getProfile(npub) || { id: npub }) });
    showContextMenu({ x, y, items });
}

function registerVoiceScreen() {
    VectorSvelte.setVoiceCallHandlers({
        join: (communityId, channelId) => joinVoiceChannel(communityId, { id: channelId }),
        leave: () => invoke('voice_leave').catch(() => {}),
        setMuted: (on) => invoke('voice_set_muted', { on }).catch((e) => VectorSvelte.showToast(String(e))),
        setDeafened: (on) => invoke('voice_set_deafened', { on }).catch((e) => VectorSvelte.showToast(String(e))),
        micAccess: () => promptMicAccess(),
        getName: (npub) => getName(npub),
        avatarSrc: (npub) => getProfileAvatarSrc(getProfile(npub)),
        myNpub: () => strPubkey,
        memberMenu: (communityId, channelId, npub, x, y) => openVoiceMemberMenu(communityId, channelId, npub, x, y),
        keyHint: (key) => voiceKeyHint(key),
        canShare: () => !!window.VideoEncoder && !!navigator.mediaDevices?.getDisplayMedia,
        canCamera: () => !!window.VideoEncoder && !!navigator.mediaDevices?.getUserMedia,
        share: (on) => startVideo('screen', on),
        camera: (on) => startVideo('camera', on),
        watch: (npub, want) => invoke('voice_watch', { npub, screen: !!want.screen, camera: !!want.camera }).catch(() => {}),
        videoCanvas: (pubkey, kind, el, gone) => attachVoiceVideo(pubkey, kind, el, gone),
        selfPreview: (kind, el) => attachSelfPreview(kind, el),
    });
    VectorSvelte.setScreen('voice', {
        h: {
            setMuted: (on) => invoke('voice_set_muted', { on }).catch((e) => VectorSvelte.showToast(String(e))),
            setDeafened: (on) => invoke('voice_set_deafened', { on }).catch((e) => VectorSvelte.showToast(String(e))),
            leave: () => invoke('voice_leave').catch(() => {}),
            keyHint: (key) => voiceKeyHint(key),
            share: (on) => startVideo('screen', on),
            camera: (on) => startVideo('camera', on),
            canShare: () => !!window.VideoEncoder && !!navigator.mediaDevices?.getDisplayMedia,
            canCamera: () => !!window.VideoEncoder && !!navigator.mediaDevices?.getUserMedia,
            channelName: (communityId, channelId) => voiceChannelOf(communityId, channelId)?.name || 'Voice',
            communityAvatar: (communityId) => findCommunityChat(communityId)?.metadata?.avatar_cached || null,
            convertFileSrc: (p) => convertFileSrc(p),
            openChannel: (communityId, channelId) => {
                const ch = voiceChannelOf(communityId, channelId);
                if (ch) openCommunityChannel(communityId, ch);
            },
        },
    });
    invoke('voice_state').then((s) => voiceOnState(s)).catch(() => {});
}

/** The mute/deafen shortcut as this OS writes it. */
function voiceKeyHint(key) {
    return platformFeatures?.os === 'macos' ? `⌘⇧${key}` : `Ctrl+Shift+${key}`;
}

// Mute and deafen from anywhere in the app while in a voice call: Cmd/Ctrl+Shift+M and +D.
document.addEventListener('keydown', (e) => {
    if (!(e.metaKey || e.ctrlKey) || !e.shiftKey || e.altKey) return;
    const v = VectorSvelte.voiceSession();
    if (!v) return;
    if (e.code === 'KeyM') {
        e.preventDefault();
        VectorSvelte.toggleVoiceMic(v, {
            setMuted: (on) => invoke('voice_set_muted', { on }).catch((err) => VectorSvelte.showToast(String(err))),
            setDeafened: (on) => invoke('voice_set_deafened', { on }).catch(() => {}),
        });
    } else if (e.code === 'KeyD') {
        e.preventDefault();
        invoke('voice_set_deafened', { on: !v.deafened }).catch(() => {});
    }
});

document.addEventListener('DOMContentLoaded', registerCallScreen, { once: true });
document.addEventListener('DOMContentLoaded', registerVoiceScreen, { once: true });
