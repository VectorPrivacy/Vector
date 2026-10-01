# vector-calls

The half of [Vector](https://vectorapp.io)'s voice and video calls that runs anywhere: the wire format, the jitter buffer, bitrate control, and the small signal-processing pieces that need no device.

Everything that touches a microphone, a speaker, a codec library or a socket lives in the client that embeds this crate. [`vector-core`](https://crates.io/crates/vector-core) runs the call itself (signalling, the Iroh connection, the call's state) on top of it with its `calls` feature, and each Vector client brings the devices.

There is no clock in here. Every function that reasons about time takes milliseconds from the caller, so the same code runs under tokio, on a plain thread, or in a browser, and the crate builds for `wasm32-unknown-unknown`.

## What's inside

| Module | What it does |
| --- | --- |
| [`wire`](src/wire.rs) | The call's wire format over one QUIC connection: a reliable control stream (length-prefixed JSON `Control` messages), unreliable datagrams for audio (`pack` / `unpack`), and one stream per video frame (`VideoHeader`). |
| [`jitter`](src/jitter.rs) | The adaptive jitter buffer: holds a few audio frames to smooth the network's unevenness, grows on late arrivals, and gives the latency back when the path settles. |
| [`rate`](src/rate.rs) | Bitrate control fed by QUIC's own loss accounting: `RateControl` walks the audio ladder (12 to 64 kbps, with FEC), `VideoRate` the camera and screen rungs. |
| [`link`](src/link.rs) | The binary messages between the media engine and whatever captures, encodes, decodes and paints video. |
| [`ring`](src/ring.rs) | A lock-free, allocation-free sample ring for audio callbacks. |
| [`resample`](src/resample.rs) | A streaming linear resampler with no seam between blocks. |
| [`declick`](src/declick.rs) | Removes the click a microphone makes when its cable is pulled or pushed in. |
| [`share`](src/share.rs) | A shared screen's sound on its way to the mixer. |
| [`stats`](src/stats.rs) | The per-call counters the media threads share. |

Audio runs at 48 kHz in 20 ms frames (`ENGINE_RATE`, `FRAME_MS`, `FRAME`).

## The audio path

The sender packs each encoded 20 ms frame into a datagram. The receiver unpacks it into the jitter buffer and pulls one frame every 20 ms, and once a second it feeds QUIC's packet counts to the rate control, which tells the encoder when to change.

```rust
use vector_calls::jitter::{Jitter, Pop};
use vector_calls::rate::RateControl;
use vector_calls::stats::MediaStats;
use vector_calls::wire;

const MUTED: u16 = 1; // a flag your sender sets on silent frames

let stats = MediaStats::default();
let mut jitter = Jitter::default();
let mut rate = RateControl::new();

// Sender: one encoded frame per datagram.
let datagram = wire::pack(seq, ms_since_start, 0, &opus_frame);

// Receiver: every datagram goes into the buffer...
if let Some(frame) = wire::unpack(&datagram) {
    jitter.push(frame.seq, frame.flags & MUTED != 0, frame.payload, now_ms, &stats);
}

// ...and the playout clock takes one frame every 20 ms.
match jitter.pop(concealed_in_a_row, now_ms, &stats) {
    Pop::Frame(opus) => { /* decode it */ }
    Pop::Fec(next) => { /* rebuild the missing frame from the FEC in `next` */ }
    Pop::Conceal => { /* let the decoder conceal the gap */ }
    Pop::Muted | Pop::Wait => { /* play silence */ }
}

// Once a second, with the connection's cumulative packet counts.
if let Some(setting) = rate.observe(sent_packets, lost_packets) {
    // encoder.set_bitrate(setting.kbps * 1000); encoder.set_fec(setting.fec_pct);
}
```

## License

MIT
