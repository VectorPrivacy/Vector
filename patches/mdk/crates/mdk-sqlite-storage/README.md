# mdk-sqlite-storage

SQLite-based persistent storage backend for [MDK](https://github.com/marmot-protocol/mdk). Implements the `MdkStorageProvider` trait from [`mdk-storage-traits`](https://crates.io/crates/mdk-storage-traits).

Designed for production use. The database is encrypted at rest using SQLCipher (ChaCha20-Poly1305) with a 256-bit key.

## Features

- Encrypted SQLite database via SQLCipher
- Automatic schema migrations
- Optional keyring integration via `keyring-core` for secure key management
- File permission hardening (mode `0600` on Unix)

## Usage

### Automatic key management (recommended)

Initialize your platform's keyring store once at app startup, then let MDK handle key generation and storage:

```rust,ignore
use mdk_sqlite_storage::MdkSqliteStorage;

// e.g., keyring_core::set_default_store(AppleStore::new());

let storage = MdkSqliteStorage::new(
    "path/to/database.db",
    "com.example.myapp",    // service identifier
    "mdk.db.key.default",   // key identifier
)?;
```

### Manual key management

```rust
use mdk_sqlite_storage::{MdkSqliteStorage, EncryptionConfig};

let key = [0u8; 32]; // your securely stored 32-byte key
let config = EncryptionConfig::new(key);
let storage = MdkSqliteStorage::new_with_key("path/to/database.db", config)?;
```

### Unencrypted (development only)

```rust
use mdk_sqlite_storage::MdkSqliteStorage;

// ⚠️ WARNING: unencrypted — for development/testing only
let storage = MdkSqliteStorage::new_unencrypted("path/to/database.db")?;
```

## Changelog

All notable changes to this library are documented in the [CHANGELOG.md](CHANGELOG.md).

## State

**This library is in an ALPHA state.** Things that are implemented generally work, but the API may change in breaking ways.

## License

This project is distributed under the MIT software license - see the [LICENSE](https://github.com/marmot-protocol/mdk/blob/master/LICENSE) file for details, or visit <https://opensource.org/licenses/MIT>.
