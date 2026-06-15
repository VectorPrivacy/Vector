# Changelog

All notable changes to `vector-sdk` are documented here. This project adheres to
[Semantic Versioning](https://semver.org).

## 0.3.0

A ground-up rewrite. The previous `0.2.x` line hand-rolled gift-wrapping, AES-GCM
file encryption, NIP-96 upload, and reactions against `nostr-sdk` directly. All of
that now lives in [`vector-core`](../vector-core) — the same engine that powers the
Vector desktop and mobile apps — so the SDK is a small, ergonomic layer on top.

### Added

- **Unified messaging.** A single `Channel` for DMs *and* Community channels;
  `bot.channel(id)` auto-detects the transport from the id. `send`, `reply`
  (threaded), `edit`, `delete`, `react` / `react_custom`, `typing`, `send_file`.
- **Receiving.** `bot.on_message(handler)` for inbound messages, and
  `bot.on_event(handler)` for the full event stream as a `BotEvent`
  (`Message`, `MessageUpdate`, `Delete`, `MemberJoin`, `MemberLeave`, `Typing`,
  `Invite`, `Removed`). `bot.listen_with(...)` as a raw escape hatch.
- **Community object model** (discord.js-style): `msg.member()` → `Member`
  (`kick`/`ban`/`unban`/`grant_admin`/`revoke_admin`/`profile`/`is_owner`/`is_admin`),
  `msg.community()` / `bot.community(id)` / `bot.communities()` → `Community`.
- **Invite policy.** `.public()` (auto-accept any), `.whitelist([npubs])`
  (auto-accept only trusted inviters), or default `Manual`. Handles live *and*
  offline-parked invites.
- **Files.** Send encrypted attachments (DM + community) and
  `bot.download_attachment` / `bot.save_attachment` to fetch + decrypt received ones.
- **Keyless auto-identity.** With no key supplied, `build()` creates and persists an
  identity (`identity.nsec`) in the data dir and reuses it across restarts — a first
  bot needs zero setup. An explicit `.nsec(...)` / `.mnemonic(...)` always wins.
- **Profiles & avatars.** `update_profile` (always tagged `bot: true`),
  `bot.upload_image(path)` for avatars/banners, plus fetch/status/block/nickname.
- **Outage resilience.** `on_message`/`listen` catch up on connect, then an
  event-driven relay health monitor reconnects dead relays and folds back missed
  state (re-foundings, rekeys, bans, metadata) — no idle polling.

### Notes

- Built on `vector-core`'s process-global state, so **one bot owns the process's
  identity at a time** (run several processes for several identities).
- Outside this monorepo, a consumer must replicate the workspace
  `[patch.crates-io] nostr = …zeroize-secretkey` line in its root `Cargo.toml`
  (see the README) until `vector-core` ships on crates.io.
