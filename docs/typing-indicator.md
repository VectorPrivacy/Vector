
NIP-XXX
======

Typing Indicators
---------

`draft` `optional`

**IMPORTANT DRAFT NOTE:** while drafted and Chatstr-specific, the client implementation will use `kind 30078` (aka, Arbitrary custom app data) and `d` tag of `vector` (our future rebrand name) in order to prevent Typing Indicator events being accidentally read by other Nostr clients, this allows room for discussion on future `kind` allocation towards Typing Indicators, wherein the Chatstr implementation will be updated accordingly.

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

The [Event Creation Implementation Code](https://github.com/JSKitty/Chatstr/blob/cb616b75c8ba49960f887d1a7cf2a052898c49a4/src-tauri/src/lib.rs#L495) that generated this example (before redacting keys).

# Client Implementation

A client SHOULD immediately display that a user is typing, if it receives a Typing Indicator Event with their Public Key, from a known user.

A client SHOULD NOT handle a Typing Indicator Event sent from an unknown user.

A client SHOULD immediately drop the Typing Indicator IF the sender has completed and sent their message. (i.e: if a received Message has a `created_at` newer than their Typing Indicator Event, then do NOT display them as typing). [Example Code](https://github.com/JSKitty/Chatstr/blob/cb616b75c8ba49960f887d1a7cf2a052898c49a4/src-tauri/src/lib.rs#L80).

A client SHOULD NOT accept Typing Indicator Events longer than 30 seconds from the current time.

The [Client Receiver Implementation Code](https://github.com/JSKitty/Chatstr/blob/cb616b75c8ba49960f887d1a7cf2a052898c49a4/src-tauri/src/lib.rs#L627), which provides an example on how to handle incoming Typing Indicator Events.

# Giftwrapping (NIP-59)

Typing Indicators were built with giftwrapping in mind, as part of the Chatstr implementation in NIP-17 DMs.

If you wish to retain maximum privacy (minimal metadata leakage), then there is no modification necessary to the original specification, just giftwrap your Typing Indicator as a rumor, and send it.

The [Event Giftwrapping Implementation Code](https://github.com/JSKitty/Chatstr/blob/cb616b75c8ba49960f887d1a7cf2a052898c49a4/src-tauri/src/lib.rs#L502) of a giftwrapped Typing Indicator.

Clients MAY add a duplicate of the `expiration` tag to the giftwrap event, therefore saving storage on Relays by allowing them to purge old Typing Indicators, but this may indicate to an outsider that the event is a Typing Indicator.

Clients MAY set a different, much longer `expiration` tag on the giftwrap event, for example, 1 week, to save Relay storage while blending-in the Typing Indicator to look like a "disappearing message".