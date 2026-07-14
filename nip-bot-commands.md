NIP-XX
======

Bot Commands and Manifests
--------------------------

`draft` `optional`

This NIP defines a transport-agnostic convention for **bot commands**: a way for automated accounts to publish a machine-readable catalog of the slash-commands they answer, and for any client to discover, render, validate, and invoke those commands.

It adds exactly one event kind, a replaceable **command manifest** (`kind:10304`), and one optional routing tag (`bot`). Command *invocations* introduce no new event kind: an invocation is an ordinary message whose text content is the command itself (`/price btc`), so every existing Nostr messaging client is already a capable command sender, and commands work over any conversation transport without change.

## Motivation

Bots are common on Nostr, but there is no shared way for a bot to tell a client *what it can do*. Each client hard-codes the bots it understands, or users memorize magic strings. There is no discovery, no argument hints, no client-side validation, and no interoperability: a command that works in one app is invisible in another.

This NIP fixes discovery and rendering while deliberately keeping invocation trivial. Two properties drive the design:

1. **Transport-agnostic.** The manifest and the invocation are *content-level* constructs. They carry no assumption about the envelope, so the same commands work in NIP-17 private messages, in encrypted group channels, or in plain public notes. The transport is whatever the conversation already uses.

2. **Zero new structure for invocations.** An invocation is just the text a human would type. This means any client, including ones that have never heard of this NIP, can send a working command simply by relaying the user's text. Rich clients layer discovery and validation on top; they do not gate participation.

## Terminology

- **Bot**: an account (keypair) that publishes a command manifest and answers invocations. A bot MUST advertise itself by setting `"bot": true` in its `kind:0` metadata (the `bot` field defined in [NIP-24](https://github.com/nostr-protocol/nips/blob/master/24.md)).
- **Manifest**: the replaceable `kind:10304` event through which a bot declares its commands.
- **Command**: one named operation a bot answers, with a typed argument list.
- **Invocation**: a message whose content is `/command args...`, addressed at one or more bots.

The key words "MUST", "MUST NOT", "REQUIRED", "SHOULD", "SHOULD NOT", "MAY", and "OPTIONAL" in this document are to be interpreted as described in RFC 2119.

## The Command Manifest (`kind:10304`)

A manifest is a regular (unwrapped, unencrypted) replaceable event signed by the bot's key.

### Identity and replacement

The manifest is a replaceable event (`kind` in the `10000`–`19999` range per NIP-01). There is exactly one manifest per bot pubkey, identified by `(kind, pubkey)`. It carries no `d` tag.

Republishing replaces it under normal replaceable-event semantics: for a given `(kind, pubkey)` a relay keeps only the newest by `created_at`, and a client that receives more than one MUST use the newest. A bot updates its interface by publishing a new manifest.

Modeling this as replaceable is deliberate. A bot's public command catalog is exactly one authoritative document per identity, the same shape as a profile (`kind:0`) or a relay list (`kind:10002`), not a collection of many documents keyed by a `d` tag. Per-context command sets (different commands inside a particular channel or chat) are intentionally out of scope; see [Out of scope](#out-of-scope-future-work).

### Content

`content` is a JSON object:

```jsonc
{
  "v": 1,                 // manifest schema version, MUST be 1
  "commands": [ /* Command objects */ ]
}
```

A **Command** object:

```jsonc
{
  "name": "price",              // lowercase slug, the command word after "/"
  "description": "Get a coin price",
  "args": [ /* Argument objects, in positional order */ ]
}
```

An **Argument** object:

```jsonc
{
  "name": "asset",              // lowercase slug
  "type": "choice",             // one of the argument types below
  "description": "Which coin",  // MAY be empty
  "required": true,             // default false
  "choices": ["btc", "xmr", "pivx"]  // present only for type "choice"
}
```

`commands` and `args` MAY be omitted when empty. `description` and `required` are always present in a conforming producer's output (empty string and `false` respectively when unset). Consumers MUST tolerate their absence and apply those defaults. Unknown object fields MUST be ignored on read, so future minor additions stay backward-compatible without a version bump.

### Argument types

The `type` field is one of:

| type     | wire form                                   | notes |
|----------|---------------------------------------------|-------|
| `string` | free text                                   | as the LAST argument, greedily takes the remainder of the invocation (see Invocation) |
| `int`    | signed 64-bit integer                       | |
| `number` | double-precision float                      | |
| `bool`   | `true`/`false`                              | consumers SHOULD also accept `yes`/`no`/`1`/`0` from typed text |
| `user`   | an `npub1…` reference                       | parsers MUST also accept the NIP-21 `nostr:npub1…` URI form and normalize it to the bare npub |
| `choice` | one member of the `choices` set             | `choices` is REQUIRED and non-empty for this type |

### Validation and limits

A manifest is untrusted input. A consumer MUST validate a fetched manifest before use and MUST ignore one that fails. Producers MUST NOT emit an invalid manifest. The rules:

- `v` MUST equal `1`.
- Command and argument `name`s MUST match `^[a-z0-9_-]{1,32}$`.
- Command names MUST be unique within a manifest; argument names MUST be unique within a command.
- At most **64** commands per manifest.
- At most **8** arguments per command.
- `description` (of a command or argument) at most **200** bytes.
- For `type: "choice"`: **1** to **32** choices, each **1** to **32** bytes; no other type may carry `choices`.
- The full serialized manifest MUST be at most **32768** bytes.

### The positional-parse contract

Because an invocation is plain text parsed positionally (see below), an argument list MUST obey two ordering rules so that parsing is deterministic:

1. **Required before optional.** All `required` arguments MUST precede any optional argument. An optional "hole" before a required argument would make a positional invocation ambiguous.
2. **Greedy tail is last only.** Only the final declared argument may act as a greedy `string` that absorbs a multi-word remainder. A producer SHOULD therefore place a free-text `string` argument last when it is meant to capture a phrase.

### Forward compatibility

`v` gates breaking schema changes; a consumer MUST reject a manifest whose `v` it does not implement. Additive changes (new optional fields, new argument types a consumer does not recognize) SHOULD instead be handled gracefully: an unknown field is ignored, and a command a client cannot fully render MAY be hidden while leaving the rest usable.

## Discovery

### Identifying which participants are bots

A client MUST determine which participants are bots from their `kind:0` metadata: an account is a bot when its metadata carries `"bot": true` (the `bot` field defined in [NIP-24](https://github.com/nostr-protocol/nips/blob/master/24.md)).

That flag is the shortlist of pubkeys whose manifests are worth fetching, and it is what keeps discovery cheap: in a 500-member room a client fetches manifests for the handful of flagged bots, not for everyone. A client MAY additionally fetch a manifest for any pubkey it likes (one a user names explicitly, say). The absence of a manifest is itself a complete answer.

### Fetching manifests

A client fetches `kind:10304` events authored by the bot pubkey(s).

- A client SHOULD query both the conversation's own relays and one or more widely-indexed public relays, because a conversation relay may not carry a bot's manifest (some relays drop events from non-members). Querying indexers in parallel makes the manifest resolvable regardless.
- For each author, the newest valid event wins. Events from authors the client did not ask for, or that fail signature verification or validation, MUST be discarded.
- Discovery results SHOULD be cached and refreshed on a bounded interval; the set of participating bots changing (a bot joining or leaving a conversation) SHOULD be treated as immediately stale.

A bot with no valid manifest simply has no discoverable interface; it MAY still answer invocations sent to it (see Bot behavior).

## Invocation

An invocation carries no dedicated event kind. It is an ordinary message in whatever conversation transport is in use (a NIP-17 rumor, a group-channel message, a public note, etc.), whose `content` is the command text.

### Grammar

```
content := "/" name ( SP arg )*
```

`name` is the command word. Everything after it is the positional argument list, matched against the bot's manifest in declared order. If `content` does not begin with `/` followed by a command the target bot declares, it is ordinary chat, not an invocation.

**The command word is matched case-insensitively.** Manifest names are guaranteed lowercase slugs, so a parser MUST fold the invocation's command word to lowercase before lookup: `/Help` and `/HELP` both resolve to `help`. Only the command word folds. Argument **values** keep their case, and a `choice` value is matched exactly.

### Tokenization

Arguments are whitespace-separated tokens with shell-style quoting:

- A token MAY be a `"quoted span"` that contains spaces.
- Inside a quoted span, `\"` is a literal quote and `\\` is a literal backslash.
- An unterminated quote makes the text malformed; it MUST NOT be treated as an invocation.
- **Greedy tail:** when the final declared argument has type `string` and the next input character is not a quote, that argument takes the raw remainder of the line verbatim (internal spacing preserved). This is what lets `/say hello there` pass a single value `hello there` with no quoting, while two multi-word values remain expressible with quotes: `/announce "Big news" "Meeting at 5pm"`.

A single argument value on the wire longer than **1024** bytes MUST cause the text to be rejected as an invocation.

A client that built the invocation from a picker SHOULD produce canonical text: values containing whitespace or a quote are wrapped in `"…"` with `\"`/`\\` escapes, so the text re-parses to exactly the arguments the user chose.

### Addressing: the `bot` tag

Two bots in a conversation MAY declare a command of the same name. Which bot should act is the one piece of an invocation not derivable from its content, so it rides a tag:

```json
["bot", "<bot-pubkey-hex>"]
```

- Up to **8** `bot` tags MAY appear on one message.
- **Tagged:** only the named bot(s) SHOULD act; a bot not named SHOULD ignore the invocation even if its manifest matches.
- **Untagged:** the invocation is a broadcast; any bot whose manifest matches MAY answer.

Clients SHOULD attach a `bot` tag when a user picks a specific bot's command. Bots MUST NOT *require* the tag, because plain clients will never send it.

This NIP deliberately does not reuse `p` for addressing. Conversation transports already use `p` for message recipients and reply parents; a "skip unless a `p` names me" rule would cause a bot to swallow a command a user sent as a reply to a human.

### Authorization

The `bot` tag is routing, not authorization. A bot MUST decide whether to honor a command based on the message **sender** (and its own policy), never on the presence or content of a `bot` tag. Anyone can address a bot; addressing is not permission.

## Response

A bot's response is an ordinary message. This NIP defines no response event kind.

A response SHOULD be a **threaded reply to the invoking message**, so a client can correlate a result with the command that produced it and render them together. This is a recommendation, not a requirement: a bot MAY simply post a normal message.

### Errors

When an invocation matches a declared command NAME but fails validation (a value of the wrong type, a `choice` outside its set, a missing required argument), the bot SHOULD reply with an error rather than stay silent. Silence is indistinguishable from a broken bot. A bot that replies with an error MUST NOT execute the command.

Error text is **canonical**, so that errors are byte-identical across implementations and a client can parse them rather than merely display them:

```
<arg>: <reason>
usage: <usage-line>
```

The first line is always `{argument-name}: {reason}`. A client recovers which argument failed, and why, by splitting on the first `": "`. The reasons are exactly:

| reason | when |
|---|---|
| `not an integer` | an `int` value did not parse |
| `not a number` | a `number` value did not parse |
| `not a boolean` | a `bool` value was none of `true`/`false`/`yes`/`no`/`1`/`0` |
| `not an npub` | a `user` value was not a valid npub (bare or `nostr:`-prefixed) |
| `not one of a, b, c` | a `choice` value was not a declared choice (the set, joined with `, `, in manifest order) |
| `required` | a required argument was absent |

The second line is `usage: ` followed by the command's **usage line**: `/name`, then each argument in declared order as `<name:type>` when required and `[name:type]` when optional, where the types render as `text`, `int`, `number`, `true|false`, `npub`, `choice`.

So `/price doge`, against a `price` command whose required `asset` is a choice of `btc`/`xmr`/`pivx`, yields exactly:

```
asset: not one of btc, xmr, pivx
usage: /price <asset:choice>
```

## Client behavior

A conforming rich client SHOULD:

- Resolve manifests for participating bots (Discovery) and render an affordance (for example a `/` menu) listing their commands with descriptions.
- Use argument specs for input: prompt per argument, offer a picker for `choice`, offer a user picker for `user`, and validate types, required-ness, and choice membership *before* sending.
- Emit the invocation as canonical text and attach a `bot` tag naming the chosen bot.

A client that does none of this is still conformant as a *sender*: a user typing `/price btc` by hand produces a valid invocation.

## Bot behavior

A conforming bot SHOULD:

- Publish its manifest as `kind:10304` to relays where its users will look for it (its own relays, and public indexers). It republishes to change its interface.
- On each incoming message, attempt to parse `content` against its manifest, type-check the arguments, and, if valid, authorize by sender and act. Non-matching content MUST be ignored silently (it is ordinary conversation).
- Honor addressing: if `bot` tags are present and do not name it, do nothing.

Argument typing at the bot: `int`/`number`/`bool` parse per their type (`bool` accepting `true|false|yes|no|1|0`, case-insensitive), `choice` MUST be a declared member, and `user` MUST be a valid `npub1…` accepted either bare (the canonical form a picker emits) or as the NIP-21 `nostr:npub1…` URI that clients commonly insert for a mention, normalized to the bare npub. An argument the manifest does not declare MUST be dropped rather than rejected, so a newer sending client naming a newer argument does not break an older bot.

## Security considerations

- **Manifests are untrusted.** Validate structure and enforce every limit before rendering or storing. A manifest that fails validation has no usable interface and MUST be ignored. The size and count bounds exist to cap the memory and render cost a hostile manifest can impose.
- **Addressing is not authorization.** A `bot` tag names a routing target; it grants nothing. Bots authorize by sender pubkey and their own policy.
- **The `bot` tag MUST ride inside the encryption envelope.** An invocation lives in whatever transport carries it, and in an encrypted one (NIP-17, an encrypted group) the tag belongs on the same inner, encrypted event as the command text, NEVER on an outer wrap or seal. Hoisting it outside would publish "this pubkey is commanding that bot" to every relay storing the wrap, leaking precisely the metadata the transport exists to hide.
- **Replaceable interface.** A bot can rotate its commands by republishing; clients SHOULD re-validate on refresh and MUST NOT assume a cached manifest is still current.
- **Argument confidentiality follows the transport.** An invocation is plaintext inside whatever envelope carries it. Over NIP-17 it inherits that end-to-end encryption; in an encrypted group it inherits the group's; in a public note it is public. Bots and clients SHOULD treat sensitive arguments (and command results) according to the transport's confidentiality, and SHOULD NOT invite secrets over a public transport.

## Examples

### A manifest

A bot that answers `/price <asset>` and `/say <count> [text]`. The event:

```jsonc
{
  "kind": 10304,
  "pubkey": "<bot-pubkey-hex>",
  "created_at": 1710000000,
  "tags": [],
  "content": "{...the JSON below, serialized...}",
  "id": "…",
  "sig": "…"
}
```

with `content` (shown expanded) being:

```json
{
  "v": 1,
  "commands": [
    {
      "name": "price",
      "description": "Get a coin price",
      "args": [
        {
          "name": "asset",
          "type": "choice",
          "description": "Which coin",
          "required": true,
          "choices": ["btc", "xmr", "pivx"]
        }
      ]
    },
    {
      "name": "say",
      "description": "Echo",
      "args": [
        { "name": "count", "type": "int", "description": "", "required": true },
        { "name": "text", "type": "string", "description": "", "required": false }
      ]
    }
  ]
}
```

### Invocations

Given that manifest, all of the following are valid message `content`:

| content | parses as |
|---------|-----------|
| `/price btc` | `price` with `asset = "btc"` (validated against the choice set) |
| `/say 3 hello there` | `say` with `count = 3`, `text = "hello there"` (greedy trailing `string`) |
| `/say 1 "quoted, with spaces"` | `say` with `count = 1`, `text = "quoted, with spaces"` |

`/price doge` is rejected by a validating client and bot: `doge` is not in the choice set. `/help` is not a command this bot declares, so it is ordinary chat.

An invocation addressed at a specific bot (for example when another bot in the room also declares `price`) carries a routing tag alongside whatever the transport already adds:

```json
"tags": [
  ["bot", "<bot-pubkey-hex>"]
]
```

## Out of scope (future work)

This NIP covers **public** command manifests: commands a bot intends to be openly discoverable and callable. It intentionally does not cover **contextual** command sets, where a bot exposes different (or additional) commands only inside a specific channel, group, or conversation, visible only to that context's members.

A public replaceable event is the wrong vehicle for that case: a single authoritative document per bot cannot hold per-context variants without leaking their existence and shape to the whole network. The natural home is a future companion extension in which a manifest is delivered *inside the context's own encrypted envelope* (a "wrapped manifest"), so both the commands and the fact that they exist inherit that context's confidentiality. Defining that mechanism, including how a client reconciles a context manifest with the bot's public one, is left to a later NIP.

## Implementations

Two implementations interoperate. They share no code and are written in different languages, which is the evidence behind the transport-agnostic and client-agnostic claims made above.

**[Vector](https://vectorapp.io)** (Rust) is the reference implementation, covering both ends: the wire layer in `vector-core` (`bot_interface.rs`), a bot SDK that generates and publishes a manifest from a bot's declared command handlers and dispatches incoming invocations back to them, and a client `/` command picker. Commands run unchanged over NIP-17 direct messages and over encrypted group channels.

**[Armada](https://gitlab.com/soapbox-pub/armada)** (TypeScript) is an independent client implementation: manifest validation, bot discovery from `kind:0`, a `/` picker with guided per-argument entry, local type-checking, and invocation over an encrypted group transport and a public one.

A bot built on Vector's SDK is discovered, rendered, argument-checked and invoked by Armada with no shared code between the two, over a transport neither one special-cases, and answers. That is the property this NIP exists to make possible.

Armada deliberately offers no bot commands over NIP-04 direct messages, whose tags are not encrypted. See [Security considerations](#security-considerations).

## Appendix: provisional kind number

`kind:10304` is proposed here and is provisional: it is not yet reserved in the NIP-01 event-kind registry, and the final assignment (in the replaceable range, `10000`–`19999`) is subject to coordination during standardization. This document will track whatever number is allocated.

The initial reference deployment used an addressable `kind:33304` before this document settled on replaceable semantics; it migrates to the replaceable kind as part of adopting this NIP.
