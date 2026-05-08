> [!NOTE]
> This is a generic documentation, [read {{lang}}-specific docs](docs.md).

# MDK Bindings for {{lang}}

Language bindings for the *Marmot Development Kit* - bringing decentralized, encrypted group messaging to your favorite language.

## What is MDK?

MDK combines [MLS (Messaging Layer Security) Protocol](https://www.rfc-editor.org/rfc/rfc9420.html) (the gold standard for group crypto) with [Nostr](https://github.com/nostr-protocol/nostr).

You get real end-to-end encryption using MLS with forward secrecy and post-compromise security. Since it's built on Nostr's distributed relay network, there's no server needed. The group functionality actually works with proper secure member management and encrypted everything. Keys rotate automatically so you can't mess anything up, and it even protects your metadata so your chatter patterns stay private.

## How It Actually Works

We use [UniFFI](https://mozilla.github.io/uniffi-rs/) to bridge the Rust MDK core to your language without the usual FFI headaches. Everything persists in a SQLite database file you specify when you create an MDK instance, holding all your groups, messages, keys, and state. The instance is internally mutexed for thread safety, but if you can avoid it don't share MDK instances across threads.

## Core Concepts

The MDK instance is your main entry point, just point it at a SQLite file and it manages all your groups, messages, key packages, and everything else. Groups are MLS conversations with unique hex IDs, names, descriptions, member lists, and associated Nostr relays. Each group has admins who handle adding and removing key packages.

Messages are MLS-encrypted group chatter that get auto-decrypted when you retrieve them, when someone adds you to a group, you get a welcome message containing all the keys and state you need to decrypt everything and participate.

## The API You'll Actually Use

Getting started is simple: first initialize your platform's keyring store (see platform setup), then create your MDK instance with `new_mdk("/path/to/your/database.db", "com.example.myapp", "mdk.db.key.default")`. MDK automatically handles encryption key generation and secure storage in your platform's keyring (Keychain on iOS/macOS, Keystore on Android, etc.). If you need to manage keys yourself, use `new_mdk_with_key()` with a 32-byte key. For development only, you can use `new_mdk_unencrypted("/path/to/database.db")` but never ship that to production. For key packages, you'll want to create one with your preferred relays using `mdk.create_key_package()`, which gives you a hex key package and tags for your Nostr event. When others send you their key package events, parse them with `mdk.parse_key_package()` so you can add them later. Check for invites using `mdk.get_pending_welcomes()` and join groups with `mdk.accept_welcome()`.

For group management, start your own group with `mdk.create_group()` - you'll need your creator pubkey, initial members, metadata, relays, and admin list. It returns the group object and welcome messages for your initial members. Adding people requires admin privileges and their key package events via `mdk.add_members()`. Remove troublemakers with `mdk.remove_members()` using their pubkeys, or update group metadata like name and description with `mdk.update_group_metadata()`.

Send encrypted messages with `mdk.create_message()`, specifying the group, your pubkey, content, and message kind - it returns an encrypted Nostr event you can publish to your relays. Retrieve message history with `mdk.get_messages()` where everything's auto-decrypted. When you pull events from Nostr relays, process them using `mdk.process_message()` which tells you if it's new, a duplicate, or if something goes wrong during processing.

## Data Models

Group objects contain the `mls_group_id` as a hex string, plus the name you gave it, description with the group vibes, list of relay URLs, an array of pubkeys for the admins, and a creation timestamp. Messages have the `event_id` from Nostr, the `mls_group_id` they belong to, the `sender_public_key`, the actual decrypted `content`, a numeric `kind` for message type, and the `created_at` timestamp. Welcome messages include the raw `welcome_json` invite data, the `group_name` and `group_description` so you know what you're joining, and the `sender_public_key` of your inviter.

## What's Next

If you want to go deeper, check out the [Marmot Protocol Spec](https://github.com/marmot-protocol/marmot) for the full details, [MLS RFC 9420](https://www.rfc-editor.org/rfc/rfc9420.html) for the crypto foundation, the [Nostr Protocol](https://github.com/nostr-protocol/nostr) for the network layer, and [UniFFI Docs](https://mozilla.github.io/uniffi-rs/) if you're curious about how the bindings work under the hood. But you don't have to!
