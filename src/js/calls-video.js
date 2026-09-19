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
            case 'link': videoLinkOpen = m.open; VectorSvelte.setVideoLink(m.open); break;
            case 'painted': VectorSvelte.setVideoPeerSize(m.width, m.height); break;
            case 'stats': VectorSvelte.setVideoStats(m.enc, m.decFps); break;
            case 'error': VectorSvelte.showToast(m.message); stopVideo(); break;
        }
    };
    return videoWorker;
}

/** Every call_state: open the link when a call goes live, tear video down when it ends. */
function callVideoOnState(s) {
    if (!s || s.phase !== 'active') {
        if (selfKind !== 'off') stopVideo(false);
        if (videoWorker && (videoLinkOpen || videoLinkCallId)) videoWorker.postMessage({ t: 'close' });
        videoLinkCallId = null;
        return;
    }
    if (videoLinkCallId === s.id || !videoCaps.decode.length) return;
    videoLinkCallId = s.id;
    invoke('call_video_link').then((url) => {
        if (videoLinkCallId !== s.id) return;
        ensureVideoWorker().postMessage({ t: 'open', url, caps: videoCaps });
    }).catch(() => { videoLinkCallId = null; });
}

/** Turn the camera or the screen on ('camera' | 'screen') or everything off ('off'). */
async function startVideo(kind) {
    if (kind === 'off' || kind === selfKind) return stopVideo(true);
    if (selfKind !== 'off') await stopVideo(true);
    let stream;
    try {
        stream = kind === 'screen'
            ? await navigator.mediaDevices.getDisplayMedia({ video: { frameRate: 15 }, audio: false, selfBrowserSurface: 'exclude' })
            : await navigator.mediaDevices.getUserMedia({ video: { width: 1280, height: 720, frameRate: 30 }, audio: false });
    } catch (e) {
        if (e && e.name !== 'NotAllowedError') VectorSvelte.showToast(kind === 'screen' ? 'Could not share the screen' : 'Could not open the camera');
        return;
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
    if (!el) return;
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

document.addEventListener('DOMContentLoaded', () => { probeVideoCaps(); }, { once: true });
