# Vector v0.4.0 TODO

## Embedded Tor Integration

**Goal**: Simple Tor toggle in Settings that routes ALL network traffic through Tor (relays, Blossom, avatars, metadata, PIVX). Iroh P2P excluded (QUIC/UDP, Tor is TCP-only).

**Crate**: `arti-client` (Tor Project's official Rust Tor client)

### Blocker: rusqlite Version Conflict

arti-client's `tor-dirmgr` requires rusqlite as a non-optional dependency. The `libsqlite3-sys` crate uses `links = "sqlite3"`, so only ONE version of rusqlite is allowed in the entire dependency tree.

| arti-client | tor-dirmgr | rusqlite required | Compatible with Vector? |
|-------------|------------|-------------------|------------------------|
| 0.28 (Mar 2025) | 0.28 | ^0.32 | Yes |
| 0.30 (May 2025) | 0.30 | ^0.32 | Yes (but 11 months stale) |
| 0.32 (Jul 2025) | 0.32 | ^0.36 | No — Vector uses 0.32 |
| 0.34+ | 0.34+ | ^0.37+ | No |
| **0.40 (Mar 2026)** | **0.40** | **^0.38** | **No — needs rusqlite upgrade** |

**Vector currently uses**: rusqlite 0.32 (pinned by MDK at rev 136a9ee)

### Resolution Path: Upgrade rusqlite to 0.38

1. **Upgrade Vector's rusqlite** from 0.32 to 0.38
   - API-compatible for Vector's usage (params!, query_row, execute)
   - Key 0.35 change: `execute()` and `prepare()` reject multi-statement SQL — Vector uses `execute_batch()` for those, so safe

2. **Patch mdk-sqlite-storage locally** to use rusqlite 0.38
   - MDK at rev 136a9ee uses rusqlite 0.32
   - Create `patches/mdk-sqlite-storage/` with bumped Cargo.toml
   - MDK's rusqlite usage is simple enough to survive the bump

3. **OR wait for MDK upgrade** (parked on branch `mdk-071-upgrade`)
   - MDK at rev bef6887 uses rusqlite 0.37
   - Blocked by V005 migration bug (reported to JeffG)
   - Once fixed, both MDK upgrade and Tor integration unblock simultaneously

### Implementation Plan (ready to execute once rusqlite unblocked)

Full plan saved at: `.claude/plans/federated-tinkering-candy.md`

**Architecture**: arti-client bootstraps in-process, runs SOCKS5 mini-proxy on `127.0.0.1:<port>`, reqwest and nostr-sdk both route through it.

**Key files to create/modify**:
- `src-tauri/src/tor.rs` — Core module: TorClient lifecycle, SOCKS5 proxy, start/stop/status commands
- `src-tauri/Cargo.toml` — Add arti-client, tor-rtcompat, reqwest `socks` feature, async-wsocket `socks` feature, `tor` feature flag
- `src-tauri/src/commands/account.rs` — Inject `Connection::proxy(addr)` into 3 Client builder sites
- `src-tauri/src/image_cache.rs`, `net.rs`, `blossom.rs`, `whisper.rs`, `pivx.rs`, `commands/mls.rs` — Replace reqwest clients with `tor::build_http_client_with_timeout()`
- `src/index.html` + `src/js/settings.js` — Toggle UI with live bootstrap progress

**Design decisions**:
- Feature flag: `tor` (like `whisper`), `--no-default-features` skips it
- Setting stored in global file `{app_data}/tor_enabled` (readable before login)
- HTTP requests switch dynamically; relay connections need restart (NOSTR_CLIENT is OnceLock)
- Android background sync skips Tor in V1
- `socks5h://` scheme for DNS-leak-free resolution through Tor

### Cargo.toml Changes (ready, tested, reverted for now)

```toml
[features]
default = ["whisper", "tor"]
tor = ["dep:arti-client", "dep:tor-rtcompat"]

[dependencies]
arti-client = { version = "0.40", optional = true, default-features = false, features = ["tokio", "rustls"] }
tor-rtcompat = { version = "0.40", optional = true, features = ["tokio", "rustls"] }
async-wsocket = { version = "0.13", features = ["socks"] }  # enables Connection::proxy() in nostr-sdk
reqwest = { version = "0.12", features = ["rustls-tls", "json", "stream", "socks"] }
```

---

## Other v0.4.0 Items

### MDK 0.7.1 Upgrade (parked on branch `mdk-071-upgrade`)
- Upgrade from rev 136a9ee to bef6887 (MIP-03: NIP-44 -> ChaCha20-Poly1305)
- Blocked by V005 migration exporter secret relabeling bug
- Reported to JeffG — awaiting fix
- Unblocks: rusqlite 0.37+, which unblocks arti-client 0.40

### Persist Peer Advertisements to SQLite
- Store Kind 30078 peer-advertisement/peer-left events in `events` table
- Fixes: Player B coming online after Player A advertised — currently missed
- Full plan in `.claude/plans/federated-tinkering-candy.md` (older version)

### nostr-sdk Zeroize SecretKey PR
- Upstream PR for the zeroize fix currently in VectorPrivacy/nostr fork
- Branch: `zeroize-secretkey`
