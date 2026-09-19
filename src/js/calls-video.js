// Video on a call: the camera or the screen in, the peer's picture out. Capture
// happens here, on the main thread, because only a document can open a camera;
// everything after that (encode, socket, decode, paint) runs in the worker.

const VIDEO_PROBE_SIZE = { width: 1280, height: 720 };
let videoWorker = null;
let videoCaps = { encode: [], decode: [] };
let videoLinkOpen = false;
let videoLinkCallId = null;
let selfStream = null;
let selfKind = 'off';
let captureVideo = null;
let selfPreviewEl = null;
let videoProbe = null;
let decodeErrorShown = false;

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
                if (!m.open && selfKind !== 'off') stopVideo(false);
                break;
            case 'painted': VectorSvelte.setVideoPeerSize(m.width, m.height); break;
            case 'stats': VectorSvelte.setVideoStats(m.enc, m.decFps); break;
            case 'constrain': constrainCapture(m); break;
            case 'error': VectorSvelte.showToast(m.message); stopVideo(); break;
            // Their picture cannot be decoded here; said once per call, then the peer
            // is asked for keyframes in the hope a later one works.
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
        if (selfKind !== 'off') stopVideo(false);
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
const VIDEO_SOURCE_WAIT_MS = 45000;
let videoStarting = false;

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

/** Turn the camera or the screen on ('camera' | 'screen') or everything off ('off'). */
async function startVideo(kind) {
    if (kind === 'off' || kind === selfKind) return stopVideo(true);
    if (videoStarting) return;
    if (selfKind !== 'off') await stopVideo(true);
    let stream;
    videoStarting = true;
    try {
        stream = await waitForSource(kind === 'screen'
            ? navigator.mediaDevices.getDisplayMedia({ video: { frameRate: 15 }, audio: false, selfBrowserSurface: 'exclude' })
            : navigator.mediaDevices.getUserMedia({ video: { width: 1280, height: 720, frameRate: 30 }, audio: false }));
    } catch (e) {
        const message = sourceFailureMessage(kind, e);
        if (message) VectorSvelte.showToast(message);
        return;
    } finally {
        videoStarting = false;
    }
    try {
        await invoke('call_video_set', { kind });
    } catch (e) {
        VectorSvelte.showToast(String(e));
        stream.getTracks().forEach((t) => t.stop());
        return;
    }
    selfStream = stream;
    selfKind = kind;
    const track = stream.getVideoTracks()[0];
    // The OS's own "stop sharing" control ends the track from outside.
    track.onended = () => { if (selfStream === stream) stopVideo(true); };
    if (selfPreviewEl) selfPreviewEl.srcObject = stream;
    const worker = ensureVideoWorker();
    worker.postMessage({ t: 'capture', kind, fps: kind === 'screen' ? 15 : 30, kbps: kind === 'screen' ? 1000 : 800 });
    // A detached element plays the stream so frames can be pulled off it.
    captureVideo = document.createElement('video');
    captureVideo.muted = true;
    captureVideo.playsInline = true;
    captureVideo.srcObject = stream;
    const el = captureVideo;
    try { await el.play(); } catch (_) {}
    const pull = () => {
        if (captureVideo !== el) return;
        if (el.videoWidth) {
            const frame = new VideoFrame(el, { timestamp: Math.round(performance.now() * 1000) });
            worker.postMessage({ t: 'frame', frame }, [frame]);
        }
        el.requestVideoFrameCallback(pull);
    };
    el.requestVideoFrameCallback(pull);
}

/** The ladder moved: ask the device for that size and rate, so no frame is captured
 *  bigger than it will be sent. Best effort; the worker scales whatever still arrives. */
function constrainCapture(m) {
    const track = selfStream && selfStream.getVideoTracks()[0];
    if (!track || track.readyState !== 'live') return;
    const c = { frameRate: m.fps };
    if (m.kind !== 'screen' && m.width && m.height) { c.width = m.width; c.height = m.height; }
    track.applyConstraints(c).catch(() => {});
}

/** Stop sending. `tell` is false when the call is already gone. */
async function stopVideo(tell = true) {
    const was = selfKind;
    selfKind = 'off';
    if (captureVideo) { captureVideo.srcObject = null; captureVideo = null; }
    if (selfStream) { selfStream.getTracks().forEach((t) => t.stop()); selfStream = null; }
    if (selfPreviewEl) selfPreviewEl.srcObject = null;
    if (videoWorker) videoWorker.postMessage({ t: 'stop' });
    if (tell && was !== 'off') await invoke('call_video_set', { kind: 'off' }).catch(() => {});
}

/** The stage's canvas, handed to the worker to paint the peer on; null when it unmounts. */
function attachPeerCanvas(el) {
    if (!el) {
        if (videoWorker) videoWorker.postMessage({ t: 'canvas', canvas: null });
        return;
    }
    const off = el.transferControlToOffscreen();
    ensureVideoWorker().postMessage({ t: 'canvas', canvas: off }, [off]);
}

/** The stage's self preview element; null when it unmounts. */
function attachSelfPreview(el) {
    selfPreviewEl = el;
    if (el) el.srcObject = selfStream;
}

// A hidden page cannot capture; an honest "camera off" beats a frozen picture.
document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'hidden' && selfKind === 'camera') stopVideo(true);
});

document.addEventListener('DOMContentLoaded', () => { videoProbe = probeVideoCaps(); }, { once: true });
