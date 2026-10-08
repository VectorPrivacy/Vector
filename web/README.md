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
- **`web/worker.js`** + **`web/storage.js`**: run the module in a dedicated worker and pick
  where it keeps things. OPFS where the browser allows it (SQLite through the `opfs-sahpool`
  VFS, files as plain OPFS files); IndexedDB in a private window, kept until the browser
  closes; memory where neither works (Tor Browser), gone on reload. Private storage skips
  the PIN, and memory allows one account, since switching reloads the page.
- **`web/sw.js`**: serves those files at `/vfs/<path>` (with Range), which is what
  `convertFileSrc` returns.
- **`web/tauri-shim.js`**: defines `window.__TAURI__` before any app script. `invoke` and
  `listen` go to the worker; file pickers and drops land in OPFS; one tab per origin owns
  the account, with takeover.
- **`web/media.js`**: commands the page answers itself: audio playback with spectrum
  waveforms, WAV voice recording, notifications, saving and copying attachments.
- **`web/miniapps.js`** + **`web/xdc/`**: mini apps. Each runs in a sandboxed iframe on its
  own origin, `<partition>.xdc.<vector host>`, so its localStorage and IndexedDB are its own
  and persist; marketplace apps keep one partition across versions, as on desktop. That
  origin's service worker (`xdc/sw.js`) serves the app out of its `.xdc` under an offline
  CSP and the desktop Permissions-Policy; `xdc/bridge.js` is `window.webxdc`. An app that
  sets `cross_origin_isolated` gets `allow="cross-origin-isolated"` on its frame. Realtime
  channels run on Iroh in the worker, relay-only and wire-compatible with desktop.
- **`web/calls.js`**, **`web/calls-media.js`**, **`web/calls-worklet.js`**: voice and video
  calls on core's call session (`crates/vector-web/src/calls.rs` is the platform under it).
  The page opens the microphone inside the tap that places or answers a call, AudioWorklets
  carry audio straight to the worker, and WebCodecs does Opus there; the datagrams, jitter
  buffer and rate ladder are the shared Rust. Video is the desktop's video worker over a
  MessagePort. On WebKit the call plays through a media element, since Safari distorts Web
  Audio's own output while the microphone is open.
- **`web/whisper.js`** + **`web/whisper/`**: voice transcription under desktop's command
  names. The page fetches the voice message and hands it to a worker of its own (a PCM16 WAV
  as bytes; anything else decoded by the browser first), started on first use and stopped
  after three idle minutes; models download in a second worker, since the CPU builds hold
  their thread for a whole transcription. The worker runs whisper.cpp built twice by
  `scripts/whisper-web/build.sh` (pinned whisper.cpp, emsdk and Dawn, plus `scripts/whisper/patches`,
  which the native builds apply too; `web/whisper/BUILD.txt` records the inputs): `whisper-gpu` keeps the model on the GPU
  through WebGPU, which needs `shader-f16`, and streams it there from OPFS in chunks;
  `whisper-cpu` runs on threads, for a cross-origin isolated page without WebGPU, with the
  model in wasm memory. A page that has neither uses the GPU build's single-threaded CPU
  path. The patches add vec4 mat-mat and mat-vec kernels for compilers without subgroups
  (Safari's, where ggml's own run several times slower) and cache bind groups, drop
  exceptions and narrow Asyncify to the calls that wait on the GPU, wait on the GPU once per
  decoded token rather than twice, and reuse the language-detection encode: with an ACFT
  model, language is detected on the clip's own length rather than a padded 30 s window,
  which agreed with the full window on 15 of 16 languages tried. The rest are shared with
  desktop and Android: a vectorised softmax and spectrogram, logits only for the tokens that
  read them, a retry over the same audio keeps its spectrogram and encoding, and flash
  attention masks the padding past the clip (unmasked, a reused state attended to the previous
  clip's audio). `scripts/whisper/test-native.sh` checks that a reused state transcribes exactly
  as a fresh one. `audio.js`, `results.js` and `download.js` are pure and tested by
  `node --test scripts/test-web-whisper.mjs`; `scripts/whisper-web/test-kernels.sh` builds
  ggml's `test-backend-ops` against the patched ggml as a page, to run in the browsers
  themselves (Safari above all).
- **`web/signer.js`**: the page half of NIP-07. Extensions inject `window.nostr` into pages
  only, so core's signer sends each request out as a `nip07_request` event and this answers
  it through `nip07_reply`.
- **vector-core on wasm32**: `rt` swaps tokio's spawn and timers for `spawn_local` and
  browser timers, `web_time` supplies the clock, `webfiles` reads and writes OPFS, and
  `db/webfs.rs` keeps the account registry.

## Works

Accounts (create, import, PIN/password, unlock, change or disable the PIN, logout, delete, export keys, add-account),
remote signers (NIP-46 bunkers by link or QR, re-authorize; NIP-07 browser extensions with NIP-44),
DMs (text, replies, reactions, edits, deletes, retries, self-destruct), attachments (send,
receive, image compression and metadata stripping, voice messages, audio playback),
profiles and avatars (edit, upload, blocks, nicknames), Concord v2 communities (create,
invite, join, channels, history, live messages, reactions, roles, moderation, pins,
images), relays and Blossom settings, notification levels and mutes, cross-device sync of
pins, blocks, mutes, nicknames and the community list, voice transcription and translation
(on-device Whisper), browser notifications, emoji and
GIF pickers, DM wallpapers, emoji pack creation, editing, reordering and animated pack emoji,
mini apps (from chats, history, `.xdc` links and the Nexus marketplace; per-app storage;
permissions; realtime multiplayer over Iroh), voice and video calls with screen sharing
(web to web and web to desktop), private windows and Tor Browser.

## Not yet

Notifications while the page is closed (no push), notification sounds, sending folders,
screen-share audio in calls, video compression. A call ends when a
phone locks or backgrounds the page.

Not planned: the PIVX wallet, Nexus publishing, in-app Tor (use Tor Browser), NIP-55
(Amber, Android-only), legacy (v1) community writes. Avatars and images load through the
media proxy; without one, hosts that send no CORS headers are shown by URL.

## Hosting

Mini apps need Vector served by host name, with every `*.xdc.<host>` subdomain reaching
the same server: `serve.mjs` answers those with only the mini app host page, its service
worker and the bridge template. On localhost that works as is (`*.localhost` resolves to
the machine); a deployment needs wildcard DNS and a wildcard certificate for
`*.xdc.<host>`, and should send the page `frame-src <scheme>://*.xdc.<host>` as serve.mjs does.

Threaded mini apps (`cross_origin_isolated = true` in the manifest) need the page cross-origin
isolated: send every response of Vector's own origin `Cross-Origin-Opener-Policy: same-origin`
and `Cross-Origin-Embedder-Policy: credentialless`, and the `*.xdc.<host>` files
`Cross-Origin-Embedder-Policy: require-corp` with `Cross-Origin-Resource-Policy: cross-origin`
and `Origin-Agent-Cluster: ?1` (their service worker adds the same to everything it serves).
`credentialless` keeps cross-origin images loading without CORP headers.

An app frame is same-site with Vector's page, and SharedArrayBuffer makes a precise timer
for Spectre, so a threaded app opens only where the frame gets a process of its own:
desktop Chromium, which gives each `Origin-Agent-Cluster` origin one. Firefox keys its
processes by site and mobile browsers share them, so those open threaded apps with a
message; serving apps from a separate registrable domain would let Firefox in.

The media proxy (Magnitude) admits a browser by its `Origin`, since a page cannot set the
`Vector/…` User-Agent desktop sends: a new Vector Web origin must be added to its `origins`.

Voice transcription downloads its models from Vector Web's own origin, at
`/models/whisper/base_acft_q8_0.bin` and `/models/whisper/small_acft_q8_0.bin`: FUTO's ACFT
Whisper models (Apache-2.0, the ones Android uses), whose CDN sends no CORS headers. Serve them
with `Content-Length` and Range support; `web/whisper.js` holds each file's exact size and
refuses any other.
