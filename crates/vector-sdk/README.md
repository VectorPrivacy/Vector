# Vector SDK

An ergonomic Rust SDK for building [Vector](https://vectorapp.io) bots and clients.

Vector is a private messenger built on the Nostr protocol. This SDK is a thin,
friendly skin over [`vector-core`](../vector-core) — the headless library that
holds **all** of Vector's protocol logic. You get NIP-17 gift-wrapped DMs, file
attachments, reactions, typing indicators, edits, deletes, and profiles without
ever touching a relay, a gift-wrap, or an encryption key directly.

> **This is a ground-up rewrite of the old `vector_sdk` (0.2.x).** The previous
> version hand-rolled gift-wrapping, AES-GCM file encryption, NIP-96 upload, and
> reactions against `nostr-sdk` 0.42 directly. All of that now lives in
> `vector-core`, so the SDK is a small ergonomic layer on top of the same engine
> that powers the Vector desktop and mobile apps.

## Quick start

```toml
[dependencies]
vector-sdk = { path = "../crates/vector-sdk" } # or git, see "Using outside the workspace"
tokio = { version = "1", features = ["full"] }
```

```rust
use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let bot = VectorBot::builder()
        .nsec("nsec1...")          // or .mnemonic("twelve words ...")
        .build()
        .await?;

    println!("Logged in as {}", bot.npub());

    // Send a message — `channel` auto-detects DM (npub) vs Community channel (hex id).
    bot.channel("npub1...").send("Hello from a bot!").await?;

    // Echo every inbound message back — the SAME handler serves DMs AND Community channels.
    bot.on_message(|_bot, msg| async move {
        if msg.is_mine() { return; }
        let _ = msg.reply(&format!("Echo: {}", msg.text())).await;
    }).await?;

    Ok(())
}
```

## One uniform API for DMs and Communities

A bot author never branches on "is this a DM or a Community channel?" — like
discord.js, a **`Channel`** is a `Channel` and you send/receive the same way.
`bot.channel(id)` **auto-detects** the transport from the id (an `npub` → DM, a
64-char hex → Community channel); `msg.reply(...)` responds wherever the
message came from. The gift-wrap-vs-Concord split lives entirely inside the SDK.

```rust
// All identical whether `msg` came from a DM or a Community channel:
msg.reply("got it").await?;               // respond in the same conversation
msg.react("👍").await?;                   // react to this message
msg.channel().typing().await?;             // "thinking…" indicator

// Or address a conversation directly (auto-detected):
bot.channel(id).send("hi").await?;
bot.channel(id).edit(&msg_id, "fixed").await?;
bot.channel(id).delete(&msg_id).await?;
// Explicit constructors when you know the kind: bot.dm(npub) / bot.community(channel_id)
```

## What you can do

**Messaging** — a unified `Channel` from `bot.channel(id)` / `bot.dm(npub)` /
`bot.community(channel_id)`, or `msg.channel()`:

| Method | Does (DM **and** Community) |
| --- | --- |
| `channel.send(text)` | Send a text message |
| `channel.react(msg_id, "👍")` | React with a unicode emoji |
| `channel.react_custom(msg_id, ":code:", url)` | React with a NIP-30 custom emoji |
| `channel.typing()` | Send a typing indicator |
| `channel.edit(msg_id, new_text)` | Edit a message you sent |
| `channel.delete(msg_id)` | Delete a message you sent |
| `channel.send_file(path)` | Send an encrypted file attachment |
| `msg.reply(text)` / `msg.react(emoji)` | Respond to an inbound message uniformly |

**Community management** — a message hands you the *actor in context* (discord.js-style),
so you act on the sender directly:

```rust
// In a community channel handler:
if let Some(member) = msg.member() {        // the sender, as a Member of this community
    member.kick().await?;                    // or .ban() / .unban()
    member.grant_admin().await?;             // or .revoke_admin()
    let prof = member.profile().await;       // their profile
    if member.is_admin() { /* ... */ }       // is_owner() too
}

// Or address a community by id:
let community = bot.community(community_id);  // also: msg.community(), bot.communities()
community.invite("npub1...").await?;
let link = community.create_invite().await?;
community.edit(Some("New name"), None).await?;
for m in community.members().await { /* ... */ }
community.leave().await?;                     // dissolve(), capabilities(), roles()
```

**Receiving** — `bot.on_message(handler)` runs an async handler per inbound
message — DMs **and** Community channel messages — each on its own task
(`msg.is_group` tells them apart if you care). For full control,
`bot.listen_with(handler)` takes a raw `InboundEventHandler`.

**Outage resilience** — `on_message`/`listen` catch up on connect, then a **relay health monitor**
takes over: it force-reconnects dead/zombie relays and, on each reconnect, folds back anything
missed while offline (re-foundings, rekeys, bans, metadata, recent messages) into local state. It's
event-driven (no idle polling) — work happens only when a relay actually (re)connects.
`bot.sync_communities()` and `bot.sync_dms(since_days)` (NIP-77 negentropy) are also exposed for
manual catch-up.

**Profiles** — `bot.fetch_profile(npub)`, `bot.update_profile(...)`,
`bot.set_status(...)`, `bot.block/unblock(...)`, `bot.set_nickname(...)`,
`bot.blocked_users()`.

**Going deeper** — `bot.core()` returns the full [`VectorCore`] facade for
everything not surfaced here, including **Community management**
(create/join/invite/sync/roles/ban/kick), custom rumors, and lower-level controls.

## Examples

```sh
# Echo bot — replies to every DM
VECTOR_NSEC=nsec1... cargo run -p vector-sdk --example echo_bot

# File bot — sends one file then exits
VECTOR_NSEC=nsec1... VECTOR_TARGET=npub1... VECTOR_FILE=./image.png \
  cargo run -p vector-sdk --example file_bot
```

## Important: one identity per process

`vector-core` is built on process-global state, so **one `VectorBot` owns the
process's identity at a time**. Build one bot per process. To run several
identities, run several processes — or use `bot.core().swap_session()` to switch
the active account in place. (This is a deliberate change from the old
`VectorBot`, which carried its own keys and client and could host many bots in
one process.)

## Accounts & keys

- `.nsec("nsec1...")` — an existing secret key.
- `.mnemonic("...")` — a BIP-39 seed phrase (NIP-06 derivation).
- `.password("pin")` — required only for accounts encrypted at rest.
- `VectorBot::generate_nsec()` — mint a fresh identity.

Storage (the SQLite DB and per-account data) defaults to a per-OS application
directory; override it with `.data_dir(path)`.

## Using outside this workspace

`vector-core` depends on a small VectorPrivacy fork of `nostr` (it zeroizes
secret keys on drop) applied via a workspace `[patch.crates-io]`. A consumer
outside this monorepo must replicate that one line in its **root** `Cargo.toml`:

```toml
[patch.crates-io]
nostr = { git = "https://github.com/VectorPrivacy/nostr.git", branch = "zeroize-secretkey" }
```

Embedded Tor (Arti) is **opt-in** via the `tor` feature, which is off by
default — so the SDK's dependency tree stays light unless you ask for it.

## License

MIT.

[`VectorCore`]: https://docs.rs/vector-core
