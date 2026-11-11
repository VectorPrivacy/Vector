# vector-media

Headless media for [Vector](https://vectorapp.io): hear and speak in Vector's voice Channels from
a program with no microphone or speaker, such as a bot or a server.

Audio arrives as 48 kHz mono PCM, one 20 ms frame per speaker, and goes out from PCM you queue,
encoded with Opus and paced in real time. It speaks the same wire format as the Vector app, and
one process can be in many calls at once, each with its own engine.

Most programs want this through [`vector_sdk`](https://crates.io/crates/vector_sdk)'s `voice`
feature, which adds `channel.join_voice()`. Use this crate directly to build a client of your own
on [`vector-core`](https://crates.io/crates/vector-core):

```rust,ignore
vector_media::install();                                   // once per process
vector_core::calls::voice::join_also(&community, &channel).await?;
let engine = vector_media::engine_in(&channel).unwrap();   // this call's media
engine.say(&tone);                                         // queue 48 kHz mono samples
while let Some(frame) = engine.listen().await {            // None once the call ends
    // frame.pcm: 960 samples; empty when frame.endpoint stopped being heard
}
```

What a program falls behind on, it loses oldest first. `say` returns false and `finished`
resolves false once the call has ended (left, or a moderator disconnected or moved the bot).

Building needs `cmake`: libopus is compiled from its bundled source.

License: MIT.
