# Vector SDK

Build a bot for [Vector](https://vectorapp.io) — a private, encrypted messenger — in
about a dozen lines of Rust. Your bot sends and receives messages, files, and reactions,
joins communities, and rides out network drops, without you ever touching the protocol
or encryption underneath.

## Quick start

```sh
cargo add vector_sdk
cargo add tokio --features full
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
| `channel.post(text, PostOptions::default().reply_to(id).expires_in(600))` | Send with a reply, an expiry, or addressed to one bot (`.to_bot(npub)`) |
| `channel.react(msg_id, "👍")` / `channel.unreact(msg_id, "👍")` | React with an emoji, or take it back |
| `channel.react_custom(msg_id, ":code:", url)` | React with a custom image emoji |
| `channel.typing()` | Show a typing indicator |
| `channel.edit(msg_id, text)` / `channel.delete(msg_id)` | Edit / delete a message the bot sent |
| `channel.send_file(path)` / `channel.send_bytes(bytes, "chart.png", PostOptions::default().caption("today"))` | Send a file, from disk or memory, with a caption |
| `bot.download_attachment(&att)` / `bot.save_attachment(&att, path)` | Get a received file |

On an incoming message: `msg.reply(text)` answers it, `msg.react(emoji)` reacts to it,
and any received files are on `msg.message.attachments`.

**Custom emoji** — equip a pack and write its emoji as `:code:` in anything the bot sends:

```rust
bot.subscribe_emoji_pack("naddr1...").await?;
for e in bot.emojis() { println!(":{}:", e.code); }  // names match Vector's picker
bot.channel(id).send("gm :wave:").await?;
```

A shortcode two packs share with different images is numbered, `:love~1:` and `:love~2:`.

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

**Running a community** — Roles, Categories, Channel permissions, Timeouts, pins and the Audit
Log, the same controls a person has in the app. Each action is checked the way every member's
client will judge it, so a refusal is an error here rather than an edit everyone ignores.

```rust
use vector_sdk::{NewChannel, Overwrite, Permissions, RoleSpec};

let mods = community.create_role(
    RoleSpec::new("Mods").color(0x59fcb3).permissions(Permissions::KICK | Permissions::MODERATE_MEMBERS).shown(),
).await?;
let support = community.create_category("Support").await?;
let tickets = community.create_channel_with("tickets", NewChannel::default().private().in_category(support.id())).await?;
tickets.set_overwrite(Overwrite::role(&mods).allow(Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES)).await?;

member.add_role(&mods).await?;
member.timeout(std::time::Duration::from_secs(600)).await?;
member.set_nickname("Kit").await?;     // everyone in the Community sees it
msg.pin().await?;
for entry in community.audit_log().await?.entries { println!("{:?} by {:?}", entry.action, entry.actor); }
```

Also on `Community`: `roles()`, `role_named(..)`, `reorder_roles(..)`, `set_everyone_permissions(..)`,
`categories()`, `channel_infos()`, `timeouts()`, `nicknames()`, `invite_links()` /
`revoke_invite(..)`, `relays()` / `move_relays(..)`, `voice_relays()` / `set_voice_relays(..)`,
`icon()` / `set_icon(bytes)` / `banner()` / `set_banner(bytes)` (images are stripped of metadata
first), and `refound(..)` / `repair_roster()` for recovery. On a Channel: `info()`, `rename`,
`set_topic`, `set_view("forum")`, `set_read_only(true)` (an announcement channel), `move_to(..)`, `overwrites()`, `sync_permissions(..)`,
`permissions_of(npub)`, `pins()`. Create and join with `bot.create_community(name, NewCommunity::default())`,
`bot.join(link)` and `bot.preview_link(link)`.

**Joining communities** — to be useful in a community, a bot has to accept invites.
Choose how:

```rust
VectorBot::builder().nsec(key).public().build().await?;                   // accept from anyone
VectorBot::builder().nsec(key).whitelist(["npub1owner…"]).build().await?; // only these accounts
```

By default, invites wait for you to handle them (`bot.pending_invites()` /
`bot.accept_invite(id)`). Auto-accept also picks up invites that arrived while the bot
was offline, so a restarted bot still joins.

**Commands** — don't parse message text by hand. Declare a slash command with typed
arguments and the SDK publishes a manifest for it, so every Vector client renders a `/`
picker with your command and a field per argument — a dropdown for a choice, a member
picker for a user, a number field for an int — and validates the input *before* it's sent.
Your handler gets the arguments already parsed and type-checked:

```rust
bot.command("weather", "Current conditions for a city")
    .string("city", "Which city", true)              // required free text
    .choice("units", "Temperature units", ["c", "f"], false)  // optional dropdown
    .run(|ctx| async move {
        let city = ctx.str("city").unwrap_or_default();
        let units = ctx.str("units").unwrap_or("c");
        ctx.reply(format!("Weather in {city} (°{})…", units.to_uppercase())).await.ok();
    });
```

Argument types: `.string()`, `.int()`, `.number()`, `.flag()` (bool), `.user()` (an npub),
and `.choice(name, desc, options, required)`; read them off `ctx` with `ctx.str/int/number/flag(name)`.
A matched command runs its handler and never reaches `on_message`, so commands and free-form
chat live side by side. The manifest publishes automatically when the bot starts listening.
See [`slash_command_bot`](examples/slash_command_bot.rs) for a full example.

**Receiving** — `bot.on_message(handler)` runs your handler for every incoming message
(DM or community); a slow handler won't hold up the others.

For more than messages, `bot.on_event(|bot, event|)` gives you the full stream — match
the parts you care about:

```rust
bot.on_event(|bot, event| async move {
    match event {
        BotEvent::Message(msg) if !msg.is_mine() => { msg.reply("hi").await.ok(); }
        BotEvent::MemberJoin { channel_id, npub, .. } => {
            bot.channel(channel_id).send("welcome!").await.ok();
        }
        BotEvent::Reaction { reaction, .. } => println!("{} reacted {}", reaction.author_id, reaction.emoji),
        BotEvent::Delete { by, moderated: true, .. } => println!("a moderator ({by:?}) removed a message"),
        BotEvent::CommunityChange { change, .. } => match change.kind {
            ChangeKind::MemberBanned { member } => println!("{member} was banned by {:?}", change.by),
            _ => {}
        },
        _ => {}
    }
}).await?;
```

`BotEvent` covers messages, reactions, edits, deletes (and whether a moderator did it), members
joining or leaving, typing in DMs and channels, invites, the bot being removed, a Community
dissolving, and `CommunityChange`: one event per change to Roles, Channels, Categories,
overwrites, bans, kicks, Timeouts, Nicknames, pins, settings and relays, each naming who made it.
`bot.run(handler)` is `on_event` that also exits cleanly on Ctrl-C.

**Staying connected** — if the bot loses its connection, it reconnects on its own and
catches up on what it missed. Your handler fires for messages
that arrive while the bot is running; to read older history, use `bot.core().get_messages(...)`.

**Profiles** — `bot.fetch_profile(npub)`, `bot.update_profile(...)`, `bot.set_status(...)`,
`bot.block(npub)` / `bot.unblock(npub)`, `bot.set_nickname(...)` (local only).

**HTTP** — `vector_sdk::net::build_http_client(timeout)` makes requests (an LLM, an API) over
the bot's own transport, so a bot on Tor or I2P never leaks its address with a side request.

**Going deeper** — `bot.core()` exposes the engine underneath. If you find yourself needing it,
or copying code out of Vector's app to make a bot work, the SDK is missing something: see
[Moving your bot onto the SDK](guides/moving-onto-the-sdk.md), and tell us what it was.

## Examples

Runnable, self-contained bots live in [`examples/`](examples) — each shows off one thing.
Most take `VECTOR_NSEC` (the bot's key); the Mini App ones create a key on first run and keep it
in their data folder.

| Example | What it shows |
| --- | --- |
| [`echo_bot`](examples/echo_bot.rs) | The minimal hello-world — replies to every message. |
| [`slash_command_bot`](examples/slash_command_bot.rs) | A `/command` router: `/ping`, `/echo`, `/roll`, `/help`. |
| [`ai_bot`](examples/ai_bot.rs) | An LLM chatbot: typing indicator, threaded replies, per-chat history. |
| [`community_admin_bot`](examples/community_admin_bot.rs) | Roles, a private category channel, `/timeout`, `/role`, `/pin`, `/audit`, and a log of every change. |
| [`moderation_bot`](examples/moderation_bot.rs) | Welcomes new members and auto-bans on a word filter. |
| [`policy_bot`](examples/policy_bot.rs) | Writes a moderation rulebook, previews it against history, then watches; acts only once armed. |
| [`raid_guard`](examples/raid_guard.rs) | Spots a raid cohort and contains it as one moderation unit. |
| [`whitelist_bot`](examples/whitelist_bot.rs) | A private bot that only joins communities it trusts. |
| [`v2_community_bot`](examples/v2_community_bot.rs) | Creates and owns a Community, prints its invite link, greets and echoes. |
| [`v2_send_once`](examples/v2_send_once.rs) | Joins by link, finds a channel by name, posts once and exits. |
| [`concordia`](examples/concordia.rs) | A command console over every send path, plus Community diagnostics. |
| [`v2_typing_probe`](examples/v2_typing_probe.rs) | A protocol probe: which relay forwards whose typing. |
| [`file_bot`](examples/file_bot.rs) | Sends one file, then exits. |
| [`save_files_bot`](examples/save_files_bot.rs) | Saves every received file to disk. |
| [`i2p_bot`](examples/i2p_bot.rs) | An echo bot that connects only through your I2P router (`i2p` feature). |
| [`voice_whisper_bot`](examples/voice_whisper_bot.rs) | A voice assistant: writes down what each person says in the call's chat and answers out loud (`whisper` feature). |
| [`voice_echo_bot`](examples/voice_echo_bot.rs) | Joins a voice Channel and says back what you say (`voice` feature). |
| [`voice_radio_bot`](examples/voice_radio_bot.rs) | Plays a track on a loop in as many voice Channels as you name (`voice` feature). |
| [`xdc_counter_bot`](examples/xdc_counter_bot.rs) | The smallest bot-backed Mini App: a number a whole chat shares (`xdc` feature). |
| [`xdc_tictactoe_2d_bot`](examples/xdc_tictactoe_2d_bot.rs) | Ships tic-tac-toe, referees it and plays O (`xdc` feature). |
| [`xdc_oracle_bot`](examples/xdc_oracle_bot.rs) | Ships its own Mini App and streams LLM answers into it (`xdc` feature). |
| [`xdc_tictactoe_bot`](examples/xdc_tictactoe_bot.rs) | Plays 3D Tic-Tac-Toe, an app it didn't write, against whoever opens it (`xdc` feature). |
| [`xdc_spy`](examples/xdc_spy.rs) | Prints every message of any app opened with it: learn an app's protocol (`xdc` feature). |
| [`xdc_probe`](examples/xdc_probe.rs) | Tests a game bot without a phone: shares the app and plays the human's side (`xdc` feature). |

```sh
# Echo bot — replies to every message
VECTOR_NSEC=nsec1... cargo run --example echo_bot

# AI bot — wire any OpenAI-compatible endpoint to your chats
OPENAI_API_KEY=sk-... VECTOR_NSEC=nsec1... cargo run --example ai_bot
```

## Checking a setup live

[`vector-lab`](https://github.com/VectorPrivacy/Vector/tree/master/crates/vector-lab) runs bots like these against real relays and checks what each one
saw: events, the gap while a bot restarts, voice, and a flood. Useful before deploying a bot, or
to see whether a relay works with Vector:

```sh
cargo run -p vector-lab -- all --relays wss://your.relay
```

## Voice Channels

With the `voice` feature a bot joins voice Channels: it hears what each member says and speaks
into the call. Audio is 48 kHz mono PCM, and one bot can be in many calls at once.

```sh
cargo add vector_sdk --features voice
```

```rust
let listener = bot.clone();
tokio::spawn(async move { listener.run(|_, _| async {}).await }); // how a call finds its members
let call = bot.channel(voice_channel_id).join_voice().await?;
call.say(&vector_sdk::voice::tone(440.0, 1000));          // queue a second of A4
while let Some(speech) = call.listen().await {             // 20 ms frames, per speaker
    println!("{:?}: {} samples", speech.npub, speech.pcm.len());
}
```

A bot must be listening (`run` or `on_event`) to join a call. Play a file with
`vector_sdk::voice::decode_with_ffmpeg(path)` then `call.say_and_wait(&pcm)`.

A call ends when a moderator disconnects or moves the bot: `listen()` returns `None` and `say`
returns false. A move says where it sent the bot, to follow if you like:

```rust
if let Some(to) = call.moved_to() {
    call = bot.channel(&to).join_voice().await?;
}
```

### Transcribing

`voice::Utterances` turns the frame stream into what each person said, one clip per pause, and
with the `whisper` feature `voice::Transcriber` turns a clip into text:

```rust
use vector_sdk::voice::{Transcriber, Utterances};

let whisper = Transcriber::load("ggml-base.en.bin")?.language("en");
let mut said = Utterances::new();
while let Some(speech) = call.listen().await {
    if let Some(utterance) = said.push(speech) {
        println!("{:?}: {}", utterance.npub, whisper.transcribe(&utterance.pcm).await?);
    }
}
```

Models are whisper.cpp's ggml files from [huggingface.co/ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp).
`whisper` builds whisper.cpp (on the GPU with Metal on macOS).

See [`voice_whisper_bot`](examples/voice_whisper_bot.rs) (a voice assistant),
[`voice_echo_bot`](examples/voice_echo_bot.rs) (says back what you say) and
[`voice_radio_bot`](examples/voice_radio_bot.rs) (one track, many Channels). Building needs
`cmake`, for libopus and whisper.cpp.

## Mini Apps (WebXDC)

A Mini App is an `.xdc` file shared in a chat: a game or tool that everyone who opens it uses
together. With the `xdc` feature your bot joins in too, as an opponent, a referee, or the server
behind an app that needs more than a phone (an LLM, a database, a game's master copy).

```sh
cargo add vector_sdk --features xdc
```

```rust
// Runs whenever someone opens an app whose manifest says id = "vector-counter",
// for as long as they have it open.
bot.xdc("vector-counter").run(|_bot, mut session| async move {
    while let Some(event) = session.next().await {
        if let XdcEvent::Data(frame) = event {
            let msg: serde_json::Value = frame.json().unwrap_or_default();
            if msg["t"] == "hello" {
                session.send_json(&serde_json::json!({ "t": "count", "value": 0 })).await.ok();
            }
        }
    }
});
```

Two guides walk through it end to end:

- [Add a bot to your Mini App](guides/bot-for-your-mini-app.md): you wrote the app, now give it a bot.
- [Write a bot for any Mini App](guides/bot-for-any-mini-app.md): learn an app's messages from its source and its traffic, then join in.

| You want to… | …you call |
| --- | --- |
| React to an app being opened | `bot.xdc("app-id").run(…)`, or `"*"` for every app |
| Join one copy yourself | `msg.xdc()?.join().await` |
| Hand out your app | `channel.send_xdc("app.xdc").await` |
| Talk to everyone in it | `session.send_json(&v)` / `send_text` / `send`, then `session.next()` |
| Know who sent a message | `frame.from.npub`, or `frame.verified_sender()` when it matters |
| Read the app's manifest | `xdc.manifest().await` |

It works the same in DMs and in Communities. Players in a session can see each other's IP
address, and the bot's; a bot on Tor or I2P stays out of sessions unless you call
`vector_sdk::xdc::allow_outside_transport(true)`.

## Accounts & keys

- **No key** — `build()` creates an identity on first run and reuses it every run after.
  Perfect for a first bot. Running several keyless bots? Give each its own `.data_dir(...)`.
- `.nsec("nsec1...")` — an existing key.
- `.mnemonic("twelve words ...")` — a 12-word seed phrase.
- `.password("...")` — only for keys that are encrypted at rest.
- `VectorBot::generate_nsec()` — mint a fresh key yourself.
- `.data_dir(path)` or `VECTOR_DATA_DIR` — where it keeps its identity and database. Every bot
  on a machine needs its own.

A keyless bot's identity is stable across restarts, so it keeps its chats and
community memberships. Storage defaults to a per-OS application directory; override it
with `.data_dir(path)`.

## One bot per process

A bot owns the process while it runs, so run **one bot per process**. To run several
bots, run several processes.

## Optional: Tor

To route the bot through Tor, enable the `tor` feature **and** call `.tor()` on the builder
(the feature alone only compiles Tor in; `.tor()` turns it on):

```sh
cargo add vector_sdk --features tor
```

```rust
let bot = VectorBot::builder().nsec(key).tor().build().await?;
// .tor_bridges(["1.2.3.4:443 <fingerprint>"]) for networks where Tor is blocked
```

Tor is bootstrapped during `build()` *before* the bot connects, so it never touches the
network in the clear. The feature is off by default, keeping the dependency tree light.

## Optional: I2P

With the `i2p` feature, `.i2p()` routes the bot through the I2P router already running on the
machine (i2pd, or Java I2P with its SAM bridge on, at `127.0.0.1:7656`). Vector runs no router of
its own. `.i2p` relays are reached inside I2P and everything else through an outproxy; `build()`
waits until I2P is ready and fails with the router's reason when it can't be used.
`.i2p_with(I2pOptions { .. })` sets another SAM port, SAM credentials, I2P-Only (no outproxy) or
your own outproxy list. The choice is saved with the bot's account: a bot whose account uses Tor
or I2P must name it again on every run, and `.clearnet()` moves it back to direct connections.

```sh
cargo add vector_sdk --features i2p
```

```rust
let bot = VectorBot::builder().nsec(key).i2p().build().await?;
```

See [`i2p_bot`](examples/i2p_bot.rs).

## License

MIT.
