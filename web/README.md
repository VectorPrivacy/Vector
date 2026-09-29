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

- **`crates/vector-web`**: a `cdylib` exposing `start`, `invoke(cmd, argsJson)` and
  `set_event_sink`. `commands.rs` answers the Tauri command names the frontend calls,
  with the same argument and return shapes. Anything unlisted rejects with
  "not available on Vector Web yet".
- **`web/worker.js`**: runs the module in a dedicated worker. SQLite lives in OPFS
  through the `opfs-sahpool` VFS, whose synchronous access handles exist only in workers.
- **`web/tauri-shim.js`**: defines `window.__TAURI__` before any app script. `invoke`
  and `listen` go to the worker; window, dialog, opener and updater APIs map to browser
  equivalents or no-ops.
- **vector-core on wasm32**: `rt` swaps tokio's spawn and timers for `spawn_local` and
  browser timers; `web_time` supplies the clock; `db/webfs.rs` stands in for the few
  loose files the account layer keeps.

## Works

Create or import an account (no PIN), unlock on reload, chat list, open a DM, history,
send and receive text DMs live.

## Not yet

PIN/password setup, communities, attachments and media, avatars (the frontend renders
cached files only), notifications, Tor, Whisper, calls, mini apps, bunker and NIP-55
signers. One tab per origin: the OPFS pool is exclusive.
