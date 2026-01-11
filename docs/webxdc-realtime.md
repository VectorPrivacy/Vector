NIP-XXX
======

WebXDC Realtime Peer Channels
---------

`draft` `optional`

**IMPORTANT DRAFT NOTE:** While drafted and Vector-specific, the client implementation will use `kind 30078` (aka, Arbitrary custom app data) and `d` tag of `vector-webxdc-peer` to prevent these events being accidentally read by other Nostr clients. This allows room for discussion on future `kind` allocation, wherein the Vector implementation will be updated accordingly.

WebXDC Realtime Peer Channels enable P2P communication between WebXDC (Mini App) instances using Iroh gossip protocol. Peer discovery is coordinated via Nostr events.

## Overview

When a WebXDC app calls `joinRealtimeChannel()`, the client:
1. Generates or retrieves the Iroh topic ID for that app instance
2. Broadcasts a peer advertisement event to chat participants
3. Connects to other peers via Iroh P2P

## WebXDC File Attachment Tags

When sending a `.xdc` file, the following additional tags SHOULD be included:

| Tag | Description |
|-----|-------------|
| `webxdc-topic` | The 32-byte Iroh topic ID (base32 encoded) for realtime channels |

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
    ["webxdc-topic", "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCDEFGHIJKLMNOPQRS"]
  ]
}
```

## Peer Advertisement Event

A Peer Advertisement is a `kind 30078` event used to announce a peer's Iroh node address for a specific WebXDC topic.

### Tags

| Tag | Required | Description |
|-----|----------|-------------|
| `p` | Yes | Public key(s) of the receiver(s) |
| `d` | Yes | Must be `vector-webxdc-peer` |
| `webxdc-topic` | Yes | The Iroh topic ID (base32 encoded) |
| `webxdc-node-addr` | Yes | The Iroh node address (base32 encoded JSON) |
| `expiration` | Yes | Must be within 5 minutes |

### Event Example

```json
{
  "id": "nevent...",
  "pubkey": "<author-public-key>",
  "created_at": 1737718530,
  "kind": 30078,
  "tags": [
    ["p", "<receiver-public-key>"],
    ["d", "vector-webxdc-peer"],
    ["webxdc-topic", "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCDEFGHIJKLMNOPQRS"],
    ["webxdc-node-addr", "KRSXG5DJNZTQ..."],
    ["expiration", "1737718830"]
  ],
  "content": "peer-advertisement"
}
```

## Client Implementation

### Sending Peer Advertisements

When a WebXDC app calls `joinRealtimeChannel()`:

1. Generate or retrieve the topic ID for this app instance (stored in message metadata)
2. Get the local Iroh node address
3. Create and send a peer advertisement event to all chat participants
4. Re-send advertisements periodically (every 2-3 minutes) while the channel is active

### Receiving Peer Advertisements

When receiving a peer advertisement:

1. Verify the topic ID matches an active WebXDC instance
2. Decode the node address
3. Add the peer to the Iroh gossip swarm for that topic

### Giftwrapping (NIP-59)

Peer advertisements SHOULD be giftwrapped for privacy, similar to typing indicators:

- Use a 5-minute public expiration on the giftwrap for relay cleanup
- Send to a trusted relay for efficiency

## Security Considerations

1. **Topic Isolation**: Each WebXDC app instance has a unique topic ID, preventing cross-app communication
2. **Chat Isolation**: Only chat participants can discover peers (via encrypted Nostr events)
3. **Ephemeral**: Peer advertisements expire quickly, and Iroh connections are not persisted
4. **No IP Leakage**: Node addresses only contain relay URLs, not direct IP addresses

## Compatibility

This implementation is designed to be compatible with DeltaChat's WebXDC realtime channels:
- Uses the same Iroh gossip protocol
- Same message format (Uint8Array with sequence number and public key suffix)
- Same 128KB message size limit

Cross-platform realtime games and collaborative apps can work between Vector and DeltaChat users.