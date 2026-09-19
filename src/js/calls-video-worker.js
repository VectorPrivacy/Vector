// The video pipeline of a call, off the main thread: encodes the frames the page
// captures, ships them to the backend over the loopback socket, decodes what the
// peer sends and paints it. The page only ever posts frames in and a canvas.
//
// Every frame's bytes are copied once, out of the encoder into a pooled buffer that
// already holds the wire header; the socket sends that buffer as it is.

const KIND_FRAME = 1;
const KIND_CONTROL = 2;
const HEADER_LEN = 16;
const FLAG_KEY = 1;
const FLAG_SCREEN = 2;
const CODEC_BYTE = { h264: 1, vp8: 2 };
const CODEC_NAME = { 1: 'h264', 2: 'vp8' };
// H.264 constrained baseline; the level is picked from the frame size, since 3.1 stops
// at 720p and a shared screen is bigger. Decoders take the top level and cope.
const H264_DECODE = 'avc1.42E033';
function codecString(name, width, height) {
    if (name === 'vp8') return 'vp8';
    const px = width * height;
    if (px <= 921600) return 'avc1.42E01F';
    if (px <= 2097152) return 'avc1.42E028';
    return 'avc1.42E033';
}
// Buffers the encoder writes into; the socket copies on send, so they cycle at once.
const POOL_SIZE = 8;
const POOL_BYTES = 256 * 1024;
const MAX_FRAME = 512 * 1024;

let ws = null;
let canvas = null;
let ctx = null;
let encoder = null;
let decoder = null;
let codec = null;
let capture = null; // { kind, width, height, fps, kbps }
let seq = 0;
let skipNext = false;
let forceKey = true;
let paused = false;
let needKey = true;
let decCodec = null;
const pool = [];
for (let i = 0; i < POOL_SIZE; i++) pool.push(new ArrayBuffer(POOL_BYTES));
const enc = { frames: 0, bytes: 0 };
const dec = { frames: 0 };
let statsTimer = null;

function send(bytes) {
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(bytes);
}

function control(obj) {
    const body = new TextEncoder().encode(JSON.stringify(obj));
    const msg = new Uint8Array(1 + body.length);
    msg[0] = KIND_CONTROL;
    msg.set(body, 1);
    send(msg);
}

function open(url, caps) {
    close();
    ws = new WebSocket(url);
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => {
        control({ t: 'caps', encode: caps.encode, decode: caps.decode });
        postMessage({ t: 'link', open: true });
        statsTimer = setInterval(stats, 1000);
    };
    ws.onmessage = (e) => onSocket(new Uint8Array(e.data));
    ws.onclose = () => {
        clearInterval(statsTimer);
        statsTimer = null;
        postMessage({ t: 'link', open: false });
        ws = null;
    };
    ws.onerror = () => {};
}

function close() {
    if (ws) { const w = ws; ws = null; try { w.close(); } catch (_) {} }
    clearInterval(statsTimer);
    statsTimer = null;
    stopEncoder();
    resetDecoder();
}

function onSocket(msg) {
    if (msg[0] === KIND_FRAME) return onFrame(msg.subarray(1));
    if (msg[0] !== KIND_CONTROL) return;
    let c;
    try { c = JSON.parse(new TextDecoder().decode(msg.subarray(1))); } catch (_) { return; }
    switch (c.t) {
        case 'rate': setRate(c); break;
        case 'skip': skipNext = true; break;
        case 'keyframe': forceKey = true; break;
        case 'peer':
            if (c.kind === 'off') resetDecoder();
            postMessage({ t: 'peer', kind: c.kind });
            break;
        case 'pause': paused = !!c.on; break;
        case 'codec':
            if (codec !== c.codec) { codec = c.codec; stopEncoder(); }
            break;
    }
}

// ── outgoing ──

function startCapture(spec) {
    capture = { kind: spec.kind, fps: spec.fps, kbps: spec.kbps, width: 0, height: 0 };
    forceKey = true;
    seq = 0;
}

function stopEncoder() {
    if (encoder) { try { encoder.close(); } catch (_) {} }
    encoder = null;
}

function configureEncoder(width, height) {
    stopEncoder();
    if (!codec || !capture) return;
    capture.width = width;
    capture.height = height;
    encoder = new VideoEncoder({ output: onChunk, error: (e) => { postMessage({ t: 'error', message: 'encoder: ' + e.message }); stopEncoder(); } });
    const cfg = {
        codec: codecString(codec, width, height), width, height,
        bitrate: capture.kbps * 1000, framerate: capture.fps,
        latencyMode: 'realtime', hardwareAcceleration: 'prefer-hardware',
    };
    if (codec === 'h264') cfg.avc = { format: 'annexb' };
    if (capture.kind === 'screen') cfg.contentHint = 'detail';
    encoder.configure(cfg);
    forceKey = true;
}

function setRate(r) {
    if (!capture) return;
    capture.kbps = r.kbps;
    capture.fps = r.fps;
    if (!encoder) return;
    if (r.width && r.height && (r.width !== capture.width || r.height !== capture.height)) {
        configureEncoder(r.width, r.height);
        return;
    }
    try {
        encoder.configure({ codec: codecString(codec, capture.width, capture.height), width: capture.width, height: capture.height, bitrate: r.kbps * 1000, framerate: r.fps, latencyMode: 'realtime', hardwareAcceleration: 'prefer-hardware', ...(codec === 'h264' ? { avc: { format: 'annexb' } } : {}) });
    } catch (_) {}
}

function onCaptured(captured) {
    let frame = captured;
    try {
        if (!capture || paused || !ws || skipNext) { skipNext = false; return; }
        // H.264 takes even sizes only; a window can be any size. Trimming a pixel
        // off the edge is a new view of the same buffer, not a copy.
        const w = captured.displayWidth & ~1;
        const h = captured.displayHeight & ~1;
        if (!w || !h) return;
        if (w !== captured.displayWidth || h !== captured.displayHeight) {
            frame = new VideoFrame(captured, { visibleRect: { x: 0, y: 0, width: w, height: h } });
        }
        if (!encoder || w !== capture.width || h !== capture.height) {
            configureEncoder(w, h);
            if (!encoder) return;
        }
        // Encoder backlog means the machine is behind: dropping a capture is safe,
        // encoding one that will never send is not.
        if (encoder.encodeQueueSize > 2) return;
        encoder.encode(frame, { keyFrame: forceKey });
        forceKey = false;
    } finally {
        if (frame !== captured) frame.close();
        captured.close();
    }
}

function onChunk(chunk) {
    const len = chunk.byteLength;
    if (1 + HEADER_LEN + len > MAX_FRAME) return;
    let buf = pool.pop();
    if (!buf || buf.byteLength < 1 + HEADER_LEN + len) buf = new ArrayBuffer(Math.max(POOL_BYTES, 1 + HEADER_LEN + len));
    const view = new DataView(buf);
    view.setUint8(0, KIND_FRAME);
    view.setUint32(1, seq >>> 0);
    seq = (seq + 1) >>> 0;
    view.setBigUint64(5, BigInt(Math.max(0, Math.round(chunk.timestamp))));
    view.setUint8(13, (chunk.type === 'key' ? FLAG_KEY : 0) | (capture && capture.kind === 'screen' ? FLAG_SCREEN : 0));
    view.setUint8(14, CODEC_BYTE[codec] || 0);
    view.setUint16(15, 0);
    chunk.copyTo(new Uint8Array(buf, 1 + HEADER_LEN, len));
    send(new Uint8Array(buf, 0, 1 + HEADER_LEN + len));
    enc.frames++;
    enc.bytes += len;
    if (pool.length < POOL_SIZE) pool.push(buf);
}

// ── incoming ──

function resetDecoder() {
    if (decoder) { try { decoder.close(); } catch (_) {} }
    decoder = null;
    decCodec = null;
    needKey = true;
}

function onFrame(frame) {
    if (frame.length < HEADER_LEN) return;
    const view = new DataView(frame.buffer, frame.byteOffset, frame.length);
    const flags = view.getUint8(12);
    const name = CODEC_NAME[view.getUint8(13)];
    const ts = Number(view.getBigUint64(4));
    const key = (flags & FLAG_KEY) !== 0;
    if (!name) return;
    if (!decoder || decCodec !== name) {
        if (!key) { needKey = true; return; }
        resetDecoder();
        decoder = new VideoDecoder({ output: paint, error: () => { resetDecoder(); control({ t: 'lost' }); } });
        decoder.configure({ codec: name === 'h264' ? H264_DECODE : 'vp8', optimizeForLatency: true, hardwareAcceleration: 'prefer-hardware' });
        decCodec = name;
    }
    if (needKey && !key) return;
    needKey = false;
    try {
        decoder.decode(new EncodedVideoChunk({ type: key ? 'key' : 'delta', timestamp: ts, data: frame.subarray(HEADER_LEN) }));
    } catch (_) {
        resetDecoder();
        control({ t: 'lost' });
    }
}

function paint(vf) {
    try {
        if (!canvas) return;
        if (canvas.width !== vf.displayWidth || canvas.height !== vf.displayHeight) {
            canvas.width = vf.displayWidth;
            canvas.height = vf.displayHeight;
            postMessage({ t: 'painted', width: vf.displayWidth, height: vf.displayHeight });
        }
        ctx.drawImage(vf, 0, 0);
        dec.frames++;
    } finally {
        vf.close();
    }
}

function stats() {
    const s = { fps: enc.frames, kbps: Math.round(enc.bytes * 8 / 1000), width: capture ? capture.width : 0, height: capture ? capture.height : 0 };
    if (capture) control({ t: 'stats', ...s });
    postMessage({ t: 'stats', enc: s, decFps: dec.frames });
    enc.frames = 0; enc.bytes = 0; dec.frames = 0;
}

self.onmessage = (e) => {
    const m = e.data;
    switch (m.t) {
        case 'open': open(m.url, m.caps); break;
        case 'close': close(); break;
        case 'canvas':
            canvas = m.canvas;
            ctx = canvas.getContext('2d', { alpha: false, desynchronized: true });
            break;
        case 'capture': startCapture(m); break;
        case 'stop': capture = null; stopEncoder(); break;
        case 'frame': onCaptured(m.frame); break;
    }
};
