# Vector

Private messaging app built with Tauri v2 (Rust backend + vanilla JS frontend) on the Nostr protocol. Supports desktop (macOS, Windows, Linux) and Android.

## Comment style — read before writing any code

Comments state the WHY of a non-obvious choice, in one or two lines max. They do NOT narrate the bug that led to the fix, the debugging session, the user's flow that surfaced it, audit/reviewer references, or which discovery sparked the change.

**Anti-patterns (do not write these):**
- "Sending a reply IS the read confirmation. updateChat's auto-mark is gated on focus, which can miss in race scenarios → user comes back to type a reply → auto-mark missed the receive. This catches the case where..."
- "(Reviewer ref: B1, B7.)"
- "Previously this pulled MY_SECRET_KEY.to_keys() directly — fine for local users, but for bunker accounts..."
- "Originally defined later alongside X; that came after Y so the catch saw `undefined`..."
- "Earlier versions did Z and ended up logging users in as device key..."
- Quoting test names, dates, or which testing pass surfaced the issue.

**Good patterns:**
- "Mark on own-send: updateChat's auto-mark is focus-gated."
- "GuardedKey vault — secret materialises in plaintext only for microseconds per op."
- "Multi-relay by design — single-relay connect URIs are a centralisation trap."

When in doubt: would this comment make sense to someone reading the code two years from now with no project context? If it requires knowing about a specific debugging episode, cut it. **Default to no comment.**

## Build & Run

```bash
npm run dev              # Desktop development (Tauri dev server)
npm run build            # Desktop release build
npm run dev:bare         # Dev without default features — no whisper AND no tor (faster compile)
npm run build:bare       # Release without default features (no whisper, no tor)
npm run android:dev      # Android dev (./scripts/android-dev.sh)
npm run android:build    # Android release (tauri android build)
scripts/fdroid-build.sh  # F-Droid flavour: source-only, unsigned, no in-app updater (docs/fdroid/)
```

Frontend build: `node scripts/build-frontend.mjs` copies `src/` to `dist/` with optional minification (terser + lightningcss in release).

Vector Core test suite: `cd crates && cargo test -p vector-core`.

## Architecture

### Vector Core (`crates/vector-core/`) — Single Source of Truth

All business logic lives here, fully decoupled from Tauri. Any client (GUI, CLI, SDK, bot) imports this crate.

- **`macros.rs`** — log_info!, log_debug!, log_trace!, log_warn! (#[macro_export])
- **`types.rs`** — Message, Attachment, Reaction, EditEntry, ImageMetadata, SiteMetadata
- **`profile/`** — Profile, ProfileFlags, SlimProfile (Box<str> optimized, u16 interner handles)
  - **`profile/sync.rs`** — ProfileSyncHandler trait, SyncPriority queue, load_profile, update_profile, update_status, block/unblock, nickname, background processor
- **`chat.rs`** — Chat, ChatType, ChatMetadata, SerializableChat
- **`compact.rs`** — CompactMessage (u64 ms timestamps), CompactMessageVec, NpubInterner, TinyVec, bitflags
- **`state.rs`** — ChatState, all globals (NOSTR_CLIENT, MY_SECRET_KEY, STATE, etc.), WrapperIdCache, processing gate
- **`crypto/`** — GuardedKey vault, GuardedSigner, Argon2id, AES-GCM, ChaCha20, decrypt_data, extension_from_mime, sanitize_filename, resolve_unique_filename, format_bytes, mime_from_magic_bytes, mime_from_extension (full MIME map)
- **`db/`** — SQLite schema, 41 atomic migrations, downgrade guard, connection pools, RAII guards, settings KV
- **`hex.rs`** — SIMD hex encode/decode (NEON ARM64, SSE2/AVX2 x86_64, scalar fallback)
- **`rumor.rs`** — process_rumor() inbound message parser, RumorEvent, 11 result variants
- **`stored_event.rs`** — StoredEvent, StoredEventBuilder, event_kind constants
- **`sending.rs`** — SendCallback trait, SendConfig, send_dm/send_file_dm/send_rumor_dm, retry_send_gift_wrap
- **`blossom.rs`** — File upload with progress tracking, retry, server failover
- **`inbox_relays.rs`** — NIP-17 kind 10050 relay resolution, stampede-protected cache, gift-wrap sending
- **`net.rs`** — SSRF protection, build_http_client
- **`stats.rs`** — CacheStats, DeepSize trait for memory benchmarking (debug builds)
- **`traits.rs`** — EventEmitter trait (abstracts UI notification), ProgressReporter

`src-tauri` consumes vector-core via `path = "../crates/vector-core"`. Types and globals are re-exported — same instances, shared memory.

### Tauri Shell (`src-tauri/src/`)

- **`lib.rs`** — App entry, plugin registration, `invoke_handler` with 150+ commands
- **`commands/`** — Tauri command handlers (thin wrappers around vector-core logic)
- **`state/`** — Re-exports vector-core globals + local TAURI_APP + TauriEventEmitter (bridges emit_event to Tauri)
- **`macros.rs`** — log_error! only (toast + log file via TAURI_APP; log_info/debug/trace/warn in vector-core)
- **`rumor.rs`** — Pure re-export of vector-core's rumor processing
- **`message/`** — Re-exports vector-core types + TauriSendCallback + file dedup logic
- **`services/`** — Event handler, subscription handler, notifications
- **`miniapps/`** — WebXDC-compatible mini apps (Tauri-specific: custom protocol, WebView, Iroh P2P)
- **`android/`** — JNI bindings, localhost media server, background sync
- **`simd/`** — SIMD image, audio, URL, HTML operations (hex moved to vector-core)

### Frontend (`src/`)

- **`main.js`** — Main application logic (~25k lines, bundled)
- **`js/`** — ES modules: chat-scroll, emoji, file-preview, marketplace, settings, voice, db, platforms/
- **`styles.css`** — All styles (~7k lines)
- **`index.html`** — Single-page app shell

Frontend communicates with backend via `window.__TAURI__.core.invoke()`.

## Key Patterns

### 🚨 Multi-account safety — read this BEFORE writing code that touches STATE, the DB, or relays

Vector supports N accounts per install, and a swap can happen at **any await point**: mid-fetch,
mid-publish, mid-sync. Work that started under account A and finishes after the swap used to write
A's data into B's storage — that is how group messages from the previous account showed up in a
fresh account's chat list, and how profile updates persisted to the wrong database.

**This is now structural, not a rule you apply.** An account's resources live on a `Session`
(`crates/vector-core/src/db/mod.rs`): its database connections, its chats and profiles, its relay
client and identity, and every per-account cache. A task started with `db::spawn_bound` carries
that session for its whole life, so `db::`, `STATE`, `nostr_client()` and the caches all resolve
the account the task **began under** — for however many awaits it spans, whoever logs in meanwhile.
A swap installs a new `Session` and drops the old one. There is no teardown list.

#### The three things to actually do

1. **Spawn with `crate::db::spawn_bound`, never bare `tokio::spawn`.** The test suite parses the
   tree and fails on an unbound spawn (`crates/vector-core/src/spawn_audit.rs`, run by vector-core,
   src-tauri and vector-agent). A task that genuinely owns no account state — a process-lifetime
   listener, a socket drain, CPU work on bytes in hand — is exempted **per site** with
   `// spawn-detached: <why>` on the line or just above it. Per site, not per file.

2. **Put a new per-account resource on the session, not in a `static`.** Declare a private marker
   type and one accessor:

   ```rust
   struct MyCache;                                    // the key, private to this module
   fn my_cache() -> Arc<Mutex<HashMap<K, V>>> {
       crate::db::current_session().scoped::<MyCache, _>()
   }
   ```

   Nothing to clear on swap: the account's instance goes when its session does. The two teardown
   lists that used to enumerate these had already drifted apart from each other.

3. **Keep the guard ONLY where the caller is unbound.** A Tauri command or an SDK call is not a
   task we spawned, so nothing binds it — a swap really does move its writes, and its return value
   goes to a UI now showing someone else. A multi-step publish there must refuse to half-commit:

   ```rust
   let session = SessionGuard::capture();
   // ...publish, then persist...
   if !session.is_valid() { return Err("account changed during …".into()); }
   ```

   That is what the ~150 remaining `is_valid()` checks are, and
   `a_swap_during_create_private_channel_aborts_without_a_write` proves one of them.

#### What is handled for you

- **UI emission.** `emit_event` / `emit_event_json` drop anything from a session that is no longer
  live (`db::session_is_live`). There is one UI showing one account; work for a previous account
  paints nothing. Emit through those, **not** a raw `AppHandle::emit`, from anywhere that can
  outlive a swap.
- **Database and chat state.** Both belong to the session. A late write lands in an orphan nobody
  reads, never in the account on screen.
- **`init_database` is idempotent and login-safe.** Re-running it for the same account keeps its
  state; binding a not-yet-bound session to a database keeps what it holds, because creating an
  account fills the session before its database exists.

#### Still resolved late — the honest exceptions

- **`MY_SECRET_KEY` / `ENCRYPTION_KEY`.** Zeroized on swap rather than re-homed, which is a
  security property the session model does not replace. Guard anything that signs or decrypts
  across an await.
- **Android biometric enrollment** (`src-tauri/src/commands/biometric.rs`) resolves its account at
  use.
- **The plane pool, warm-relay set and relay breaker** (`community/transport.rs`) still key on the
  session generation. They need an active disconnect on swap, which dropping cannot perform.

#### Smell signals for a review

- A bare `tokio::spawn(` (the suite catches it, but catch it earlier)
- `static` / `OnceLock` / `LazyLock` holding anything per-account
- A raw `handle.emit(...)` inside a spawned task
- New tables or settings keys created without `account_dir(npub)` scoping

### Adding new Tauri commands

Every new `#[tauri::command]` requires THREE things:

1. Permission TOML in `src-tauri/permissions/autogenerated/<command_name>.toml` (create `allow-` and `deny-` entries)
2. `"allow-<command-name-with-hyphens>"` added to `src-tauri/capabilities/default.json`
3. Registration in the `invoke_handler` macro in `lib.rs`

Missing any = `invoke()` silently rejects with "Command X not allowed by ACL".

**If the command mutates per-account state**, see the multi-account section above: commands are unbound, so a multi-step publish still needs a `SessionGuard` at entry and an abort before the persist.

### Adding a database migration

Every new migration in `crates/vector-core/src/db/schema.rs` requires TWO things:

1. A `run_atomic_migration(conn, <id>, ...)` call, using the next id **above the current highest**
2. `HIGHEST_MIGRATION_ID` bumped to that same id

**Forgetting step 2 locks users out of their own accounts.** The migration applies fine on first
run, then the downgrade guard reads the DB as newer than the build that wrote it and refuses to
open on the second run. Two guards catch it before release: a `debug_assert` in
`run_atomic_migration` (fires on any debug run) and `highest_migration_id_matches_the_runner`
(parses the file, so it can't drift). Neither runs in CI today.

Ids are a high-water mark, never a count. **33-39 and 45-61 are burned** and must never be reused
— some DBs recorded them as applied without the ALTER landing, so reuse is a silent skip. Live ids
are 19-32, 40-44, 62-87. Read `HIGHEST_MIGRATION_ID` rather than this line when adding one: the
constant is enforced by a test, this sentence is not.

### SendCallback — Unified DM Send Pipeline

All DM sends (text + file) flow through vector-core's `send_dm`/`send_file_dm`/`send_rumor_dm`:

- **`SendCallback` trait** — 7 lifecycle hooks (on_pending, on_sent, on_failed, on_upload_progress, on_upload_complete, on_attachment_preview, on_persist) with default no-ops
- **`SendConfig`** — per-call config: max_send_attempts, retry_delay, self_send, cancel_token. Presets: `gui()` (12 retries), `headless()` (3), `default()` (1)
- **`TauriSendCallback`** — emits to JS frontend + DB persistence
- **`CliSendCallback`** — terminal output for sent/failed/progress
- Text DMs: `message()` short-circuits to `vector_core::send_dm` with `TauriSendCallback`
- File DMs: src-tauri handles dedup + upload, then calls `vector_core::send_rumor_dm` for gift-wrap + retry
- Community channels: their own send path in `vector_core::community` (Concord), not this pipeline

### ProfileSyncHandler — Unified Profile Pipeline

All profile operations (fetch, publish, block, nickname) flow through vector-core's `profile::sync` module:

- **`ProfileSyncHandler` trait** — `on_profile_fetched(slim, avatar_url, banner_url)` with default no-op. Covers DB persistence + image caching.
- **`TauriProfileSyncHandler`** — spawns `db::set_profile` + `cache_profile_images`
- **`EventEmitter` trait** — abstracts UI notification. `TauriEventEmitter` bridges to `TAURI_APP.emit()`, registered at startup.
- Profile ops in vector-core: `load_profile`, `update_profile`, `update_status`, `block_user`, `unblock_user`, `set_nickname`, `get_blocked_users`
- Sync queue: `SyncPriority` (Critical/High/Medium/Low), `ProfileSyncQueue`, `start_profile_sync_processor`
- src-tauri profile commands are one-line delegates to vector-core

### State access

Global state lives in `src-tauri/src/state/` and is re-exported at crate root:
- `TAURI_APP`, `MY_SECRET_KEY` (process-global vaults); `STATE`, `nostr_client()`, `my_public_key()` resolve through the live `Session`
- `STATE` holds `Arc<Mutex<AppState>>` with chats, profiles, settings
- Multi-account: separate SQLite DB per account in `~/.local/share/io.vectorapp/data/<npub>/`

### Error handling

All commands return `Result<T, String>`. Errors are string-formatted for frontend display.

### Android-specific

- WebView `shouldInterceptRequest` threads have NO tokio runtime — `Handle::current()` will PANIC. Use `try_lock()` with retry loops for STATE access from JNI threads.
- Localhost media server (`android/media_server.rs`) serves files because `asset://` doesn't support Range requests for audio/video.
- rustls must use `ring` provider (not `aws-lc-rs`) — currently satisfied naturally (no aws-lc in the lock); re-verify if a new dependency pulls rustls with default providers.

### Compact messages

`message/compact.rs` defines `CompactMessage` / `CompactMessageVec` — a memory-optimized format using `Box<str>`, `u16` npub interning, and `[u8; 32]` IDs instead of hex strings. Messages are stored in compact form in memory and converted to full `Message` structs for frontend serialization.

### File attachments

Files are encrypted (NIP-96/Blossom), uploaded to media servers, and referenced via SHA-256 hash. The `name` field carries the original filename through the protocol. Downloads save with human-readable names + collision suffixes (`-1`, `-2`). Hash-based dedup prevents re-downloading identical content.

## Dependencies

Key crates: `nostr-sdk` 0.45, `tauri` 2.10, `tokio` 1.49, `rusqlite` 0.37, `iroh` 1.0, `iroh-gossip` 0.101, `aes-gcm`, `argon2`, `image` 0.25

`[patch.crates-io]` in `src-tauri/Cargo.toml` pins two forks:
- `nostr` — `SecretKey`'s Drop used `non_secure_erase`, which the compiler can optimise away; the fork zeroizes with volatile writes
- `whisper-rs-sys` — Vector's build fixes

## Platform Notes

- **macOS**: WKWebView white flash prevented by the window's `backgroundColor` in `tauri.conf.json` plus `macOSPrivateApi`. Metal GPU for Whisper.
- **Linux**: `WEBKIT_DISABLE_DMABUF_RENDERER=1` set for WebKitGTK compatibility.
- **Android**: API 26+. Vulkan GPU disabled for Whisper (device freeze). OpenSSL vendored.
- **Feature flag**: `whisper` (default) — enables OpenAI Whisper transcription. Use `--no-default-features` to skip.
