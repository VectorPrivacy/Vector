# Vector SDK

Build a bot for [Vector](https://vectorapp.io) — a private, encrypted messenger — in
about a dozen lines of Rust. Your bot sends and receives messages, files, and reactions,
joins communities, and rides out network drops, without you ever touching the protocol
or encryption underneath.

## Quick start

```toml
[dependencies]
vector_sdk = "0.3"
tokio = { version = "1", features = ["full"] }
```

```rust
use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let bot = VectorBot::builder()
        .nsec("nsec1...")          // the bot's key — or omit it and one is created for you
        .build()
        .await?;

    println!("Online as {}", bot.npub());

    // Reply to every message the bot receives.
    bot.on_message(|_bot, msg| async move {
        if msg.is_mine() { return; }
        let _ = msg.reply(&format!("You said: {}", msg.text())).await;
    }).await?;

    Ok(())
}
```

## One API, everywhere

Your bot sends and receives through a **`Channel`** — a direct-message chat or a
community channel, handled **identically**. You never branch on "is this a DM or a
community?"; you just send and reply.

```rust
// `msg` could be from a DM or a community channel — same code either way:
msg.reply("got it").await?;        // reply in the same chat or channel
msg.react("👍").await?;            // react to it
msg.channel().typing().await?;     // show a typing indicator

// Or message a chat or channel directly by its id:
bot.channel(id).send("hi").await?;
```

## What your bot can do

**Messaging** — on any chat or channel (`bot.channel(id)`, `bot.dm(npub)`, or `msg.channel()`):

| Method | What it does |
| --- | --- |
| `channel.send(text)` | Send a message |
| `channel.reply(msg_id, text)` | Reply to a specific message (threaded) |
| `channel.react(msg_id, "👍")` | React with an emoji |
| `channel.react_custom(msg_id, ":code:", url)` | React with a custom image emoji |
| `channel.typing()` | Show a typing indicator |
| `channel.edit(msg_id, text)` / `channel.delete(msg_id)` | Edit / delete a message the bot sent |
| `channel.send_file(path)` | Send a file |
| `bot.download_attachment(&att)` / `bot.save_attachment(&att, path)` | Get a received file |

On an incoming message: `msg.reply(text)` answers it, `msg.react(emoji)` reacts to it,
and any received files are on `msg.message.attachments`.

**Communities** — when a message comes from a community, you get the sender as a member
you can act on directly:

```rust
if let Some(member) = msg.member() {
    member.kick().await?;          // or .ban() / .unban()
    member.grant_admin().await?;   // or .revoke_admin()
    if member.is_admin() { /* ... */ }
}

// Or manage a community directly:
let community = bot.community(community_id);  // also: msg.community(), bot.communities()
community.invite("npub1...").await?;
let link = community.create_invite().await?;
for m in community.members().await { /* ... */ }
```

**Joining communities** — to be useful in a community, a bot has to accept invites.
Choose how:

```rust
VectorBot::builder().nsec(key).public().build().await?;                   // accept from anyone
VectorBot::builder().nsec(key).whitelist(["npub1owner…"]).build().await?; // only these accounts
```

By default, invites wait for you to handle them (`bot.pending_invites()` /
`bot.accept_invite(id)`). Auto-accept also picks up invites that arrived while the bot
was offline, so a restarted bot still joins.

**Receiving** — `bot.on_message(handler)` runs your handler for every incoming message
(DM or community); a slow handler won't hold up the others.

For more than messages, `bot.on_event(|bot, event|)` gives you the full stream — match
the parts you care about:

```rust
bot.on_event(|bot, event| async move {
    match event {
        BotEvent::Message(msg) if !msg.is_mine() => { msg.reply("hi").await.ok(); }
        BotEvent::MemberJoin { channel_id, npub } => {
            bot.channel(channel_id).send("welcome!").await.ok();
        }
        BotEvent::MessageUpdate { .. } => { /* a reaction or edit landed */ }
        _ => {}
    }
}).await?;
```

`BotEvent` covers messages, reactions/edits, deletes, members joining or leaving, typing,
invites, and the bot being removed.

**Staying connected** — if the bot loses its connection, it reconnects on its own and
catches up on what it missed. Your handler fires for messages
that arrive while the bot is running; to read older history, use `bot.core().get_messages(...)`.

**Profiles** — `bot.fetch_profile(npub)`, `bot.update_profile(...)`, `bot.set_status(...)`,
`bot.block(npub)` / `bot.unblock(npub)`, `bot.set_nickname(...)`.

**Going deeper** — `bot.core()` exposes the full engine for anything not surfaced here
(creating communities, reading history, and lower-level controls).

## Examples

Runnable, self-contained bots live in [`examples/`](examples) — each shows off one thing.
Every one needs `VECTOR_NSEC` (the bot's key); a few take extra env vars.

| Example | What it shows |
| --- | --- |
| [`echo_bot`](examples/echo_bot.rs) | The minimal hello-world — replies to every message. |
| [`slash_command_bot`](examples/slash_command_bot.rs) | A `/command` router: `/ping`, `/echo`, `/roll`, `/help`. |
| [`ai_bot`](examples/ai_bot.rs) | An LLM chatbot: typing indicator, threaded replies, per-chat history. |
| [`moderation_bot`](examples/moderation_bot.rs) | Welcomes new members and auto-bans on a word filter. |
| [`whitelist_bot`](examples/whitelist_bot.rs) | A private bot that only joins communities it trusts. |
| [`file_bot`](examples/file_bot.rs) | Sends one file, then exits. |
| [`save_files_bot`](examples/save_files_bot.rs) | Saves every received file to disk. |

```sh
# Echo bot — replies to every message
VECTOR_NSEC=nsec1... cargo run --example echo_bot

# AI bot — wire any OpenAI-compatible endpoint to your chats
OPENAI_API_KEY=sk-... VECTOR_NSEC=nsec1... cargo run --example ai_bot
```

## Accounts & keys

- **No key** — `build()` creates an identity on first run and reuses it every run after.
  Perfect for a first bot. Running several keyless bots? Give each its own `.data_dir(...)`.
- `.nsec("nsec1...")` — an existing key.
- `.mnemonic("twelve words ...")` — a 12-word seed phrase.
- `.password("...")` — only for keys that are encrypted at rest.
- `VectorBot::generate_nsec()` — mint a fresh key yourself.

A keyless bot's identity is stable across restarts, so it keeps its chats and
community memberships. Storage defaults to a per-OS application directory; override it
with `.data_dir(path)`.

## One bot per process

A bot owns the process while it runs, so run **one bot per process**. To run several
bots, run several processes.

## Optional: Tor

To route the bot through Tor, enable the `tor` feature **and** call `.tor()` on the builder
(the feature alone only compiles Tor in; `.tor()` turns it on):

```toml
vector_sdk = { version = "0.3", features = ["tor"] }
```

```rust
let bot = VectorBot::builder().nsec(key).tor().build().await?;
// .tor_bridges(["1.2.3.4:443 <fingerprint>"]) for networks where Tor is blocked
```

Tor is bootstrapped during `build()` *before* the bot connects, so it never touches the
network in the clear. The feature is off by default, keeping the dependency tree light.

## License

MIT.
