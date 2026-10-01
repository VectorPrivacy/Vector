# Vector Agent

`vector-agent` runs a [Vector](https://vectorapp.io) account as an MCP server over stdio. Any MCP
client (Claude Code, Claude Desktop, other LLM agents) can then message people, run communities
and join Mini App sessions as that account.

## Setup

### Build

```sh
cd crates
cargo build --release -p vector-agent
```

The binary is `crates/target/release/vector-agent` (`vector-agent.exe` on Windows). There are no
cargo features to choose: the Mini App tools are always built in.

### Environment

| Variable | Meaning |
| --- | --- |
| `VECTOR_NSEC` | Required. The account's private key (`nsec1...`). A seed phrase also works. |
| `VECTOR_DATA_DIR` | Where the agent keeps its accounts, history and keys. Defaults below. |
| `VECTOR_PASSWORD` | The PIN or password of an account with encryption turned on, such as a data dir copied from the Vector app. A fresh data dir doesn't need it. |
| `VECTOR_LOG` | Log level: `trace`, `debug`, `info`, `warn`, `error` or `off`. Default `warn`. Logs go to stderr. |

Default data dir:

| OS | Path |
| --- | --- |
| macOS | `~/Library/Application Support/io.vectorapp/agent` |
| Linux | `$XDG_DATA_HOME/io.vectorapp/agent`, else `~/.local/share/io.vectorapp/agent` |
| Windows | `%APPDATA%\io.vectorapp\agent` |

Each account gets its own folder inside, named after its npub.

### Keep the key private

The nsec is the account's private key. Anyone who has it can read the account's messages and
post as it.

- Give the agent its own account (create a new one in Vector and copy its nsec) and keep your
  personal account out of it.
- Keep the nsec out of shared config: no committed `.mcp.json`, no shared dotfiles.
- The data dir stores the key as well, so keep it private too.
- `send_file` can send any file the agent's process can read.

### Claude Code

```sh
claude mcp add vector -e VECTOR_NSEC=nsec1... -- /absolute/path/to/vector-agent
```

Add more `-e` flags for the other variables. This saves the server to your own Claude Code config.
Avoid `--scope project`, which writes the key into the repository's `.mcp.json`. To keep the key out
of your shell history, read it from a file: `-e VECTOR_NSEC="$(cat ~/.vector-agent.nsec)"`.

`claude mcp get vector` checks that it starts.

### Claude Desktop

Add the server to `claude_desktop_config.json` (in `~/Library/Application Support/Claude/` on macOS,
`%APPDATA%\Claude\` on Windows), then restart Claude Desktop:

```json
{
  "mcpServers": {
    "vector": {
      "command": "/absolute/path/to/vector-agent",
      "env": {
        "VECTOR_NSEC": "nsec1..."
      }
    }
  }
}
```

### Other MCP clients

Run the binary as a stdio server, with no arguments and the environment above:

```sh
VECTOR_NSEC=nsec1... /absolute/path/to/vector-agent
```

Most clients take the same `command` and `env` fields as Claude Desktop. On start the agent logs
in, connects to relays and prints `MCP server ready (stdio)` to stderr.

## Tools

### Accounts

| Tool | What it does | Arguments |
| --- | --- | --- |
| `my_npub` | This account's npub (public key). | |
| `current_account` | The active account's npub. | |
| `list_accounts` | Accounts stored in the data dir, with the active one flagged. | |
| `add_account` | Add an account and switch to it. Omit `nsec` to create a new identity, which keeps the key out of the conversation. | `nsec` (optional) |
| `swap_account` | Switch to an account stored in the data dir. No key is passed. | `npub` |

The agent always starts on the `VECTOR_NSEC` account. Switching accounts leaves any Mini App
sessions and empties the `get_new_messages` queue.

### Messages and DMs

| Tool | What it does | Arguments |
| --- | --- | --- |
| `get_new_messages` | Messages and files received since the last call, from DMs and Community channels. Returns them and empties the queue. | |
| `list_chats` | Every chat with its latest message. | |
| `get_messages` | A chat's most recent messages, in chronological order. | `chat_id` (an npub for a DM, a `channel_id` for a community channel), `limit` (default 50), `offset` (how many of the newest to skip, to page back; default 0) |
| `send_dm` | Send an encrypted DM. | `to_npub`, `content` |
| `send_file` | Send a file as an encrypted DM attachment. | `to_npub`, `file_path` (absolute, on the agent's machine) |
| `sync_dms` | Fetch DMs from relays that this account hasn't seen yet. | `since_days` (optional, omit for the full history) |

### Profiles

| Tool | What it does | Arguments |
| --- | --- | --- |
| `get_profile` | A user's profile from the local cache. | `npub` |
| `load_profile` | Fetch a user's profile from relays and cache it. | `npub` |
| `update_profile` | Publish this account's profile. Empty fields keep their current value. | `name`, `avatar` (URL), `banner` (URL), `about` |
| `set_nickname` | Give a user a nickname that only this account sees. | `npub`, `nickname` |
| `block_user` | Block a user. | `npub` |
| `unblock_user` | Unblock a user. | `npub` |
| `get_blocked_users` | Profiles of every blocked user. | |

### Communities: joining and posting

| Tool | What it does | Arguments |
| --- | --- | --- |
| `list_communities` | Communities this account owns or joined, each with its channels (`channel_id`, `name`, `private`, `readable`). `dissolved: true` means the community is closed for good. | |
| `join_community` | Join from an invite link. | `invite_url` |
| `list_pending_invites` | Invites sent to this account by DM, waiting to be accepted. | |
| `accept_pending_invite` | Accept a pending invite and join. | `community_id` |
| `sync_community_channel` | Fetch a channel's latest messages from relays. Read them with `get_messages`. | `channel_id`, `limit` (default 20) |
| `send_community_message` | Post in a channel. Returns the message id. | `channel_id`, `content`, `replied_to` (optional message id) |
| `leave_community` | Leave, and remove its channels from this account. Rejoining needs a new invite. | `community_id` |

### Communities: creating and settings

| Tool | What it does | Arguments |
| --- | --- | --- |
| `create_community` | Create a community owned by this account, with one `general` channel. Returns its ids. | `name` |
| `edit_community_metadata` | Change the name or description. An omitted field stays as it is; an empty `description` clears it. | `community_id`, `name` (optional), `description` (optional) |
| `delete_community` | Owner only, and permanent: close the community so nobody can post or change anything again. Past messages stay. | `community_id` |

### Communities: channels

| Tool | What it does | Arguments |
| --- | --- | --- |
| `create_channel` | Add a channel. Only members granted access can read a private one. Returns the channel id. | `community_id`, `name`, `private` (default false) |
| `rename_channel` | Rename a channel. Its id and history stay. | `community_id`, `channel_id`, `name` |
| `delete_channel` | Delete a channel permanently. | `community_id`, `channel_id` |
| `grant_channel_access` | Let a member read a private channel. They get its key on their next sync. | `community_id`, `channel_id`, `npub` |
| `revoke_channel_access` | Take a member out of a private channel and change its key, so they can't read anything new. | `community_id`, `channel_id`, `npub` |
| `channel_access` | Who can read a private channel, and whether this account holds its key yet (`readable`). | `community_id`, `channel_id` |

### Communities: invites

| Tool | What it does | Arguments |
| --- | --- | --- |
| `create_public_invite` | Create an invite link anyone can use. Returns the URL. | `community_id` |
| `list_public_invites` | This account's invite links, with each one's `token`, URL and expiry. | `community_id` |
| `revoke_public_invite` | Revoke an invite link. Revoking the last one makes the community private and changes its keys. | `community_id`, `token` |
| `send_private_invite` | Invite one person by DM. They accept it in their own client. | `community_id`, `npub` |

### Communities: members and moderation

| Tool | What it does | Arguments |
| --- | --- | --- |
| `get_community_members` | Members seen posting or joining, minus anyone who left or was banned, with `last_active`. | `community_id` |
| `get_community_roles` | The owner and the admins. | `community_id` |
| `get_community_capabilities` | What this account may do here (kick, ban, manage roles, edit metadata and so on). | `community_id` |
| `grant_community_admin` | Make a member an admin. | `community_id`, `npub` |
| `revoke_community_admin` | Take away a member's admin role. | `community_id`, `npub` |
| `kick_community_member` | Remove a member. They can come back with a new invite. | `community_id`, `npub` |
| `ban_community_member` | Remove a member for good. In a private community this also changes its keys. | `community_id`, `npub` |
| `unban_community_member` | Lift a ban so they can rejoin. | `community_id`, `npub` |

Moderation needs the matching permission and a higher rank than the target.

### Communities: sync and repair

| Tool | What it does | Arguments |
| --- | --- | --- |
| `sync_communities` | Refresh every community from relays and pick up key changes this account missed. | |
| `explain_epoch_state` | Explain whether this account is behind on a community's keys, and why. Read-only. | `community_id` |
| `repair_community_roster` | Let a stuck member and role list be replaced by the community's current one. Publishes nothing. | `community_id` |

### Mini Apps

| Tool | What it does | Arguments |
| --- | --- | --- |
| `xdc_apps` | Mini Apps shared in a chat, newest first, with `message_id`, `file`, `topic`, `from_me`, `joined` and `playing` (who's in it now). | `chat_id`, `scan` (messages to look through, default 200) |
| `xdc_join` | Join an app's live session. Returns its `topic`. | `message_id` |
| `xdc_read` | Return and clear what arrived since the last read, up to 100 events at a time. | `topic`, `wait_ms` (wait this long for the next event, max 30000) |
| `xdc_send` | Send a frame to everyone in the session, up to 128000 bytes. | `topic`, `text` |
| `xdc_leave` | Leave the session and tell the chat. | `topic` |

## Direct messages

DMs are end-to-end encrypted NIP-17 messages. A DM chat's `chat_id` is the other person's npub.

To answer new messages:

1. `get_new_messages` returns everything that arrived since the last call. Each entry has the
   `chat_id`, `is_group` (false for a DM), the message `id`, `content`, `attachments` and `mine`
   (true for this account's own).
2. `get_messages` with that `chat_id` gives the conversation so far.
3. `send_dm` with `to_npub` set to the `chat_id` sends the reply.

The queue starts filling when the agent starts. Messages that came in while it was off are synced
into the history on start: `list_chats` shows where there's new activity and `get_messages` reads
it. `sync_dms` fetches from relays again on demand.

Received files show up in `attachments` with their name and size; the agent has no tool to open
them. DMs the agent sends are not copied to the account's other devices.

## Communities

Communities are group spaces with channels, roles and moderation, built on Vector's Concord
protocol. A channel's `channel_id` works anywhere a tool takes a `chat_id`.

To join and talk:

1. `join_community` with an invite link. For an invite someone sent by DM, use
   `list_pending_invites` and then `accept_pending_invite`.
2. `list_communities` gives the `community_id` and each channel's `channel_id`.
3. `sync_community_channel` fetches a channel's latest messages, and `get_messages` with the
   `channel_id` reads them.
4. `send_community_message` posts. Set `replied_to` to a message id to reply to it.

`get_new_messages` includes new channel messages, marked `is_group: true`, with the `chat_id` set
to the channel's id. Messages from before the agent started come in with `sync_community_channel`.

To run one:

- `create_community` makes a community with a `general` channel. `create_channel` adds more; with
  `private: true` the channel is readable only by members you add with `grant_channel_access`.
- `create_public_invite` makes a link anyone can use. `send_private_invite` invites one person.
- `get_community_capabilities` shows what this account is allowed to do. Run
  `sync_community_channel` first so roles are current.
- `kick_community_member` removes someone who can return with a new invite.
  `ban_community_member` keeps them out.

If a community looks stale (a channel stops updating, or the member list differs from another
client), run `sync_communities`. `explain_epoch_state` reports whether this account is behind and
why.

## Mini Apps

Mini Apps are small games and tools (`.xdc` files) shared in a chat. The people who open one share
a live session, and the app talks to the other players by sending frames. The agent can join that
session as a player.

Joining a tic-tac-toe game Alice shared in a DM, as tool calls in order:

1. Find the app.

   ```
   xdc_apps { "chat_id": "npub1alice..." }
   → [{ "message_id": "9f2c...", "file": "tictactoe.xdc", "topic": "MZXW6...",
        "from_me": false, "joined": false, "playing": ["npub1alice..."] }]
   ```

2. Join its session with the message id.

   ```
   xdc_join { "message_id": "9f2c..." }
   → { "topic": "MZXW6...", "chat_id": "npub1alice...", "self_addr": "npub1agent..." }
   ```

3. Read what the other players send. `wait_ms` waits for the next event when nothing is waiting.

   ```
   xdc_read { "topic": "MZXW6...", "wait_ms": 10000 }
   → [{ "type": "joined", "npub": "npub1alice..." },
      { "type": "data", "from": "npub1alice...", "verified": true, "bytes": null,
        "text": "{\"move\":4}" }]
   ```

4. Reply in the app's format.

   ```
   xdc_send { "topic": "MZXW6...", "text": "{\"move\":0}" }
   ```

5. Leave when done.

   ```
   xdc_leave { "topic": "MZXW6..." }
   ```

Frames are the app's own protocol, usually JSON (the moves above are made up). The agent learns
it by reading frames, or from the app's source code. Apps that use `selfAddr` see the agent's
npub.

- `from` is the sender's npub, or null when unknown. `verified: true` means that npub is
  confirmed; on other frames, treat `from` as the sender's claim.
- Binary frames show only their size, in `bytes`. Text longer than 8192 characters is cut.
- Up to 500 events wait between reads. A `lagged` event means some frames were dropped.
- Frames reach only the players in the session at that moment.
- A session with no `xdc_read` or `xdc_send` for 15 minutes is left automatically.

Joining a session shows this machine's IP address to the other players in it.

## Example prompts

- "Check my Vector DMs and summarize what's new."
- "Reply to npub1... on Vector and tell them the release is out."
- "Join the community at https://vectorapp.io/invite/... and say hello in general."
- "Create a community called Book Club with a private channel for organizers, and invite npub1..."
- "Show me who has been active in my community this week."
- "Join the tic-tac-toe game Alice shared and play it."

## Troubleshooting

- Run the agent by hand to see what it does:
  `VECTOR_NSEC=nsec1... /absolute/path/to/vector-agent`. It prints `Logged in as npub1...` and then
  `MCP server ready (stdio)` to stderr. Press Ctrl-C to stop it.
- Logs go to stderr. `VECTOR_LOG=info` (or `debug`, `trace`) adds detail in a debug build
  (`cargo build -p vector-agent`); a release build prints warnings and errors only. `RUST_LOG=info` shows
  logs from the MCP and relay libraries.
- Inside a client: `claude --debug` shows the server's stderr in Claude Code. Claude Desktop writes
  it to `~/Library/Logs/Claude/mcp-server-vector.log` on macOS and `%APPDATA%\Claude\logs\` on
  Windows.
- A fresh data dir is fine. The agent creates it and syncs DMs and communities from relays on
  start, so the first calls may see history still arriving. An account `add_account` created
  without an `nsec` exists only in its data dir, so back the folder up before deleting it.
- Run one process per data dir. Two agents, or the agent and the Vector app, sharing a data dir
  will conflict. Give each agent its own `VECTOR_DATA_DIR`.
