# mdk-memory-storage

In-memory storage backend for [MDK](https://github.com/marmot-protocol/mdk). Implements the `MdkStorageProvider` trait from [`mdk-storage-traits`](https://crates.io/crates/mdk-storage-traits).

Intended for testing and development. Data is not persisted across restarts. For production use, see [`mdk-sqlite-storage`](https://crates.io/crates/mdk-sqlite-storage).

## Features

- LRU (Least Recently Used) caching with configurable capacity (default: 1000 items)
- Thread-safe via `parking_lot::RwLock` for efficient read-heavy workloads
- No external dependencies or setup required

## Example Usage

```rust
use mdk_memory_storage::MdkMemoryStorage;

// Default cache size (1000 items)
let storage = MdkMemoryStorage::default();

// Custom cache size
let storage = MdkMemoryStorage::with_cache_size(500);
```

## Changelog

All notable changes to this library are documented in the [CHANGELOG.md](CHANGELOG.md).

## State

**This library is in an ALPHA state.** Things that are implemented generally work, but the API may change in breaking ways.

## License

This project is distributed under the MIT software license - see the [LICENSE](https://github.com/marmot-protocol/mdk/blob/master/LICENSE) file for details, or visit <https://opensource.org/licenses/MIT>.
