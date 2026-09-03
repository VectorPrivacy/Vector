# Releasing Vector

## Version format

There is one version string, in `src-tauri/Cargo.toml`. Everything downstream derives from it.

```toml
version = "0.4.2-1"   # first public preview of 0.4.2
version = "0.4.2-2"   # second preview
version = "0.4.2"     # the official release
```

Semver orders these `0.4.1 < 0.4.2-1 < 0.4.2-2 < 0.4.2`, and that ordering is what carries
preview users onto the official build.

**The preview identifier must be a plain number, 1 to 98.** Not `rc1`, not `beta.1`. Tauri's MSI
bundler rejects any non-numeric pre-release identifier and fails the Windows build outright, and
MSI is the Windows update path, so it can't be dropped. The Android build script fails loudly on
the same input rather than mint a versionCode that can't be ordered.

## Cutting a release

1. Bump `version` in `src-tauri/Cargo.toml`, and `package.json` to match. Mirror the pair into
   `src-tauri/gen/android/app/version.properties` (F-Droid reads it; Gradle and a test refuse a
   stale one).
2. Push to the `release` branch.
3. CI builds every platform and derives the rest from the version string alone:
   - a `-N` suffix flags the GitHub release as a pre-release and builds against the preview
     updater endpoint
   - no suffix builds an ordinary stable release
4. Write the changelog on the draft release, then publish it.
5. Publishing fires `preview-pointer.yaml`, which re-points the preview channel at whatever you
   just published.
6. On a stable release, publishing also fires `distribute.yaml`, which publishes to Zapstore
   through a bunker (`docs/zapstore-bunker.md`; a no-op until the secret is set). The
   F-Droid flavour (`Vector-fdroid.apk`) was uploaded by CI alongside the store APKs; F-Droid
   itself builds from the tag (`docs/fdroid/README.md`).

Don't hand-edit the pre-release checkbox. It's derived so it can't drift from the endpoint the
binary was compiled against.

## Channels

|                      | Stable build                            | Preview build                        |
| -------------------- | --------------------------------------- | ------------------------------------ |
| Version              | `0.4.2`                                 | `0.4.2-1`                            |
| Updater endpoint     | `releases/latest/download/latest.json`  | `releases/download/preview/latest.json` |
| Offered previews?    | Only with the Beta Updates opt-in (Settings > Updates), which reads the preview pointer at runtime | Yes                                |
| Offered stable?      | Yes                                     | Yes, the pointer tracks both channels |

`preview` is a permanent pointer release whose only asset is an update manifest. It exists because
GitHub has no "latest including pre-releases" redirect.

Because the pointer tracks the newest build of *either* channel, a preview user is offered the
official release the moment it publishes. Once they install it they're running a stable build
reading the stable endpoint, and the channels have converged with no manual reinstall.

## Tripwires

- **`bundle > windows > allowDowngrades` must stay `true`** (its default, and `tauri.conf.json`
  currently sets no `bundle.windows` block at all). MSI puts the preview number in the 4th version
  field (`0.4.2-1` becomes ProductVersion `0.4.2.1`), but Windows Installer
  [ignores the 4th field entirely](https://learn.microsoft.com/en-us/windows/win32/msi/productversion).
  So every preview of `0.4.2` and `0.4.2` itself are the *same version* to Windows, and moving
  between them is a same-version transition. Per the WiX docs, "AllowDowngrades already allows two
  products with the same version number to upgrade each other", which is the only thing making
  preview to official work there. Set it to `false` and Windows silently stops taking preview
  updates while every other platform keeps working.

  Corollary: the preview digit is **cosmetic on Windows**. No numbering scheme can encode ordering
  in a field the installer discards. Ordering there comes from the semver in `latest.json`, which
  the updater compares before msiexec is ever invoked.
- **Previews stay flagged pre-release on GitHub forever.** That applies to every `-N` release *and*
  to the permanent `preview` pointer. Only official releases get a normal GitHub Release. Anything
  not flagged wins `/releases/latest`, which is the stable channel.

  CI sets the flag from the version string, so you never tick it by hand, and the client no longer
  depends on it being right: a stable build refuses a pre-release version outright
  (`default_version_comparator` in `lib.rs`, `version_is_newer` for Android). Worst case from an
  unticked box is stable users seeing "no update available" until it's fixed, instead of being
  shipped an unfinished build. Belt and braces, because the blast radius is every user.
- **Android versionCode only ever grows.** The formula is
  `(major*10000 + minor*100 + patch) * 100 + slot`, with previews taking slots 1-98 and stable
  taking 99. Never lower it; a shipped code can't be walked back.
- **Previews never go to app stores.** Testers download them by hand from GitHub releases, and hop
  to the official build when it publishes. Only run `zsp publish` for stable releases. Because
  `zsp` resolves the *newest* GitHub release, run
  `zsp utils check-releases https://github.com/VectorPrivacy/Vector` first if a preview has landed
  since, and confirm it reports the stable version you meant.
  The Android client already reflects this: on a preview build the update button bypasses the
  `market://` store hand-off entirely and deep-links to the exact GitHub release it found
  (`/releases/tag/v0.4.2-2`), because the store has no preview build to offer.
