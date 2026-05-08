# MDK UniFFI Bindings

Cross-platform language bindings for [mdk-core](../mdk-core) using [UniFFI](https://mozilla.github.io/uniffi-rs/). This crate wraps MDK's secure group messaging functionality with SQLite storage, exposing it to Kotlin, Swift, Python, and Ruby.

## Supported Platforms

- **Kotlin/JVM** (Android)
- **Swift** (iOS/macOS)
- **Python**
- **Ruby**

## Features

- Full MLS group lifecycle: create groups, add/remove members, send/receive messages
- Key package creation and management
- Welcome message processing
- Group image encryption (MIP-01)
- Encrypted media support (MIP-04, via `mip04` feature flag)
- SQLite storage with SQLCipher encryption at rest
- Configurable MLS sender ratchet and message validation settings

## Usage

This crate is primarily consumed through generated bindings rather than directly from Rust. To generate bindings for your target language:

```bash
cargo run --bin uniffi-bindgen generate \
    --library target/release/libmdk_uniffi.dylib \
    --language kotlin \
    --out-dir out
```

Replace `kotlin` with `swift`, `python`, or `ruby` as needed.

## Changelog

All notable changes to this library are documented in the [CHANGELOG.md](CHANGELOG.md).

## State

**This library is in an ALPHA state**, things that are implemented generally work but the API will change in breaking ways.

## License

This project is distributed under the MIT software license - see the [LICENSE](../../LICENSE) file for details
