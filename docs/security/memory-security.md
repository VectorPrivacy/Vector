# Vector — Memory Security Architecture

## Overview

Vector stores private keys using a memory-hardened vault designed to resist automated extraction tools, forensic imaging, and targeted reverse engineering. This document describes the protection layers, threat model analysis, and platform-specific hardening.

## Vault Architecture

Private keys are **never stored as contiguous values** in process memory. Instead:

1. Each key is **XOR-split into 4 random shares** (secret splitting)
2. Each share is stored as individual `usize` values scattered across **128 static arrays**
3. Of the 128 arrays, only a handful contain real key data — the rest are **indistinguishable decoys** filled with identical random values
4. Share positions are derived from **ASLR addresses** that change every launch, with **guaranteed uniqueness** — no two positions can collide (rehash on collision)
5. All derivation uses **zero compile-time constants** — multipliers and iteration counts are computed from ASLR addresses that change every launch

### Properties

- **Zero heap allocations** — no pointers, no allocation metadata, no structural fingerprints
- **Zero searchable constants** — no magic numbers in the binary for reverse engineers to find
- **Provably indistinguishable arrays** — all 128 arrays have identical size, content distribution, initialization timing, and modification patterns
- **Snapshot-diff resistant** — during key storage, ALL 128 arrays receive random writes, making real writes invisible
- **Multi-key isolation** — multiple keys (signing key, encryption key) share the same vault arrays but cannot corrupt each other. Decoy writes from one key's set/clear operations exclude positions belonging to other active keys
- **Zero side-channel** — key retrieval performs only atomic reads (no memory writes)
- **Computational hardening** — thousands of mixing iterations per derivation, making brute-force expensive

### Design Rationale

**128 arrays** — power-of-2 so modular arithmetic compiles to AND masks (no searchable magic divisor constants). Enough decoys to be provably indistinguishable, small enough to not create an excessively distinctive memory footprint.

**4,096 entries per array** — matches common crypto table sizes (S-boxes, hash constants), maximizing false positives during memory scanning (thousands of candidates in a typical process). Power-of-2 for the same AND-mask benefit.

Increasing either dimension does not improve security against the primary attack vector (instance address brute-force), but does increase memory usage and binary fingerprint distinctiveness. Decreasing them reduces false positives. The current values are a deliberate balance of stealth, chaff density, and resource cost (4 MB on 64-bit, 2 MB on 32-bit).

## Platform Hardening

In release builds, Vector applies platform-specific protections at startup to prevent external processes from reading its memory or attaching debuggers.

### macOS
- Debugger attachment and `task_for_pid` blocked via `ptrace(PT_DENY_ATTACH)`

### Linux / Android
- `/proc/pid/mem` access, `ptrace` attachment, and core dumps blocked via `prctl(PR_SET_DUMPABLE, 0)`

### Windows
- Debugger detection at startup
- Unsigned DLL injection blocked via process mitigation policy (Microsoft-signed only)
- `ReadProcessMemory` blocked by stripping `PROCESS_VM_READ` from the process DACL

All protections are release-only — debug builds remain debuggable.

## Threat Model

### Protected against

- **Info-stealers and automated memory scanners** — no known key patterns, signatures, or structural fingerprints exist in memory. Generic scanning tools find nothing actionable.
- **Forensic imaging tools** — without a Vector-specific extraction module, standard forensic suites cannot identify key material among the vault's random data.
- **Memory dump analysis** — a raw memory dump contains 128 arrays of random values. Even with the derivation algorithm (open source), extracting keys requires brute-forcing ASLR-dependent addresses — a computationally expensive search with no shortcuts.
- **Core dump exposure** — on Linux/Android, `PR_SET_DUMPABLE=0` suppresses core dumps, preventing key material from being written to disk on crash. On macOS, `PT_DENY_ATTACH` prevents third-party memory capture. On Windows, the DACL blocks external memory reads.
- **Swap file / hibernation** — if pages are swapped to disk under memory pressure (possible on all platforms since mlock is not used), key shares exist as individual `usize` values indistinguishable from random data. An attacker with a swap file would face the same brute-force as a memory dump.

### Resistant to (requires significant effort)

- **Targeted reverse engineering** — an attacker with Vector's source code, the release binary, and a memory snapshot must perform manual binary analysis (hours in tools like Ghidra) to determine the runtime relationship between the vault arrays and key storage. LTO and ASLR make static layout unpredictable across builds.
- **Brute-force with source code** — with the algorithm and a memory dump, the attacker must brute-force the ASLR instance address. Computational hardening (~25 μs/attempt) limits throughput. The search space depends on platform ASLR entropy (minutes to hours, single-core). The vault is the second defensive layer — anti-debug protections prevent obtaining the dump in the first place.

### Not protected against

- **Code injection with elevated privileges** — an attacker who can load code into Vector's process can call the key retrieval function directly, bypassing the vault entirely. This requires either root/admin access (to override anti-debug protections), pre-launch environment control (to inject libraries before protections activate), or binary modification (to patch out the protections, invalidating the code signature). None of these can be performed by a same-privilege, post-launch process.
- **Kernel-level access** — a kernel rootkit cannot crack the vault's mathematics (it faces the same brute-force as any memory reader), but it can bypass the vault entirely by modifying Vector's code in memory to intercept keys during the brief plaintext window of signing operations, or by setting hardware breakpoints on vault access. This is an active code-modification attack, not a passive memory-reading attack.
- **Keylogging** — capturing the PIN/password before it reaches the application bypasses all memory protection.

## Comparison with Other Messaging Applications

Analysis performed 2026-03-23 against Signal Desktop ([`a8118faf`](https://github.com/signalapp/Signal-Desktop/tree/a8118faf0888682a53dc2cd5c11fcd0cc4a30573)), libsignal ([`a5e76674`](https://github.com/signalapp/libsignal/tree/a5e76674882a89bac1ed3f4a982120652966d21e)), and Telegram Desktop ([`ba4715a3`](https://github.com/telegramdesktop/tdesktop/tree/ba4715a3afc49554aef36a67335c519d05701162)).

| Protection | Vector | Signal Desktop | Telegram Desktop |
|---|---|---|---|
| Key splitting | 4 XOR shares across 128 arrays | None | None |
| Memory obfuscation | 128 decoy arrays, zero constants | None | None |
| Key zeroization | All paths (passwords, seeds, keys) | Symmetric cipher state only | DH exchange temps only |
| Anti-debug | PT_DENY_ATTACH / PR_SET_DUMPABLE / DACL | None | None |
| Anti-DLL injection | Microsoft-signed only policy (Windows) | None | None |
| Key extraction difficulty | Hours (with source + binary + snapshot) | Instant (grep for 32 bytes) | Instant (follow pointer) |

**Signal Desktop** stores identity keys in a [plain JavaScript `Map`](https://github.com/signalapp/Signal-Desktop/blob/a8118faf0888682a53dc2cd5c11fcd0cc4a30573/ts/SignalProtocolStore.preload.ts#L249) with no memory protection. At the Rust layer, libsignal's [`PrivateKey` derives `Copy`](https://github.com/signalapp/libsignal/blob/a5e76674882a89bac1ed3f4a982120652966d21e/rust/core/src/curve.rs#L236-L239) with no `Zeroize` or `Drop` — the raw `[u8; 32]` is freely duplicated on the stack without clearing. The `zeroize` crate exists in the workspace but is [not applied to any asymmetric key type](https://github.com/signalapp/libsignal/blob/a5e76674882a89bac1ed3f4a982120652966d21e/rust/core/Cargo.toml). Signal uses Electron's `safeStorage` for the database key at rest, but this is disk encryption — not memory hardening.

**Telegram Desktop** stores the MTProto auth key as a [plain `std::array<gsl::byte, 256>`](https://github.com/telegramdesktop/tdesktop/blob/ba4715a3afc49554aef36a67335c519d05701162/Telegram/SourceFiles/mtproto/mtproto_auth_key.h#L16-L63) with no custom destructor. The only [`OPENSSL_cleanse` usage](https://github.com/telegramdesktop/tdesktop/blob/ba4715a3afc49554aef36a67335c519d05701162/Telegram/SourceFiles/mtproto/details/mtproto_dc_key_creator.cpp#L368-L375) is in `DcKeyCreator::Attempt::~Attempt()` — cleaning DH exchange temporaries after the key has already been [copied into an unprotected `AuthKey`](https://github.com/telegramdesktop/tdesktop/blob/ba4715a3afc49554aef36a67335c519d05701162/Telegram/SourceFiles/mtproto/details/mtproto_dc_key_creator.cpp#L829-L839). No `mlock`, `mprotect`, anti-debug, or memory-hardening of any kind exists.

### Known limitations

- **Transient key exposure** — during signing or encryption operations, the reconstructed 32-byte key exists on the stack for microseconds. This window is minimized by the patched nostr SDK (which uses `zeroize` volatile writes on `SecretKey::Drop`), but is not zero.
- **Binary size fingerprint** — the 128 arrays create a ~4 MB contiguous block in BSS. This is detectable as "large high-entropy static data" in a memory scan. The block itself doesn't reveal the key, but it identifies where the vault data resides. Extracting the key still requires the full brute-force.

## Additional Protections

Beyond the vault, Vector applies defense-in-depth:

- **Zeroize** — passwords, mnemonic seeds, nsec strings, and temporary key copies are overwritten with zeros immediately after use via the `zeroize` crate (volatile writes that resist compiler optimization).
- **Per-sign key reconstruction** — the Nostr signing interface reconstructs the key from the vault for each operation. The key exists in plaintext only for microseconds during signing, then is dropped and zeroized.
- **No permanent key copies** — the `GuardedSigner` reads from the vault on demand, replacing the previous approach of holding a permanent `Keys` copy in memory.
- **No reusable cracker** — ASLR changes the instance address every launch, and LTO changes binary offsets every build. A cracker crafted for one version breaks on the next update, ruling out commodity tooling and redistributable extraction scripts.
