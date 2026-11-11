# Changelog

All notable changes to `vector-sdk` are documented here. This project adheres to
[Semantic Versioning](https://semver.org).

## 0.12.0 (2026-11-11)

Requires `vector-core` 0.11.0, and with `voice` the new `vector-media` 0.1.0. `vector-core` 0.11
depends on `vector-calls` 0.2.0 and the new `vector-push` 0.1.0; all five are published together.
`vector_sdk` re-exports `vector_core`, so the transport changes below reach SDK users through
`vector_sdk::vector_core`.

### Added

- **Voice Channels, behind the `voice` feature.** `channel.join_voice()` joins a voice
  Channel's call and returns a `VoiceCall`: `listen()` gives what each member says as 48 kHz
  mono PCM, a 20 ms frame at a time with who said it; `say(pcm)` queues audio sent in real time
  (`say_and_wait`, `finished`, `hush`, `queued`); `set_muted`, `set_deafened`, `state`,
  `connected` and `leave`. A bot can be in many calls at once, each its own `VoiceCall`. When a
  moderator disconnects or moves the bot, the call ends: `listen()` returns `None`, `say` returns
  false, and `moved_to()` names the channel a move sent it to.
  `channel.voice_members()` and `BotEvent::VoicePresence` say who is in a call.
  `voice::tone`, `voice::level` and `voice::decode_with_ffmpeg` help with audio. Opus comes
  from the new `vector-media` crate (libopus built with cmake). A bot joins a call while it
  listens (`run` or `on_event`). Examples: `voice_echo_bot`, `voice_radio_bot`.
- **Transcription, behind the `whisper` feature.** `voice::Utterances` gathers a call's frames
  into one clip per speaker per pause; `voice::Transcriber::load(model)` runs whisper.cpp over
  a clip (`transcribe`), on the GPU on macOS. The `voice_whisper_bot` example is a voice
  assistant built on both: it writes down what's said in the Channel's chat and answers out loud.
- **Run a Community from a bot.** Roles (`create_role(RoleSpec)`, `Role::edit`, `rename`,
  `set_permissions`, `delete`, `holders`, `reorder_roles`, `set_everyone_permissions`), member
  Roles (`add_role`, `remove_role`, `set_roles`, `roles`, `permissions`), Categories
  (`create_category`, `Category::rename/move_to/delete/set_overwrite`), Channel settings
  (`create_channel_with(NewChannel)` for private, in-category, voice or forum Channels with a
  topic; `Channel::info`, `rename`, `set_topic`, `set_view`, `set_read_only` for an announcement
  channel, `move_to`), permission overwrites
  (`Channel::set_overwrite(Overwrite::role(&mods).allow(..))`, `clear_overwrite`, `overwrites`,
  `sync_permissions`, `permissions_of`), Timeouts (`Member::timeout`, `remove_timeout`,
  `timed_out_until`, `Community::timeouts`), Community Nicknames (`Member::nickname`,
  `set_nickname`, `Community::nicknames`), pins (`Channel::pin/unpin/pins`, `IncomingMessage::pin`)
  and a typed Audit Log (`Community::audit_log` with `AuditAction` and `AuditTarget`). Each action
  is checked the way every member's client will judge it before anything publishes.
- **`BotEvent::CommunityChange`**: one event per change to a Community's metadata, relays, Roles,
  `@everyone`, member Roles, bans, kicks, Channels, Categories, layout, overwrites, syncs, pins,
  Timeouts and Nicknames, each naming who made it (`change.by`). Match on `ChangeKind`.
- `BotEvent::Reaction`, `ReactionRemoved`, `Edit` and `Dissolved`, each with `at`, when it was
  sent. DM edits and DM typing now reach the event stream.
- `CommunityChange::at`, when a change was made, and `also_by`: everyone else whose edit a
  change folds in, when several people changed one thing while the bot wasn't looking.
- `bot.create_community(name, NewCommunity)`, `bot.join(link)` and `bot.preview_link(link)`. A
  preview says whether it was read from the Community itself (`verified`) or is the link's word.
- `Member::add_role` and `remove_role` change one Role against the freshest view, so a Role
  another moderator granted or took meanwhile is left as they left it.
- `Community::icon`, `banner`, `set_icon`, `set_banner`, `remove_icon`, `remove_banner`. Images are
  stripped of metadata and fitted before they're encrypted for members.
- `Community::relays`, `check_relays`, `move_relays`, `voice_relays`, `set_voice_relays`,
  `invite_links`, `revoke_invite`, `revoke_all_invites`, `primary_channel`, `owner`, `admins`,
  `description`, `categories`, `channel_infos`, `role`, `role_named`, `refound`,
  `rescue_stranded`, `repair_roster`, `explain_epoch`, `raid_report`.
- `Channel::post(text, PostOptions)`: a reply, an expiry, or the one bot a command is meant for
  (the `["bot", pk]` tag), in one call. `send_file_with(path, opts)` and `send_bytes(bytes, name,
  opts)` send files with a caption and a reply, from disk or memory. `unreact(message_id, emoji)`
  withdraws a reaction.
- The `VECTOR_DATA_DIR` environment variable chooses a bot's data directory when the builder
  doesn't: two bots on one machine each need their own.
- `bot.run(handler)`: `on_event` that exits cleanly on Ctrl-C. `builder().catch_up(CatchUp)`
  chooses whether the bot catches up before going live.
- `vector_sdk::net`: HTTP over the bot's own transport, for the requests a bot makes itself.
- `Permissions` is re-exported, and its bit constants combine: `Permissions::KICK | Permissions::BAN`.
- The guide [Moving your bot onto the SDK](guides/moving-onto-the-sdk.md), and the
  `community_admin_bot` example.
- **Custom emoji in what a bot writes.** `subscribe_emoji_pack(naddr)` equips a pack, and every
  `:code:` the bot sends that names one of its emoji carries the image tag. `emojis()` lists each
  code exactly as Vector's picker names it (`love~1`, `love~2` when packs collide);
  `unsubscribe_emoji_pack`, `emoji_packs` and `refresh_emoji_packs` round it out. Equipped packs
  refresh at startup and hourly.
- `Community::set_channel_private(channel_id, private)`: make a public channel private, or back.
- **Bots skip every tier.** Any number of emoji packs with every emoji in each, the top tier's
  12 new reactions per message (the message's own ceiling) and the top tier's pinned chats. Re-exports `EmojiPack`, `PackEmoji` and `SendableEmoji`.
- **I2P, behind the `i2p` feature.** `.i2p()` and `.i2p_with(I2pOptions { .. })` route the bot
  through the I2P router already running on the machine (SAMv3); `.clearnet()` moves a bot back
  to direct connections. A bot whose account chose Tor or I2P must name its network on every run.
- `xdc::allow_outside_transport`, the network-neutral name of `xdc::allow_outside_tor`.
- `SendCallback::on_failed_reason`: one line saying why a send failed, right before `on_failed`
  (the network refusing the recipient's relays, the network being down, no relay accepting it).
- `net::HttpClient::without_direct()` and `transport::ws::apply_transport_without_direct`: a fetch
  allowed only because an anonymity network was chosen fails rather than going direct after a
  switch to Clearnet.

### Changed

- **Nothing sent while the bot listens is missed, and nothing arrives twice.** Everything sent
  since the bot began listening (with a five-minute allowance for senders' clocks) arrives once,
  however it reaches the bot: live, through the startup catch-up, after a reconnect, after a
  relay drops a subscription, or after the machine sleeps. Community messages sent while the bot
  started were stored without an event, and DMs sent during an outage were dropped; reactions,
  edits and deletions a catch-up carried never arrived. What was sent before the bot began
  listening is history, as before: `channel.history()`.
- `CommunityChange` follows the same rule: a change made while the bot was offline is part of
  what it starts from, and one made after it began listening is reported even when a catch-up
  carried it. A Roster or banlist change a full read folded was reported only at the next
  refresh, if any.
- **Pictures are compressed before they're sent**, `send_file` included: fitted to 2560px,
  re-encoded and stripped of metadata, the file's name following its new format. A PNG stays a
  PNG when it fits, so a chart stays sharp. One too large to re-encode is stripped without
  re-encoding, and one that can't be stripped either is refused rather than sent with its
  metadata. `PostOptions::default().compress(false)` sends the bytes as they are.
- A private Channel's messages sent since the bot began listening arrive as `Message` once its
  key does; older ones are history, as before.
- **Every connection is decided per connect, and fails closed.** vector-core routes relay sockets
  and HTTP through one transport layer: a network that is chosen but not up refuses in process
  instead of going direct.
- `vector_core::net::build_http_client` and `shared_http_client` return a `net::HttpClient` (a
  descriptor that picks its route per request), not a `reqwest::Client`. Requests go through
  `HttpClient::get/post/..`, then `Req::send` and `Resp`; errors are `net::HttpError`.
  `proxied_request` takes an `HttpClient` and returns a `Req`.
- `TorService::start` fails with "A network is already running." while any network instance is
  installed. `TorService::socks_addr()` and `proxy_url()` point at Vector's authenticated local
  bridge: a stock SOCKS client using them is refused. `proxy_url()` is for display only.
- `tor::blackhole_proxy_addr()` is deprecated and returns port 0, where nothing can listen.
- `.onion` hosts are refused on every network with "Vector can't reach .onion addresses yet."
  (`Refusal::NotReachable`): this build's Tor has no onion-service client.
- A DM to a contact whose inbox list couldn't be looked up because the network was down or
  failing no longer falls back to the bot's own relays: the send waits for the network, then
  fails with the network's reason.
- `Community::name`, `channels` and `is_dissolved` read one Community instead of listing them all.
- A DM edit applies only from the author of the message it edits.
- `ChangeKind::ChannelDeleted` comes from the deletion's purge, so a deletion a catch-up carried
  is reported too, once.
- A DM file's caption reaches the recipient, as a reply to the file: a DM file carries no text.
- `vector-core` and `vector_sdk` require the nostr crates by caret (`nostr ^0.45.7`,
  `nostr-sdk ^0.45.6`), not exactly: the exact versions they named were yanked upstream, which
  left 0.11 unresolvable for a new bot.
- A bot's own Community changes don't come back as `CommunityChange` events: the call that made
  them returned their result.
- An invite that arrives while the bot catches up (at startup, or after a reconnect) reaches the
  invite policy at once. It used to wait, parked, for the next restart.
- The slash-command manifest is published to indexers and Community relays without adding them
  to the bot's own relays, where they carried its DM subscription and DM sync. A bot's startup
  no longer waits on an indexer's NIP-77 timeout.
- Startup runs one DM catch-up, behind the live subscription, instead of one before it.
- A refused action is checked again after catching the Community up, so a grant that hasn't
  folded yet doesn't read as `NotPermitted`.

### Breaking

- `BotEvent::MessageUpdate` is replaced by `Reaction`, `ReactionRemoved` and `Edit`.
- `BotEvent::Delete` gains `by`, `moderated` and `at`; `MemberJoin` gains `community_id`, `invited_by`
  and `invite_label`; `MemberLeave` gains `community_id`.
- `update_profile`, `set_status`, `block`, `unblock` and `set_nickname` return `Result<()>`.
- `Community::roles()` returns `Vec<RoleInfo>`; the old `{ owner, admins }` JSON is
  `owner()` and `admins()`.
- `bot.channel(id)` reads a hex pubkey as a DM to that key unless it names a channel the bot holds.
- `VectorError` is `#[non_exhaustive]` and adds `NotPermitted { needs, action }` and `NotFound`.
- `BotEvent::Delete`'s `by` is an `Option`: `None` when the Community's rules took back a post
  held for review.

The guide's last section walks through each.

## 0.11.0

Requires `vector-core` 0.10.0.

### Added

- **Mini Apps, behind the `xdc` feature.** Bots join the realtime sessions of Mini Apps
  (`.xdc` files) shared in DMs and Communities, and speak the app's own messages.
  `bot.xdc("app-id").run(..)` runs a handler for each copy of an app someone opens (`"*"`
  for every app, `.join_when(JoinWhen::Shared)` to join on share), `msg.xdc()?.join()`
  joins one by hand, and `channel.send_xdc(path)` hands an app out. A session sends to
  everyone in it (`send_json`, `send_text`, `send`, or a `sender()` from another task) and
  yields `XdcEvent`s; `XdcFrame::verified_sender()` names a sender you can act on.
  Handler sessions end after 3 minutes with nobody connected or 15 without a message
  (`idle_timeout`, `quiet_timeout`). The builder's `xdc_download_limit` (default 32 MiB)
  caps the download used to read an app's manifest.
- **`BotEvent::XdcPresence`**: someone opened or closed a Mini App in a chat the bot is in.
- Guides: [Add a bot to your Mini App](guides/bot-for-your-mini-app.md) and
  [Write a bot for any Mini App](guides/bot-for-any-mini-app.md).
- Examples: `xdc_counter_bot` and `xdc_tictactoe_2d_bot` (each ships its own app),
  `xdc_oracle_bot`, `xdc_tictactoe_bot`, `xdc_spy` and `xdc_probe`.

### Changed

- The SDK's log lines (slash commands, the interface manifest) go to stderr through
  `VECTOR_LOG`, like vector-core's, instead of stdout.
- `listen()` subscribes to live DMs before its startup catch-up, so a message sent while
  the bot starts arrives live instead of at the next resync.

## 0.10.0

Requires `vector-core` 0.9.0.

### Added

- **Moderation policies.** `community.policies()` writes a rulebook from named rules (words,
  links, repetition, flooding, a joining cohort), previews it against real history, enables and
  retires it, and watches with verdicts a bot can act on. `is_provable()` separates what survives
  replay from inference, so a strong hunch lands in `needs_human()` rather than `actionable()`.
  Acting is dry-run until `.arm()`, and never removes anyone on less than proof. Verdicts carry
  their band, standing and findings (rule, severity, the messages that convicted).
- The `policy_bot` and `raid_guard` examples.

### Changed

- A join flood is a second way to convict a raid, alongside one script posted by many accounts.

## 0.9.0

Requires `vector-core` 0.8.0.

### Changed

- **Racing bans compose.** Concurrent `ban()` calls (one handler task per message during a spam
  wave) used to build editions from the same snapshot, so the last one un-banned the rest. Bans
  now queue per Community and drain into one edition, one strip pass and one key rotation.
  `ban_many` and `unban_many` batch a whole wave into one rotation.
- Messages and member joins reach handlers only when they're news: a relay backfilling old events
  no longer greets members who joined a month ago.

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
