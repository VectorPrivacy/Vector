# Write a bot for any Mini App

When the app isn't yours, your bot has to learn what it says before it can join in. There are two ways in, and they work best together: read the app's source, and watch it talk.

The worked example is [`xdc_tictactoe_bot`](../examples/xdc_tictactoe_bot.rs), a bot that plays 3D Tic-Tac-Toe, an app it had no hand in.

## 1. Find the app's id

An `.xdc` file is a zip. Unpack it and open `manifest.toml`:

```sh
unzip -o 3d-tic-tac-toe.xdc -d 3d-tic-tac-toe
cat 3d-tic-tac-toe/manifest.toml
```

```toml
name = "3D Tic Tac Toe"
id = "3d-tic-tac-toe"
```

`bot.xdc("3d-tic-tac-toe")` handles every copy with that id. An app with no `id` is matched by its `name` instead. To handle one exact build, match its file hash with `bot.xdc(XdcMatch::Hash(..))`.

## 2. Read the source

The realtime parts of any app look much the same. Search the unpacked files for:

```sh
grep -rn "joinRealtimeChannel\|setListener\|\.send(" 3d-tic-tac-toe
```

- `joinRealtimeChannel()` is where the app connects.
- The function passed to `setListener` handles incoming messages: its branches are the message types the app understands.
- Each `channel.send(...)` is a message the app sends. Look at what it encodes, nearly always `JSON.stringify` of an object with a type field.

Write down every message type, its fields, and when the app sends it: on start, on a click, in reply to something. In 3D Tic-Tac-Toe the listener branches on `m.t`, and the sends give you `hello`, `state`, `move` and `new`.

## 3. Watch it talk

Reading tells you what the app can say; watching shows you what it does say, in what order. [`xdc_spy`](../examples/xdc_spy.rs) joins every app opened with it and prints each message with its sender and time:

```sh
cargo run --example xdc_spy --features xdc
```

Share the app with the spy in a DM (or add it to a Community and share it there), open it, and play a round, ideally with a friend or a second device so you see both sides:

```
== 3D Tic Tac Toe in npub1abc… (topic 2ELKK…)
    0.0s + npub1alice…
    0.3s npub1alice…: {"t":"hello","name":"Alice","at":1790878149820,"by":"npub1alice…"}
    6.1s + npub1bob…
    6.4s npub1bob…: {"t":"hello","name":"Bob","at":1790878155101,"by":"npub1bob…"}
    6.5s npub1alice…: {"t":"hello","name":"Alice","at":1790878149820,"by":"npub1alice…"}
    9.2s npub1alice…: {"t":"state","board":[1,0,…],"scores":{…},"gameOver":false,"by":"npub1alice…"}
```

(npubs and the board are shortened here.)

The spy is a player like any other, so it shows up in the app's lobby.

## 4. Map the protocol

Put what you read and saw into one list. For 3D Tic-Tac-Toe:

| Message | Sent | Meaning |
| --- | --- | --- |
| `hello {name, at}` | on opening, and in reply to a new player's hello | "I'm here." The two earliest `at` times take seats X and O |
| `state {board, scores, gameOver}` | by X after every change, and to a newcomer mid-game | The whole game; everyone else adopts it |
| `move {cell, player, board}` | by O | O's move, with the board after it |
| `new` | by either player | Start a fresh board |

Every message also carries `by`, the sender's npub, which the app reads from `webxdc.selfAddr`. Your bot's own `selfAddr` is its npub too (`session.self_addr()`).

Look for the handshake first. Nearly every multiplayer app has one: something like `hello` on arrival, then a reply carrying the state. Your bot has to take part in it, or the app won't know the bot is there.

## 5. Write the bot

Answer the handshake, then play by the rules you mapped:

```rust
bot.xdc("3d-tic-tac-toe").run(|_bot, mut session| async move {
    let me = session.self_addr().to_string();
    let opened_at = now_ms(); // milliseconds since 1970, like the app's Date.now()
    while let Some(event) = session.next().await {
        let XdcEvent::Data(frame) = event else { continue };
        let Some(msg) = frame.json::<Value>() else { continue };
        match msg["t"].as_str() {
            // Someone arrived: introduce ourselves, the way the app does.
            Some("hello") => {
                let hi = json!({ "t": "hello", "name": "TicTacBot", "at": opened_at, "by": me });
                let _ = session.send_json(&hi).await;
            }
            Some("state") => { /* adopt the board; if it's our turn, move */ }
            Some("move") => { /* apply it; if we hold X, send the new state */ }
            _ => {}
        }
    }
});
```

The full bot, with seats, turns and a move picker, is [`xdc_tictactoe_bot`](../examples/xdc_tictactoe_bot.rs). For the session itself (events, sending, who sent what, how long it lasts), [Add a bot to your Mini App](bot-for-your-mini-app.md) covers the same calls in more detail.

## 6. Test it

Run your bot, share the app with it in Vector, and play. To test without a phone, [`xdc_probe`](../examples/xdc_probe.rs) plays the human's side of 3D Tic-Tac-Toe from a second headless account, in a DM or a Community, and exits with an error if the bot never answers. Copy it and swap in your app's messages.

## Where your bot plays

- **Direct messages**: share the app in a DM with the bot and open it.
- **Communities**: add the bot to a Community and share the app in a channel; everyone who opens it plays together, the bot included. Build the bot with `.public()` to accept every Community invite, or accept the ones you want with `bot.pending_invites()` and `bot.accept_invite(id)`.

## Good manners

- Apps change. When an update changes the messages, your bot may misread them: check `session.app().manifest().await?.version`, or match one build by hash.
- If the app is open source, tell its author. A bot is a new kind of player, and a small change on their side (like re-greeting a returning player) can make it much smoother.
- Players see your bot in the lobby like anyone else, and in a session they can see its IP address, as it sees theirs.

Rather have an LLM do the playing? [`vector-agent`](../../vector-agent) gives any MCP client the same sessions through its `xdc_*` tools.
