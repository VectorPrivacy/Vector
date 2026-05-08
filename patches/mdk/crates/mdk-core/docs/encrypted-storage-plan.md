# Encrypted SQLite Storage Implementation Plan

This document outlines the plan for implementing encrypted SQLite storage using SQLCipher, addressing the security audit finding regarding unencrypted MLS state storage.

## Background

### Audit Finding (Issue F)

The MLS state is stored in an unencrypted SQLite database with default file permissions, exposing sensitive data including:
- Messages and message content
- Group metadata
- Exporter secrets (enables retrospective traffic decryption)

### Document Structure

This document is split into two parts:

1. **Part A (MDK-generic)**: The design and implementation plan intended to be useful for any MDK user, regardless of platform.
2. **Part B (whitenoise-rs-specific)**: Non-normative integration notes and examples for `whitenoise-rs` and Flutter/FRB.

---

## Part A: MDK-generic design

### Goals

1. **Encrypt MLS state at rest** in `mdk-sqlite-storage` using SQLCipher.
2. **Keep MDK platform-agnostic**: use the `keyring-core` ecosystem for cross-platform secure credential storage.
3. **Minimize footguns**: explicit keying procedure, clear failure modes, and safe defaults for file placement/permissions.

### Non-goals (for this workstream)

1. **Backups / restore / portability** are not supported yet. (Future work could add explicit export/import tooling, but that changes the threat model and must be opt-in.)
2. **In-memory zeroization / secure buffers** are out of scope here and will be addressed separately.
3. **Defense against a compromised runtime** (root/jailbreak/malware that can read process memory or intercept callbacks) is out of scope. This plan primarily targets offline/exfiltration threats.

### Threat Model

**Assets to protect**

- MLS state stored by `mdk-sqlite-storage`, especially **exporter secrets** (which enable retrospective traffic decryption).
- Group metadata and message content stored in the DB.
- The SQLCipher database encryption key (and any other secrets stored via secure storage).

**Primary attacker we are designing for**

- An attacker who can obtain **a copy of the SQLite database files** (e.g., via device theft, filesystem exfiltration, misconfigured file permissions, developer backups), but who does **not** have access to platform secure storage (Keychain/Keystore/etc.) and does not control the running process.

**Out of scope / explicitly not defended**

- A compromised host application (malicious integration).
- A compromised device / OS (root/jailbreak) or malware that can call secure storage APIs or read process memory.
- Side-channel attacks, hardware attacks, and "evil maid" runtime tampering.

**Trust boundaries**

- MDK trusts the credential store implementations from `keyring-core` to keep secrets confidential.

### Solution Overview (MDK-generic)

1. **Database Encryption**: Use SQLCipher via `rusqlite` `bundled-sqlcipher`.
2. **Secure Storage via `keyring-core`**: Use the [`keyring-core`](https://crates.io/crates/keyring-core) ecosystem instead of a custom abstraction:
   - `keyring-core` provides a unified cross-platform API for credential storage
   - Platform-native stores are provided as separate crates (see table below)
   - No custom traits needed—MDK uses `keyring-core::Entry` directly
3. **File Permissions**: Restrict database directories (mode `0700`) and files (mode `0600`) on Unix-like platforms, and apply ACL hardening on Windows to restrict access to the current user.

### Why `keyring-core`?

The [`keyring-core`](https://github.com/open-source-cooperative/keyring-core) ecosystem (v0.7+) provides:

- **Unified API**: Single `Entry` type for all platforms with `set_secret()`, `get_secret()`, `delete_credential()`
- **Native platform stores**: Each platform has a dedicated crate that implements the `CredentialStoreApi` trait
- **Thread-safe by design**: All credentials are `Send + Sync`
- **Android support**: The [`android-native-keyring-store`](https://crates.io/crates/android-native-keyring-store) crate provides native Android Keystore integration
- **Active maintenance**: Maintained by the [Open Source Cooperative](https://github.com/open-source-cooperative)


---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                               Host Application                               │
│     (Swift/Kotlin/Flutter/React Native/Desktop/etc.)                         │
│                                   │                                          │
│            (Optional: platform-specific store initialization)                │
│                                   │                                          │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                               MDK (Rust)                               │  │
│  │                                                                       │  │
│  │  ┌─────────────────────────────────────────────────────────────────┐  │  │
│  │  │                         keyring-core                             │  │  │
│  │  │  - Entry::new(service, user)                                     │  │  │
│  │  │  - entry.set_secret() / get_secret() / delete_credential()       │  │  │
│  │  └─────────────────────────────────────────────────────────────────┘  │  │
│  │                                   │                                    │  │
│  │         ┌─────────────────────────┼─────────────────────────┐         │  │
│  │         │                         │                         │         │  │
│  │         ▼                         ▼                         ▼         │  │
│  │  ┌─────────────┐         ┌─────────────────┐       ┌─────────────────┐│  │
│  │  │ Apple Store │         │ Android Store   │       │ Windows/Linux   ││  │
│  │  │(macOS+iOS)  │         │(native keystore)│       │ stores          ││  │
│  │  └─────────────┘         └─────────────────┘       └─────────────────┘│  │
│  │                                                                       │  │
│  │  ┌─────────────────────────────────────────────────────────────────┐  │  │
│  │  │                       mdk-sqlite-storage                         │  │  │
│  │  │  - SQLCipher-encrypted SQLite                                    │  │  │
│  │  │  - Uses keyring-core Entry to obtain/store DB key                │  │  │
│  │  └─────────────────────────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Credential Store Crates by Platform

| Platform | Crate | Backend | Initialization |
|----------|-------|---------|----------------|
| macOS | [`apple-native-keyring-store`](https://crates.io/crates/apple-native-keyring-store) | Keychain Services | Automatic |
| iOS | [`apple-native-keyring-store`](https://crates.io/crates/apple-native-keyring-store) | Keychain Services | Automatic |
| Windows | [`windows-native-keyring-store`](https://crates.io/crates/windows-native-keyring-store) | Credential Manager | Automatic |
| Linux | [`linux-keyutils-keyring-store`](https://crates.io/crates/linux-keyutils-keyring-store) | Kernel keyutils | Automatic |
| Linux | [`dbus-secret-service-keyring-store`](https://crates.io/crates/dbus-secret-service-keyring-store) | D-Bus Secret Service | Automatic |
| Linux | [`zbus-secret-service-keyring-store`](https://crates.io/crates/zbus-secret-service-keyring-store) | D-Bus (async zbus) | Automatic |
| Android | [`android-native-keyring-store`](https://crates.io/crates/android-native-keyring-store) | Android Keystore | **Requires init** (see below) |

---

## Technical Design

### 1. SQLCipher Integration

#### Crypto Backends by Platform

| Platform | Crypto Backend | Notes |
|----------|---------------|-------|
| iOS | CommonCrypto (Security.framework) | Native, no OpenSSL |
| macOS | CommonCrypto (Security.framework) | Native, no OpenSSL |
| Android | libcrypto (NDK) | Provided by NDK |
| Linux | libcrypto (OpenSSL) | System dependency |
| Windows | OpenSSL | Requires configuration |

#### Cargo.toml Changes

```toml
# Workspace Cargo.toml
[workspace.dependencies]
rusqlite = { version = "0.32", default-features = false }

# mdk-sqlite-storage/Cargo.toml
[dependencies]
rusqlite = { workspace = true, features = ["bundled-sqlcipher"] }
```

**Windows note:** SQLCipher builds typically require OpenSSL headers/libs to be available at build time (or a vendored OpenSSL build strategy).

#### Keying Procedure (precise `PRAGMA key` format)

MDK will use a **random 32-byte (256-bit) key** generated once and stored via `keyring-core` (platform secure credential storage). When opening a database connection:

- `mdk-sqlite-storage` uses multiple SQLite connections to the same database file (e.g., OpenMLS storage and MDK tables). The keying procedure must run **on each connection**.

1. **Call `PRAGMA key` as the first operation** on the database connection.
2. Use **raw key data** (not a passphrase) so we do not depend on passphrase KDF settings:
   - SQLCipher expects a **64 character hex string** inside a blob literal, which it converts to 32 bytes of key data:

```sql
PRAGMA key = "x'2DD29CA851E7B56E4697B0E1F08507293D761A05CE4D1B628663F411A8086D99'";
```

3. **Immediately after setting the key**, pin SQLCipher defaults and prevent temp spill:

```sql
PRAGMA cipher_compatibility = 4;
PRAGMA temp_store = MEMORY;
```

`PRAGMA cipher_compatibility` is applied through the SQLCipher codec context, so it must run **after** `PRAGMA key` (key first still applies).

4. **Validate the key immediately**: SQLCipher will not always error on `PRAGMA key` alone if the key is wrong. A simple schema read is the recommended check:

```sql
SELECT count(*) FROM sqlite_master;
```

**Alternative:** SQLCipher also exposes `sqlite3_key()` / `sqlite3_key_v2()` as programmatic equivalents to `PRAGMA key`. (The `PRAGMA` interface calls these internally.)

5. Only after the above succeeds should the connection execute other pragmas (e.g., `PRAGMA foreign_keys = ON;`) and run migrations / normal queries.

#### Cipher Parameters (defaults, but pinned intentionally)

SQLCipher’s **major versions have different default settings**, and existing databases can require migration when defaults change. This plan will:

- **Stick to SQLCipher defaults** for the selected compatibility baseline.
- Use `PRAGMA cipher_compatibility = 4;` on **every connection** to pin SQLCipher 4.x defaults so that future SQLCipher upgrades do not silently change parameters.
  - The default Rust SQLCipher bundle used by `rusqlite`/`libsqlite3-sys` is currently SQLCipher **4.5.7 (community)**.
- If we ever need to open databases created under older defaults, use SQLCipher’s supported migration mechanisms (e.g., `PRAGMA cipher_migrate` or `sqlcipher_export`) rather than guessing parameters.

#### SQLite Sidecar Files and Temporary Files

SQLCipher encrypts more than just the `*.db` file, but there are important nuances:

- **Rollback journals (`*-journal`)**: data pages are encrypted with the same key as the main database. The rollback journal includes an **unencrypted header**, but it does not contain data.
- **WAL (`*-wal`)**: page data stored in the WAL file is encrypted using the database key.
- **Statement journals**: encrypted; when file-based temp is disabled, these remain in memory.
- **Master journal**: does not contain data (it contains pathnames for rollback journals).
- **Other transient files are not encrypted**: SQLite can write temporary files for sorts, indexes, etc. To avoid plaintext transient spill to disk, we must disable file-based temporary storage at compile-time **and** enforce in-memory temp storage at runtime as a defense-in-depth measure.

Operational guidance:

- Treat `*.db`, `*-wal`, `*-shm`, and `*-journal` as sensitive and ensure they live in a private directory with restrictive permissions.
- **Compile-time**: Prefer building SQLCipher with `SQLITE_TEMP_STORE=3` (“always use memory for temporary storage”) when feasible.
  - The common Rust SQLCipher bundle uses `SQLITE_TEMP_STORE=2` (temp is in-memory unless `temp_store` is explicitly forced to file), and Android builds commonly use `SQLITE_TEMP_STORE=3`.
- **Runtime**: Always set `PRAGMA temp_store = MEMORY;` on **each connection** as an additional safeguard, even if compile-time settings should prevent file-based temp storage.

### 2. Secure Storage via `keyring-core`

Instead of creating a custom secure-storage abstraction, MDK uses the [`keyring-core`](https://crates.io/crates/keyring-core) ecosystem directly. This provides:

- A well-maintained, community-supported cross-platform credential storage API
- Native platform stores for all major platforms (including Android and iOS)
- Thread-safe `Send + Sync` credentials by design
- Built-in mock store for testing

#### `keyring-core` API Overview

The `keyring-core` crate provides a simple API for credential storage:

```rust
use keyring_core::{Entry, set_default_store, get_default_store, Result};

// Set the default credential store (platform-specific)
set_default_store(my_platform_store);

// Create an entry and manage secrets
// Use a host-provided service identifier (recommend: reverse-DNS / bundle id) to avoid collisions.
let entry = Entry::new("com.example.app", "mdk.db.key.default")?;
entry.set_secret(b"32-byte-encryption-key-here...")?;
let secret: Vec<u8> = entry.get_secret()?;
entry.delete_credential()?;
```

#### Key `keyring-core` Types

| Type | Description |
|------|-------------|
| `Entry` | A named credential in a store (identified by service + user) |
| `CredentialStore` | `Box<dyn CredentialStoreApi + Send + Sync>` — thread-safe store |
| `Credential` | `Box<dyn CredentialApi + Send + Sync>` — thread-safe credential |
| `Error` | Error enum including `NoEntry`, `NoStorageAccess`, etc. |

#### Platform Store Initialization

Each platform has a dedicated store crate. Most initialize automatically, but some (Android, Flutter) require explicit setup:

**Desktop platforms (macOS, Windows, Linux):**

```rust
// macOS / iOS
use apple_native_keyring_store::AppleStore;
keyring_core::set_default_store(AppleStore::new());

// Windows
use windows_native_keyring_store::WindowsStore;
keyring_core::set_default_store(WindowsStore::new());

// Linux (kernel keyutils)
use linux_keyutils_keyring_store::KeyutilsStore;
keyring_core::set_default_store(KeyutilsStore::new());
```

**Android (requires initialization):**

The `android-native-keyring-store` crate uses JNI to interact with Android Keystore. It requires initialization from the Android runtime.

**Option 1: With `ndk-context` feature** (for Dioxus Mobile, Tauri Mobile, android-activity):

```rust
use android_native_keyring_store::AndroidStore;
use keyring_core::set_default_store;

// Call at app startup
set_default_store(AndroidStore::from_ndk_context().unwrap());
```

**Option 2: Manual initialization via Kotlin** (for Flutter/FRB and other frameworks):

Add this Kotlin code to your Android project:

```kotlin
package io.crates.keyring

import android.content.Context

class Keyring {
    companion object {
        init {
            // Load the native library containing android-native-keyring-store
            System.loadLibrary("your_rust_lib")
        }

        external fun setAndroidKeyringCredentialBuilder(context: Context)
    }
}
```

Then call from MainActivity:

```kotlin
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Keyring.setAndroidKeyringCredentialBuilder(this)
        // Now keyring-core can be used from Rust
    }
}
```

**Flutter:**

Flutter apps can use `android-native-keyring-store` on Android with the same Kotlin initialization approach shown above. On iOS, `apple-native-keyring-store` works automatically. Add the Kotlin initialization code to your Flutter project's `MainActivity.kt`.

### 3. Integration with MDK

#### How MDK Uses `keyring-core`

**Key identifiers (important):**

- MDK uses `Entry::new(service_id, db_key_id)` to store the database encryption key.
- `service_id` should be a **stable, host-defined application identifier** (recommend: reverse-DNS / bundle id like `"com.example.app"`). This prevents collisions between multiple apps on the same OS user account.
- The `db_key_id` should be a **stable, host-defined identifier** (e.g., `"mdk.db.key.default"` or `"mdk.db.key.<profile_id>"`).
- **Do not derive the key identifier from an absolute `db_path`** (hashing paths is fragile across reinstalls, sandbox path changes, migrations, and renames).
- Neither identifier is secret; they are indexes into secure storage.

**Failure modes (expected and user-actionable):**

- If the database file exists but the keyring entry is missing, MDK must return a clear error (do **not** generate a new key and silently “brick” the existing database).
- Distinguish wrong key vs missing key vs plaintext database encountered when encryption is required vs corrupted database vs “secure storage unavailable / not initialized”.

```rust
// mdk-sqlite-storage/src/lib.rs

use keyring_core::{Entry, Error as KeyringError};

impl MdkSqliteStorage {
    /// Creates encrypted storage using the default keyring store.
    ///
    /// This is the primary constructor for production use. The keyring store
    /// must be initialized before calling this (see platform-specific setup).
    pub fn new<P>(file_path: P, service_id: &str, db_key_id: &str) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        // Get or create the 32-byte encryption key
        let key = get_or_create_db_key(service_id, db_key_id)?;
        let key_array: [u8; 32] = key
            .try_into()
            .map_err(|_| Error::InvalidKeyLength)?;

        let config = EncryptionConfig { key: key_array };
        Self::new_internal(file_path, Some(config))
    }

    /// Creates unencrypted storage.
    ///
    /// ⚠️ **WARNING**: This creates an unencrypted database. Only use for testing
    /// or development. Production applications should use `new()` with encrypted
    /// storage.
    pub fn new_unencrypted<P>(file_path: P) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        Self::new_internal(file_path, None)
    }

    fn new_internal<P>(file_path: P, config: Option<EncryptionConfig>) -> Result<Self, Error>
    where
        P: AsRef<Path>,
    {
        // Implementation details...
    }
}

/// Get an existing DB encryption key or generate and store a new one.
///
/// **Concurrency:** This operation must be atomic (at least within the current process).
fn get_or_create_db_key(service_id: &str, db_key_id: &str) -> Result<Vec<u8>, Error> {
    use std::sync::{Mutex, OnceLock};

    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();

    let entry = Entry::new(service_id, db_key_id)?;

    // Try to get existing key
    match entry.get_secret() {
        Ok(secret) => return Ok(secret),
        Err(KeyringError::NoEntry) => {
            // Key doesn't exist, generate a new one
        }
        Err(e) => return Err(e.into()),
    }

    // Generate a new 32-byte key
    let mut new_key = vec![0u8; 32];
    getrandom::getrandom(&mut new_key)
        .map_err(|e| Error::KeyGeneration(e.to_string()))?;

    // Store it
    entry.set_secret(&new_key)?;

    Ok(new_key)
}
```

**Note on concurrency:** the in-process mutex only coordinates within a single process. If a host can start multiple processes that open the same profile concurrently, the host should provide higher-level coordination.

#### Cargo.toml Changes

**Implemented configuration** (encryption is always enabled, platform stores are host responsibility):

```toml
# Workspace Cargo.toml
[workspace.dependencies]
keyring-core = "0.7"  # v4 architecture - see https://github.com/open-source-cooperative/keyring-rs/issues/259
getrandom = "0.3"
hex = "0.4"

# mdk-sqlite-storage/Cargo.toml
[dependencies]
keyring-core.workspace = true
getrandom.workspace = true
hex = { workspace = true, features = ["std"] }
rusqlite = { workspace = true, features = ["bundled-sqlcipher"] }
```

**Note:** Platform-specific store crates (`apple-native-keyring-store`, `android-native-keyring-store`, etc.) are **not bundled** with MDK. The host application is responsible for:

1. Adding the appropriate platform store crate to their dependencies
2. Initializing the store at app startup via `keyring_core::set_default_store(...)`
3. Calling `MdkSqliteStorage::new(path, service_id, db_key_id)`

This design keeps MDK platform-agnostic and avoids pulling in unnecessary platform-specific dependencies.

### 4. Project-Specific Usage Examples (see Part B)

Downstream integrations (including `whitenoise-rs`) are documented in **Part B** as non-normative examples.

### 7. File Permission Hardening

**Goal:** prevent other local users/processes from reading the encrypted database files.

#### Unix-like (macOS/Linux/etc.)

Create the database directory with mode `0700` (owner read/write/execute only) and database files with mode `0600` (owner read/write only). Execute permission is not needed for files.

**Footgun avoidance (recommended):**

- Only apply restrictive permissions to directories/files created for the MDK database (or a dedicated MDK subdirectory). Do not `chmod` arbitrary existing parent directories provided by the host.
- To avoid a short window where SQLite creates a new database file with default permissions (umask-dependent), prefer **pre-creating** the database file with mode `0600` before opening it.
- Policy recommendation: if an existing database directory or file is too-permissive, **fail closed** (return an error) rather than silently continuing.

#### iOS/Android

Rely on the application sandbox, but still store databases in app-private directories.

#### Windows

Windows does not have Unix-style chmod permissions. Instead, Windows uses Access Control Lists (ACLs) within security descriptors:

- **DACL (Discretionary Access Control List)**: Specifies which users/groups can access the file and what operations they can perform.
- **SACL (System Access Control List)**: Used for auditing (not required for our use case).

**Implementation approach for Windows (host responsibility):**

1. **Store in per-user locations**: Always store database files in the user's private app data directory (e.g., `%LOCALAPPDATA%\<app_name>\`). This provides baseline isolation since other non-admin users cannot access these directories by default.

2. **Apply explicit ACL restrictions**: Use Windows APIs to set a DACL that grants access only to the current user:
   - Use `SetNamedSecurityInfoW` or `SetSecurityInfo` to modify the file's security descriptor.
   - Create a DACL with a single ACE (Access Control Entry) granting `GENERIC_ALL` to the current user's SID.
   - Disable inheritance from parent directories to prevent inherited permissions from granting broader access.

MDK does not currently implement Windows ACL hardening internally. This is intentionally left to host applications because correct ACL handling is subtle and requires Windows-specific testing (inheritance, effective permissions, and principal selection).

**Reference implementation sketch (conceptual):**

```rust
#[cfg(windows)]
fn set_secure_file_permissions_windows(path: &Path) -> std::io::Result<()> {
    use windows::Win32::Security::{
        SetNamedSecurityInfoW, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows::Win32::Security::Authorization::{
        SetEntriesInAclW, EXPLICIT_ACCESS_W, SET_ACCESS, NO_INHERITANCE,
        TRUSTEE_IS_SID, TRUSTEE_W,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_USER};

    // 1. Get current user's SID from process token
    // 2. Build an EXPLICIT_ACCESS entry granting GENERIC_ALL to current user
    // 3. Create a new ACL with SetEntriesInAclW
    // 4. Apply it with SetNamedSecurityInfoW, using:
    //    - DACL_SECURITY_INFORMATION to set the DACL
    //    - PROTECTED_DACL_SECURITY_INFORMATION to disable inheritance

    // Implementation details TBD during development phase
    Ok(())
}
```

**Full implementation example (for reference):**

```rust
#[cfg(windows)]
mod windows_permissions {
    use std::path::Path;
    use std::ptr;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, PSID};
    use windows::Win32::Security::Authorization::{
        SetEntriesInAclW, EXPLICIT_ACCESS_W, SET_ACCESS, NO_INHERITANCE,
        TRUSTEE_IS_SID, TRUSTEE_W, TRUSTEE_FORM, TRUSTEE_TYPE,
    };
    use windows::Win32::Security::{
        GetTokenInformation, SetNamedSecurityInfoW, TokenUser,
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        SE_FILE_OBJECT, TOKEN_QUERY, TOKEN_USER, ACL,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    pub fn set_owner_only_permissions(path: &Path) -> std::io::Result<()> {
        // Get current user SID
        let sid = get_current_user_sid()?;

        // Build explicit access for current user only
        let mut explicit_access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS.0,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_TYPE::default(),
                ptstrName: PCWSTR(sid.0 as *const u16),
                ..Default::default()
            },
        };

        // Create new ACL with only this entry
        let mut new_acl: *mut ACL = ptr::null_mut();
        unsafe {
            SetEntriesInAclW(
                Some(&[explicit_access]),
                None,
                &mut new_acl,
            )?;
        }

        // Apply to file (with protected DACL to disable inheritance)
        let path_wide: Vec<u16> = path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(path_wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(new_acl),
                None,
            )?;
        }

        Ok(())
    }

    fn get_current_user_sid() -> std::io::Result<PSID> {
        // Implementation: OpenProcessToken, GetTokenInformation(TokenUser), extract SID
        // ...
        todo!("Implement SID retrieval")
    }
}
```

#### Combined implementation

```rust
// mdk-sqlite-storage/src/lib.rs

#[cfg(unix)]
fn create_secure_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path)?;
    let perms = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(unix)]
fn set_secure_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if path.exists() {
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_secure_directory(path: &Path) -> std::io::Result<()> {
    // On iOS/Android, the app sandbox generally restricts filesystem access.
    std::fs::create_dir_all(path)
}

#[cfg(not(unix))]
fn set_secure_file_permissions(_path: &Path) -> std::io::Result<()> {
    // On mobile platforms, we rely on app sandboxing.
    Ok(())
}
```

---

## Implementation Tasks

### Phase 1: Validate `keyring-core` Ecosystem

- [x] Verify MSRV compatibility (MDK requires Rust 1.90.0) — `keyring-core` 0.7.x compiles with Rust 1.90.0
- [ ] Test `keyring-core` + `apple-native-keyring-store` on macOS
- [ ] Test `keyring-core` + `android-native-keyring-store` on Android emulator
- [x] Document any initialization quirks or platform-specific requirements — documented in `keyring` module and lib.rs docs
- [x] Evaluate `keyring-core` error handling for our use cases — added `Error::Keyring` and `Error::KeyringNotInitialized` variants

### Phase 2: SQLCipher Integration in `mdk-sqlite-storage`

- [x] Update `Cargo.toml` to use `bundled-sqlcipher` feature
- [x] Add `keyring-core` dependency (platform stores are host responsibility, not bundled)
- [x] Add `EncryptionConfig` struct
- [x] Implement `get_or_create_db_key(service_id, db_key_id)` helper using `keyring_core::Entry`
- [x] Rename existing unencrypted constructor to `MdkSqliteStorage::new_unencrypted()`
- [x] Add `MdkSqliteStorage::new(file_path, service_id, db_key_id)` (encrypted) as the primary constructor
- [x] Also added `MdkSqliteStorage::new_with_key(file_path, config)` for direct key injection
- [x] Apply SQLCipher pragmas on **each** new connection before any migrations / foreign key pragmas:
  - [x] `PRAGMA key = "x'...'"` (**must be the first operation**)
  - [x] `PRAGMA cipher_compatibility = 4;`
  - [x] `PRAGMA temp_store = MEMORY;`
  - [x] Validate with a read (e.g., `SELECT count(*) FROM sqlite_master;`) to distinguish wrong key from other failures
- [x] **Compile-time**: Investigate temp-store hardening — **Findings**: Default `SQLITE_TEMP_STORE=2` + runtime `PRAGMA temp_store = MEMORY` is sufficient; Android already uses `=3`. Users can optionally set `LIBSQLITE3_FLAGS="SQLITE_TEMP_STORE=3"` for maximum hardening. See `SECURITY.md` for details.
- [x] Add explicit errors for missing key for an existing DB, wrong key, plaintext DB when encryption is required, corrupted DB, and secure-storage-unavailable/uninitialized
- [x] Add file permission hardening for Unix platforms (0700 for directories, 0600 for files; avoid `chmod` on arbitrary existing directories; pre-create DB file with 0600 to avoid permission races)
- [X] Windows filesystem hardening (ACLs) is left to host applications (see `SECURITY.md`)
- [x] Add unit tests for encrypted storage
- [ ] Test cross-platform compilation (iOS, Android, macOS, Linux, Windows)

### Phase 3: Android Integration Testing

- [ ] Set up test project with `android-native-keyring-store`
- [ ] Test manual Kotlin initialization path (for Flutter compatibility)
- [ ] Test `ndk-context` initialization path (for native Android apps)
- [ ] Document Android-specific setup in README
- [ ] Verify credential persistence across app restarts

### Phase 4: UniFFI Binding Updates

- [x] Add `new_mdk(db_path, service_id, db_key_id)` as the primary constructor (encrypted by default)
- [x] Add `new_mdk_unencrypted(db_path)` for testing/development use with clear warnings
- [x] Add `new_mdk_with_key(db_path, encryption_key)` for direct key injection
- [ ] Export keyring store initialization functions if needed
- [x] Update documentation for Swift, Kotlin, Python, Ruby (updated README.md and language-specific docs)
- [x] Add documentation for platform-specific store setup

### Phase 5: Migration Support

- [ ] Add utility to migrate unencrypted database to encrypted
- [ ] Add utility to re-key encrypted database
- [ ] Handle edge case: app upgrade from unencrypted to encrypted storage
- [ ] Document supported migration paths and failure modes (e.g., missing key vs wrong key vs corrupt DB)

---

## Security Considerations

### Key Storage Best Practices

1. **Never log or expose the encryption key**
2. **Use platform-specific secure storage** - Don't store keys in SharedPreferences, UserDefaults, or files
3. **Use device-bound keys where possible** - Prefer `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` on iOS
4. **Consider biometric protection** - For high-security use cases, require biometric auth to access the key
5. **Use an app-unique `service_id`** (reverse-DNS / bundle id) when storing credentials to avoid cross-application collisions on shared OS key stores

### Android-Specific Security Notes

1. **Use EncryptedSharedPreferences** - This is backed by Android Keystore but allows storing arbitrary secrets
2. **Avoid plain SharedPreferences** - Even with obfuscation, this is not secure
3. **MasterKey.KeyScheme.AES256_GCM** - Use the strongest available key scheme
4. **Android Keystore limitations** - Keys generated in Keystore cannot be exported as raw bytes, which is why we use EncryptedSharedPreferences for SQLCipher keys
5. **Minimum API level** - EncryptedSharedPreferences requires API 23+ (Android 6.0+)

### Database Security

1. **SQLCipher security design** - SQLCipher encrypts pages with 256-bit AES-CBC and authenticates page writes with HMAC-SHA512 (see SQLCipher design docs).
2. **Keying is explicit** - Use `PRAGMA key = "x'...'"` (raw 32-byte key data), then `PRAGMA cipher_compatibility = 4;` and `PRAGMA temp_store = MEMORY;`, and validate with a read (e.g., `SELECT count(*) FROM sqlite_master;`).
3. **Defaults, but pinned** - Use `PRAGMA cipher_compatibility = 4;` on each connection to avoid unexpected default changes across SQLCipher major versions.
4. **Sidecar + temp files** - WAL/journal page data is encrypted, but other transient files are not; ensure in-memory temp store and strict directory permissions.
5. **Optional hardening** - Consider `PRAGMA cipher_memory_security = ON` if the performance impact is acceptable.

### Backup / Restore (Not Supported Yet)

MDK does not currently provide backup/restore/export tooling. Hosts should assume that copying the database file(s) alone is insufficient without a compatible key management strategy.

### Breaking Changes and API Design

As part of the security audit work, MDK is making breaking changes to establish secure defaults:

- **Encrypted storage is the default**: The primary constructor (`new()`, `new_mdk()`) creates encrypted storage.
- **Unencrypted storage is explicitly opt-in**: Use `new_unencrypted()` / `new_unencrypted_mdk()` with clear warnings.
- **No backwards compatibility shims**: We are not maintaining deprecated APIs for unencrypted storage. Existing users must migrate to encrypted storage.

### Trust Boundaries with `keyring-core`

MDK trusts the `keyring-core` ecosystem and its platform-native credential stores. Key trust boundaries:

1. **Native stores are trusted**: We rely on Apple Keychain, Windows Credential Manager, Linux Secret Service, and Android Keystore to protect secrets appropriately.
2. **Store initialization**: On Android, the app must call `setAndroidKeyringCredentialBuilder(context)` before MDK can store secrets. Failure to initialize results in clear errors.

---

## Testing Strategy

### Unit Tests

```rust
#[test]
fn test_encrypted_storage_creation() {
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir.path().join("encrypted.db");
    let key = [0u8; 32]; // Test key

    let config = EncryptionConfig { key };
    let storage = MdkSqliteStorage::new(&db_path, Some(config));
    assert!(storage.is_ok());
}

#[test]
fn test_encrypted_storage_wrong_key_fails() {
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir.path().join("encrypted.db");

    // Create with key1
    let key1 = [1u8; 32];
    let config1 = EncryptionConfig { key: key1 };
    let storage1 = MdkSqliteStorage::new(&db_path, Some(config1)).unwrap();
    drop(storage1);

    // Try to open with key2
    let key2 = [2u8; 32];
    let config2 = EncryptionConfig { key: key2 };
    let result = MdkSqliteStorage::new(&db_path, Some(config2));
    assert!(result.is_err());
}

#[test]
fn test_unencrypted_cannot_read_encrypted() {
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir.path().join("encrypted.db");

    // Create encrypted database
    let key = [0u8; 32];
    let config = EncryptionConfig { key };
    let storage = MdkSqliteStorage::new(&db_path, Some(config)).unwrap();
    drop(storage);

    // Try to open without encryption
    let result = MdkSqliteStorage::new(&db_path, None);
    assert!(result.is_err());
}
```

### Integration Tests

- [ ] Test on iOS Simulator
- [ ] Test on Android Emulator
- [ ] Test host callback integration on both platforms (native iOS + native Android)
- [ ] Performance benchmarks (encryption overhead)

---

## References

### SQLCipher

- [SQLCipher Design](https://www.zetetic.net/sqlcipher/design/)
- [SQLCipher Documentation](https://www.zetetic.net/sqlcipher/sqlcipher-api/)
- [rusqlite SQLCipher feature](https://github.com/rusqlite/rusqlite#optional-features)

### keyring-core Ecosystem

- [keyring-core crate](https://crates.io/crates/keyring-core)
- [keyring-core documentation](https://docs.rs/keyring-core/)
- [keyring-core GitHub](https://github.com/open-source-cooperative/keyring-core)
- [Keyring ecosystem wiki](https://github.com/open-source-cooperative/keyring-rs/wiki/Keyring)

### Platform Credential Stores

- [apple-native-keyring-store](https://crates.io/crates/apple-native-keyring-store)
- [android-native-keyring-store](https://crates.io/crates/android-native-keyring-store) — [GitHub](https://github.com/open-source-cooperative/android-native-keyring-store)
- [windows-native-keyring-store](https://crates.io/crates/windows-native-keyring-store)
- [linux-keyutils-keyring-store](https://crates.io/crates/linux-keyutils-keyring-store)
- [dbus-secret-service-keyring-store](https://crates.io/crates/dbus-secret-service-keyring-store)
- [zbus-secret-service-keyring-store](https://crates.io/crates/zbus-secret-service-keyring-store)

### Platform Documentation

- [iOS Keychain Services](https://developer.apple.com/documentation/security/keychain_services)
- [Android Keystore](https://developer.android.com/training/articles/keystore)
- [Android EncryptedSharedPreferences](https://developer.android.com/reference/androidx/security/crypto/EncryptedSharedPreferences)
- [AndroidX Security Crypto Library](https://developer.android.com/jetpack/androidx/releases/security)

### UniFFI

- [UniFFI Callback Interfaces](https://mozilla.github.io/uniffi-rs/latest/udl/callback_interfaces.html)

---

## Part B: `whitenoise-rs` / Flutter Integration Notes (Non-normative)

This section captures downstream work that is useful for `whitenoise-rs`, but is intentionally separated from the MDK-generic design.

### Background: Current whitenoise-rs Key Storage Problem

`whitenoise-rs` (which depends on MDK) currently handles Nostr key storage using:

- `keyring` crate (v3) for most platforms
- Android: file-based obfuscation (not secure)

With `keyring-core`, `whitenoise-rs` can use the same credential storage for:

- The SQLCipher DB encryption key (MDK storage)
- Nostr secret keys (whitenoise-rs)

### Strategy (Downstream)

With `keyring-core`, the strategy is simpler:

| Platform | Store | Initialization |
|----------|-------|----------------|
| macOS | `apple-native-keyring-store` | Automatic |
| iOS | `apple-native-keyring-store` | Automatic |
| Windows | `windows-native-keyring-store` | Automatic |
| Linux | `linux-keyutils-keyring-store` | Automatic |
| Android (native) | `android-native-keyring-store` | Kotlin init required |
| Flutter (Android) | `android-native-keyring-store` | Kotlin init required |
| Flutter (iOS) | `apple-native-keyring-store` | Automatic |

### `whitenoise-rs` Usage (Sketch)

```rust
// In whitenoise-rs (sketch)

use keyring_core::Entry;

fn open_mdk(db_path: &Path) -> Result<MDK<MdkSqliteStorage>, Error> {
    // keyring-core store must be initialized before this call
    let service_id = "com.whitenoise.app";
    let mdk_storage = MdkSqliteStorage::new(db_path, service_id, "mdk.db.key.default")?;
    Ok(MDK::new(mdk_storage))
}

fn get_or_create_nostr_key() -> Result<Vec<u8>, Error> {
    let service_id = "com.whitenoise.app";
    let entry = Entry::new(service_id, "nostr.secret_key.default")?;

    match entry.get_secret() {
        Ok(secret) => return Ok(secret),
        Err(keyring_core::Error::NoEntry) => {
            // Generate new key
            let mut key = vec![0u8; 32];
            getrandom::getrandom(&mut key)?;
            entry.set_secret(&key)?;
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
}
```

### Flutter Integration

Flutter apps use the native keyring stores directly:

- **iOS**: `apple-native-keyring-store` works automatically
- **Android**: `android-native-keyring-store` with Kotlin initialization in `MainActivity.kt`

This is simpler than a callback-based approach and uses the same secure storage mechanisms.

### Downstream Tasks (whitenoise / Flutter)

- [ ] Update `whitenoise-rs` to use `keyring-core` instead of `keyring` v3
- [ ] Add platform-specific store initialization for Android (including Flutter)
- [ ] Test on all platforms (macOS, iOS, Windows, Linux, Android)
- [ ] Replace any Android file-obfuscation key storage with `android-native-keyring-store`
