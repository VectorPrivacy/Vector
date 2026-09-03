# Vector on F-Droid

Three tiers, each one a superset of the last. The in-tree work is the same for all of them.

| Tier | Who builds | Who signs | What it takes |
| --- | --- | --- | --- |
| IzzyOnDroid | Vector CI | Vector's release key | One issue on their tracker |
| f-droid.org | F-Droid's builders, from the git tag | F-Droid | A `fdroiddata` merge request with `io.vectorapp.yml` |
| f-droid.org, reproducible | F-Droid's builders | Vector's release key, verified byte-for-byte | The above, plus `Binaries` + `AllowedAPKSigningKeys` |

## The F-Droid flavour

F-Droid's inclusion policy forbids self-updating apps, so the build F-Droid ships has the updater
compiled out rather than switched off: Cargo feature `fdroid` removes the manifest check and the APK
download from `src-tauri/src/commands/updates.rs`, and `get_platform_features().self_update` tells
the frontend to show "Updates arrive through F-Droid" instead of a button.

`scripts/fdroid-build.sh` builds it. The script is the recipe F-Droid runs (`prebuild` and `build`
in the metadata call its two stages) and the recipe CI runs to produce the `Vector-fdroid.apk`
release asset, so the two cannot drift. It is source-only:

- the Tauri CLI comes from `cargo install tauri-cli` (the npm package is a prebuilt binary)
- Svelte is bundled with rollup (esbuild is a prebuilt binary; `scripts/build-svelte.mjs` falls back
  when it is absent)
- CSS ships unminified (lightningcss is a prebuilt binary)
- every native npm package is deleted after `npm ci`, and the script refuses to continue if a
  `.node` or `.wasm` file remains

Output is one unsigned universal APK for arm64. F-Droid keys builds on versionCode, and the
per-ABI splits share one, so a single artifact is the only shape it accepts.

### Try it locally

```bash
NDK_HOME=$HOME/Library/Android/sdk/ndk/29.0.14206865 scripts/fdroid-build.sh
```

F-Droid's scanner can be run over the tree with `scripts/fdroid-scan.py` (needs
`pip install fdroidserver`). The `fdroid` GitHub workflow does both on every change to the recipe.

## Version discovery

F-Droid's update bot reads `versionName`/`versionCode` from a flat file, and Gradle computes ours
from `Cargo.toml`. `src-tauri/gen/android/app/version.properties` mirrors the pair; Gradle refuses
to build when it is stale and `android_version_properties_mirror_cargo` in `updates.rs` fails
before that. Bump it with every version.

## IzzyOnDroid

Open an issue at https://gitlab.com/IzzyOnDroid/repo/-/issues with the repository URL. Their bot
picks the arm64 APK off each GitHub release. Two things they check: no trackers (none), and APK
size. Their default ceiling is 30 MB and `Vector-arm64-v8a.apk` is about 65 MB, so ask for the
exception in the issue and say why (Tor, whisper.cpp and Iroh are compiled in; no model weights).

## f-droid.org

1. Fill in `commit:` in `io.vectorapp.yml` with the full hash of the tagged release commit.
2. Fork https://gitlab.com/fdroid/fdroiddata, add the file as `metadata/io.vectorapp.yml`, open a
   merge request. Their CI runs `fdroid build` on it, so a green pipeline means the recipe works on
   their builders.
3. Expect one to three months in the review queue. The reviewers will read the flavour: point them
   at the `fdroid` feature and the script.

Anti-features they may ask about: none apply. Blossom media servers and the update manifest are
not contacted by this flavour beyond what the user chooses; whisper models are downloaded only on
request. Declare `NonFreeNet` only if they insist.

Once merged, `AutoUpdateMode: Version` makes their bot add a build entry for every `vX.Y.Z` tag
(previews, `vX.Y.Z-N`, are excluded by the tag regex).

## Reproducible

The CI job `publish-android-fdroid` builds the same flavour with the same script, signs it with the
release key using `apksigner`, and uploads it as `Vector-fdroid.apk`. When F-Droid's rebuild of a
tag matches it (F-Droid strips both signatures and compares), uncomment `Binaries` and
`AllowedAPKSigningKeys` in the metadata. From then on F-Droid publishes Vector's own signature, and
the app carries the reproducible badge.

What already holds the two builds together: the Rust toolchain and `tauri-cli` versions pinned at
the top of `scripts/fdroid-build.sh`, NDK `29.0.14206865`, `--remap-path-prefix` on every path that differs between
machines, and `SOURCE_DATE_EPOCH` from the commit. What can still break it and has not been
measured yet: whisper.cpp's cmake build, R8 output across JDK builds, and Gradle's APK packaging.
The first F-Droid build of a tag will say which, in their `verification` log.

## Release checklist additions

- Bump `src-tauri/gen/android/app/version.properties` with `Cargo.toml` and `package.json`.
- `distribute.yaml` publishes to Zapstore on `release: published` for stable tags, when the
  `ZAPSTORE_SIGN_WITH` secret is set. Use a NIP-46 bunker URL, not the nsec: the key stays on your
  device and the grant is revocable. Unset, the job is a no-op and you publish by hand. Setup: `docs/zapstore-bunker.md`.
