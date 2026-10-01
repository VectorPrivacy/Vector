# Add a bot to your Mini App

You wrote the app, so you already know what it says. A bot joins the same realtime channel your app uses and speaks your messages: it can be the opponent, the referee, or the server behind the app, holding whatever a phone can't (a model, a database, the master copy of a game).

This guide builds two working examples, both in [`examples/`](../examples):

- [`xdc_counter_bot`](../examples/xdc_counter_bot.rs): a number everyone in a chat shares, with the bot keeping count.
- [`xdc_tictactoe_2d_bot`](../examples/xdc_tictactoe_2d_bot.rs): tic-tac-toe where the bot referees and plays O.

Each ships its own app from a folder beside it ([`xdc_counter/`](../examples/xdc_counter), [`xdc_tictactoe_2d/`](../examples/xdc_tictactoe_2d)).

## How it fits together

A Mini App is an `.xdc` file: a zip with an `index.html` and a `manifest.toml`. Someone shares it in a chat, and everyone who opens it joins its **realtime channel**, where a message one of them sends reaches the others. Each shared copy of the app has a channel of its own.

Your bot joins that channel the moment someone opens the app, and leaves when they're done. It sees the same messages the app does and sends its own.

## 1. Give your app an id

```toml
# manifest.toml
name = "Counter"
id = "vector-counter"
version = "1.0.0"
```

The bot recognises your app by `id`, so keep it the same across versions.

## 2. Decide the messages

Most apps send JSON with a `t` field naming the message. The counter needs three:

| Direction | Message | Meaning |
| --- | --- | --- |
| app → bot | `{"t":"hello"}` | What's the count? |
| app → bot | `{"t":"add","by":1}` | Change it by +1 or -1 |
| bot → app | `{"t":"count","value":7}` | Here it is |

Two habits make an app and its bot hold up in real chats:

- Send the whole state after every change, never just the change. A message can get lost on the way, and the next full state repairs it.
- Greet everyone who arrives. Someone who opens the app later missed everything before them, so the bot sends them the state. The app also says `hello` when it starts, in case the bot was there first.

## 3. The app side

Every Mini App reaches the channel through `window.webxdc`:

```js
const enc = new TextEncoder();
const dec = new TextDecoder();
const channel = window.webxdc.joinRealtimeChannel();
const send = (msg) => channel.send(enc.encode(JSON.stringify(msg)));

channel.setListener((bytes) => {
  const msg = JSON.parse(dec.decode(bytes));
  if (msg.t === 'count') document.getElementById('count').value = msg.value;
});

document.getElementById('plus').onclick = () => send({ t: 'add', by: 1 });
send({ t: 'hello' });
```

The channel carries bytes, up to 128,000 per message, so encode your JSON as above. `webxdc.selfAddr` is the player's npub and `webxdc.selfName` their display name, if you want to show who did what.

## 4. The bot side

Turn on the `xdc` feature:

```toml
[dependencies]
vector_sdk = { version = "0.10", features = ["xdc"] }  # `xdc` needs the first release after 0.10.0
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

Then handle your app by its id:

```rust
use serde_json::{json, Value};
use vector_sdk::{VectorBot, XdcEvent};

let bot = VectorBot::builder().data_dir("./counter-data").public().build().await?;

bot.xdc("vector-counter").run(|_bot, mut session| async move {
    let mut count = 0i64; // lives as long as this session; step 6 keeps it longer
    while let Some(event) = session.next().await {
        match event {
            XdcEvent::PeerJoined(_) => {}                       // someone arrived: greet them
            XdcEvent::Data(frame) => {
                let Some(msg) = frame.json::<Value>() else { continue };
                match msg["t"].as_str() {
                    Some("hello") => {}
                    Some("add") => count += msg["by"].as_i64().unwrap_or(0).clamp(-1, 1),
                    _ => continue,
                }
            }
            _ => continue,
        }
        let _ = session.send_json(&json!({ "t": "count", "value": count })).await;
    }
});
```

The handler runs once for each open copy of the app, for as long as someone has it open. `session.next()` gives you:

- `XdcEvent::Data(frame)`: a message. `frame.json()` parses it, `frame.text()` reads it as text, `frame.payload` is the raw bytes.
- `XdcEvent::PeerJoined(peer)` / `XdcEvent::PeerLeft(peer)`: someone opened or closed the app.

`session.send_json`, `send_text` and `send` reach everyone in the session at once. To send from another task while this one reads, take a `session.sender()`.

When the last player closes the app, the bot waits a few minutes, then `next()` returns `None` and the session ends. `.idle_timeout(..)` before `.run(..)` changes how long it waits.

## 5. Ship the app

People can share your `.xdc` themselves, and the bot joins any copy with your id. The bot can also hand it out:

```rust
bot.on_message(move |_bot, msg| {
    let app = app.clone(); // a PathBuf to your .xdc
    async move {
        if msg.text().trim() == "!counter" {
            let _ = msg.channel().send_xdc(&app).await;
        }
    }
}).await?;
```

The examples zip their app's files at startup (`package_app` at the bottom of each), so the app lives inside the bot's binary and can't drift out of step with it.

## 6. Keep state between sessions

A session ends when everyone leaves, but the shared app stays in the chat, and people come back to it. Key your state by `session.app().message_id()`, which names that shared copy, and a reopened app picks up where it left off:

```rust
let counts: Arc<Mutex<HashMap<String, i64>>> = Arc::default();

bot.xdc("vector-counter").run(move |_bot, mut session| {
    let counts = counts.clone();
    async move {
        let copy = session.app().message_id().to_string();
        // ... read and change counts[&copy] ...
    }
});
```

That map lives as long as the bot runs. Write it to disk or a database if it should survive a restart.

## 7. Know who sent a message

`frame.from.npub` is the sender's npub. When it matters who sent it (a move, a vote, a purchase), use `frame.verified_sender()`: it's set when the message came straight from that player, so nobody can act in someone else's name. The tic-tac-toe bot gives the X seat to the first verified player to move and ignores moves from anyone else.

## Where your bot plays

The same code serves both kinds of chat.

- **Direct messages**: someone messages the bot (`!counter`) or shares the app in a DM with it. Only the two of you are in that channel.
- **Communities**: add the bot to a Community and share the app in a channel. Everyone in the channel who opens it plays together, with the bot. Build the bot with `.public()` to accept every Community invite, or leave it off and accept the ones you want with `bot.pending_invites()` and `bot.accept_invite(id)`.

## Good to know

- Only people with the app open receive a message, and nothing is stored. A player who opens the app later learns the state from whoever greets them, which is why the bot does.
- Players in a session can see each other's IP address, and the bot's. A bot with Tor on stays out of sessions unless you call `vector_sdk::xdc::allow_outside_tor(true)`.

## Try it

```sh
cargo run --example xdc_counter_bot --features xdc
cargo run --example xdc_tictactoe_2d_bot --features xdc
```

Each prints its npub. Message it `!counter` (or `!tictactoe`) from Vector, open the app it sends, and play.

Writing a bot for an app you didn't write? See [Write a bot for any Mini App](bot-for-any-mini-app.md).
