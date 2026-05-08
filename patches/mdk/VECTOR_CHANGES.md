# Vector MDK patch

This is an in-repo fork of [parres-hq/mdk](https://github.com/parres-hq/mdk)
branched from upstream rev `136a9ee929580206ea0357d48d9766427918186d` (mdk-core 0.6.0).

The fork adds *one* capability not in upstream: returning the kind-445 wrapper's
ephemeral signing keypair to the caller, so Vector can later publish an
author-signed NIP-09 deletion against the wrapper. Upstream discards the key
after signing, making published group messages permanently un-deletable at the
relay layer.

## Changes vs upstream `136a9ee`

Two new public methods, both wire-compatible siblings of the existing functions
(relays cannot distinguish events produced by the new vs. old paths).

| File | Change |
|------|--------|
| `crates/mdk-core/src/groups.rs` | Added `pub(crate) fn build_message_event_retained(group_id, content) -> Result<(Event, Keys)>`. Refactored existing `build_message_event` to delegate to it and discard the key, preserving the original behaviour. |
| `crates/mdk-core/src/messages/create.rs` | Added `pub fn create_message_retained(group_id, rumor) -> Result<(Event, Keys)>`. Refactored existing `create_message` to delegate to it and discard the key. Added `Keys` import to the `nostr::` use line. |

No other files are modified. No new dependencies. No public API removals.

## Total diff size

Roughly 30 lines of added Rust against upstream. Run `git diff <upstream-rev>..HEAD -- patches/mdk/`
from a workspace where the upstream is checked out separately to inspect.

## Rebase strategy

When MDK upstream moves and we want to take the bump:

1. Fetch the new upstream rev locally (e.g. `git -C ../mdk fetch origin && git -C ../mdk checkout <new-rev>`).
2. Copy the new `crates/mdk-core` (and any other changed crates) over the in-repo `patches/mdk/crates/*`.
3. Re-apply the two changes above (search for `build_message_event_retained` and `create_message_retained` markers).
4. Update the `rev = "..."` pin in `crates/vector-core/Cargo.toml` and `src-tauri/Cargo.toml` to the new upstream rev so the `[patch."https://github.com/marmot-protocol/mdk.git"]` blocks resolve correctly.
5. `cargo check` from both workspaces. Run `cargo test -p vector-core --lib`.

If upstream ever adds a key-retention API that subsumes our patch, this fork can be retired by switching the in-repo path back to a plain git rev pin.

## Why not push to a Vector-controlled GitHub fork?

We may eventually. For now, in-repo means anyone cloning Vector compiles
without needing an external checkout, and the patch surface is small enough to
review at a glance.
