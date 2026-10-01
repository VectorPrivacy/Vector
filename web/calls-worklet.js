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

/** Frames of decoded audio the speaker keeps ahead: the least, and the most it grows to. */
const MIN_AHEAD = 2;
const MAX_AHEAD = 12;
/** Seconds without running dry before the speaker gives a frame of that margin back. */
const CALM_SECS = 15;

/** Keeps decoded audio queued and asks the worker for the next frame as it drains.
 *  Engines render in bursts of several quanta (WebKit on a phone especially), so
 *  the margin starts at two frames, grows a frame each time it runs dry, and
 *  shrinks back once playout has been calm for a while. After running dry it
 *  refills before it plays again, so one late answer cannot become a stutter. */
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
        this.playing = false;
        this.per = Math.round((FRAME * sampleRate) / RATE);
        this.ahead = MIN_AHEAD;
        this.calmSince = currentTime;
        // Silence played while the call was running, in samples, reported once a second.
        this.starved = 0;
        this.reportAt = currentTime + 1;
        this.port.onmessage = ({ data }) => {
            if (data.port) {
                this.src = data.port;
                this.src.onmessage = ({ data }) => this.take(data);
            }
            if (data.rate && data.rate !== RATE) {
                this.rs = new Resampler(RATE, data.rate);
            }
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
        const target = this.ahead * this.per;
        if (!this.playing && this.buffered >= target) this.playing = true;
        let i = 0;
        if (this.playing) {
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
            if (i < out.length) {
                this.starved += out.length - i;
                this.playing = false;
                this.ahead = Math.min(MAX_AHEAD, this.ahead + 1);
                this.calmSince = currentTime;
            } else if (this.ahead > MIN_AHEAD && currentTime - this.calmSince > CALM_SECS) {
                this.ahead--;
                this.calmSince = currentTime;
            }
        }
        if (this.src) {
            if (currentTime >= this.idleUntil) {
                while (this.buffered + this.asked * this.per < this.ahead * this.per) {
                    this.src.postMessage({ ahead: Math.round((this.buffered * 1000) / sampleRate) });
                    this.asked++;
                }
            }
            if (currentTime >= this.reportAt) {
                this.reportAt = currentTime + 1;
                if (this.starved) {
                    this.src.postMessage({ starved: Math.ceil(this.starved / this.per) });
                    this.starved = 0;
                }
            }
        }
        return true;
    }
}

registerProcessor('vector-mic', Mic);
registerProcessor('vector-speaker', Speaker);
