
NIP-XXX
======

Typing Indicators
---------

`draft` `optional`

**IMPORTANT DRAFT NOTE:** while drafted and Vector-specific, the client implementation will use `kind 30078` (aka, Arbitrary custom app data) and `d` tag of `vector` in order to prevent Typing Indicator events being accidentally read by other Nostr clients, this allows room for discussion on future `kind` allocation towards Typing Indicators, wherein the Vector implementation will be updated accordingly.

A Typing Indicator is a `kind 30078` event, paired with the Public Key of the receiver(s), and a short expiry, that is used to indicate the Sender is currently typing a message intended for them.

A Typing Indicator event is composed of a `content` of "`typing`", and an `expiration` tag up to 30-seconds in lifetime.

# Tags

The `p` tag MUST have at least one Public Key, and MAY have several `p` tags denoting multiple receivers (i.e: Group Chats, send-to-many draftings, etc).

The `expiration` tag MUST be set within the next 30 seconds, it CANNOT be higher than 30 seconds, if a user continues typing for longer than 30 seconds, new Typing Indicator events should be sent, once - or slightly before - the previous indicator expires.

# Event Example

**Note:** The arbitrary app-name `d` tag and `kind` are temporary, until a community agreed `kind` is assigned.

```json
{
  "id": "nevent...",
  "pubkey": "<author-public-key>",
  "created_at": 1737718530,
  "kind": 30078,
  "tags": [
    [ "p", "<receiver-public-key>" ],
    [ "d", "vector" ],
    [ "expiration", "1737718560" ]
  ],
  "content": "typing"
}
```

The [Event Creation Implementation Code](https://github.com/VectorPrivacy/Vector/blob/cb616b75c8ba49960f887d1a7cf2a052898c49a4/src-tauri/src/lib.rs#L495) that generated this example (before redacting keys).

# Client Implementation

A client SHOULD immediately display that a user is typing, if it receives a Typing Indicator Event with their Public Key, from a known user.

A client SHOULD NOT handle a Typing Indicator Event sent from an unknown user.

A client SHOULD immediately drop the Typing Indicator IF the sender has completed and sent their message. (i.e: if a received Message has a `created_at` newer than their Typing Indicator Event, then do NOT display them as typing). [Example Code](https://github.com/VectorPrivacy/Vector/blob/cb616b75c8ba49960f887d1a7cf2a052898c49a4/src-tauri/src/lib.rs#L80).

A client SHOULD NOT accept Typing Indicator Events longer than 30 seconds from the current time.

The [Client Receiver Implementation Code](https://github.com/VectorPrivacy/Vector/blob/cb616b75c8ba49960f887d1a7cf2a052898c49a4/src-tauri/src/lib.rs#L627), which provides an example on how to handle incoming Typing Indicator Events.

# Giftwrapping (NIP-59)

Typing Indicators were built with giftwrapping in mind, as part of the Vector implementation in NIP-17 DMs: any configuration you use will NOT affect clients that have properly implemented NIP-59 and Typing Indicator Events, however, you CAN configure Typing Indicator Events for improved Relay and network efficiency, some of which are documented below.

Since giftwrapping hides the `expiration` timestamp from relays, they have no way to automatically purge Typing Indicator Events, leading to a large buildup of 'past indicators' in relays, slowing down your client giftwrap sync and cluttering relays. This can be fully avoided by publishing an extended `expiration` timestamp on the giftwrap event itself, which is the Vector implementation.

A sufficiently long public expiration timestamp makes it more difficult to know if the event is a "disappearing message" or a Typing Indicator.

If your client supports disappearing messages, and you are actively using them, then setting your Typing Indicator's expiration to match their timestamp will make them further indistinguishable.

For additional privacy, you may select a singular "Trusted Relay" to handle Typing Indicator Events, an approach also taken by the Vector client.

The [Event Giftwrapping Implementation Code](https://github.com/VectorPrivacy/Vector/blob/f2fa50543c740a7054b04fa5d341ca14ed8b7a13/src-tauri/src/lib.rs#L508) of a giftwrapped Typing Indicator with extended public `expiration` timestamp.
