// Vector Web calls on the audio thread: the microphone's and the speaker's ends.
// Each talks straight to the worker over its own port, so no audio crosses the
// page's main thread. The call runs at 48 kHz; a context that does not is resampled.

const RATE = 48000;
const FRAME = 960;

/** Linear, streaming: enough for a voice. */
class Resampler {
    constructor(from, to) {
        this.step = from / to;
        this.t = 0;
        this.last = 0;
    }

    process(input, emit) {
        const n = input.length;
        const at = (i) => (i < 0 ? this.last : input[i]);
        let t = this.t;
        while (t < n - 1) {
            const i = Math.floor(t);
            const a = at(i);
            emit(a + (at(i + 1) - a) * (t - i));
            t += this.step;
        }
        this.t = t - n;
        if (n) this.last = input[n - 1];
    }
}

class Mic extends AudioWorkletProcessor {
    constructor() {
        super();
        this.out = null;
        this.frame = new Float32Array(FRAME);
        this.n = 0;
        this.rs = null;
        this.done = false;
        this.port.onmessage = ({ data }) => {
            if (data.port) this.out = data.port;
            if (data.rate && data.rate !== RATE) this.rs = new Resampler(data.rate, RATE);
            if (data.stop) this.done = true;
        };
    }

    push(s) {
        this.frame[this.n++] = s;
        if (this.n < FRAME) return;
        if (this.out) this.out.postMessage(this.frame, [this.frame.buffer]);
        this.frame = new Float32Array(FRAME);
        this.n = 0;
    }

    process(inputs) {
        if (this.done) return false;
        const ch = inputs[0]?.[0];
        if (ch) {
            if (this.rs) this.rs.process(ch, (s) => this.push(s));
            else for (let i = 0; i < ch.length; i++) this.push(ch[i]);
        }
        return true;
    }
}

/** Keeps a little decoded audio queued and asks the worker for the next frame as it drains. */
class Speaker extends AudioWorkletProcessor {
    constructor() {
        super();
        this.src = null;
        this.queue = [];
        this.head = 0;
        this.buffered = 0;
        this.asked = 0;
        this.rs = null;
        this.idleUntil = 0;
        this.done = false;
        // Two frames ahead: the jitter buffer upstream holds the network's margin.
        this.target = Math.round((FRAME * 2 * sampleRate) / RATE);
        this.per = Math.round((FRAME * sampleRate) / RATE);
        this.port.onmessage = ({ data }) => {
            if (data.port) {
                this.src = data.port;
                this.src.onmessage = ({ data }) => this.take(data);
            }
            if (data.rate && data.rate !== RATE) this.rs = new Resampler(RATE, data.rate);
            if (data.stop) this.done = true;
        };
    }

    take(data) {
        this.asked = Math.max(0, this.asked - 1);
        let pcm = data.pcm;
        if (!pcm) {
            // Nothing buffered yet: ask again shortly rather than every quantum.
            this.idleUntil = currentTime + 0.01;
            return;
        }
        if (this.rs) {
            const out = [];
            this.rs.process(pcm, (s) => out.push(s));
            pcm = Float32Array.from(out);
        }
        this.queue.push(pcm);
        this.buffered += pcm.length;
    }

    process(_, outputs) {
        if (this.done) return false;
        const out = outputs[0][0];
        let i = 0;
        while (i < out.length && this.queue.length) {
            const q = this.queue[0];
            const n = Math.min(out.length - i, q.length - this.head);
            out.set(q.subarray(this.head, this.head + n), i);
            i += n;
            this.head += n;
            this.buffered -= n;
            if (this.head >= q.length) {
                this.queue.shift();
                this.head = 0;
            }
        }
        if (this.src && currentTime >= this.idleUntil) {
            while (this.buffered + this.asked * this.per < this.target) {
                this.src.postMessage({ ahead: Math.round((this.buffered * 1000) / sampleRate) });
                this.asked++;
            }
        }
        return true;
    }
}

registerProcessor('vector-mic', Mic);
registerProcessor('vector-speaker', Speaker);
