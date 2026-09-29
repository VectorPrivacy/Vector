# Vector Web

Vector in a browser tab: the desktop frontend, unmodified, over vector-core compiled
to WebAssembly.

```bash
npm run web:build            # wasm (dev) + frontend → dist-web/
npm run web:build -- --release
npm run web:serve            # http://localhost:8790
```

Needs `wasm-pack` and the `wasm32-unknown-unknown` target.

## How it fits together

- **`crates/vector-web`**: a `cdylib` exposing `start`, `invoke(cmd, argsJson)`,
  `invoke_bytes` and `set_event_sink`. `commands.rs` answers the Tauri command names the
  frontend calls with the same argument and return shapes; feature modules
  (`chat_ops`, `profile_ops`, `network_ops`, `community_ops`) take what it doesn't.
  Anything unlisted rejects with "not available on Vector Web yet".
- **`web/worker.js`**: runs the module in a dedicated worker. SQLite lives in OPFS through
  the `opfs-sahpool` VFS; attachments and cached images are plain OPFS files.
- **`web/sw.js`**: serves those files at `/vfs/<path>` (with Range), which is what
  `convertFileSrc` returns.
- **`web/tauri-shim.js`**: defines `window.__TAURI__` before any app script. `invoke` and
  `listen` go to the worker; file pickers and drops land in OPFS; one tab per origin owns
  the account, with takeover.
- **`web/media.js`**: commands the page answers itself: audio playback with spectrum
  waveforms, WAV voice recording, notifications, saving and copying attachments.
- **vector-core on wasm32**: `rt` swaps tokio's spawn and timers for `spawn_local` and
  browser timers, `web_time` supplies the clock, `webfiles` reads and writes OPFS, and
  `db/webfs.rs` keeps the account registry.

## Works

Accounts (create, import, PIN/password, unlock, change or disable the PIN, logout, delete, export keys, add-account),
DMs (text, replies, reactions, edits, deletes, retries, self-destruct), attachments (send,
receive, image compression and metadata stripping, voice messages, audio playback),
profiles and avatars (edit, upload, blocks, nicknames), Concord v2 communities (create,
invite, join, channels, history, live messages, reactions, roles, moderation, pins,
images), relays and Blossom settings, notification levels and mutes, cross-device sync of
pins, blocks, mutes, nicknames and the community list, browser notifications, emoji and
GIF pickers, DM wallpapers, emoji pack creation, editing, reordering and animated pack emoji.

## Not yet

Mini apps, calls, the PIVX wallet, transcription, Tor, bunker and NIP-55 signers, legacy (v1)
community writes. Avatars and images load through the media proxy; without one, hosts
that send no CORS headers are shown by URL.
