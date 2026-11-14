// A video call with alice: their camera and screen on, ours on; window.__CALL__ is video or video-open.
(() => {
    const alice = 'npub190vqdjtlpcq27xslcveglfmr4ynfwg7gmw86cnun4acakxrdd6gqnp929a';
    const mode = window.__CALL__;
    setTimeout(() => {
        VectorSvelte.setVideoCaps({ encode: ['h264'], decode: ['h264'] });
        VectorSvelte.setCallState({ id: 'fixture', peer: alice, outgoing: true, phase: 'active', muted: false, peer_muted: false, volume: 1, active_ms: 83000,
            video_mine: { camera: true, screen: false }, video_peer: { camera: true, screen: true }, peer_decodes: ['h264'] });
        VectorSvelte.setCallAudio({ auto_gain: true, echo_cancel: true, noise_suppress: true, mic_volume: 1, speaker_volume: 1 });
        [62, 58, 64, 71, 66, 60, 59, 63, 120, 95, 70, 64, 61, 60, 58, 62, 66, 63, 61, 60].forEach((rtt, i) => {
            const concealed = (i === 9 ? 3 : 0) + (i > 9 ? 3 : 0);
            VectorSvelte.setCallStats({ id: 'fixture', rtt_ms: rtt, path: 'relay', bitrate_kbps: 64, received: (i + 1) * 50, concealed, net_loss: 0,
                video: { camera: { fps: 30, kbps: 780, width: 1280, height: 720 }, screen: { fps: 0, kbps: 0, width: 0, height: 0 } } });
        });
        VectorSvelte.setCallLevels({ id: 'fixture', mic: 0.62, peer: 0.7 });
    }, 400);
    if (mode === 'video-open') setTimeout(() => {
        const row = document.querySelector('.call-pill-row');
        const r = row.getBoundingClientRect();
        const o = { bubbles: true, clientX: r.left + 60, clientY: r.top + 20, pointerId: 1, button: 0 };
        row.dispatchEvent(new PointerEvent('pointerdown', o));
        row.dispatchEvent(new PointerEvent('pointerup', o));
    }, 900);
})();
