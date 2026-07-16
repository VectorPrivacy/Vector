//! Sending and fetching Community channel messages over a [`Transport`].
//!
//! `publish_message` seals a message (envelope) and publishes the outer event to
//! the Community's relays. `fetch_channel_messages` queries the channel's current
//! epoch pseudonym, then decrypts + verifies each event, silently dropping any that
//! fail (wrong key, splice, bad signature) — a non-member, or a spliced event, never
//! surfaces. Both are transport-agnostic so they run identically against the live
//! client and the in-memory test relay.

use nostr_sdk::prelude::*;

use super::derive::channel_pseudonym;
use super::envelope::{open_message_multi, seal_message_with_ephemeral, seal_with_signed_inner, OpenedMessage};
#[cfg(test)]
use super::envelope::open_message;
use super::transport::{Evidence, Query, Transport};
use super::{Channel, Community};
use crate::stored_event::event_kind;

/// Seal `content` and publish it to the Community's relays.
///
/// Returns the published outer event AND its **retained ephemeral signing key**.
/// The key is one-time on the wire (no persistent author↔channel linkage), but
/// the sender keeps it so they can later [`delete_own_message`] their own message
/// Persist it (see `db::community::store_message_key`) — exactly like Vector's
/// `nip17_wrap_keys` for DMs. Discarding it just means that message can't be deleted.
pub async fn publish_message<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    author: &Keys,
    content: &str,
    ms: u64,
) -> Result<(Event, Keys), String> {
    let ephemeral = Keys::generate();
    let outer = seal_message_with_ephemeral(
        &ephemeral, author, &channel.key, &channel.id, channel.epoch, content, ms,
    )
    .map_err(|e| e.to_string())?;
    transport.publish(&outer, &community.relays).await?;
    Ok((outer, ephemeral))
}

/// Publish a message whose inner authorship event was already signed externally (via the
/// active `NostrSigner` — local keys OR a NIP-46 bunker). Mirrors [`publish_message`] but
/// is signer-agnostic, so bunker accounts can post (parity with DMs). Returns the
/// published outer event + its retained ephemeral key.
/// `durable`: control/moderation events (a hide, a presence-join that must reliably land so the sender
/// stays in the observed recipient set) broadcast durably (per-relay retry); ordinary chat messages
/// pass `false` for the latency-sensitive single-attempt path.
pub async fn publish_signed_message<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    inner: &Event,
    durable: bool,
) -> Result<(Event, Keys), String> {
    let ephemeral = Keys::generate();
    let outer = seal_with_signed_inner(&ephemeral, inner, &channel.key, &channel.id, channel.epoch)
        .map_err(|e| e.to_string())?;
    if durable {
        transport.publish_durable(&outer, &community.relays).await?;
    } else {
        transport.publish(&outer, &community.relays).await?;
    }
    Ok((outer, ephemeral))
}

/// Delete a message the sender previously published, via its retained ephemeral key
/// (NIP-09 — the deletion must be signed by the same key that signed the event, so
/// only the original sender can delete their own message).
pub async fn delete_own_message<T: Transport + ?Sized>(
    transport: &T,
    relays: &[String],
    ephemeral: &Keys,
    outer_event_id: EventId,
) -> Result<(), String> {
    let deletion = EventBuilder::delete(EventDeletionRequest::new().ids([outer_event_id]))
        .sign_with_keys(ephemeral)
        .map_err(|e| e.to_string())?;
    transport.publish_durable(&deletion, relays).await
}

/// Every epoch pseudonym the member can derive for a channel (one per retained `(epoch, key)`), so a
/// fetch spans ALL held epochs — messages posted under an older epoch aren't stranded after a rekey
/// catch-up. Falls back to the head epoch for send-built/test channels (`read_epoch_keys`).
fn channel_read_pseudonyms(channel: &Channel) -> Vec<String> {
    channel
        .read_epoch_keys()
        .iter()
        .map(|(epoch, key)| channel_pseudonym(key, &channel.id, *epoch).to_hex())
        .collect()
}

/// Fetch + open all messages across the channel's held epochs. Events that fail to
/// open (wrong key, splice, forged signature) are dropped, not surfaced.
pub async fn fetch_channel_messages<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
) -> Result<Vec<OpenedMessage>, String> {
    let query = Query {
        kinds: vec![event_kind::COMMUNITY_MESSAGE],
        z_tags: channel_read_pseudonyms(channel),
        since: None,
        // Positive-data read: signed messages from any relay are safe, gaps
        // heal via the straggler sink + live sub. No verdict drawn from absence.
        evidence: Evidence::Fast,
        ..Default::default()
    };
    let events = transport.fetch(&query, &community.relays).await?;
    // Drop events that fail to open (wrong key, splice, forged sig, bad version) —
    // a non-member or spliced event must never surface. Log drops (id + error only,
    // never content or keys) so a flood of garbage under a known pseudonym is visible
    // rather than indistinguishable from an empty channel.
    let epoch_keys = channel.read_epoch_keys();
    let mut opened: Vec<OpenedMessage> = Vec::new();
    let mut dropped = 0usize;
    for ev in &events {
        match open_message_multi(ev, &channel.id, &epoch_keys) {
            Ok(msg) => opened.push(msg),
            Err(e) => {
                dropped += 1;
                crate::log_debug!("[community] dropped event {}: {}", ev.id.to_hex(), e);
            }
        }
    }
    if dropped > 0 {
        crate::log_debug!(
            "[community] channel {} fetch: {} opened, {} dropped",
            channel.id.to_hex(),
            opened.len(),
            dropped
        );
    }
    // Dedup on the INNER (message) id, never the outer wrapper id. One inner
    // message can ride multiple outer wrappers — a member re-broadcasting, redundant
    // multi-relay copies, or an exact replay — and they must collapse to one row. Keep
    // the first occurrence.
    {
        let mut seen = std::collections::HashSet::new();
        opened.retain(|m| seen.insert(m.message_id));
    }
    // Deterministic chat order: inner authenticated ms timestamp, ties by inner id.
    opened.sort_by(|a, b| {
        a.ms.unwrap_or(0)
            .cmp(&b.ms.unwrap_or(0))
            .then_with(|| a.message_id.to_hex().cmp(&b.message_id.to_hex()))
    });
    Ok(opened)
}

/// Raw fetch of every append-plane event — messages (3300), reactions (3301), edits (3302)
/// — for a channel's CURRENT-epoch pseudonym. Backfill/cold-start primitive ("recent on
/// open"): unlike [`fetch_channel_messages`] this returns the un-opened outer events of all
/// sub-kinds so the caller can run them through `inbound::process_channel_batch`, which opens,
/// verifies, dedups (inner id), and applies reactions/edits to their target messages.
pub async fn fetch_channel_events<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
) -> Result<Vec<Event>, String> {
    let query = Query {
        kinds: vec![
            event_kind::COMMUNITY_MESSAGE,
            event_kind::COMMUNITY_REACTION,
            event_kind::COMMUNITY_EDIT,
            event_kind::COMMUNITY_DELETE,
            event_kind::COMMUNITY_PRESENCE,
            event_kind::COMMUNITY_KICK,
            event_kind::COMMUNITY_WEBXDC,
        ],
        z_tags: channel_read_pseudonyms(channel),
        since: None,
        // Positive-data read (see fetch_channel_messages).
        evidence: Evidence::Fast,
        ..Default::default()
    };
    transport.fetch(&query, &community.relays).await
}

/// Fetch one PAGE of a channel's append-plane events (3300/3301/3302) for its current-epoch
/// pseudonym, newest-first, capped at `limit`. `until` (seconds, inclusive) pages OLDER
/// history — pass the oldest-known message's `created_at` to step back a page; pass `None`
/// for the latest page. The Discord-style sync primitive: latest-page on open/join/boot,
/// older-page when local DB history is exhausted on scroll-up. Returns raw outer events for
/// `inbound::process_channel_batch`.
pub async fn fetch_channel_page<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    until: Option<u64>,
    since: Option<u64>,
    limit: usize,
) -> Result<Vec<Event>, String> {
    let query = Query {
        kinds: vec![
            event_kind::COMMUNITY_MESSAGE,
            event_kind::COMMUNITY_REACTION,
            event_kind::COMMUNITY_EDIT,
            event_kind::COMMUNITY_DELETE,
            event_kind::COMMUNITY_PRESENCE,
            event_kind::COMMUNITY_KICK,
            event_kind::COMMUNITY_WEBXDC,
        ],
        // OR-set over every held epoch pseudonym: the relay returns the newest `limit` events ACROSS
        // epochs for this `until`, so a "latest 20" page naturally spans rekeys (newest epoch fills
        // first, older epochs backfill the deficit) and scroll-back keeps walking older epochs.
        z_tags: channel_read_pseudonyms(channel),
        until,
        // `since` (latest-page only) skips re-pulling events already held — set to the newest wire
        // time seen. Inclusive on the relay, so the boundary second is re-admitted (dedup drops it),
        // catching any sibling event sharing that second. Epoch spanning is unaffected (it's in
        // z_tags, above), and back-pagination passes `None` here.
        since,
        limit: Some(limit),
        // Latest pages are positive-data reads. Older pages (`until` set) are
        // force-promoted to Full by the transport — the history-start latch
        // needs the completest union the reachable relays allow.
        evidence: Evidence::Fast,
        ..Default::default()
    };
    transport.fetch(&query, &community.relays).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::transport::memory::MemoryRelay;
    use crate::community::{Channel, ChannelKey, Epoch};

    /// A Community with a fixed relay set for tests.
    fn community() -> Community {
        Community::create("HQ", "general", vec!["r1".into(), "r2".into(), "r3".into()])
    }

    /// Simulate a second member: same Community keys (they were handed the keys on
    /// join), but a distinct identity for authorship.
    fn member_view(of: &Community) -> Community {
        Community {
            id: of.id,
            server_root_key: of.server_root_key.clone(),
            server_root_epoch: of.server_root_epoch,
            name: of.name.clone(),
            description: of.description.clone(),
            icon: of.icon.clone(),
            banner: of.banner.clone(),
            relays: of.relays.clone(),
            channels: of.channels.clone(),
            owner_attestation: of.owner_attestation.clone(),
            dissolved: of.dissolved,
        }
    }

    #[tokio::test]
    async fn two_clients_exchange_via_relay() {
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();

        let alice = Keys::generate();
        publish_message(&relay, &community, &channel, &alice, "gm from alice", 100)
            .await
            .unwrap();

        // Bob holds the same channel key (joined member) and reads it back.
        let bob_view = member_view(&community);
        let msgs = fetch_channel_messages(&relay, &bob_view, &bob_view.channels[0])
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "gm from alice");
        assert_eq!(msgs[0].author, alice.public_key());
    }

    /// Build a single-epoch channel VIEW (for publishing under a specific epoch key).
    fn epoch_view(base: &Channel, key: ChannelKey, epoch: u64) -> Channel {
        Channel {
            id: base.id, key, epoch: Epoch(epoch), name: base.name.clone(),
            banned: Vec::new(), protected: Vec::new(), roster: Default::default(), epoch_keys: Vec::new(),
            dissolved: false,
        }
    }

    #[tokio::test]
    async fn fetch_spans_held_epochs_after_rekeys() {
        // multi-epoch read: a member who caught up across rekeys (holds keys for epochs 0,1,2) fetches
        // the messages posted under EACH — none stranded by a rekey.
        let relay = MemoryRelay::new();
        let community = community();
        let base = community.channels[0].clone();
        let alice = Keys::generate();
        let k0 = base.key.clone();
        let k1 = ChannelKey([0x11u8; 32]);
        let k2 = ChannelKey([0x22u8; 32]);

        publish_message(&relay, &community, &epoch_view(&base, k0.clone(), 0), &alice, "epoch0", 100).await.unwrap();
        publish_message(&relay, &community, &epoch_view(&base, k1.clone(), 1), &alice, "epoch1", 200).await.unwrap();
        publish_message(&relay, &community, &epoch_view(&base, k2.clone(), 2), &alice, "epoch2", 300).await.unwrap();

        // Reader holds ALL three epoch keys (head = epoch 2). One fetch returns all three, time-ordered.
        let mut reader = member_view(&community);
        reader.channels[0] = Channel {
            id: base.id, key: k2.clone(), epoch: Epoch(2), name: base.name.clone(),
            banned: Vec::new(), protected: Vec::new(), roster: Default::default(),
            epoch_keys: vec![(Epoch(0), k0), (Epoch(1), k1), (Epoch(2), k2.clone())],
            dissolved: false,
        };
        let msgs = fetch_channel_messages(&relay, &reader, &reader.channels[0]).await.unwrap();
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["epoch0", "epoch1", "epoch2"], "every held epoch's messages returned, none stranded");

        // Regression: a reader holding ONLY the head epoch (no archive → single-epoch fallback) sees just
        // the current epoch — the old behavior, confirming the archive is what unlocks history.
        let mut head_only = member_view(&community);
        head_only.channels[0] = epoch_view(&base, k2, 2);
        let only = fetch_channel_messages(&relay, &head_only, &head_only.channels[0]).await.unwrap();
        assert_eq!(only.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), vec!["epoch2"],
            "head-only reader sees only the current epoch (single-epoch fallback)");
    }

    #[tokio::test]
    async fn page_spans_the_epoch_boundary() {
        // A single page query covers the held-epoch OR-set, so one page can carry messages from BOTH the
        // head epoch AND an older one (the "fill the page across epochs" mechanic) — and each opens under
        // its own epoch key via the per-event #z selection.
        let relay = MemoryRelay::new();
        let community = community();
        let base = community.channels[0].clone();
        let alice = Keys::generate();
        let k0 = base.key.clone();
        let k1 = ChannelKey([0x33u8; 32]);
        publish_message(&relay, &community, &epoch_view(&base, k0.clone(), 0), &alice, "old-a", 100).await.unwrap();
        publish_message(&relay, &community, &epoch_view(&base, k0.clone(), 0), &alice, "old-b", 200).await.unwrap();
        publish_message(&relay, &community, &epoch_view(&base, k1.clone(), 1), &alice, "new-c", 300).await.unwrap();

        let mut reader = member_view(&community);
        reader.channels[0] = Channel {
            id: base.id, key: k1.clone(), epoch: Epoch(1), name: base.name.clone(),
            banned: Vec::new(), protected: Vec::new(), roster: Default::default(),
            epoch_keys: vec![(Epoch(0), k0), (Epoch(1), k1)],
            dissolved: false,
        };
        let page = fetch_channel_page(&relay, &reader, &reader.channels[0], None, None, 20).await.unwrap();
        let opened: Vec<String> = page.iter()
            .filter_map(|e| open_message_multi(e, &reader.channels[0].id, &reader.channels[0].read_epoch_keys()).ok())
            .map(|m| m.content)
            .collect();
        assert!(opened.contains(&"new-c".to_string()), "head-epoch message in the page");
        assert!(opened.contains(&"old-a".to_string()) && opened.contains(&"old-b".to_string()),
            "older-epoch messages in the SAME page (across the epoch boundary)");
    }

    #[tokio::test]
    async fn three_clients_one_broadcast() {
        // O(1) broadcast: Alice publishes once; Bob AND Carol both decrypt it.
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        publish_message(&relay, &community, &channel, &alice, "hello all", 1)
            .await
            .unwrap();

        for reader in [member_view(&community), member_view(&community)] {
            let msgs = fetch_channel_messages(&relay, &reader, &reader.channels[0])
                .await
                .unwrap();
            assert_eq!(msgs.len(), 1);
            assert_eq!(msgs[0].content, "hello all");
        }
    }

    #[tokio::test]
    async fn non_member_with_wrong_key_cannot_read() {
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        publish_message(&relay, &community, &channel, &alice, "secret", 1)
            .await
            .unwrap();

        // Outsider holds a DIFFERENT channel key (same id/relays). They can't even
        // derive the right pseudonym, so the query returns nothing...
        let mut outsider = member_view(&community);
        outsider.channels[0].key = ChannelKey([0xeeu8; 32]);
        let msgs = fetch_channel_messages(&relay, &outsider, &outsider.channels[0])
            .await
            .unwrap();
        assert!(msgs.is_empty(), "wrong key derives a different pseudonym → no hits");

        // ...and even handed the raw event, opening it fails (MAC).
        let raw = relay
            .fetch(
                &Query { kinds: vec![event_kind::COMMUNITY_MESSAGE], ..Default::default() },
                &community.relays,
            )
            .await
            .unwrap();
        assert_eq!(raw.len(), 1);
        assert!(open_message(&raw[0], &outsider.channels[0].key, &channel.id, channel.epoch).is_err());
    }

    #[tokio::test]
    async fn messages_return_in_ms_order() {
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        // Publish out of order.
        publish_message(&relay, &community, &channel, &alice, "third", 300).await.unwrap();
        publish_message(&relay, &community, &channel, &alice, "first", 100).await.unwrap();
        publish_message(&relay, &community, &channel, &alice, "second", 200).await.unwrap();

        let reader = member_view(&community);
        let msgs = fetch_channel_messages(&relay, &reader, &reader.channels[0]).await.unwrap();
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["first", "second", "third"]);
    }

    #[tokio::test]
    async fn other_channel_traffic_is_not_returned() {
        // A second channel's messages (different key+id → different pseudonym) must
        // not appear when fetching the first channel.
        let relay = MemoryRelay::new();
        let community = community();
        let chan_a = community.channels[0].clone();
        let chan_b = Channel {
            id: super::super::ChannelId([0x77u8; 32]),
            key: ChannelKey([0x88u8; 32]),
            epoch: Epoch(0),
            name: "other".into(),
            banned: Vec::new(),
            protected: Vec::new(), roster: Default::default(),
            epoch_keys: Vec::new(),
            dissolved: false,
        };
        let alice = Keys::generate();
        publish_message(&relay, &community, &chan_a, &alice, "in A", 1).await.unwrap();
        publish_message(&relay, &community, &chan_b, &alice, "in B", 1).await.unwrap();

        let reader = member_view(&community);
        let msgs = fetch_channel_messages(&relay, &reader, &reader.channels[0]).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "in A");
    }

    #[tokio::test]
    async fn two_distinct_messages_are_not_collapsed() {
        // Two DISTINCT inner messages (different message_id) must return as two rows —
        // dedup keys on the inner id, so it must not over-collapse genuinely different
        // messages.
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        publish_message(&relay, &community, &channel, &alice, "one", 1).await.unwrap();
        publish_message(&relay, &community, &channel, &alice, "two", 2).await.unwrap();

        let reader = member_view(&community);
        let msgs = fetch_channel_messages(&relay, &reader, &reader.channels[0]).await.unwrap();
        assert_eq!(msgs.len(), 2);
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert!(contents.contains(&"one") && contents.contains(&"two"));
    }

    #[tokio::test]
    async fn backfill_fetches_and_applies_messages_then_reactions() {
        // Cold-start backfill core: fetch the raw channel events and process them as a batch
        // into a fresh STATE. Messages must ingest AND a reaction must land on its target —
        // which only works if the batch processes messages before control events (relay
        // return order is arbitrary).
        use super::super::inbound::{process_channel_batch, IncomingEvent};
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        let bob = Keys::generate();

        let (m1_outer, _) = publish_message(&relay, &community, &channel, &alice, "hello", 1).await.unwrap();
        publish_message(&relay, &community, &channel, &alice, "world", 2).await.unwrap();
        // Bob reacts to m1 (a 3301 referencing m1's INNER id).
        let m1_inner = open_message(&m1_outer, &channel.key, &channel.id, channel.epoch).unwrap().message_id.to_hex();
        let react_inner = super::super::envelope::build_inner_typed(
            bob.public_key(), &channel.id, channel.epoch,
            event_kind::COMMUNITY_REACTION, "🔥", 3, Some(&m1_inner), &[],
        ).sign_with_keys(&bob).unwrap();
        let react_outer = seal_with_signed_inner(&Keys::generate(), &react_inner, &channel.key, &channel.id, channel.epoch).unwrap();
        relay.publish(&react_outer, &community.relays).await.unwrap();

        let events = fetch_channel_events(&relay, &community, &channel).await.unwrap();
        assert_eq!(events.len(), 3, "two messages + one reaction fetched");
        let mut state = crate::state::ChatState::new();
        let applied = process_channel_batch(&mut state, &events, &channel, &bob.public_key());

        let new_msgs = applied.iter().filter(|e| matches!(e, IncomingEvent::NewMessage(_))).count();
        let updates: Vec<&String> = applied.iter().filter_map(|e| match e {
            IncomingEvent::Updated { target_id, .. } => Some(target_id),
            _ => None,
        }).collect();
        assert_eq!(new_msgs, 2, "both messages backfilled");
        assert_eq!(updates.len(), 1, "reaction applied during backfill");
        assert_eq!(updates[0], &m1_inner, "reaction landed on its target message");
    }

    #[tokio::test]
    async fn same_inner_message_in_two_wrappers_collapses() {
        // dedup on the INNER id. The SAME signed inner message, sealed into two
        // DIFFERENT outer wrappers (distinct ephemeral keys → distinct outer ids, e.g. a
        // member re-broadcast or a replay), must collapse to a single row on fetch.
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();

        // One signed inner authorship event → one message_id.
        let inner = super::super::envelope::build_inner_event(alice.public_key(), &channel.id, channel.epoch, "dup me", 1, None)
            .sign_with_keys(&alice)
            .unwrap();
        // Two independent outer wrappers carrying that exact inner.
        let outer_a = seal_with_signed_inner(&Keys::generate(), &inner, &channel.key, &channel.id, channel.epoch).unwrap();
        let outer_b = seal_with_signed_inner(&Keys::generate(), &inner, &channel.key, &channel.id, channel.epoch).unwrap();
        assert_ne!(outer_a.id, outer_b.id, "distinct outer wrappers");
        relay.publish(&outer_a, &community.relays).await.unwrap();
        relay.publish(&outer_b, &community.relays).await.unwrap();

        let reader = member_view(&community);
        let msgs = fetch_channel_messages(&relay, &reader, &reader.channels[0]).await.unwrap();
        assert_eq!(msgs.len(), 1, "same inner id collapses across wrappers");
        assert_eq!(msgs[0].content, "dup me");
    }

    #[tokio::test]
    async fn bad_event_dropped_good_event_kept_in_same_batch() {
        // A garbage event under the same pseudonym must be dropped while a valid one
        // in the same fetch is returned (open-failure isolation).
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        // Valid message.
        publish_message(&relay, &community, &channel, &alice, "valid", 1).await.unwrap();
        // Garbage event carrying the right pseudonym but undecryptable content.
        let pseudonym =
            super::super::derive::channel_pseudonym(&channel.key, &channel.id, channel.epoch);
        let garbage = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "not-base64-or-cipher!!")
            .tags([
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                    [pseudonym.to_hex()],
                ),
                Tag::custom(TagKind::Custom("v".into()), ["1".to_string()]),
            ])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        relay.publish(&garbage, &community.relays).await.unwrap();

        let reader = member_view(&community);
        let msgs = fetch_channel_messages(&relay, &reader, &reader.channels[0]).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "valid");
    }

    #[tokio::test]
    async fn publish_retains_key_and_owner_can_delete() {
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();

        let (outer, ephemeral) =
            publish_message(&relay, &community, &channel, &alice, "deletable", 1).await.unwrap();
        // The retained key is exactly the one that signed the outer event.
        assert_eq!(ephemeral.public_key(), outer.pubkey);

        let before = fetch_channel_messages(&relay, &community, &channel).await.unwrap();
        assert_eq!(before.len(), 1);

        // Delete via the retained ephemeral key → gone (MemoryRelay honors NIP-09).
        delete_own_message(&relay, &community.relays, &ephemeral, outer.id).await.unwrap();
        let after = fetch_channel_messages(&relay, &community, &channel).await.unwrap();
        assert!(after.is_empty(), "owner's deletion should remove the message");
    }

    #[tokio::test]
    async fn deletion_by_a_different_key_is_ignored() {
        // NIP-09 same-pubkey rule: only the original (ephemeral) signer can delete.
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        let (outer, ephemeral) =
            publish_message(&relay, &community, &channel, &alice, "mine", 1).await.unwrap();

        let attacker = Keys::generate();
        delete_own_message(&relay, &community.relays, &attacker, outer.id).await.unwrap();
        let after = fetch_channel_messages(&relay, &community, &channel).await.unwrap();
        assert_eq!(after.len(), 1, "a foreign key must not delete someone else's message");

        // Prove the deletion machinery actually works (so the assert above isn't
        // passing merely because deletion is a no-op): the real key DOES delete it.
        delete_own_message(&relay, &community.relays, &ephemeral, outer.id).await.unwrap();
        let gone = fetch_channel_messages(&relay, &community, &channel).await.unwrap();
        assert!(gone.is_empty(), "the original signer's key must delete it");
    }

    #[tokio::test]
    async fn deletion_must_reach_all_relays_to_take_effect() {
        // NIP-09 deletion is not magically global: if the delete lands on only one of
        // a redundant relay set, the event survives on the others (redundancy cuts
        // both ways). Documents that a real delete must be sent to every server relay.
        let relay = MemoryRelay::new();
        let community = community(); // relays r1, r2, r3
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        let (outer, ephemeral) =
            publish_message(&relay, &community, &channel, &alice, "sticky", 1).await.unwrap();

        // Delete on ONLY r1.
        delete_own_message(&relay, &["r1".to_string()], &ephemeral, outer.id).await.unwrap();
        // Still fetchable across the full set (lives on r2/r3).
        let still = fetch_channel_messages(&relay, &community, &channel).await.unwrap();
        assert_eq!(still.len(), 1, "deletion on a subset must not remove it everywhere");

        // Delete on all relays → finally gone.
        delete_own_message(&relay, &community.relays, &ephemeral, outer.id).await.unwrap();
        let gone = fetch_channel_messages(&relay, &community, &channel).await.unwrap();
        assert!(gone.is_empty());
    }

    /// LIVE on-relay end-to-end test against jskitty.com (the user's strfry).
    /// `#[ignore]`d — run explicitly with `cargo test -p vector-core -- --ignored
    /// live_relay`. Publishes under a RANDOM channel pseudonym (so it pollutes no
    /// real namespace), verifies fetch+open over the wire, then ALWAYS NIP-09-deletes
    /// its events (cleanup runs before any assertion so a failure can't leak garbage).
    #[tokio::test]
    #[ignore]
    async fn live_relay_roundtrip_and_cleanup() {
        use super::super::transport::LiveTransport;
        use crate::community::derive::channel_pseudonym;

        let _ = rustls::crypto::ring::default_provider().install_default();

        let relays = vec!["wss://jskitty.com/nostr".to_string()];
        let community = Community::create("LiveTest", "general", relays.clone());
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));

        let pseudonym = channel_pseudonym(&channel.key, &channel.id, channel.epoch).to_hex();
        eprintln!("[live] channel pseudonym (z tag) = {pseudonym}");

        // 1. Publish two messages, retaining each ephemeral key for later deletion.
        // Use a real epoch-ms so the split-out created_at is "now" (relays reject
        // events dated absurdly far in the past/future).
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mut published: Vec<(Keys, Event)> = Vec::new();
        let mut publish_errs: Vec<String> = Vec::new();
        for (i, body) in ["live hello one", "live hello two"].iter().enumerate() {
            let ephemeral = Keys::generate();
            let outer = seal_message_with_ephemeral(
                &ephemeral, &alice, &channel.key, &channel.id, channel.epoch, body, now_ms + i as u64,
            )
            .expect("seal");
            match transport.publish(&outer, &relays).await {
                Ok(()) => {
                    eprintln!("[live] published event {}", outer.id.to_hex());
                    published.push((ephemeral, outer));
                }
                Err(e) => publish_errs.push(e),
            }
        }

        // 2. Fetch back over the wire.
        let fetched = fetch_channel_messages(&transport, &community, &channel).await;

        // 3. CLEANUP FIRST — delete every published event via its retained ephemeral
        //    key, before any assertion can panic and strand garbage on the relay.
        let mut cleanup_ok = true;
        for (ephemeral, outer) in &published {
            let del = EventBuilder::delete(EventDeletionRequest::new().ids([outer.id]))
                .sign_with_keys(ephemeral)
                .expect("build deletion");
            if let Err(e) = transport.publish(&del, &relays).await {
                cleanup_ok = false;
                eprintln!("[live] CLEANUP FAILED for {}: {e} — manual delete may be needed", outer.id.to_hex());
            } else {
                eprintln!("[live] deleted event {}", outer.id.to_hex());
            }
        }

        // 4. Now it's safe to assert.
        assert!(publish_errs.is_empty(), "publish errors: {publish_errs:?}");
        let msgs = fetched.expect("fetch failed");
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert!(contents.contains(&"live hello one"), "got {contents:?}");
        assert!(contents.contains(&"live hello two"), "got {contents:?}");
        for m in &msgs {
            assert_eq!(m.author, alice.public_key(), "author recovered over the wire");
        }
        assert!(cleanup_ok, "cleanup deletion failed — check relay for leftover events");
        eprintln!("[live] OK: {} messages round-tripped and cleaned up", msgs.len());
    }

    #[tokio::test]
    async fn redundancy_a_dropped_relay_still_delivers() {
        // The message lands on only one of the three relays (the others "missed" it);
        // a member fetching across the set still receives it.
        let relay = MemoryRelay::new();
        let community = community();
        let channel = community.channels[0].clone();
        let alice = Keys::generate();

        let outer = seal_message_with_ephemeral(
            &Keys::generate(), &alice, &channel.key, &channel.id, channel.epoch, "survives", 1,
        )
        .unwrap();
        relay.inject(&outer, &["r2".to_string()]); // only r2 has it

        let reader = member_view(&community);
        let msgs = fetch_channel_messages(&relay, &reader, &reader.channels[0]).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "survives");
    }
}
