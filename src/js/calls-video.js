// Video on a call: the camera and the screen out, the peer's pictures in. Capture
// happens here, on the main thread, because only a document can open a camera;
// everything after that (encode, socket, decode, paint) runs in the worker. Each
// track (camera, screen) has its own stream, capture element and preview.

const VIDEO_PROBE_SIZE = { width: 1280, height: 720 };
let videoWorker = null;
let videoCaps = { encode: [], decode: [] };
let videoLinkOpen = false;
let videoLinkCallId = null;
let videoProbe = null;
let decodeErrorShown = false;
// Per track: the stream, the detached element frames are pulled from, the preview.
const selfTracks = { camera: null, screen: null };
const captureEls = { camera: null, screen: null };
const previewEls = { camera: null, screen: null };
const videoStarting = { camera: false, screen: false };

/** What this device can encode and decode; told to the backend for the next offer or answer. */
async function probeVideoCaps() {
    const encode = [];
    const decode = [];
    if (window.VideoEncoder && window.VideoDecoder && navigator.mediaDevices) {
        const tries = [
            ['h264', { codec: 'avc1.42E01F', avc: { format: 'annexb' } }],
            ['vp8', { codec: 'vp8' }],
        ];
        for (const [name, base] of tries) {
            try {
                const e = await VideoEncoder.isConfigSupported({ ...base, ...VIDEO_PROBE_SIZE, bitrate: 1_000_000, framerate: 30, latencyMode: 'realtime' });
                if (e.supported) encode.push(name);
            } catch (_) {}
            try {
                const d = await VideoDecoder.isConfigSupported({ codec: base.codec, codedWidth: VIDEO_PROBE_SIZE.width, codedHeight: VIDEO_PROBE_SIZE.height });
                if (d.supported) decode.push(name);
            } catch (_) {}
        }
    }
    videoCaps = { encode, decode };
    VectorSvelte.setVideoCaps(videoCaps);
    invoke('call_video_caps', { encode, decode }).catch(() => {});
}

function ensureVideoWorker() {
    if (videoWorker) return videoWorker;
    videoWorker = new Worker('js/calls-video-worker.js');
    videoWorker.onmessage = (e) => {
        const m = e.data;
        switch (m.t) {
            case 'link':
                videoLinkOpen = m.open;
                VectorSvelte.setVideoLink(m.open);
                // The backend dropped the socket: nothing we capture goes anywhere now.
                if (!m.open) stopAllVideo(false);
                break;
            case 'painted': VectorSvelte.setVideoPeerSize(m.kind, m.width, m.height); break;
            case 'stats': VectorSvelte.setVideoStats(m.tracks); break;
            case 'constrain': constrainCapture(m); break;
            case 'error': VectorSvelte.showToast(m.message); stopVideo(m.kind, true); break;
            // Their picture cannot be decoded here; said once per call, then the peer
            // is asked for another codec.
            case 'decode_error':
                if (!decodeErrorShown) { decodeErrorShown = true; VectorSvelte.showToast(`Could not decode their ${m.codec} video here (${m.message}); asking them for another codec`); }
                break;
        }
    };
    return videoWorker;
}

/** Every call_state: open the link when a call goes live, tear video down when it ends.
 *  The link is worth opening only when both sides can take video: a peer that named
 *  no decoders is on a build without any of this. */
async function callVideoOnState(s) {
    if (!s || s.phase !== 'active') {
        stopAllVideo(false);
        if (videoWorker && (videoLinkOpen || videoLinkCallId)) videoWorker.postMessage({ t: 'close' });
        videoLinkCallId = null;
        return;
    }
    if (videoLinkCallId === s.id || !(s.peer_decodes || []).length) return;
    // A reloaded page hears about the call before its probe has finished.
    if (videoProbe) await videoProbe;
    if (videoLinkCallId === s.id || !videoCaps.decode.length) return;
    videoLinkCallId = s.id;
    decodeErrorShown = false;
    invoke('call_video_link').then((url) => {
        if (videoLinkCallId !== s.id) return;
        ensureVideoWorker().postMessage({ t: 'open', url, caps: videoCaps });
    }).catch(() => { videoLinkCallId = null; });
}

/** A request the platform never answers, rather than refuses, leaves the button dead:
 *  WebView2 opens no surface picker and settles getDisplayMedia neither way. */
const VIDEO_SOURCE_WAIT_MS = 120000;

function waitForSource(request) {
    let settled = false;
    const mark = (fn) => (v) => { settled = true; return fn(v); };
    const giveUp = new Promise((_, reject) => setTimeout(() => {
        if (settled) return;
        // A source that arrives after we gave up must not hold the device open.
        request.then((s) => s.getTracks().forEach((t) => t.stop()), () => {});
        const e = new Error('the system never answered');
        e.name = 'SourceTimeout';
        reject(e);
    }, VIDEO_SOURCE_WAIT_MS));
    return Promise.race([request.then(mark((s) => s), mark((e) => { throw e; })), giveUp]);
}

/** What to tell the user when a source request fails; null when their own refusal needs no toast. */
function sourceFailureMessage(kind, e) {
    const screen = kind === 'screen';
    if (e && e.name === 'SourceTimeout') {
        return screen ? 'Screen sharing is not available on this system' : 'The camera never answered';
    }
    // The OS privacy switch raises NotAllowedError without ever asking, unlike a refusal at the prompt.
    if (e && e.name === 'NotAllowedError') {
        return /\bsystem\b/i.test(e.message || '')
            ? (screen ? 'Screen sharing is blocked in your system privacy settings' : 'Camera access is blocked in your system privacy settings')
            : null;
    }
    return screen ? 'Could not share the screen' : 'Could not open the camera';
}

function requestSource(kind) {
    return waitForSource(kind === 'screen'
        ? navigator.mediaDevices.getDisplayMedia({ video: { frameRate: 15 }, audio: false, selfBrowserSurface: 'exclude' })
        : navigator.mediaDevices.getUserMedia({ video: { width: 1280, height: 720, frameRate: 30 }, audio: false }));
}

/** Pull frames off a playing element into the worker until the stream is replaced. */
function pumpFrames(kind, stream) {
    const worker = ensureVideoWorker();
    // A detached element plays the stream so frames can be pulled off it.
    const el = document.createElement('video');
    el.muted = true;
    el.playsInline = true;
    el.srcObject = stream;
    captureEls[kind] = el;
    el.play().catch(() => {});
    const pull = () => {
        if (captureEls[kind] !== el) return;
        if (el.videoWidth) {
            const frame = new VideoFrame(el, { timestamp: Math.round(performance.now() * 1000) });
            worker.postMessage({ t: 'frame', kind, frame }, [frame]);
        }
        el.requestVideoFrameCallback(pull);
    };
    el.requestVideoFrameCallback(pull);
}

/** Turn one of our pictures on or off. Camera and screen are independent. */
async function startVideo(kind, on = true) {
    if (!on) return stopVideo(kind, true);
    if (selfTracks[kind] || videoStarting[kind]) return;
    let stream;
    videoStarting[kind] = true;
    try {
        stream = await requestSource(kind);
    } catch (e) {
        const message = sourceFailureMessage(kind, e);
        if (message) VectorSvelte.showToast(message);
        return;
    } finally {
        videoStarting[kind] = false;
    }
    try {
        await invoke('call_video_set', { kind, on: true });
    } catch (e) {
        VectorSvelte.showToast(String(e));
        stream.getTracks().forEach((t) => t.stop());
        return;
    }
    attachSource(kind, stream);
    ensureVideoWorker().postMessage({ t: 'capture', kind, fps: kind === 'screen' ? 15 : 30, kbps: kind === 'screen' ? 1000 : 800 });
}

function attachSource(kind, stream) {
    selfTracks[kind] = stream;
    const track = stream.getVideoTracks()[0];
    // The OS's own "stop sharing" control ends the track from outside.
    track.onended = () => { if (selfTracks[kind] === stream) stopVideo(kind, true); };
    if (previewEls[kind]) previewEls[kind].srcObject = stream;
    pumpFrames(kind, stream);
}

/** Pick another window or screen while the share is on; the send never stops. */
async function changeScreenSource() {
    if (!selfTracks.screen || videoStarting.screen) return;
    let stream;
    videoStarting.screen = true;
    try {
        stream = await requestSource('screen');
    } catch (e) {
        const message = sourceFailureMessage('screen', e);
        if (message) VectorSvelte.showToast(message);
        return;
    } finally {
        videoStarting.screen = false;
    }
    if (!selfTracks.screen) { stream.getTracks().forEach((t) => t.stop()); return; }
    const old = selfTracks.screen;
    attachSource('screen', stream);
    old.getTracks().forEach((t) => t.stop());
}

/** Stop one of our pictures. `tell` is false when the call is already gone. */
async function stopVideo(kind, tell = true) {
    const was = selfTracks[kind];
    selfTracks[kind] = null;
    if (captureEls[kind]) { captureEls[kind].srcObject = null; captureEls[kind] = null; }
    if (was) was.getTracks().forEach((t) => t.stop());
    if (previewEls[kind]) previewEls[kind].srcObject = null;
    if (videoWorker) videoWorker.postMessage({ t: 'stop', kind });
    if (tell && was) await invoke('call_video_set', { kind, on: false }).catch(() => {});
}

function stopAllVideo(tell) {
    for (const kind of ['camera', 'screen']) if (selfTracks[kind]) stopVideo(kind, tell);
}

/** The ladder moved: ask the device for that size and rate, so no frame is captured
 *  bigger than it will be sent. Best effort; the worker scales whatever still arrives. */
function constrainCapture(m) {
    const stream = selfTracks[m.kind];
    const track = stream && stream.getVideoTracks()[0];
    if (!track || track.readyState !== 'live') return;
    const c = { frameRate: m.fps };
    if (m.kind !== 'screen' && m.width && m.height) { c.width = m.width; c.height = m.height; }
    track.applyConstraints(c).catch(() => {});
}

/** A canvas for one of the peer's pictures, handed to the worker; null when it unmounts. */
function attachPeerCanvas(kind, el) {
    if (!el) {
        if (videoWorker) videoWorker.postMessage({ t: 'canvas', kind, canvas: null });
        return;
    }
    const off = el.transferControlToOffscreen();
    ensureVideoWorker().postMessage({ t: 'canvas', kind, canvas: off }, [off]);
}

/** The preview element for one of our pictures; null when it unmounts. */
function attachSelfPreview(kind, el) {
    previewEls[kind] = el;
    if (el) el.srcObject = selfTracks[kind];
}

// A hidden page cannot capture; an honest "camera off" beats a frozen picture. And
// nobody here is watching theirs, so they may stop spending upload on it until we are back.
document.addEventListener('visibilitychange', () => {
    const hidden = document.visibilityState === 'hidden';
    if (hidden && selfTracks.camera) stopVideo('camera', true);
    if (videoLinkCallId) invoke('call_video_pause', { on: hidden }).catch(() => {});
});

document.addEventListener('DOMContentLoaded', () => { videoProbe = probeVideoCaps(); }, { once: true });
