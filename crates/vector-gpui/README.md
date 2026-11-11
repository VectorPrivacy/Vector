# vector-gpui

Vector drawn natively with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), Zed's
GPU UI framework, instead of a webview. The goal is parity with the Tauri client: the same layout,
measurements, palette, fonts and behaviour, from the same `vector-core`.

It is an experiment on the `gpui` branch and builds on its own (its own workspace and lockfile), so
nothing here touches the Tauri client's dependency graph.

## Run

```bash
cd crates/vector-gpui
cargo run -- --open <npub>          # fixture data from reference/fixtures.json
cargo run -- --size 385x650         # any window size
cargo run --release                 # fat LTO, one codegen unit
```

Linux needs the usual GPUI packages: `libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev
libx11-xcb-dev libvulkan-dev libfontconfig1-dev libfreetype-dev`.

## Layout

- `src/theme.rs`: the palette, as the CSS resolves it.
- `src/data.rs`: the store (vector-core's `SerializableChat` / `SlimProfile`) and the derivations the
  JS makes from them (names, previews, unread counts, time labels). Each names the JS function it mirrors.
- `src/ui.rs`: the widescreen shell: window chrome, rail, chat list, chat.
- `src/composer.rs`: the message field (GPUI's input example, restyled), with IME support.
- `src/assets.rs`: icons and fonts compiled in. `icons/` *is* `src/icons`, so an icon the Tauri
  client changes arrives here on the next build.
- `assets/fonts`: Rubik (static instances cut from `src/fonts/Rubik.woff2`) and Inter.

GPUI is pinned to one Zed revision in `Cargo.toml`, and moves only on purpose.

## Parity checks

`reference/` renders the real Tauri frontend in Chromium on the same fixtures, so the two can be
compared pixel by pixel:

```bash
node reference/gen-fixtures.mjs                       # fixtures.json + deterministic ids
npm run svelte:build                                  # (repo root) the Svelte bundle
NODE_PATH=$(npm root -g) node reference/capture.mjs /tmp/ref 1200 760 reference/open-first-chat.js
reference/shot.sh /tmp/gpui.png 1200x760 --open <npub>   # Xvfb on Linux
python3 reference/compare.py /tmp/ref.png /tmp/gpui.png /tmp/diff.png
```

`capture.mjs` also writes every element's box, font, colour and border to `<out>.boxes.json`,
which is where the numbers in `ui.rs` come from. `mock-tauri.js` boots the frontend through its
debug hot-reload path with the fixtures, so it needs no Rust, account or network.
