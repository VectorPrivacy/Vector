// Taps a shared screen's sound: gathers the 128-frame quanta into 20 ms chunks of
// interleaved samples and posts them, buffer and all, to the page. Nothing is
// played here; the chunk ends up in the backend, which cancels and encodes it.
class ShareTap extends AudioWorkletProcessor {
    constructor() {
        super();
        this.chunkFrames = Math.round(sampleRate / 50);
        this.channels = 2;
        this.buf = new Float32Array(this.chunkFrames * this.channels);
        this.filled = 0;
    }
    process(inputs) {
        const input = inputs[0];
        if (!input || !input.length) return true;
        const frames = input[0].length;
        const left = input[0];
        const right = input[1] || input[0];
        for (let i = 0; i < frames; i++) {
            this.buf[this.filled * 2] = left[i];
            this.buf[this.filled * 2 + 1] = right[i];
            this.filled++;
            if (this.filled === this.chunkFrames) {
                const out = this.buf;
                this.port.postMessage({ rate: sampleRate, channels: 2, samples: out }, [out.buffer]);
                this.buf = new Float32Array(this.chunkFrames * this.channels);
                this.filled = 0;
            }
        }
        return true;
    }
}
registerProcessor('share-tap', ShareTap);
