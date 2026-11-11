# Vector Core

The headless engine behind [Vector](https://vectorapp.io), a private messenger on the
[Nostr](https://nostr.com) protocol. `vector-core` is the **single source of truth** for every
Vector client: the desktop and mobile apps, Vector Web, the CLIs, the
[bot SDK](https://docs.rs/vector_sdk) and the MCP agent all import this one crate. It holds all of
the protocol logic, so a client never has to touch a relay, a gift wrap or an encryption key
directly.

> **Building a bot?** You almost certainly want [`vector_sdk`](https://crates.io/crates/vector_sdk),
> the ergonomic layer over this crate. Use `vector-core` directly when you're building a full
> client of your own and want the low-level facade.

## What's inside

- **NIP-17 gift-wrapped DMs**: send, reply, edit, delete, reactions (NIP-30 custom emoji
  included), typing indicators, read state.
- **Communities (Concord)**: end-to-end encrypted servers and Channels over Nostr. Epoch-keyed
  Channels, Roles and permission overwrites, Categories, voice and forum Channels, invites
  (public links and gift-wrapped), bans, kicks, Timeouts and Nicknames, pins, an Audit Log, and
  a typed feed of every change with who made it (`community::v2::changes`).
- **Encrypted files**: Blossom upload with progress, retry and server failover; AES-GCM at rest;
  hash-based dedup. `image_prep` fits, re-encodes and strips the metadata of a picture before it
  is shared.
- **Profiles**: fetch, update, status, blocking, local nicknames, a prioritised background sync
  queue.
- **Storage and crypto**: per-account SQLite (atomic migrations, connection pools), a
  `GuardedKey` vault (a secret is in plaintext only for microseconds per operation), Argon2id,
  AES-GCM, ChaCha20, and SIMD hex (NEON, SSE2 and AVX2, with a scalar fallback).
- **Resilience**: NIP-77 negentropy sync, live subscriptions restored when a relay closes them,
  and a catch-up after every reconnect or sleep. A listener is told everything sent since it
  began listening exactly once, whichever path carried it (`news`).
- **One transport layer**: every connection goes through Clearnet, embedded Tor or I2P, as the
  account chooses, and fails closed when the chosen network isn't up.
- **Headless by design**: UI integration sits behind the `EventEmitter` and
  `InboundEventHandler` traits, so one engine drives a GUI, a CLI or a bot.

## Quick start

```sh
cargo add vector-core
cargo add tokio --features full
```

```rust
use vector_core::{CoreConfig, VectorCore};

#[tokio::main]
async fn main() -> vector_core::Result<()> {
    let core = VectorCore::init(CoreConfig {
        data_dir: "./vector-data".into(),
        event_emitter: None,
    })?;

    core.login("nsec1...", None).await?;          // or a BIP-39 mnemonic
    core.send_dm("npub1...", "Hello from vector-core").await?;

    Ok(())
}
```

The `VectorCore` facade is a zero-sized handle over process-global state: cheap to `Copy`, with
every method (DMs, Communities, profiles, files, sync) hanging off it. To receive, implement
[`InboundEventHandler`] and call `core.listen(handler).await`.

## Features

| Feature | What it adds |
| --- | --- |
| `calls` | 1:1 calls and voice Channels over Iroh: the call session and its video track. A client supplies the devices through `calls::platform` ([`vector-media`](https://crates.io/crates/vector-media) does it headless). |
| `xdc` | Mini Apps (WebXDC): join a shared app's realtime session. |
| `tor` | Embedded Tor (Arti) as the account's network. |
| `i2p` | I2P through a router the machine already runs (SAMv3). |

## One identity per process

`vector-core` is built on process-global state, so **one account is active per process at a
time**. Several identities means several processes, or `core.swap_session()` to switch the
active account in place. Each account's state lives on its `Session`, so work started under one
account finishes under it even if another logs in meanwhile.

## The `nostr` dependency

`vector-core` depends on stock [`nostr`](https://crates.io/crates/nostr) from crates.io. Within
the Vector monorepo, a workspace `[patch.crates-io]` swaps in a small fork that zeroizes secret
keys on drop (defense in depth for the app). That patch is local to the workspace: it doesn't
reach this published crate or its users, who get stock `nostr` and need no patch.

## License

MIT.

[`InboundEventHandler`]: https://docs.rs/vector-core/latest/vector_core/trait.InboundEventHandler.html
