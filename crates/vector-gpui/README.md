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

Video attachments decode through FFmpeg (the default `video` feature): the system's
(`libavcodec-dev libavformat-dev libavutil-dev clang`), or the desktop
client's selective static build with `FFMPEG_DIR` set and `--features video-static`.
`--no-default-features` draws every clip as a file.

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

## Text and languages

Every string a person reads comes from `crates/vector-i18n/locales/en-GB` through `t::`, and the
`i18n_audit` test fails on new hard-coded English. Translators work from
`crates/vector-i18n/TRANSLATING.md`.

A right-to-left language mirrors the layout inside the GPUI fork (`gpui::layout_rtl()`), so most
screens need nothing. The exceptions:

- `.keep_ltr()` on what reads left to right in every language: code, PIN and digit entry,
  playback controls, a picture panned under the pointer.
- Message text is `Inline::dir_auto()`, so an English paragraph stays left; any other `Inline`
  sits at the start edge.
- A sideways scroller reads its position with `kit::scrolled` and moves with
  `kit::scroll_from_start`.
- An icon that points along the reading direction goes in `lang::MIRRORED_ICONS`.
- Counts and clocks built in code go through `lang::num` and `lang::digits`.
- A sentence styled in one piece is `kit::run_text`. One built from several elements (a link, a
  hover name) puts its pieces through `kit::visual` in a `keep_ltr` row; where a piece is a
  widget, the row takes `keep_ltr` only when `kit::reads_ltr`.

`VECTOR_LANG=ar-XB` shows English mirrored, for checking a screen.

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
