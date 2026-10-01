# Mini Apps (WebXDC) for Vector

Mini Apps are small web apps shared and run inside Vector chats: games, polls, whiteboards, anything a chat can do together. They follow the [WebXDC specification](https://webxdc.org/) from Delta Chat, so most WebXDC apps run in Vector as they are.

## What is a Mini App?

A Mini App is a `.xdc` file, which is a ZIP archive containing:

- `index.html` - The main entry point (required)
- `manifest.toml` - Metadata about the app (optional, recommended)
- Any other web assets (JS, CSS, images, etc.)

Share it in a DM or a Community channel like any file. Everyone in that chat can open it, and everyone who has it open at the same time can play together.

## Creating a Mini App

### Basic Structure

```
my-app/
├── index.html      # Required: Main entry point
├── manifest.toml   # Optional: App metadata
├── icon.png        # Optional: App icon
├── style.css       # Optional: Styles
└── app.js          # Optional: JavaScript
```

### manifest.toml

```toml
name = "My Mini App"
id = "my-mini-app"
description = "A simple example Mini App"
version = "1.0.0"
icon = "icon.png"
source_code_url = "https://github.com/me/my-mini-app"
```

| Key | Meaning |
| --- | --- |
| `name` | Shown on the app card and window title |
| `id` | A stable id for your app across versions. Bots recognise your app by it |
| `description` | Shown on the app card |
| `version` | Your app's version |
| `icon` | An image in the package, shown on the app card |
| `source_code_url` | Where people can read your code |

### A first app

Two buttons that everyone with the app open sees, live:

```html
<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Wave</title>
    <script src="webxdc.js"></script>
</head>
<body>
    <button id="wave">👋 Wave</button>
    <ul id="log"></ul>
    <script>
        const enc = new TextEncoder();
        const dec = new TextDecoder();
        const channel = webxdc.joinRealtimeChannel();

        channel.setListener((bytes) => {
            const msg = JSON.parse(dec.decode(bytes));
            const li = document.createElement('li');
            li.textContent = `${msg.name} waved`;
            document.getElementById('log').append(li);
        });

        document.getElementById('wave').onclick = () => {
            channel.send(enc.encode(JSON.stringify({ t: 'wave', name: webxdc.selfName })));
        };
    </script>
</body>
</html>
```

## The webxdc API

Mini Apps get a `window.webxdc` object. Vector adds `webxdc.js` to every page; including `<script src="webxdc.js"></script>` yourself keeps your app portable to other WebXDC hosts.

### Properties

- `webxdc.selfAddr`: the current user's address (their npub)
- `webxdc.selfName`: the current user's display name

### `webxdc.joinRealtimeChannel()`

Joins the app's realtime channel and returns it. Everyone who has this shared copy of the app open is on the same channel.

| Method | What it does |
| --- | --- |
| `channel.send(bytes)` | Send a `Uint8Array` (up to 128,000 bytes) to everyone else on the channel |
| `channel.setListener(fn)` | `fn(bytes)` runs for every message someone else sends |
| `channel.leave()` | Stop sending and receiving |

You don't receive your own messages back.

### `webxdc.sendUpdate` and `webxdc.setUpdateListener`

Present so WebXDC apps load and run. Vector keeps them on the device: updates aren't shared with other players yet. For anything multiplayer, use the realtime channel.

## Multiplayer

The realtime channel is live, and nothing on it is stored:

- A message reaches the people who have the app open right now. Someone who opens it later has missed everything before.
- A message can get lost on the way.
- Players on a channel can see each other's IP address. With Tor on, Vector asks before opening an app that uses the realtime channel.

Three habits make an app feel solid in real chats:

1. **Greet on arrival.** Send a `hello` when your app starts, and answer every `hello` with your state, so a newcomer catches up at once. Answer a returning player too: someone who closes and reopens the app sends `hello` again, from the same `selfAddr`.
2. **Send whole state.** After a change, send the full state (or your whole part of it) rather than just the change, so a lost message heals with the next one.
3. **Tag your messages.** JSON with a type field (`{"t":"move", …}`) keeps your listener simple and lets you add message types later.

The clicker example below shows all three in about thirty lines.

To keep things between sessions on one device, use `localStorage` or `IndexedDB`: each app has its own.

## Bots

Bots can join your app's channel and play, referee, or serve it: an opponent for a solo player, a host that keeps the game's state, a backend with a database or an LLM. They're written with the [Vector SDK](../../crates/vector-sdk): see [Add a bot to your Mini App](../../crates/vector-sdk/guides/bot-for-your-mini-app.md).

Your app works best with bots when it has a stable `id` in its manifest, JSON messages with a type field, and greets everyone who arrives (the habits above).

## Security

Mini Apps run in a tightly restricted environment:

- **No network access**: Mini Apps cannot make HTTP requests or open their own connections. The realtime channel is their only way to talk, and Vector carries it.
- **No WebRTC**: Peer-to-peer connections are disabled
- **No geolocation**: Location APIs are disabled
- **No camera/microphone**: Media capture is off unless the user grants it to the app
- **Strict CSP**: Content Security Policy prevents loading external resources

## Building a .xdc File

ZIP your app's files:

```bash
cd my-app
zip -r ../my-app.xdc *
```

Make sure `index.html` is at the root of the archive, not in a subdirectory.

## Testing

1. Build the `.xdc` as above.
2. Send it in a Vector chat, then open it.
3. For multiplayer, open the same shared copy from a second account, on another device or in [Vector Web](https://web.vectorapp.io) alongside the desktop app.

## Examples

- [`examples/clicker`](examples/clicker): a multiplayer clicker with a live scoreboard. `examples/build.sh` packs it.
- [`xdc_counter`](../../crates/vector-sdk/examples/xdc_counter) and [`xdc_tictactoe_2d`](../../crates/vector-sdk/examples/xdc_tictactoe_2d): small apps served by a bot, with the bot's code beside them.
- [WebXDC apps](https://webxdc.org/apps/): the WebXDC app collection.

## Compatibility

Vector implements the WebXDC API, including the realtime channel, so apps built for Delta Chat run in Vector and the other way round. Apps that share state through `sendUpdate` open in Vector but play solo until update sharing lands.
