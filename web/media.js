// Vector Web: the commands desktop answers natively and a browser answers in the
// page: audio playback (the engine's contract, on <audio>), voice recording to
// WAV, and saving/copying attachments.
(() => {
    'use strict';
    const { register, emit, backend, storeFiles, fileUrl } = window.__vectorWeb;

    // --- Audio engine ------------------------------------------------------
    const FPS = 30;
    const BINS = 64;
    const FFT = 1024;
    const sources = new Map();
    let nextId = 1;
    let decodeCtx = null;

    function fft(re, im) {
        const n = re.length;
        for (let i = 1, j = 0; i < n; i++) {
            let bit = n >> 1;
            for (; j & bit; bit >>= 1) j ^= bit;
            j ^= bit;
            if (i < j) { [re[i], re[j]] = [re[j], re[i]]; [im[i], im[j]] = [im[j], im[i]]; }
        }
        for (let len = 2; len <= n; len <<= 1) {
            const ang = (-2 * Math.PI) / len;
            const wr = Math.cos(ang), wi = Math.sin(ang);
            for (let i = 0; i < n; i += len) {
                let cr = 1, ci = 0;
                for (let k = 0; k < len / 2; k++) {
                    const a = i + k, b = a + len / 2;
                    const tr = re[b] * cr - im[b] * ci, ti = re[b] * ci + im[b] * cr;
                    re[b] = re[a] - tr; im[b] = im[a] - ti;
                    re[a] += tr; im[a] += ti;
                    [cr, ci] = [cr * wr - ci * wi, cr * wi + ci * wr];
                }
            }
        }
    }

    // Log-spaced bands from 60 Hz to 12 kHz, 0-255 over an 80 dB range.
    function waveform(buffer) {
        const sr = buffer.sampleRate;
        const mono = new Float32Array(buffer.length);
        for (let c = 0; c < buffer.numberOfChannels; c++) {
            const d = buffer.getChannelData(c);
            for (let i = 0; i < d.length; i++) mono[i] += d[i] / buffer.numberOfChannels;
        }
        const edges = Array.from({ length: BINS + 1 }, (_, i) => Math.min(FFT / 2, Math.max(1, Math.round((60 * Math.pow(12000 / 60, i / BINS)) / (sr / FFT)))));
        const hop = sr / FPS;
        const frames = Math.max(1, Math.floor(mono.length / hop));
        const out = new Uint8Array(frames * BINS);
        const re = new Float64Array(FFT), im = new Float64Array(FFT);
        for (let f = 0; f < frames; f++) {
            const start = Math.floor(f * hop);
            for (let i = 0; i < FFT; i++) {
                const w = 0.5 - 0.5 * Math.cos((2 * Math.PI * i) / (FFT - 1));
                re[i] = (mono[start + i] || 0) * w;
                im[i] = 0;
            }
            fft(re, im);
            for (let b = 0; b < BINS; b++) {
                let peak = 0;
                for (let k = edges[b]; k <= Math.max(edges[b], edges[b + 1] - 1); k++) peak = Math.max(peak, Math.hypot(re[k], im[k]));
                const db = 20 * Math.log10(peak / (FFT / 4) + 1e-9);
                out[f * BINS + b] = Math.max(0, Math.min(255, Math.round(((db + 80) / 80) * 255)));
            }
        }
        let bin = '';
        for (let i = 0; i < out.length; i += 0x8000) bin += String.fromCharCode(...out.subarray(i, i + 0x8000));
        return btoa(bin);
    }

    async function analyse(id, url) {
        try {
            decodeCtx ??= new AudioContext();
            const buffer = await decodeCtx.decodeAudioData(await (await fetch(url)).arrayBuffer());
            emit('audio_duration', { id, duration_ms: Math.round(buffer.duration * 1000) });
            emit('audio_waveform', { id, waveform: waveform(buffer), waveform_fps: FPS, bins: BINS });
        } catch (e) {
            console.warn('[web] waveform unavailable:', e);
        }
    }

    function metadataOf(url) {
        return new Promise((resolve, reject) => {
            const el = new Audio();
            el.preload = 'metadata';
            el.onloadedmetadata = () => resolve(el);
            el.onerror = () => reject('Failed to load audio');
            el.src = url;
        });
    }

    const ms = (el) => Math.round((el.currentTime || 0) * 1000);
    const source = (id) => {
        const el = sources.get(id);
        if (!el) throw new Error(`No audio source ${id}`);
        return el;
    };

    const urlOf = async (path) => {
        const url = await fileUrl(path);
        if (!url) throw new Error(`${path} not found`);
        return url;
    };

    async function load({ path }) {
        const url = await urlOf(path);
        const el = await metadataOf(url);
        el.preload = 'auto';
        const id = nextId++;
        sources.set(id, el);
        el.onended = () => emit('audio_ended', { id });
        analyse(id, url);
        return { id, duration_ms: Number.isFinite(el.duration) ? Math.round(el.duration * 1000) : 0, waveform_fps: FPS, bins: BINS };
    }

    register('audio_probe', async ({ path }) => {
        const el = await metadataOf(await urlOf(path));
        return Number.isFinite(el.duration) ? Math.round(el.duration * 1000) : 0;
    });
    register('audio_load', load);
    register('audio_play', async ({ id }) => { const el = source(id); await el.play(); return ms(el); });
    register('audio_pause', ({ id }) => { const el = source(id); el.pause(); return ms(el); });
    register('audio_seek', ({ id, positionMs }) => { source(id).currentTime = positionMs / 1000; });
    register('audio_set_volume', ({ id, volume }) => { source(id).volume = Math.max(0, Math.min(1, volume)); });
    register('audio_stop', ({ id }) => {
        const el = sources.get(id);
        if (el) { el.pause(); el.removeAttribute('src'); el.load(); sources.delete(id); }
    });
    register('audio_stop_all', () => { for (const id of [...sources.keys()]) window.__TAURI__.core.invoke('audio_stop', { id }); });
    register('get_audio_metadata', () => null);
    register('audio_devices_list', () => ({ inputs: [], outputs: [], input: null, output: null }));
    register('audio_devices_set', () => null);

    // --- Voice recording ---------------------------------------------------
    const VOICE_RATE = 24000;
    let recording = null;
    let pendingVoice = null;

    function toWav(chunks, fromRate) {
        const total = chunks.reduce((n, c) => n + c.length, 0);
        const input = new Float32Array(total);
        let o = 0;
        for (const c of chunks) { input.set(c, o); o += c.length; }
        const ratio = fromRate / VOICE_RATE;
        const len = Math.floor(total / ratio);
        const buf = new ArrayBuffer(44 + len * 2);
        const v = new DataView(buf);
        const str = (off, s) => { for (let i = 0; i < s.length; i++) v.setUint8(off + i, s.charCodeAt(i)); };
        str(0, 'RIFF'); v.setUint32(4, 36 + len * 2, true); str(8, 'WAVE');
        str(12, 'fmt '); v.setUint32(16, 16, true); v.setUint16(20, 1, true); v.setUint16(22, 1, true);
        v.setUint32(24, VOICE_RATE, true); v.setUint32(28, VOICE_RATE * 2, true); v.setUint16(32, 2, true); v.setUint16(34, 16, true);
        str(36, 'data'); v.setUint32(40, len * 2, true);
        for (let i = 0; i < len; i++) {
            const pos = i * ratio, i0 = Math.floor(pos), t = pos - i0;
            const s = (input[i0] || 0) * (1 - t) + (input[i0 + 1] || 0) * t;
            v.setInt16(44 + i * 2, Math.max(-1, Math.min(1, s)) * 0x7fff, true);
        }
        return new Blob([buf], { type: 'audio/wav' });
    }

    register('start_recording', async () => {
        const stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } });
        const ctx = new AudioContext();
        const src = ctx.createMediaStreamSource(stream);
        const proc = ctx.createScriptProcessor(4096, 1, 1);
        const chunks = [];
        proc.onaudioprocess = (e) => chunks.push(new Float32Array(e.inputBuffer.getChannelData(0)));
        src.connect(proc);
        proc.connect(ctx.destination);
        recording = { stream, ctx, src, proc, chunks };
    });

    register('stop_recording', async () => {
        const r = recording;
        if (!r) throw new Error('Not recording');
        recording = null;
        r.proc.disconnect();
        r.src.disconnect();
        r.stream.getTracks().forEach((t) => t.stop());
        const wav = toWav(r.chunks, r.ctx.sampleRate);
        r.ctx.close();
        const [path] = await storeFiles([new File([wav], 'voice.wav', { type: 'audio/wav' })]);
        const loaded = await load({ path });
        pendingVoice = { path, id: loaded.id };
        return loaded;
    });

    register('send_recording', async ({ receiver, repliedTo }) => {
        const voice = pendingVoice;
        if (!voice) throw new Error('No pending recording');
        pendingVoice = null;
        window.__TAURI__.core.invoke('audio_stop', { id: voice.id });
        return backend('web_send_voice', { receiver, repliedTo: repliedTo || '', filePath: voice.path });
    });

    register('transcribe', () => { throw new Error('Transcription is not available on Vector Web'); });

    // --- Attachment actions ------------------------------------------------
    const fileName = (path) => path.split('/').pop() || 'file';

    async function save(path) {
        const a = document.createElement('a');
        a.href = await urlOf(path);
        a.download = fileName(path);
        document.body.appendChild(a);
        a.click();
        a.remove();
    }

    // Only media opens in a tab; anything else is saved, never navigated to.
    const VIEWABLE = /\.(png|jpe?g|gif|webp|avif|bmp|mp4|webm|mov|mp3|m4a|aac|ogg|opus|wav|flac)$/i;
    const openOrSave = async (path) => (VIEWABLE.test(path) ? window.open(await urlOf(path), '_blank', 'noopener') : save(path));
    register('open_attachment', ({ path }) => { openOrSave(path); });
    register('share_attachment', async ({ path }) => {
        const blob = await (await fetch(await urlOf(path))).blob();
        const file = new File([blob], fileName(path), { type: blob.type });
        if (navigator.canShare?.({ files: [file] })) return navigator.share({ files: [file] });
        save(path);
    });
    register('write_clipboard_files', async ({ paths }) => {
        const blob = await (await fetch(await urlOf(paths[0]))).blob();
        const type = blob.type.startsWith('image/') ? 'image/png' : blob.type;
        const png = type === blob.type ? blob : await new Promise((resolve) => {
            createImageBitmap(blob).then((bmp) => {
                const c = new OffscreenCanvas(bmp.width, bmp.height);
                c.getContext('2d').drawImage(bmp, 0, 0);
                c.convertToBlob({ type: 'image/png' }).then(resolve);
            });
        });
        await navigator.clipboard.write([new ClipboardItem({ [png.type]: png })]);
    });
    register('read_clipboard_files', () => []);
    register('reveal_attachment', ({ path }) => save(path));
    // "Show in folder" has no folder here: save the file instead.
    window.__TAURI__.opener.revealItemInDir = async (path) => save(path);
    window.__TAURI__.opener.openPath = async (path) => { openOrSave(path); };

    // --- Notifications -----------------------------------------------------
    // Asked in-app once signed in, and from Settings; the browser's own prompt only
    // follows a tap on Allow, since it refuses prompts without a gesture.
    // Private browsers keep no permission past the session and deny the prompt anyway.
    const permission = () => {
        const kept = window.__vectorWeb.storage();
        if (kept === 'session' || kept === 'memory') return 'unsupported';
        return window.Notification?.permission || 'unsupported';
    };
    register('web_notifications', () => permission());
    register('request_web_notifications', async () => {
        if (permission() === 'default') await Notification.requestPermission();
        return permission();
    });
    register('offer_web_notifications', async () => {
        if (permission() !== 'default') return;
        const invoke = window.__TAURI__.core.invoke;
        if (await invoke('get_sql_setting', { key: 'web_notif_prompted' }).catch(() => null)) return;
        await invoke('set_sql_setting', { key: 'web_notif_prompted', value: 'true' });
        const allow = await popupConfirm('Notifications', 'Vector can let you know when messages arrive while it is in the background.<br><br>You can change this later in Settings.', false, '', 'vector_warning.svg', '', 'Allow');
        if (allow) await Notification.requestPermission();
        initNotificationSettings();
    });

    window.__TAURI__.event.listen('web_notify', async ({ payload }) => {
        if (window.Notification?.permission !== 'granted') return;
        if (document.visibilityState === 'visible' && document.hasFocus()) return;
        // Through the service worker: iOS web apps have no Notification constructor, and a
        // push for the same chat replaces this rather than stacking under it.
        const reg = await navigator.serviceWorker?.ready.catch(() => null);
        if (!reg) return;
        const icon = payload.icon ? await fileUrl(payload.icon) : null;
        await reg.showNotification(payload.title, {
            body: payload.body,
            icon: icon || '/icon-192.png',
            tag: `chat:${payload.chat_id}`,
            data: { chat: payload.chat_id },
        }).catch((e) => console.warn('[Notify] not shown:', e));
    });
})();
