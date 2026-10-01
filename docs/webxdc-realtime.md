NIP-XXX
======

WebXDC Realtime Peer Channels
---------

`draft` `optional`

**IMPORTANT DRAFT NOTE:** While drafted and Vector-specific, the client implementation uses `kind 30078` (aka, Arbitrary custom app data) and a `d` tag of `vector-webxdc-peer` for direct messages, so other Nostr clients don't mistake these events for their own. This leaves room for a future `kind` allocation, and the Vector implementation will follow it.

WebXDC Realtime Peer Channels connect the copies of a WebXDC (Mini App) that are open at the same time in one chat, so they can exchange messages live through `joinRealtimeChannel()`. The messages travel over Iroh gossip; Nostr carries only who is playing and how to reach them.

The same flow works in NIP-17 direct messages and in Concord Communities. Only the envelope of the signals differs.

## Overview

1. The sender of a `.xdc` file mints a topic for it and puts it on the file attachment.
2. When someone opens the app, their client joins the topic on its Iroh node and advertises the node to the chat.
3. Clients already playing dial the advertised node; the newcomer dials everyone already advertised in that chat.
4. When the app closes, the client leaves the topic and announces its departure.

## WebXDC File Attachment Tags

When sending a `.xdc` file, the sender SHOULD include:

| Tag | Description |
|-----|-------------|
| `webxdc-topic` | A 32-byte Iroh topic ID, base32 (RFC 4648, no padding, 52 characters) |

Vector mints the topic once per send, as `sha256("webxdc-realtime-v1:" + file_hash + ":" + sender + ":" + time + ":" + counter)`, so every copy opened from that message shares one topic and the same app shared again gets a new one.

A file without the tag gets a derived topic: `sha256("webxdc-realtime-v1:" + app_name + ":" + chat_id + ":" + message_id)`.

### Example File Attachment Event

```json
{
  "kind": 14,
  "content": "",
  "tags": [
    ["file-type", "application/zip"],
    ["size", "12345"],
    ["encryption-algorithm", "aes-gcm"],
    ["decryption-key", "<key>"],
    ["decryption-nonce", "<nonce>"],
    ["ox", "<file-hash>"],
    ["webxdc-topic", "YFLKK3KDFMLO5GJXVLY6CIMNTMMN53BKOYAJ7LXBSW4DOMFWTCXA"]
  ]
}
```

## Peer Signals

Two signals describe a player:

- **Advertisement**: "I'm playing, reach me at this node." Sent when the app opens, and again after a neighbor drops, so a peer that comes back with a new node can find the sender.
- **Departure**: "I stopped playing." Sent when the app closes. A client that never advertised on a topic (an app opened outside any chat) sends neither.

### Node Addresses

A node address is the Iroh `EndpointAddr` serialized as JSON, then base32 encoded. It carries the node ID and relay URLs only, never a direct IP address. Receivers MUST drop any direct address in a received node address, and SHOULD dial only relays they use themselves (Vector dials iroh's default relays).

### In Direct Messages (NIP-17)

A signal is an unsigned `kind 30078` rumor, gift-wrapped (NIP-59) to the other participant like any NIP-17 message.

| Tag | Required | Description |
|-----|----------|-------------|
| `p` | Yes | The receiver's public key |
| `d` | Yes | `vector-webxdc-peer` |
| `webxdc-topic` | Yes | The topic ID |
| `webxdc-node-addr` | Advertisement only | The node address |

The `content` is `peer-advertisement` or `peer-left`.

```json
{
  "pubkey": "<author-public-key>",
  "created_at": 1737718530,
  "kind": 30078,
  "tags": [
    ["p", "<receiver-public-key>"],
    ["d", "vector-webxdc-peer"],
    ["webxdc-topic", "YFLKK3KDFMLO5GJXVLY6CIMNTMMN53BKOYAJ7LXBSW4DOMFWTCXA"],
    ["webxdc-node-addr", "PMRGSZBCHIRDINJZ..."]
  ],
  "content": "peer-advertisement"
}
```

### In Communities (Concord)

A signal is a `kind 3310` rumor in the channel where the app was shared, sealed into the channel's chat stream like any channel message, so only the channel's members can read it. Its `content` is JSON:

```json
{ "op": "ad", "topic": "YFLKK3KDFMLO5GJXVLY6CIMNTMMN53BKOYAJ7LXBSW4DOMFWTCXA", "addr": "PMRGSZBCHIRDINJZ..." }
```

```json
{ "op": "left", "topic": "YFLKK3KDFMLO5GJXVLY6CIMNTMMN53BKOYAJ7LXBSW4DOMFWTCXA" }
```

## Client Behaviour

### Who is playing

A client keeps the latest signal per (topic, author, chat). An author is playing when their latest signal there is an advertisement. On equal `created_at` a departure wins, so a client that reopens an app within the second it left MUST date its advertisement after its own departure.

Signals are persisted, so a client that opens the app later can dial everyone still playing. An advertisement can arrive before the `.xdc` message it refers to (the sender's upload may still be in flight); clients SHOULD keep it until the message lands.

`created_at` is the sender's claim: receivers SHOULD clamp a far-future value (Vector allows at most 5 minutes ahead).

### Joining

When the app calls `joinRealtimeChannel()`, the client:

1. Subscribes to the topic, bootstrapping from the nodes advertised in this chat.
2. Dials those nodes.
3. Sends its advertisement to the chat.

When a new advertisement arrives for a topic the client has joined, it dials that node and adds it to the topic.

### Who sent a message

The first author to advertise a node in a chat is that node's player. A second author advertising the same node makes it anonymous rather than moving it. A message's sender is the node key in its trailer (below), which is certain only when the message came straight from that node rather than through a relaying peer.

## Messages

Each gossip message is the app's payload followed by a trailer:

```
payload ‖ sequence number (i32, little-endian) ‖ sender's 32-byte node key
```

The payload is at most 128,000 bytes. Receivers strip the trailer and hand the payload to the app's listener. A client never delivers its own messages back to its app.

## Security Considerations

1. **Topic isolation**: every shared copy of an app has its own topic.
2. **Chat isolation**: signals are encrypted to the chat (NIP-17 gift wraps, or the Community channel's sealed stream), and a client bootstraps only from advertisements received in the same chat.
3. **Relay-only addresses**: advertised node addresses carry relay URLs only, and clients dial only relays they trust.
4. **IP exposure**: Iroh connects outside Tor and may still reveal IP addresses to connected peers. Clients SHOULD tell users before starting a session while Tor is on.

## Compatibility

Designed to be compatible with Delta Chat's WebXDC realtime channels:

- the same Iroh gossip protocol
- the same message trailer (sequence number and node key)
- the same 128 KB message size limit
