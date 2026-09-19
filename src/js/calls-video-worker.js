// The video pipeline of a call, off the main thread: encodes the frames the page
// captures, ships them to the backend over the loopback socket, decodes what the
// peer sends and paints it. The page only ever posts frames in and canvases.
// A side can send its camera and its screen at once, so everything here is per
// track: an encoder, a decoder and a canvas for each.
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
// Buffers the encoders write into; the socket copies on send, so they cycle at once.
const POOL_SIZE = 8;
const POOL_BYTES = 256 * 1024;
const MAX_FRAME = 512 * 1024;
// The picture the peer sends is painted at its own size, up to what a decoder can
// reasonably be asked for.
const MAX_DECODE_PX = 4096 * 2304;

let ws = null;
let codec = null;
let paused = false;
const pool = [];
for (let i = 0; i < POOL_SIZE; i++) pool.push(new ArrayBuffer(POOL_BYTES));
let statsTimer = null;
// The backend names each rung as soon as sending is agreed, which can be before the
// page has posted that track's capture spec; the rung waits for it.
const lastRate = { camera: null, screen: null };
// Decoder errors per codec this link; a second one means the codec, not a frame.
const decErrors = {};

// One of these per track, both ways.
function newTrack(kind) {
    return {
        kind,
        // outgoing
        capture: null, // { fps, kbps, width, height, targetW, targetH, maxH }
        encoder: null,
        seq: 0,
        skipNext: false,
        forceKey: true,
        lastSentUs: -1,
        scaler: null,
        scalerCtx: null,
        enc: { frames: 0, bytes: 0 },
        // incoming
        canvas: null,
        ctx: null,
        decoder: null,
        decCodec: null,
        needKey: true,
        dec: { frames: 0 },
    };
}
const tracks = { camera: newTrack('camera'), screen: newTrack('screen') };

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
    const w = new WebSocket(url);
    ws = w;
    w.binaryType = 'arraybuffer';
    w.onopen = () => {
        if (ws !== w) return;
        control({ t: 'caps', encode: caps.encode, decode: caps.decode });
        postMessage({ t: 'link', open: true });
        statsTimer = setInterval(stats, 1000);
    };
    w.onmessage = (e) => { if (ws === w) onSocket(new Uint8Array(e.data)); };
    // A replaced socket's close must not tear down the one that replaced it.
    w.onclose = () => {
        if (ws !== w) return;
        clearInterval(statsTimer);
        statsTimer = null;
        postMessage({ t: 'link', open: false });
        ws = null;
    };
    w.onerror = () => {};
}

function close() {
    if (ws) { const w = ws; ws = null; try { w.close(); } catch (_) {} }
    clearInterval(statsTimer);
    statsTimer = null;
    for (const t of Object.values(tracks)) { stopEncoder(t); resetDecoder(t); }
}

function onSocket(msg) {
    if (msg[0] === KIND_FRAME) return onFrame(msg.subarray(1));
    if (msg[0] !== KIND_CONTROL) return;
    let c;
    try { c = JSON.parse(new TextDecoder().decode(msg.subarray(1))); } catch (_) { return; }
    const t = tracks[c.kind];
    switch (c.t) {
        case 'rate': if (t) setRate(t, c); break;
        case 'skip': if (t) t.skipNext = true; break;
        case 'keyframe': if (t) t.forceKey = true; break;
        case 'peer':
            for (const kind of ['camera', 'screen']) if (!c.tracks[kind]) resetDecoder(tracks[kind]);
            postMessage({ t: 'peer', tracks: c.tracks });
            break;
        case 'pause': paused = !!c.on; break;
        case 'codec':
            if (codec !== c.codec) { codec = c.codec; for (const tr of Object.values(tracks)) stopEncoder(tr); }
            break;
    }
}

// ── outgoing ──

function startCapture(t, spec) {
    t.capture = { fps: spec.fps, kbps: spec.kbps, width: 0, height: 0, targetW: 0, targetH: 0, maxH: 0 };
    t.forceKey = true;
    t.seq = 0;
    t.lastSentUs = -1;
    if (lastRate[t.kind]) setRate(t, lastRate[t.kind]);
}

function stopEncoder(t) {
    if (t.encoder) { try { t.encoder.close(); } catch (_) {} }
    t.encoder = null;
}

function encoderConfig(t, width, height, kbps, fps) {
    // No hardware hint: Chromium reads prefer-hardware as "fail without hardware", and
    // a virtual machine has none. Left to itself it picks the hardware path when there is one.
    const cfg = { codec: codecString(codec, width, height), width, height, bitrate: kbps * 1000, framerate: fps, latencyMode: 'realtime' };
    if (codec === 'h264') cfg.avc = { format: 'annexb' };
    if (t.kind === 'screen') cfg.contentHint = 'detail';
    return cfg;
}

function configureEncoder(t, width, height) {
    stopEncoder(t);
    if (!codec || !t.capture) return;
    t.capture.width = width;
    t.capture.height = height;
    t.encoder = new VideoEncoder({
        output: (chunk) => onChunk(t, chunk),
        error: (e) => { postMessage({ t: 'error', kind: t.kind, message: 'encoder: ' + e.message }); stopEncoder(t); },
    });
    t.encoder.configure(encoderConfig(t, width, height, t.capture.kbps, t.capture.fps));
    t.forceKey = true;
}

function setRate(t, r) {
    lastRate[t.kind] = r;
    if (!t.capture) return;
    t.capture.kbps = r.kbps;
    t.capture.fps = r.fps;
    if (t.kind === 'screen') {
        t.capture.maxH = r.height || 0;
    } else {
        t.capture.targetW = r.width || 0;
        t.capture.targetH = r.height || 0;
    }
    // The page asks the device for the new size and rate; whatever still arrives
    // bigger is scaled here.
    postMessage({ t: 'constrain', kind: t.kind, width: r.width, height: r.height, fps: r.fps });
    if (!t.encoder) return;
    try {
        t.encoder.configure(encoderConfig(t, t.capture.width, t.capture.height, r.kbps, r.fps));
    } catch (_) {}
}

// The size a captured frame is sent at: the rung's size for a camera, at most the
// rung's height for a screen, always even, never upscaled.
function targetSize(t, cw, ch) {
    let w = cw, h = ch;
    const c = t.capture;
    if (t.kind === 'screen') {
        if (c.maxH && h > c.maxH) { w = Math.round(cw * c.maxH / ch); h = c.maxH; }
    } else if (c.targetW && c.targetH && (cw > c.targetW || ch > c.targetH)) {
        const s = Math.min(c.targetW / cw, c.targetH / ch);
        w = Math.round(cw * s); h = Math.round(ch * s);
    }
    return [w & ~1, h & ~1];
}

function onCaptured(t, captured) {
    let frame = captured;
    try {
        if (!t.capture || paused || !ws || t.skipNext) { t.skipNext = false; return; }
        // Keep the rung's share of the device's frames, by their timestamps.
        const minGapUs = 1e6 / t.capture.fps * 0.9;
        if (t.lastSentUs >= 0 && captured.timestamp - t.lastSentUs < minGapUs) return;
        t.lastSentUs = captured.timestamp;
        const [w, h] = targetSize(t, captured.displayWidth, captured.displayHeight);
        if (!w || !h) return;
        if (w !== captured.displayWidth || h !== captured.displayHeight) {
            if (w >= captured.displayWidth - 1 && h >= captured.displayHeight - 1) {
                // H.264 takes even sizes only; a window can be any size. Trimming a pixel
                // off the edge is a new view of the same buffer, not a copy.
                frame = new VideoFrame(captured, { visibleRect: { x: 0, y: 0, width: w, height: h } });
            } else {
                if (!t.scaler || t.scaler.width !== w || t.scaler.height !== h) {
                    t.scaler = new OffscreenCanvas(w, h);
                    t.scalerCtx = t.scaler.getContext('2d', { alpha: false, desynchronized: true });
                }
                t.scalerCtx.drawImage(captured, 0, 0, w, h);
                frame = new VideoFrame(t.scaler, { timestamp: captured.timestamp });
            }
        }
        if (!t.encoder || w !== t.capture.width || h !== t.capture.height) {
            configureEncoder(t, w, h);
            if (!t.encoder) return;
        }
        // Encoder backlog means the machine is behind: dropping a capture is safe,
        // encoding one that will never send is not.
        if (t.encoder.encodeQueueSize > 2) return;
        t.encoder.encode(frame, { keyFrame: t.forceKey });
        t.forceKey = false;
    } finally {
        if (frame !== captured) frame.close();
        captured.close();
    }
}

function onChunk(t, chunk) {
    const len = chunk.byteLength;
    if (1 + HEADER_LEN + len > MAX_FRAME) return;
    let buf = pool.pop();
    if (!buf || buf.byteLength < 1 + HEADER_LEN + len) buf = new ArrayBuffer(Math.max(POOL_BYTES, 1 + HEADER_LEN + len));
    const view = new DataView(buf);
    view.setUint8(0, KIND_FRAME);
    view.setUint32(1, t.seq >>> 0);
    t.seq = (t.seq + 1) >>> 0;
    view.setBigUint64(5, BigInt(Math.max(0, Math.round(chunk.timestamp))));
    view.setUint8(13, (chunk.type === 'key' ? FLAG_KEY : 0) | (t.kind === 'screen' ? FLAG_SCREEN : 0));
    view.setUint8(14, CODEC_BYTE[codec] || 0);
    view.setUint16(15, 0);
    chunk.copyTo(new Uint8Array(buf, 1 + HEADER_LEN, len));
    send(new Uint8Array(buf, 0, 1 + HEADER_LEN + len));
    t.enc.frames++;
    t.enc.bytes += len;
    if (pool.length < POOL_SIZE) pool.push(buf);
}

// ── incoming ──

function resetDecoder(t) {
    if (t.decoder) { try { t.decoder.close(); } catch (_) {} }
    t.decoder = null;
    t.decCodec = null;
    t.needKey = true;
}

function onFrame(frame) {
    if (frame.length < HEADER_LEN) return;
    const view = new DataView(frame.buffer, frame.byteOffset, frame.length);
    const flags = view.getUint8(12);
    const name = CODEC_NAME[view.getUint8(13)];
    const ts = Number(view.getBigUint64(4) & 0x1fffffffffffffn);
    const key = (flags & FLAG_KEY) !== 0;
    const t = (flags & FLAG_SCREEN) ? tracks.screen : tracks.camera;
    if (!name) return;
    if (!t.decoder || t.decCodec !== name) {
        if (!key) { t.needKey = true; return; }
        resetDecoder(t);
        t.decoder = new VideoDecoder({
            output: (vf) => paint(t, vf),
            error: (e) => {
                decErrors[name] = (decErrors[name] || 0) + 1;
                resetDecoder(t);
                if (decErrors[name] >= 2) {
                    postMessage({ t: 'decode_error', message: e.message, codec: name });
                    control({ t: 'unsupported', codec: name });
                } else {
                    control({ t: 'lost', kind: t.kind });
                }
            },
        });
        t.decoder.configure({ codec: name === 'h264' ? H264_DECODE : 'vp8', optimizeForLatency: true });
        t.decCodec = name;
    }
    if (t.needKey && !key) return;
    t.needKey = false;
    try {
        t.decoder.decode(new EncodedVideoChunk({ type: key ? 'key' : 'delta', timestamp: ts, data: frame.subarray(HEADER_LEN) }));
    } catch (_) {
        resetDecoder(t);
        control({ t: 'lost', kind: t.kind });
    }
}

function paint(t, vf) {
    try {
        if (!t.canvas) return;
        if (vf.displayWidth * vf.displayHeight > MAX_DECODE_PX) {
            resetDecoder(t);
            control({ t: 'lost', kind: t.kind });
            return;
        }
        if (t.canvas.width !== vf.displayWidth || t.canvas.height !== vf.displayHeight) {
            t.canvas.width = vf.displayWidth;
            t.canvas.height = vf.displayHeight;
            postMessage({ t: 'painted', kind: t.kind, width: vf.displayWidth, height: vf.displayHeight });
        }
        t.ctx.drawImage(vf, 0, 0);
        t.dec.frames++;
    } finally {
        vf.close();
    }
}

function stats() {
    const report = {};
    for (const t of Object.values(tracks)) {
        const s = { fps: t.enc.frames, kbps: Math.round(t.enc.bytes * 8 / 1000), width: t.capture ? t.capture.width : 0, height: t.capture ? t.capture.height : 0 };
        if (t.capture) control({ t: 'stats', kind: t.kind, ...s });
        report[t.kind] = { enc: s, decFps: t.dec.frames };
        t.enc.frames = 0; t.enc.bytes = 0; t.dec.frames = 0;
    }
    postMessage({ t: 'stats', tracks: report });
}

self.onmessage = (e) => {
    const m = e.data;
    const t = tracks[m.kind];
    switch (m.t) {
        case 'open': open(m.url, m.caps); break;
        case 'close': close(); break;
        case 'canvas':
            if (!t) break;
            t.canvas = m.canvas;
            t.ctx = m.canvas ? m.canvas.getContext('2d', { alpha: false, desynchronized: true }) : null;
            break;
        case 'capture': if (t) startCapture(t, m); break;
        case 'stop': if (t) { t.capture = null; lastRate[t.kind] = null; stopEncoder(t); } break;
        case 'frame': if (t) onCaptured(t, m.frame); else m.frame.close(); break;
    }
};
