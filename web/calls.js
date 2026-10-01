// Vector Web calls, the page's half: the microphone and the speaker. A browser lets
// audio start only inside a tap, so placing or answering a call opens both there;
// once the call connects, AudioWorklets carry the audio straight to the worker.
(() => {
    'use strict';
    const web = window.__vectorWeb;
    const { register, backend, emit } = web;

    const supported = () =>
        typeof AudioEncoder === 'function' && typeof AudioDecoder === 'function'
        && typeof AudioWorkletNode === 'function' && !!navigator.mediaDevices?.getUserMedia;

    let ctx = null;
    let worklets = null;
    let mic = null;
    let preparing = null;
    let graph = null;
    let test = null;
    let chimeBuf = null;
    /** WebKit's call output: a media element playing the speaker's stream. */
    let out = null;

    function constraints(s) {
        return {
            audio: {
                echoCancellation: s?.echo_cancel ?? true,
                noiseSuppression: s?.noise_suppress ?? true,
                autoGainControl: s?.auto_gain ?? true,
                channelCount: 1,
            },
        };
    }

    function micMessage(e) {
        if (e?.name === 'NotAllowedError' || e?.name === 'SecurityError') return 'Microphone access was denied';
        if (e?.name === 'NotFoundError') return 'No microphone was found';
        return String(e?.message ?? e);
    }

    /** Synchronous up to the context's start, which must happen inside the tap. */
    function context() {
        if (!ctx || ctx.state === 'closed') {
            ctx = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
            worklets = null;
        }
        ctx.resume().catch(() => {});
        return ctx;
    }

    /** While the microphone is open, WebKit ducks and distorts Web Audio's own output,
     *  but plays a MediaStream through the same voice-processing unit as the capture, as
     *  it does a WebRTC call: undistorted, and heard by the echo canceller. The element
     *  starts here, inside the tap, since WebKit lets no media start outside one. */
    function output(c) {
        if (out || !('audioSession' in navigator)) return;
        const dest = c.createMediaStreamDestination();
        const player = new Audio();
        player.srcObject = dest.stream;
        out = { dest, player, playing: false };
        const o = out;
        player.play().then(() => { o.playing = true; }, () => {});
    }

    function prepare() {
        if (!supported()) return Promise.reject(new Error("Calls aren't supported in this browser"));
        const c = context();
        output(c);
        worklets ??= c.audioWorklet.addModule('/web/calls-worklet.js');
        preparing ??= (async () => {
            const settings = await backend('call_audio_settings_get').catch(() => null);
            mic ??= await navigator.mediaDevices.getUserMedia(constraints(settings));
            await worklets;
        })().catch((e) => {
            preparing = null;
            throw e;
        });
        return preparing;
    }

    function stopGraph() {
        if (!graph) return;
        graph.mic.port.postMessage({ stop: true });
        graph.spk.port.postMessage({ stop: true });
        for (const node of [graph.src, graph.mic, graph.sink, graph.spk]) node.disconnect();
        graph = null;
    }

    /** Everything off: the microphone light goes out with the call. */
    function release() {
        stopGraph();
        if (out) {
            out.player.pause();
            out.player.srcObject = null;
            out = null;
        }
        mic?.getTracks().forEach((t) => t.stop());
        mic = null;
        preparing = null;
        if (!test) ctx?.suspend().catch(() => {});
    }

    async function startGraph() {
        try {
            await prepare();
            stopGraph();
            const src = ctx.createMediaStreamSource(mic);
            const micNode = new AudioWorkletNode(ctx, 'vector-mic', { numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [1], channelCount: 1, channelCountMode: 'explicit' });
            // A silent path to the output keeps the capture node pulled on every engine.
            const sink = ctx.createGain();
            sink.gain.value = 0;
            src.connect(micNode);
            micNode.connect(sink);
            sink.connect(ctx.destination);
            const spk = new AudioWorkletNode(ctx, 'vector-speaker', { numberOfInputs: 0, numberOfOutputs: 1, outputChannelCount: [1] });
            // A media element that failed to start leaves the context's own output, which still plays.
            spk.connect(out?.playing ? out.dest : ctx.destination);
            const toMic = new MessageChannel();
            const toSpk = new MessageChannel();
            micNode.port.postMessage({ port: toMic.port1, rate: ctx.sampleRate }, [toMic.port1]);
            spk.port.postMessage({ port: toSpk.port1, rate: ctx.sampleRate }, [toSpk.port1]);
            graph = { src, mic: micNode, sink, spk };
            web.callPost({ t: 'call-ports', mic: toMic.port2, spk: toSpk.port2 }, [toMic.port2, toSpk.port2]);
        } catch (e) {
            web.callPost({ t: 'call-failed', error: micMessage(e) });
        }
    }

    /** The call-ended chime, through the call's own context: it outlives the call by a moment. */
    async function chime() {
        const c = ctx;
        if (!c) return;
        try {
            await c.resume();
            chimeBuf ??= await c.decodeAudioData(await (await fetch('/web/ended.wav')).arrayBuffer());
            const src = c.createBufferSource();
            src.buffer = chimeBuf;
            src.connect(c.destination);
            src.onended = () => { if (!graph && !test) c.suspend().catch(() => {}); };
            src.start();
        } catch (_) {}
    }

    web.onCall = (msg) => {
        switch (msg.op) {
            case 'start':
                startGraph();
                break;
            case 'stop':
                release();
                break;
            case 'settings':
                mic?.getAudioTracks()[0]?.applyConstraints(constraints(msg.settings).audio).catch(() => {});
                break;
            case 'ended':
                chime();
                break;
        }
    };

    // Placing and answering open the microphone first, inside the tap.
    const withAudio = (cmd) => (args) =>
        prepare().then(
            () => backend(cmd, args).catch((e) => { release(); throw e; }),
            (e) => { release(); throw micMessage(e); },
        );
    register('call_start', withAudio('call_start'));
    register('call_accept', withAudio('call_accept'));

    // The video worker's link to the backend: a port where desktop hands out a socket URL.
    register('call_video_link', () => {
        const { port1, port2 } = new MessageChannel();
        web.callPost({ t: 'call-link', port: port2 }, [port2]);
        return { port: port1 };
    });

    // A call that never connected still took the microphone.
    window.__TAURI__.event.listen('call_state', ({ payload }) => {
        if (payload?.phase === 'ended') release();
    });

    function level(buf) {
        let sum = 0;
        for (let i = 0; i < buf.length; i++) sum += buf[i] * buf[i];
        const rms = Math.sqrt(sum / buf.length);
        return rms > 0 ? Math.min(1, Math.max(0, (20 * Math.log10(rms) + 60) / 60)) : 0;
    }

    function stopTest() {
        if (!test) return;
        clearInterval(test.timer);
        test.src.disconnect();
        test.stream.getTracks().forEach((t) => t.stop());
        test = null;
        if (!graph) ctx?.suspend().catch(() => {});
    }

    // The microphone test runs here: the browser's own processing is what a call sends.
    register('call_mic_test_start', async () => {
        if (graph) throw 'A call is in progress';
        if (!supported()) throw "Calls aren't supported in this browser";
        stopTest();
        const c = context();
        const settings = await backend('call_audio_settings_get').catch(() => null);
        let stream;
        try {
            stream = await navigator.mediaDevices.getUserMedia(constraints(settings));
        } catch (e) {
            throw micMessage(e);
        }
        const src = c.createMediaStreamSource(stream);
        const analyser = c.createAnalyser();
        analyser.fftSize = 2048;
        src.connect(analyser);
        const buf = new Float32Array(analyser.fftSize);
        const gain = Number.isFinite(settings?.mic_volume) ? Math.min(1, Math.max(0, settings.mic_volume)) : 1;
        const timer = setInterval(() => {
            analyser.getFloatTimeDomainData(buf);
            if (gain !== 1) for (let i = 0; i < buf.length; i++) buf[i] *= gain;
            emit('mic_level', { id: '', mic: level(buf), peer: 0 });
        }, 100);
        test = { stream, src, timer };
    });
    register('call_mic_test_stop', () => stopTest());
})();
