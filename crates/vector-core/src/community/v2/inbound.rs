//! v2 inbound bridge — turns opened v2 events into the protocol-agnostic
//! [`InboundEventHandler`] callbacks the rest of Vector (and the SDK's
//! `on_message`) already consumes. The handler is the seam: v1 and v2 both feed
//! it, so a bot receives v2 messages with no SDK change.
//!
//! Dispatch is by which plane a kind-1059 wrap opens under. A received wrap is
//! tried against each held channel's Chat-Plane key (author match, no trial
//! decrypt), then the Guestbook plane; a control-plane fold is a heavier
//! separate path (metadata/roster), handled by the service refresh, not here.

use nostr_sdk::prelude::PublicKey;
use nostr_sdk::ToBech32;

use super::super::attachments::attachments_from_tags;
use super::chat::{self, ChatEvent};
use super::community::CommunityV2;
use super::guestbook::{self, GuestbookEntry};
use super::stream;
use crate::event_handler::InboundEventHandler;
use crate::state::ChatState;
use crate::types::{EmojiTag, Message, Reaction};

/// Build a protocol-agnostic [`Message`] from an opened v2 chat Message event.
/// Mirrors v1's `build_message` field-for-field (id = the rumor id, ms time,
/// `mine`, npub, imeta attachments, NIP-30 emoji, the reply reference), so the
/// frontend/SDK renderers treat a v2 message identically to a v1 or DM one.
pub fn chat_message_to_message(
    opened: &stream::OpenedStream,
    reply_to: &Option<chat::ReplyRef>,
    emoji: &[(String, String)],
    my_pubkey: &PublicKey,
) -> Message {
    let (replied_to, replied_to_npub) = match reply_to {
        Some(r) => (
            crate::simd::hex::bytes_to_hex_32(&r.id),
            r.author.and_then(|a| a.to_bech32().ok()),
        ),
        None => (String::new(), None),
    };
    Message {
        id: opened.rumor_id.to_hex(),
        content: opened.rumor.content.clone(),
        replied_to,
        replied_to_npub,
        at: opened.at_ms,
        mine: opened.author == *my_pubkey,
        npub: opened.author.to_bech32().ok(),
        attachments: attachments_from_tags(opened.rumor.tags.iter(), &crate::db::get_download_dir()),
        emoji_tags: emoji
            .iter()
            .map(|(shortcode, url)| EmojiTag { shortcode: shortcode.clone(), url: url.clone() })
            .collect(),
        wrapper_event_id: Some(opened.wrapper_id.to_hex()),
        ..Default::default()
    }
}

/// What applying a v2 chat event to STATE yielded — the caller persists it (async)
/// once the STATE lock is dropped. Mirrors v1's `IncomingEvent` for the chat sub-kinds:
/// a message row is saved fresh or re-saved (a landed reaction rides the row), a delete
/// drops it. Persistence is the caller's so the apply step stays sync + lock-scoped.
pub enum ChatPersist {
    /// A brand-new message — save its row.
    New(Message),
    /// A message changed: a reaction landed (`edit_event` None → re-save the row, which
    /// carries reactions) or an edit applied (`edit_event` Some → save the folded
    /// MESSAGE_EDIT event, event-sourced like v1 + DMs, never a row overwrite).
    Updated { message: Message, edit_event: Option<Box<crate::stored_event::StoredEvent>> },
    /// A message removed by its author — drop its row.
    Removed(String),
}

/// Apply an opened v2 [`ChatEvent`] to STATE (dedup + aggregate onto the SHARED
/// [`ChatState`]), mirroring v1's `ingest_message`/`apply_reaction`/`apply_delete`. Sync:
/// the DB dedup read + STATE mutation run under the caller's lock; the caller then does the
/// async DB persist on the returned [`ChatPersist`] (see [`persist_chat`]). Returns `None`
/// for a duplicate, a non-persisted kind (typing/webxdc), an edit (increment 2), or an
/// aggregate whose target isn't resident in this channel.
pub fn apply_chat_to_state(state: &mut ChatState, event: &ChatEvent, channel_id: &str, my_pubkey: &PublicKey) -> Option<ChatPersist> {
    match event {
        ChatEvent::Message { opened, reply_to, emoji } => {
            let msg = chat_message_to_message(opened, reply_to, emoji, my_pubkey);
            // DB dedup: a known inner id is already stored — don't re-ingest/re-emit (a
            // catch-up sweep re-fetches the whole page; in-memory STATE holds only a window).
            if crate::db::events::event_exists(&msg.id).unwrap_or(false) {
                return None;
            }
            state.ensure_community_chat(channel_id);
            // Persist regardless of the STATE-add result: `event_exists` already proved
            // it's not in the DB, so a `false` here means only that another writer put it
            // in STATE first — the row must still be saved, or it's lost until re-fetch.
            state.add_message_to_chat(channel_id, msg.clone());
            Some(ChatPersist::New(msg))
        }
        ChatEvent::Reaction { opened, target, emoji, emoji_url, .. } => {
            let target_id = crate::simd::hex::bytes_to_hex_32(target);
            // Cross-channel guard: a reaction lands only on a target resident in the SAME
            // channel it was sealed under (its binding authenticates its own channel, never
            // the target's) — else a member could inject reactions across channels.
            if !matches!(state.find_message(&target_id), Some((chat, _)) if chat.id == channel_id) {
                return None;
            }
            let reaction = Reaction {
                id: opened.rumor_id.to_hex(),
                reference_id: target_id.clone(),
                author_id: opened.author.to_hex(),
                emoji: emoji.clone(),
                emoji_url: emoji_url.clone(),
            };
            let (_c, added) = state.add_reaction_to_message(&target_id, reaction)?;
            added.then(|| state.find_message(&target_id).map(|(_c, m)| ChatPersist::Updated { message: m, edit_event: None }))?
        }
        ChatEvent::Edit { opened, target, new_content } => {
            let target_id = crate::simd::hex::bytes_to_hex_32(target);
            // Author-scoped + same-channel: only the original author edits their own message.
            let editor_npub = opened.author.to_bech32().ok()?;
            if !matches!(state.find_message(&target_id), Some((chat, m)) if chat.id == channel_id && m.npub.as_deref() == Some(editor_npub.as_str())) {
                return None;
            }
            // Apply to STATE via the shared canonical applier (seeds history with the
            // original once, dedups by `edited_at`, swaps content) — reused from v1/DMs.
            let edited_at = opened.at_ms;
            let (_c, message) = state.update_message(&target_id, |m| m.apply_edit(new_content.clone(), edited_at, Vec::new()))?;
            // Persist as a folded MESSAGE_EDIT event (chat_id set at save time), matching v1.
            let edit_event = crate::stored_event::StoredEventBuilder::new()
                .id(opened.rumor_id.to_hex())
                .kind(crate::stored_event::event_kind::MESSAGE_EDIT)
                .content(new_content.clone())
                .reference_id(Some(target_id.clone()))
                .created_at(edited_at / 1000)
                .mine(opened.author == *my_pubkey)
                .npub(opened.author.to_bech32().ok())
                .build();
            Some(ChatPersist::Updated { message, edit_event: Some(Box::new(edit_event)) })
        }
        ChatEvent::Delete { opened, target, .. } => {
            let target_id = crate::simd::hex::bytes_to_hex_32(target);
            // Author-scoped: a delete removes its author's OWN message. A moderation-hide
            // (deleting another's message under MANAGE_MESSAGES) is a gated follow-up.
            let own = matches!(state.find_message(&target_id), Some((_, m)) if m.npub.as_deref() == opened.author.to_bech32().ok().as_deref());
            own.then(|| state.remove_message(&target_id).map(|_| ChatPersist::Removed(target_id)))?
        }
        ChatEvent::Typing { .. } | ChatEvent::Webxdc { .. } => None,
    }
}

/// Open a received wrap under `channel_id`'s plane and persist its chat event to the shared
/// store — the LIVE counterpart of [`crate::VectorCore::v2_backfill_channel`]'s catch-up
/// persistence. `channel_id` is the channel the dispatcher already matched. Best-effort: a
/// non-chat / unopenable wrap is a no-op.
pub async fn persist_incoming_chat(
    community: &CommunityV2,
    wrap: &nostr_sdk::Event,
    channel_id: &str,
    my_pubkey: &PublicKey,
    session: &crate::state::SessionGuard,
) -> Option<ChatPersist> {
    let ch = community.channels.iter().find(|c| crate::simd::hex::bytes_to_hex_32(&c.id.0) == channel_id)?;
    // A keyless private channel is unreadable — never derive it from the root plane.
    if ch.private && ch.key.is_none() {
        return None;
    }
    let (secret, epoch) = community.channel_secret(ch);
    let group = super::derive::channel_group_key(&secret, &ch.id, epoch);
    let event = chat::open_chat_event(wrap, &group, &ch.id, epoch).ok()?;
    let outcome = {
        let mut st = crate::state::STATE.lock().await;
        // A swap can land on the lock await: only mutate THIS account's STATE.
        if !session.is_valid() {
            return None;
        }
        apply_chat_to_state(&mut st, &event, channel_id, my_pubkey)
    }?;
    // …and only persist to THIS account's DB (the save straddles an await).
    if !session.is_valid() {
        return None;
    }
    persist_chat(channel_id, &outcome).await;
    Some(outcome)
}

/// Persist an [`apply_chat_to_state`] outcome to the shared events DB — async, run by the
/// caller AFTER the STATE lock drops (a message row carries its reactions, so a reaction
/// re-saves the row; a delete drops it).
pub async fn persist_chat(channel_id: &str, outcome: &ChatPersist) {
    match outcome {
        ChatPersist::New(m) => {
            let _ = crate::db::events::save_message(channel_id, m).await;
        }
        // An edit is event-sourced: save the MESSAGE_EDIT row (folded on reload), never a
        // row overwrite. A reaction rides the message row, so re-save it.
        ChatPersist::Updated { message, edit_event } => match edit_event {
            Some(ev) => {
                let mut ev = (**ev).clone();
                // get-or-CREATE: a lookup-only id would leave a fresh channel's edit at
                // chat_id 0 (orphaned, dropped on the reload fold).
                if let Ok(cid) = crate::db::id_cache::get_or_create_chat_id(channel_id) {
                    ev.chat_id = cid;
                }
                let _ = crate::db::events::save_event(&ev).await;
            }
            None => {
                let _ = crate::db::events::save_message(channel_id, message).await;
            }
        },
        ChatPersist::Removed(id) => {
            let _ = crate::db::events::delete_event(id).await;
        }
    }
}

/// The typed outcome of dispatching one v2 wrap — the callback that fired (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchedV2 {
    /// A new chat message on `channel_id` (hex).
    Message { channel_id: String, message_id: String },
    /// A reaction/edit updating `target_id` on `channel_id`.
    Update { channel_id: String, target_id: String },
    /// A delete removing `target_id` on `channel_id`.
    Removed { channel_id: String, target_id: String },
    /// A typing indicator from `npub` on `channel_id`.
    Typing { channel_id: String, npub: String },
    /// A Guestbook join/leave for `npub`.
    Presence { npub: String, joined: bool },
    /// A wrap on this community's Control Plane — its metadata/channel set may
    /// have changed. Recognized here (address match) but NOT folded: the fold
    /// needs the whole edition chain, so the realtime layer re-fetches + re-folds
    /// + re-subscribes. `community_id` is hex.
    Control { community_id: String },
    /// A wrap on one of this community's next-epoch rekey planes — a rotation is in
    /// flight. Recognized by address; the realtime layer runs the stateful catch-up
    /// ([`super::service::follow_rekeys`]) across every scope. `community_id` is hex.
    Rekey { community_id: String },
    /// A verified owner-signed tombstone at the dissolved plane (CORD-02 §9): the
    /// community is dead. The realtime layer seals it read-only. `community_id` is hex.
    Dissolved { community_id: String },
    /// The wrap opened on a v2 plane but carries nothing the handler renders
    /// (e.g. a WebXDC signal, or a kick we don't surface in the first cut).
    Ignored,
    /// Not a v2 plane of this community — try elsewhere / drop.
    NotOurs,
}

/// Dispatch a received kind-1059 wrap for `community`: route it to the right
/// plane, bridge it, and fire the matching [`InboundEventHandler`] callback.
/// Returns what fired. Purely in-memory + callback — persistence is the caller's
/// (the service/realtime layer) so this stays offline-testable.
pub fn dispatch_wrap(
    wrap: &nostr_sdk::Event,
    community: &CommunityV2,
    my_pubkey: &PublicKey,
    handler: &dyn InboundEventHandler,
) -> DispatchedV2 {
    // 1. Chat planes: try each held channel by its group key (author match).
    for ch in &community.channels {
        // A keyless private channel is UNREADABLE — never address it at the root plane
        // (channel_secret falls back to the root, which would be a private→public leak).
        if ch.private && ch.key.is_none() {
            continue;
        }
        let (secret, epoch) = community.channel_secret(ch);
        let group = super::derive::channel_group_key(&secret, &ch.id, epoch);
        if wrap.pubkey != group.pk() {
            continue;
        }
        let Ok(event) = chat::open_chat_event(wrap, &group, &ch.id, epoch) else {
            return DispatchedV2::NotOurs;
        };
        let channel_id = crate::simd::hex::bytes_to_hex_32(&ch.id.0);
        return dispatch_chat_event(&event, &channel_id, my_pubkey, handler);
    }

    // 2. Guestbook plane: join/leave presence.
    let gb = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    if wrap.pubkey == gb.pk() {
        if let Ok(opened) = stream::open_wrap(wrap, &gb) {
            if let Ok(ev) = guestbook::parse_guestbook_event(&opened) {
                return dispatch_guestbook(&ev.entry, community, handler);
            }
        }
        return DispatchedV2::Ignored;
    }

    // 3. Control plane: a metadata/channel edition. Recognized by address only —
    // the fold needs the whole chain, which the realtime layer re-fetches. Shares
    // one address helper with the subscription so the two can't drift.
    if wrap.pubkey == super::realtime::control_author(community) {
        return DispatchedV2::Control { community_id: crate::simd::hex::bytes_to_hex_32(&community.id().0) };
    }

    // 4. Rekey planes: a rotation in flight (base or a private channel), addressed
    // at the next epoch. Same author-set the subscription rides — one source of
    // truth ([`super::realtime::rekey_authors`]) so recognition and subscription
    // can't drift.
    if super::realtime::rekey_authors(community).iter().any(|p| *p == wrap.pubkey) {
        return DispatchedV2::Rekey { community_id: crate::simd::hex::bytes_to_hex_32(&community.id().0) };
    }

    // 5. Dissolved plane: the terminal tombstone (CORD-02 §9). Honor ONLY a valid
    // owner seal — a foreign event at this public address is noise.
    if wrap.pubkey == super::derive::dissolved_group_key(community.id()).pk() {
        if super::dissolution::verify_dissolved(wrap, &community.identity) {
            return DispatchedV2::Dissolved { community_id: crate::simd::hex::bytes_to_hex_32(&community.id().0) };
        }
        return DispatchedV2::Ignored;
    }

    DispatchedV2::NotOurs
}

fn dispatch_chat_event(
    event: &ChatEvent,
    channel_id: &str,
    my_pubkey: &PublicKey,
    handler: &dyn InboundEventHandler,
) -> DispatchedV2 {
    match event {
        ChatEvent::Message { opened, reply_to, emoji } => {
            let msg = chat_message_to_message(opened, reply_to, emoji, my_pubkey);
            let message_id = msg.id.clone();
            handler.on_community_message(channel_id, &msg, true);
            DispatchedV2::Message { channel_id: channel_id.to_string(), message_id }
        }
        ChatEvent::Reaction { opened, target, emoji, emoji_url, .. } => {
            // Surface as an update on the target message; the fold applies it.
            let target_id = crate::simd::hex::bytes_to_hex_32(target);
            let mut msg = chat_message_to_message(opened, &None, &[], my_pubkey);
            msg.content = emoji.clone();
            if let Some(url) = emoji_url {
                msg.emoji_tags = vec![EmojiTag { shortcode: emoji.clone(), url: url.clone() }];
            }
            handler.on_community_update(channel_id, &target_id, &msg);
            DispatchedV2::Update { channel_id: channel_id.to_string(), target_id }
        }
        // Edit + Delete are AUTHOR-SCOPED (only the original author may edit/delete their
        // own message). The author check needs the resident target, so its callback is
        // fired by the realtime layer FROM the persist outcome — never optimistically here,
        // or a forged edit/delete would grief the live view and then reappear on reload.
        ChatEvent::Edit { target, .. } => {
            DispatchedV2::Update { channel_id: channel_id.to_string(), target_id: crate::simd::hex::bytes_to_hex_32(target) }
        }
        ChatEvent::Delete { target, .. } => {
            DispatchedV2::Removed { channel_id: channel_id.to_string(), target_id: crate::simd::hex::bytes_to_hex_32(target) }
        }
        ChatEvent::Typing { opened } => {
            let npub = opened.author.to_bech32().unwrap_or_default();
            let until = opened.at_ms / 1000 + 30;
            handler.on_community_typing(channel_id, &npub, until);
            DispatchedV2::Typing { channel_id: channel_id.to_string(), npub }
        }
        ChatEvent::Webxdc { .. } => DispatchedV2::Ignored,
    }
}

fn dispatch_guestbook(ev: &GuestbookEntry, community: &CommunityV2, handler: &dyn InboundEventHandler) -> DispatchedV2 {
    // Presence is announced against the community, keyed to `#general` for the
    // handler's channel-scoped signature (v1's convention).
    let chat_id = community
        .channels
        .first()
        .map(|c| crate::simd::hex::bytes_to_hex_32(&c.id.0))
        .unwrap_or_default();
    match ev {
        GuestbookEntry::Join { member, at_ms, invited_by } => {
            let npub = member.to_bech32().unwrap_or_default();
            let (by, label) = match invited_by {
                Some((c, l)) => (Some(c.as_str()), Some(l.as_str())),
                None => (None, None),
            };
            handler.on_community_presence(&chat_id, &npub, true, "", at_ms / 1000, by, label);
            DispatchedV2::Presence { npub, joined: true }
        }
        GuestbookEntry::Leave { member, at_ms } => {
            let npub = member.to_bech32().unwrap_or_default();
            handler.on_community_presence(&chat_id, &npub, false, "", at_ms / 1000, None, None);
            DispatchedV2::Presence { npub, joined: false }
        }
        // Kicks/snapshots aren't surfaced to the handler in the first cut.
        GuestbookEntry::Kick { .. } | GuestbookEntry::Snapshot { .. } => DispatchedV2::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::service;
    use crate::community::transport::memory::MemoryRelay;
    use crate::community::transport::Transport;
    use nostr_sdk::prelude::Keys;
    use std::sync::Mutex;

    /// A handler that records every callback it receives.
    #[derive(Default)]
    struct Recorder {
        messages: Mutex<Vec<(String, Message)>>,
        updates: Mutex<Vec<(String, String)>>,
        removed: Mutex<Vec<(String, String)>>,
        presence: Mutex<Vec<(String, bool)>>,
    }
    impl InboundEventHandler for Recorder {
        fn on_community_message(&self, chat_id: &str, msg: &Message, _is_new: bool) {
            self.messages.lock().unwrap().push((chat_id.to_string(), msg.clone()));
        }
        fn on_community_update(&self, chat_id: &str, target: &str, _msg: &Message) {
            self.updates.lock().unwrap().push((chat_id.to_string(), target.to_string()));
        }
        fn on_community_removed(&self, chat_id: &str, target: &str) {
            self.removed.lock().unwrap().push((chat_id.to_string(), target.to_string()));
        }
        fn on_community_presence(&self, _c: &str, npub: &str, joined: bool, _e: &str, _a: u64, _b: Option<&str>, _l: Option<&str>) {
            self.presence.lock().unwrap().push((npub.to_string(), joined));
        }
    }

    fn init() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>, Keys) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(90_000);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut acct = String::from("npub1");
        let mut v = n as usize;
        for _ in 0..58 {
            acct.push(B[v % 32] as char);
            v = v / 32 + 7;
        }
        std::fs::create_dir_all(tmp.path().join(&acct)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(acct.clone()).unwrap();
        crate::db::init_database(&acct).unwrap();
        let _ = crate::state::take_nostr_client();
        let me = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&me, &[]);
        crate::state::set_my_public_key(me.public_key());
        (tmp, guard, me)
    }

    #[tokio::test]
    async fn a_received_message_wrap_fires_on_community_message() {
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "In", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        service::send_message(&relay, &community, &general, "ping").await.unwrap();

        // Fetch the raw wrap the way a live sub would deliver it, and dispatch it.
        let authors = vec![super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch).pk_hex()];
        let q = crate::community::transport::Query { kinds: vec![stream::KIND_WRAP], authors, ..Default::default() };
        let wraps = relay.fetch(&q, &community.relays).await.unwrap();

        let rec = Recorder::default();
        let mut fired = Vec::new();
        for w in &wraps {
            fired.push(dispatch_wrap(w, &community, &me.public_key(), &rec));
        }
        let msgs = rec.messages.lock().unwrap();
        assert_eq!(msgs.len(), 1, "exactly one message dispatched");
        assert_eq!(msgs[0].1.content, "ping");
        assert_eq!(msgs[0].0, crate::simd::hex::bytes_to_hex_32(&general.0), "chat_id is the channel hex");
        assert!(msgs[0].1.mine, "authored by me");
        assert!(fired.iter().any(|d| matches!(d, DispatchedV2::Message { .. })));
    }

    #[tokio::test]
    async fn a_guestbook_join_wrap_fires_presence() {
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        // create_community publishes the owner's genesis Join to the guestbook.
        let community = service::create_community(&relay, "GB", vec!["wss://r".into()], None).await.unwrap();
        let gb = super::super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
        let q = crate::community::transport::Query { kinds: vec![stream::KIND_WRAP], authors: vec![gb.pk_hex()], ..Default::default() };
        let wraps = relay.fetch(&q, &community.relays).await.unwrap();

        let rec = Recorder::default();
        for w in &wraps {
            dispatch_wrap(w, &community, &me.public_key(), &rec);
        }
        let pres = rec.presence.lock().unwrap();
        assert_eq!(pres.len(), 1, "the owner's genesis Join fires one presence");
        assert!(pres[0].1, "it's a join");
        assert_eq!(pres[0].0, me.public_key().to_bech32().unwrap());
    }

    #[tokio::test]
    async fn v2_chat_events_persist_into_the_shared_store() {
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "Persist", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);
        let group = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let me_hex = me.public_key().to_hex();

        let msg_id = service::send_message(&relay, &community, &general, "persist me").await.unwrap();
        service::send_reaction(&relay, &community, &general, &msg_id, &me_hex, "🔥", None).await.unwrap();

        // Open every chat wrap, oldest first (fetch_channel's order — a target must be
        // resident before its reaction lands).
        let q = crate::community::transport::Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], ..Default::default() };
        let wraps = relay.fetch(&q, &community.relays).await.unwrap();
        let mut events: Vec<ChatEvent> = wraps.iter().filter_map(|w| chat::open_chat_event(w, &group, &general, community.root_epoch).ok()).collect();
        events.sort_by_key(|e| e.opened().at_ms);

        let mut new = 0;
        for ev in &events {
            let outcome = {
                let mut st = crate::state::STATE.lock().await;
                apply_chat_to_state(&mut st, ev, &cid, &me.public_key())
            };
            if let Some(o) = outcome {
                if matches!(o, ChatPersist::New(_)) {
                    new += 1;
                }
                persist_chat(&cid, &o).await;
            }
        }
        assert_eq!(new, 1, "exactly one new message persisted");
        assert!(crate::db::events::event_exists(&msg_id).unwrap(), "the message row is in the shared events store");
        let reacted = {
            let st = crate::state::STATE.lock().await;
            st.find_message(&msg_id).map(|(_, m)| m.reactions.iter().any(|r| r.emoji == "🔥")).unwrap_or(false)
        };
        assert!(reacted, "the reaction aggregated onto the stored message");

        // Re-applying the same message event dedups against the DB.
        let dup = {
            let mut st = crate::state::STATE.lock().await;
            apply_chat_to_state(&mut st, &events[0], &cid, &me.public_key())
        };
        assert!(dup.is_none(), "a known message dedups on re-apply");
    }

    #[tokio::test]
    async fn a_v2_edit_persists_as_a_folded_edit_event() {
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "Edit", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);
        let group = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);

        let msg_id = service::send_message(&relay, &community, &general, "original").await.unwrap();
        service::send_edit(&relay, &community, &general, &msg_id, "edited!").await.unwrap();

        // Apply messages BEFORE their edits (a target must be resident to edit).
        let q = crate::community::transport::Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], ..Default::default() };
        let wraps = relay.fetch(&q, &community.relays).await.unwrap();
        let mut events: Vec<ChatEvent> = wraps.iter().filter_map(|w| chat::open_chat_event(w, &group, &general, community.root_epoch).ok()).collect();
        events.sort_by_key(|e| (!matches!(e, ChatEvent::Message { .. }), e.opened().at_ms));
        for ev in &events {
            let outcome = {
                let mut st = crate::state::STATE.lock().await;
                apply_chat_to_state(&mut st, ev, &cid, &me.public_key())
            };
            if let Some(o) = outcome {
                persist_chat(&cid, &o).await;
            }
        }

        let content = {
            let st = crate::state::STATE.lock().await;
            st.find_message(&msg_id).map(|(_, m)| m.content)
        };
        assert_eq!(content.as_deref(), Some("edited!"), "the edit applied to the stored message");
        let edit_id = events.iter().find_map(|e| matches!(e, ChatEvent::Edit { .. }).then(|| e.opened().rumor_id.to_hex())).unwrap();
        assert!(crate::db::events::event_exists(&edit_id).unwrap(), "the MESSAGE_EDIT event is persisted (folds on reload)");
    }

    #[tokio::test]
    async fn a_forged_delete_from_a_non_author_is_ignored() {
        use nostr_sdk::prelude::Timestamp;
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "Forge", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);
        let group = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);

        // `me` posts a message and it's persisted into STATE.
        let msg_id = service::send_message(&relay, &community, &general, "mine").await.unwrap();
        let q = crate::community::transport::Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], ..Default::default() };
        let wraps = relay.fetch(&q, &community.relays).await.unwrap();
        for w in &wraps {
            if let Ok(ev) = chat::open_chat_event(w, &group, &general, community.root_epoch) {
                let mut st = crate::state::STATE.lock().await;
                apply_chat_to_state(&mut st, &ev, &cid, &me.public_key());
            }
        }

        // A STRANGER (a member, so holds the channel key) forges a delete of `me`'s message.
        let stranger = nostr_sdk::prelude::Keys::generate();
        let del = chat::build_delete_rumor(stranger.public_key(), &general, community.root_epoch, &msg_id, super::super::kind::MESSAGE, 9_000);
        let (wrap, _) = chat::seal_chat_rumor(&del, &group, &stranger, Timestamp::from_secs(9), false).unwrap();
        let event = chat::open_chat_event(&wrap, &group, &general, community.root_epoch).unwrap();

        let outcome = {
            let mut st = crate::state::STATE.lock().await;
            apply_chat_to_state(&mut st, &event, &cid, &me.public_key())
        };
        assert!(outcome.is_none(), "a forged delete from a non-author yields no removal");
        let survives = {
            let st = crate::state::STATE.lock().await;
            st.find_message(&msg_id).is_some()
        };
        assert!(survives, "the message survives the forged delete (live view + DB stay consistent)");
    }

    #[tokio::test]
    async fn a_foreign_wrap_is_not_ours() {
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "X", vec!["wss://r".into()], None).await.unwrap();

        // A wrap from an unrelated stream key (e.g. a DM giftwrap, or another
        // community) must not match any plane.
        let stranger = super::super::derive::channel_group_key(&[0x99u8; 32], &community.channels[0].id, community.root_epoch);
        let rumor = chat::build_message_rumor(me.public_key(), &community.channels[0].id, community.root_epoch, "not yours", None, &[], vec![], 1_000);
        let (wrap, _) = chat::seal_chat_rumor(&rumor, &stranger, &me, nostr_sdk::prelude::Timestamp::from_secs(1), false).unwrap();

        let rec = Recorder::default();
        assert_eq!(dispatch_wrap(&wrap, &community, &me.public_key(), &rec), DispatchedV2::NotOurs);
        assert!(rec.messages.lock().unwrap().is_empty());
    }
}
