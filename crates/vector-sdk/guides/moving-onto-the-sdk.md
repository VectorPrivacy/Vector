# Moving your bot onto the SDK

Plenty of Vector bots began as a copy of Vector's own code: `vector-core` internals, a slice of the desktop app's commands, a hand-written `InboundEventHandler`. They work, until the code they copied changes underneath them. The SDK is the part that doesn't: it's versioned, it keeps its promises across releases, and it already handles what those bots had to get right by hand (account safety, transport routing, retries, freshness, permission checks).

This guide maps the usual internals onto SDK calls. Most bots shrink by half.

## Start the bot

```rust
let bot = vector_sdk::VectorBot::builder()
    .nsec(std::env::var("VECTOR_NSEC")?)  // or nothing: an identity is made once and kept
    .public()                              // accept Community invites (or .whitelist([...]))
    .build()
    .await?;

bot.run(|bot, event| async move { /* match on BotEvent */ }).await?;
```

That replaces `VectorCore::init`, `login`, `connect`, building a relay client, `listen` with your own handler, and a reconnect loop. `run` reconnects, catches up on what it missed, and exits cleanly on Ctrl-C.

## Events

If you implemented `InboundEventHandler`, each hook has a `BotEvent`:

| Your hook | `BotEvent` |
| --- | --- |
| `on_dm_received`, `on_file_received`, `on_community_message` | `Message(msg)` (DM or Channel, same type) |
| `on_reaction_received`, `on_community_update` (a reaction) | `Reaction { message, reaction }` |
| `on_community_update` (an edit), DM edits you never got | `Edit { message }` |
| `on_message_deleted`, `on_community_removed` | `Delete { by, moderated }` (`by: None` when the rules took a post back) |
| `on_community_presence` | `MemberJoin { invited_by, invite_label }` / `MemberLeave` |
| `on_community_typing`, DM typing you never got | `Typing` |
| `on_community_refreshed`, then diffing state yourself | `CommunityChange { change }`: one per change, with `change.by` |
| `on_community_dissolved` | `Dissolved` |
| `on_community_self_removed` | `Removed` |
| `on_channel_keyed` | `ChannelKeyed { backfilled }` |
| `on_subscription_ready` | `Ready` |

Everything sent since the bot began listening arrives once, however it reaches the bot: live, or through a catch-up after it starts, reconnects or wakes. What came before is history, not news: a relay replaying a month-old join doesn't greet anyone, and old messages a sync pulls in never arrive as `Message`. Read history with `channel.history(n)` and page it with `Cursor`. Each event carries `at`, when it was sent.

`CommunityChange` is the one to look at if your bot reloaded roles or channels on every refresh to work out what moved. Each event says what changed and who did it:

```rust
use vector_sdk::{BotEvent, ChangeKind};

if let BotEvent::CommunityChange { change, .. } = event {
    match change.kind {
        ChangeKind::MemberRolesChanged { member, added, removed } => { /* ... */ }
        ChangeKind::OverwriteChanged { scope_id, target, allow, deny } => { /* ... */ }
        ChangeKind::TimedOut { member, until } => { /* ... */ }
        _ => {}
    }
}
```

## Calls

| What you reached for | The SDK call |
| --- | --- |
| `send_dm`, `send_community_message`, building a rumor | `bot.channel(id).send(text)` (DM or Channel, picked from the id) |
| `send_community_message_expiring`, a `["bot", pk]` tag by hand | `channel.post(text, PostOptions::default().expires_in(600).to_bot(npub))` |
| `send_file_dm` with a caption, `send_community_file` | `channel.send_file_with(path, opts)` / `channel.send_bytes(bytes, name, opts)` |
| `revoke_reaction`, finding your own reaction id | `channel.unreact(message_id, emoji)` |
| `create_community_v2`, renaming `#general` | `bot.create_community(name, NewCommunity::default().first_channel("lobby"))` |
| `join_community`, `preview_public_invite` | `bot.join(link)` / `bot.preview_link(link)` |
| `load_community_v2(..).name`, `list_communities()` JSON | `community.name()`, `community.channels()`, `community.channel_infos()` |
| `create_role`, `edit_role_shown`, decimal permission strings | `community.create_role(RoleSpec::new("Mods").permissions(Permissions::KICK))`, `role.edit(..)` |
| `set_member_roles` with a hand-built id list | `member.add_role(&role)` / `remove_role` / `set_roles` |
| `set_channel_overwrite(cid, scope, "role", ..)` | `channel.set_overwrite(Overwrite::role(&mods).allow(Permissions::VIEW_CHANNEL))` |
| `create_channel_in`, `edit_channel_details` | `community.create_channel_with(name, NewChannel::default().in_category(id))`, `channel.set_topic(..)`, `set_view("forum")`, `set_read_only(true)` |
| `create_category`, `move_channel`, `set_channel_sync` | `community.create_category(name)`, `channel.move_to(..)`, `channel.sync_permissions(..)` |
| `timeout_member`, `community_timeouts` | `member.timeout(duration)`, `member.timed_out_until()` |
| `set_community_nickname`, `member_nickname` | `member.set_nickname(..)`, `member.nickname()` |
| `pin_community_message`, `get_channel_pins` | `msg.pin()`, `channel.pins()` |
| `community_audit_log` and its JSON | `community.audit_log()`: typed `AuditAction` and `AuditTarget` |
| The app's `set_community_image` | `community.set_icon(bytes)` / `set_banner(bytes)` (metadata stripped first) |
| `move_community_relays`, `set_voice_relays` | `community.move_relays(..)`, `community.set_voice_relays(..)` |
| `member_has_permission(cid, npub, bit)` | `member.can(Permissions::BAN)`, `channel.permissions_of(npub)` |
| `reqwest::Client`, `build_http_client` from core | `vector_sdk::net::build_http_client(timeout)` |

Permissions are `Permissions` bits: `Permissions::KICK | Permissions::BAN` combine, and every SDK call that takes permissions takes those.

## What to stop doing

- **Reaching for core's globals** (`STATE`, `nostr_client()`, the database). Their shapes change between releases, and the SDK's calls cover what bots used them for.
- **Reading the secret key.** Nothing in the SDK needs it. If you held `MY_SECRET_KEY` to sign something, the call you wanted is above.
- **Sending HTTP with your own client.** A bot on Tor or I2P leaks its address through a side request. `vector_sdk::net::build_http_client` routes it the same way as the bot.
- **Publishing a permission change and hoping.** The SDK checks a change the way every member's client will before it publishes, and returns `Error::NotPermitted { needs, .. }` instead of an edition everyone ignores.
- **Copying the app's Tauri commands.** They carry UI echoes, toasts and optimistic rows a bot doesn't want, and they change with the UI.

## Upgrading an older bot

What a bot written for an earlier release changes (the [CHANGELOG](../CHANGELOG.md) has each release in full):

- `BotEvent::MessageUpdate` is gone: a reaction is `Reaction`, a withdrawn one `ReactionRemoved`, an edit `Edit`.
- `BotEvent::Delete` adds `by`, `moderated` and `at`; `MemberJoin` adds `community_id`, `invited_by` and `invite_label`; `MemberLeave` adds `community_id`. Patterns that name fields need `..`.
- `Channel::send_file` compresses pictures and strips their metadata. `send_file_with(path, PostOptions::default().compress(false))` sends the file as it is.
- `update_profile`, `set_status`, `block`, `unblock` and `set_nickname` return `Result<()>`, not `bool`.
- `Community::roles()` returns the Roles themselves (`Vec<RoleInfo>`). The owner and staff are `community.owner()` and `community.admins()`.
- `bot.channel(hex)` reads a hex pubkey as a DM to that key; only a channel this bot holds reads as a Channel.
- `VectorError` is `#[non_exhaustive]`, with `NotPermitted` and `NotFound` added. Match with a `_` arm.
- `BotEvent::Delete { by }` is an `Option<String>`: `None` when the Community's rules took back a post held for review.

## Still need `bot.core()`?

Then the SDK is missing something. Open an issue saying what the bot does and which core call it needed: closing those gaps is the point of the SDK.
