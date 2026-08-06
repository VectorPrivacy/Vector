# Changelog

All notable changes to `vector-sdk` are documented here. This project adheres to
[Semantic Versioning](https://semver.org).

## 0.8.0

Requires `vector-core` 0.7.0. Closes the two gaps in [#83](https://github.com/VectorPrivacy/Vector/issues/83):
back-filled history was invisible to bots, and the cold-start subscription window was unobservable.

### Added

- **History reads.** `Channel::history(limit)` and `Channel::history_before(&cursor, limit)` —
  the newest messages this bot holds locally, chronological, unit = **messages**. Local-only
  and pull-only: back-filled history is *never* dispatched as live events, this is how you
  read it. Works for DMs and community channels alike.
- **`Cursor`** — a durable `(at_ms, id)` position for paging and self-built sync mechanisms.
  Compared by value, so it survives its message being deleted (NIP-09, moderation,
  self-destruct timers) and stays unambiguous inside a same-millisecond burst. Serde-
  serializable, plus a **wire-stable** string form `"<at_ms>:<message id hex>"` you can
  persist anywhere and `parse()` back. Obtain one with `Cursor::of(&message)`.
- **Relay syncing.** `Channel::sync(max_events)`, `sync_before(&cursor, max_events)`,
  `sync_since(since_ms, max_events)` — fetch ONE page of up to `max_events` events (per
  relay; clamped to 500) from the channel's relays, ingest, and return how many *messages*
  were new. `0` means "nothing new in this page", the natural stop of a walk-until-dry
  loop (see `sync_before`'s docs for the loop). The unit is **events**: reactions, edits,
  deletes and presence spend the budget too. Community channels only — DM recovery stays
  `VectorBot::sync_dms`.
- **`BotEvent::Ready { communities }`** — fires once per listen when the realtime
  subscription registers. Fires even with 0 communities, so "subscribed to nothing"
  and "still connecting" are finally different states. `VectorBot::subscription_ready()`
  is the pollable twin for health checks outside the handler.

### Fixed

- **The cold-start deaf window is no longer gated on the slowest relay.** Startup
  used to prime stream auth across EVERY community relay *before* registering the
  live subscription, so one dead or AUTH-refusing relay left the bot deaf to all
  community traffic for a minute or more while the healthy relays sat idle. The
  subscription now registers right after the startup catch-up sync; auth priming
  runs in the background, and each AUTH-gating relay joins the stream the moment
  its own auth completes, gating nobody else. `Ready` reports the exact moment.

### Changed

- **`BotEvent::ChannelKeyed` now carries `backfilled: usize`** — how many messages
  predating the grant were pulled into local state before the event fired. They are
  history, not delivery: none of them arrive as `BotEvent::Message`, *ever*. Read them
  with `Channel::history` and decide what deserves acting on. (Breaking: add the field
  to your pattern or match with `..`.)
- **`BotEvent` is now `#[non_exhaustive]`.** Match with a `_` arm; future event kinds
  will then stop being breaking changes.

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
