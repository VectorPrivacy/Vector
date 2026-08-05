# Changelog

All notable changes to `vector-sdk` are documented here. This project adheres to
[Semantic Versioning](https://semver.org).

## 0.7.0

Requires `vector-core` 0.6.0.

### Added

- **Private community channels.** `Community::channels()` lists every channel with
  `is_private()`, `is_readable()` and `epoch()` — **including private ones this bot
  cannot read yet**. `is_readable() == false` means "granted, key hasn't arrived",
  which was previously indistinguishable from "nobody has talked to me": a bot added
  to a private channel simply went mute, with no error and no way to diagnose it.
- **Channel management.** `create_channel`, `create_private_channel`, `rename_channel`,
  `delete_channel`, `grant_access`, `revoke_access`, `channel_members`, `channel_access`.
  Granting adds the channel's access role and sends the member its key; revoking drops
  the role and rekeys the channel so the removal actually severs them.
- **`BotEvent::ChannelKeyed`** — fires when a vended key lands, so a granted bot acts on
  the transition instead of polling.

### Fixed

- **Headless clients never back-filled Concord v2 chat.** `sync_communities` only
  refolded consensus and skipped chat entirely, so a bot saw *only* messages that
  arrived live — anything sent while it was offline was invisible forever. Independent
  of private channels, and affects every existing bot.
- `Community::members()` was documented as "best-effort, from recent activity". It is
  in fact the full Complete Memberlist: Guestbook joins/leaves ∪ anyone observed
  publishing ∪ role-holders ∪ the proven owner, minus bans, with leave/rejoin ordering.
  Behaviour unchanged; the doc was wrong.

### Notes

Verified live cross-client against Armada in both directions, through the whole
lifecycle: join → grant → key vend → adopt → read history → reply → revoke → rekey →
severed → re-grant.

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
