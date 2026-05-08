# mdk-storage-traits

Storage abstraction layer for [MDK](https://github.com/marmot-protocol/mdk). This crate defines the `MdkStorageProvider` trait and associated types that storage backends must implement.

Storage backends in the MDK ecosystem:

- [`mdk-memory-storage`](https://crates.io/crates/mdk-memory-storage): In-memory storage for testing and development
- [`mdk-sqlite-storage`](https://crates.io/crates/mdk-sqlite-storage): SQLite-based persistent storage for production use

## Changelog

All notable changes to this library are documented in the [CHANGELOG.md](CHANGELOG.md).

## State

**This library is in an ALPHA state.** Things that are implemented generally work, but the API may change in breaking ways.

## License

This project is distributed under the MIT software license - see the [LICENSE](https://github.com/marmot-protocol/mdk/blob/master/LICENSE) file for details, or visit <https://opensource.org/licenses/MIT>.
