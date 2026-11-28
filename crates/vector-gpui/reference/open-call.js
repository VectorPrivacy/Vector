// A call with alice: window.__CALL__ is ringing, calling or open (active, panel open).
(() => {
    const alice = 'npub190vqdjtlpcq27xslcveglfmr4ynfwg7gmw86cnun4acakxrdd6gqnp929a';
    const mode = window.__CALL__;
    setTimeout(() => {
        const phase = mode === 'open' ? 'active' : 'ringing';
        VectorSvelte.setCallState({ id: 'fixture', peer: alice, outgoing: mode !== 'ringing', phase, muted: false, peer_muted: false, volume: 1, active_ms: 83000 });
        VectorSvelte.setCallAudio({ auto_gain: true, echo_cancel: true, noise_suppress: true, mic_volume: 1, speaker_volume: 1 });
        if (phase !== 'active') return;
        [62, 58, 64, 71, 66, 60, 59, 63, 120, 95, 70, 64, 61, 60, 58, 62, 66, 63, 61, 60].forEach((rtt, i) => {
            const concealed = (i === 9 ? 3 : 0) + (i > 9 ? 3 : 0);
            VectorSvelte.setCallStats({ id: 'fixture', rtt_ms: rtt, path: 'relay', bitrate_kbps: 64, received: (i + 1) * 50, concealed, net_loss: 0 });
        });
        VectorSvelte.setCallLevels({ id: 'fixture', mic: 0.62, peer: 0.7 });
    }, 400);
    if (mode === 'open') setTimeout(() => {
        const row = document.querySelector('.call-pill-row');
        const r = row.getBoundingClientRect();
        const o = { bubbles: true, clientX: r.left + 60, clientY: r.top + 20, pointerId: 1, button: 0 };
        row.dispatchEvent(new PointerEvent('pointerdown', o));
        row.dispatchEvent(new PointerEvent('pointerup', o));
    }, 900);
})();
