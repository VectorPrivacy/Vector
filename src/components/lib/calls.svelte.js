// The one call: a mirror of the backend's `call_state` events, the latest stats line,
// and a minute of history for the connection graph. `receivedAt` lets the timer run
// between events.
const HISTORY = 60;

const c = $state({ id: null, peer: null, outgoing: false, phase: null, reason: null,
    muted: false, peerMuted: false, volume: 1, activeMs: 0, receivedAt: 0,
    stats: null, history: [], quality: null, levels: { mic: 0, peer: 0 }, tick: 0,
    // Video: what each side sends ('off' | 'camera' | 'screen'), what the peer can decode,
    // whether the offer was for a video call, and whether the peer has hidden our picture.
    videoMine: 'off', videoPeer: 'off', peerDecodes: [], videoOffered: false, pausedByPeer: false });
// The device's video ability (from the boot probe), the link to the worker, and what
// the worker reports: the peer's picture size and the encoder's numbers.
const video = $state({ encode: [], decode: [], link: false, peerWidth: 0, peerHeight: 0,
    enc: { fps: 0, kbps: 0, width: 0, height: 0 }, decFps: 0,
    // The stage collapsed back to the pill while video is on.
    stageHidden: false });
// The voice processing switches and the microphone test, shared by the pill and Settings.
const audio = $state({ autoGain: true, echoCancel: true, noiseSuppress: true, loaded: false,
    micTest: false, micLevel: 0,
    // Every microphone and speaker on the machine, the defaults, and the preference (null = default).
    devices: { inputs: [], outputs: [], defaultInput: '', defaultOutput: '', input: null, output: null } });
let endedTimer = null;

export function callState() { return c; }
export function callAudio() { return audio; }
export function callVideo() { return video; }

export function setVideoCaps(caps) {
    video.encode = caps?.encode || [];
    video.decode = caps?.decode || [];
}
export function setVideoLink(open) { video.link = !!open; }
export function setVideoPeerSize(w, h) { video.peerWidth = w; video.peerHeight = h; }
export function setVideoStats(enc, decFps) {
    if (enc) video.enc = enc;
    video.decFps = decFps || 0;
}
export function setStageHidden(on) { video.stageHidden = !!on; }

/** Ten times a second during a call: how loud each side is, 0 to 1. */
export function setCallLevels(l) {
    if (!l || l.id !== c.id) return;
    c.levels = { mic: l.mic, peer: l.peer };
}

/** The switches as the backend holds them. */
export function setCallAudio(s) {
    if (!s) return;
    audio.autoGain = !!s.auto_gain; audio.echoCancel = !!s.echo_cancel; audio.noiseSuppress = !!s.noise_suppress;
    audio.loaded = true;
}

/** The backend's device list and preference. */
export function setAudioDevices(l) {
    if (!l) return;
    audio.devices = {
        inputs: l.inputs || [], outputs: l.outputs || [],
        defaultInput: l.default_input || '', defaultOutput: l.default_output || '',
        input: l.prefs?.input ?? null, output: l.prefs?.output ?? null,
    };
}

export function setMicTest(on, level) {
    if (on != null) audio.micTest = !!on;
    if (level != null) audio.micLevel = level;
    if (!audio.micTest) audio.micLevel = 0;
}

/** `s` is the backend CallState, or null when there is no call. */
export function setCallState(s) {
    clearTimeout(endedTimer);
    endedTimer = null;
    if (!s) {
        c.id = null; c.phase = null; c.stats = null; c.history = []; c.quality = null; c.tick++;
        return;
    }
    if (s.id !== c.id) { c.stats = null; c.history = []; c.quality = null; }
    c.id = s.id; c.peer = s.peer; c.outgoing = s.outgoing; c.phase = s.phase;
    c.reason = s.reason || null; c.muted = s.muted; c.peerMuted = s.peer_muted;
    c.volume = typeof s.volume === 'number' ? s.volume : 1;
    c.activeMs = s.active_ms; c.receivedAt = Date.now();
    c.videoMine = s.video_mine || 'off'; c.videoPeer = s.video_peer || 'off';
    c.peerDecodes = s.peer_decodes || []; c.videoOffered = !!s.video_offered; c.pausedByPeer = !!s.paused_by_peer;
    if (s.phase !== 'active') { video.stageHidden = false; video.peerWidth = 0; video.peerHeight = 0; }
    // An ended call lingers long enough to read why.
    if (s.phase === 'ended') {
        endedTimer = setTimeout(() => {
            if (c.id === s.id && c.phase === 'ended') { c.id = null; c.phase = null; c.tick++; }
        }, 2500);
    }
    c.tick++;
}

/** One stats line per second from the backend. */
export function setCallStats(st) {
    if (!st || st.id !== c.id) return;
    const prev = c.stats;
    // Lost audio for THIS second: frames the speaker had to fill in, over frames that arrived.
    let lost = 0;
    if (prev && st.received > prev.received) {
        lost = 100 * (st.concealed - prev.concealed) / (st.received - prev.received);
    }
    c.stats = st;
    c.history.push({ rtt: st.rtt_ms, lost: Math.max(0, lost) });
    if (c.history.length > HISTORY) c.history.shift();
    c.quality = qualityOf(c.history);
}

// Judged on the last ten seconds so a spike shows and then clears.
function qualityOf(history) {
    const recent = history.slice(-10);
    if (!recent.length) return null;
    const avg = (k) => recent.reduce((a, r) => a + r[k], 0) / recent.length;
    const lost = avg('lost');
    const rtt = avg('rtt');
    if (lost < 0.5 && rtt < 150) return 'excellent';
    if (lost < 2 && rtt < 300) return 'good';
    if (lost < 5 && rtt < 500) return 'fair';
    return 'poor';
}
