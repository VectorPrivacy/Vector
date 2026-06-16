# Vector Core

The headless engine behind [Vector](https://vectorapp.io) — a private messenger on
the [Nostr](https://nostr.com) protocol. `vector-core` is the **single source of
truth** for every Vector client: the desktop and mobile apps, the CLIs, the
[bot SDK](https://docs.rs/vector-sdk), and the MCP agent all import this one crate.
It holds all of the protocol logic so a client never has to touch a relay, a
gift-wrap, or an encryption key directly.

> **Building a bot?** You almost certainly want [`vector-sdk`](https://crates.io/crates/vector-sdk),
> an ergonomic, discord.js-style layer over this crate. Use `vector-core` directly
> when you're building a full custom client and want the low-level facade.

## What's inside

- **NIP-17 gift-wrapped DMs** — send/reply/edit/delete, reactions (incl. NIP-30 custom
  emoji), typing indicators, read state.
- **Communities (Concord)** — a Discord-style end-to-end-encrypted server/channel
  protocol over Nostr: epoch-keyed channels, roles & capabilities, invites
  (public links + gift-wrapped), bans/kicks with read-cut rekeys, re-foundings.
- **Encrypted files** — NIP-96 / Blossom upload with progress, retry, and server
  failover; AES-GCM at rest; hash-based dedup. Plus unencrypted public-image upload
  for avatars/banners.
- **Profiles** — fetch/update/status, blocking, local nicknames, a prioritized
  background sync queue.
- **Storage & crypto** — per-account SQLite (atomic migrations, connection pools),
  a `GuardedKey` vault (secret material in plaintext only microseconds per op),
  Argon2id, AES-GCM, ChaCha20, and SIMD hex (NEON / SSE2 / AVX2 with scalar fallback).
- **Resilience** — NIP-77 negentropy sync and an event-driven relay health monitor
  that reconnects dead relays and folds back anything missed offline.
- **Headless by design** — UI integration is abstracted behind the `EventEmitter`
  and `InboundEventHandler` traits, so the same engine drives a GUI, a CLI, or a bot.

## Quick start

```toml
[dependencies]
vector-core = "0.1"
tokio = { version = "1", features = ["full"] }
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

The `VectorCore` facade is a zero-sized handle over process-global state — cheap to
`Copy`, and every method (DMs, communities, profiles, files, sync) hangs off it.
To receive, implement [`InboundEventHandler`] and call `core.listen(handler).await`.

## One identity per process

`vector-core` is built on process-global state, so **one account is active per
process at a time**. Multiple identities means multiple processes — or
`core.swap_session()` to switch the active account in place. (Swaps can happen at
any await point; account-scoped work must guard against them — see `SessionGuard`.)

## The `nostr` dependency

`vector-core` depends on stock [`nostr`](https://crates.io/crates/nostr) from
crates.io. Within the Vector monorepo, a workspace `[patch.crates-io]` swaps in a
small fork that zeroizes secret keys on drop (defense-in-depth for the GUI app);
that patch is local to the workspace and does **not** affect this published crate or
its consumers, who get stock `nostr` and need no patch.

## License

MIT.

[`InboundEventHandler`]: https://docs.rs/vector-core/latest/vector_core/trait.InboundEventHandler.html
