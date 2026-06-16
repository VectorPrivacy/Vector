# Changelog

All notable changes to `vector-sdk` are documented here. This project adheres to
[Semantic Versioning](https://semver.org).

## 0.3.0

The first release of the rewritten SDK — a small, ergonomic layer over the
[`vector-core`](https://crates.io/crates/vector-core) engine that powers the Vector
desktop and mobile apps.

### Added

- **Unified messaging.** One `Channel` for direct messages *and* community channels;
  `bot.channel(id)` opens either. `send`, `reply` (threaded), `edit`, `delete`,
  `react` / `react_custom`, `typing`, `send_file`.
- **Receiving.** `bot.on_message(handler)` for incoming messages, and
  `bot.on_event(handler)` for the full event stream as a `BotEvent`
  (`Message`, `MessageUpdate`, `Delete`, `MemberJoin`, `MemberLeave`, `Typing`,
  `Invite`, `Removed`).
- **Communities** (discord.js-style): `msg.member()` → `Member`
  (`kick`/`ban`/`unban`/`grant_admin`/`revoke_admin`/`profile`/`is_owner`/`is_admin`),
  and `msg.community()` / `bot.community(id)` / `bot.communities()` → `Community`.
- **Invite policy.** `.public()` (accept from anyone), `.whitelist([...])`
  (accept only from trusted accounts), or the default (handle them yourself).
  Picks up invites that arrived while the bot was offline, too.
- **Files.** Send files in DMs and communities; `bot.download_attachment` /
  `bot.save_attachment` to get received ones.
- **Keyless auto-identity.** With no key supplied, `build()` creates and reuses a
  persistent identity — a first bot needs zero setup. An explicit `.nsec(...)` /
  `.mnemonic(...)` always wins.
- **Profiles & avatars.** `update_profile` (bots are tagged as bots automatically),
  `bot.upload_image(path)` for avatars, plus status / block / nickname.
- **Stays connected.** Reconnects on its own after a network drop and catches up on
  what it missed.

### Notes

- One bot owns the process while it runs — run several processes for several bots.
