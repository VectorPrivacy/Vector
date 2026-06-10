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
npm run dev:bare         # Dev without whisper feature (faster compile)
npm run build:bare       # Release without whisper feature
npm run android:dev      # Android dev (./scripts/android-dev.sh)
npm run android:build    # Android release (tauri android build)
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
- **`db/`** — SQLite schema, 20 atomic migrations, connection pools, RAII guards, settings KV
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
- **`rumor.rs`** — Thin wrapper: re-exports vector-core + parse_mls_imeta_attachments + process_rumor_with_mls + resolve_download_dir
- **`message/`** — Re-exports vector-core types + TauriSendCallback + file dedup logic
- **`services/`** — Event handler, subscription handler, notifications
- **`mls/`** — MLS group encryption via OpenMLS/MDK (not yet in vector-core)
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

### 🚨 Multi-account session safety — read this BEFORE writing any code that touches STATE, DB, or relays

Vector supports N accounts per install. A `swap_session` / `reset_session` can happen at **any await point**: the user might switch accounts mid-fetch, mid-publish, mid-MLS-sync, mid-anything. When that happens:

- `STATE` (chats/profiles) is replaced with the new account's data
- The DB pool (`POOL_GENERATION`) is swapped to the new account's vector.db
- `MY_KEYS` / `MY_PUBLIC_KEY` / `ENCRYPTION_KEY` are rebound
- The per-account marker file points at the new npub

**Any task still running with values captured before the swap will write account A's data into account B's storage.** This has caused multiple real bugs: MLS messages from the previous account appearing in a fresh account's chat list, profile updates persisting to the wrong DB, kind-10063 server lists merging across accounts. The damage is invisible until the user opens the wrong chat.

#### The SessionGuard contract

Use `vector_core::state::SessionGuard` to defend against this:

```rust
let session = SessionGuard::capture();   // snapshot generation NOW
// ...network/file/long-await work...
if !session.is_valid() { return; }       // bail if the generation advanced
// ...STATE/DB mutation...
```

**Rules — apply every single one of these:**

1. **Every `tokio::spawn` that touches per-account state needs a captured `SessionGuard` BEFORE the spawn boundary and an `is_valid()` check before its first side effect.** Capturing inside the `async move` block is too late — the spawn order is unobserved.
2. **Every long async function (≥ ~1s, anything network-bound) that ends in a write needs a re-check before that write**, even if the caller already validated. Fetches can take seconds; the validation must straddle the I/O.
3. **Every Tauri command that mutates per-account settings/DB needs a guard at entry.** Pattern: capture `SessionGuard`, do the read/mutate/save sandwich, re-check `is_valid()` immediately before `save_*`. Don't trust `get_current_account()` alone — it returns the *current* account, which may not be the one the caller expects.
4. **Per-group locks and account-scoped service instances (`MlsService`, etc.) freeze their per-account paths at construction.** A stale instance keeps decrypting account A's MLS storage successfully and writes the plaintext into account B's STATE. Gate every method that mutates state on a `SessionGuard` captured at the call site, not at construction.
5. **The debounced republish pattern (`republish_*_debounced`) captures `SessionGuard` before the sleep.** Copy that pattern for any debounced effect.

#### Smell signals — grep for these in any PR

- `tokio::spawn(` without a `SessionGuard::capture()` on the lines just before it
- `client.fetch_events`, `client.send_event_builder`, `tokio::time::sleep` between two writes to STATE/DB (without an `is_valid()` between fetch and save)
- `static` / `OnceLock` / `LazyLock` storing anything per-account that doesn't refresh on swap
- A function that takes `&Client` plus an `npub` / `PublicKey` argument *and* writes to STATE or per-account DB — almost certainly needs a `SessionGuard` parameter too
- New tables / settings keys created without `account_dir(npub)` scoping
- Anything pre-fetched into a `Vec<String>` before a `for` loop that does network/DB writes — the loop must re-check session each iteration

#### Reference implementations (copy these patterns)

- `crates/vector-core/src/inbox_relays.rs::republish_inbox_relays_debounced` — debounced publish with SessionGuard
- `crates/vector-core/src/blossom_servers.rs::fetch_and_merge_own_list` — long fetch + write, takes `SessionGuard` parameter, re-checks before save AND before cache refresh
- `crates/vector-core/src/mls/service.rs::sync_group_since_cursor` — SessionGuard captured at entry, re-validated before each per-rumor STATE write
- `src-tauri/src/commands/relays.rs::require_active_blossom_session` — entry-guard helper for mutation commands

When in doubt, add a guard. Cost: one atomic load. Cost of the bug it prevents: catastrophic cross-account data corruption.

### Adding new Tauri commands

Every new `#[tauri::command]` requires THREE things:

1. Permission TOML in `src-tauri/permissions/autogenerated/<command_name>.toml` (create `allow-` and `deny-` entries)
2. `"allow-<command-name-with-hyphens>"` added to `src-tauri/capabilities/default.json`
3. Registration in the `invoke_handler` macro in `lib.rs`

Missing any = `invoke()` silently rejects with "Command X not allowed by ACL".

**If the command mutates per-account state**, also see the multi-account section above — capture `SessionGuard` at entry, re-validate before any `save_*` call.

### SendCallback — Unified DM Send Pipeline

All DM sends (text + file) flow through vector-core's `send_dm`/`send_file_dm`/`send_rumor_dm`:

- **`SendCallback` trait** — 7 lifecycle hooks (on_pending, on_sent, on_failed, on_upload_progress, on_upload_complete, on_attachment_preview, on_persist) with default no-ops
- **`SendConfig`** — per-call config: max_send_attempts, retry_delay, self_send, cancel_token. Presets: `gui()` (12 retries), `headless()` (3), `default()` (1)
- **`TauriSendCallback`** — emits to JS frontend + DB persistence
- **`CliSendCallback`** — terminal output for sent/failed/progress
- Text DMs: `message()` short-circuits to `vector_core::send_dm` with `TauriSendCallback`
- File DMs: src-tauri handles dedup + upload, then calls `vector_core::send_rumor_dm` for gift-wrap + retry
- MLS groups: stay in src-tauri (MDK dependency)

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
- `TAURI_APP`, `NOSTR_CLIENT`, `MY_KEYS`, `MY_PUBLIC_KEY`, `STATE`
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

Key crates: `nostr-sdk` 0.44, `tauri` 2.10, `tokio` 1.49, `rusqlite` 0.32, `openmls`, `iroh` 0.96, `iroh-gossip` 0.96, `aes-gcm`, `argon2`, `image` 0.25

Local path deps: `../../mdk/crates/mdk-*` (MDK media/encryption library)

wry fork: `[patch.crates-io]` in Cargo.toml points to local `../../wry` for WKWebView background color fix.

## Platform Notes

- **macOS**: WKWebView white flash prevented via `drawsBackground` KVC on config (wry fork). Metal GPU for Whisper.
- **Linux**: `WEBKIT_DISABLE_DMABUF_RENDERER=1` set for WebKitGTK compatibility.
- **Android**: API 26+. Vulkan GPU disabled for Whisper (device freeze). OpenSSL vendored.
- **Feature flag**: `whisper` (default) — enables OpenAI Whisper transcription. Use `--no-default-features` to skip.
