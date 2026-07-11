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
        addressed_bots: crate::bot_interface::addressed_bots(opened.rumor.tags.iter()),
        wrapper_event_id: Some(opened.wrapper_id.to_hex()),
        ..Default::default()
    }
}

/// Whether `author` sits on the banlist of the community owning `channel_id`
/// (fail-open on a lookup error: availability must never hide honest traffic —
/// the ban re-applies on the next fold).
fn author_is_banned_here(channel_id: &str, author: &PublicKey) -> bool {
    let Ok(Some(cid_hex)) = crate::db::community::community_id_for_channel(channel_id) else {
        return false;
    };
    let banned = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default();
    !banned.is_empty() && banned.contains(&author.to_hex())
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
    // CORD-04 §4: a banned npub VANISHES — every chat event they author (message,
    // reaction, edit, delete) is dropped at fold time. Severance (the rekey) only
    // cuts their READ of new epochs; they still hold old epoch keys and can post
    // to old planes forever — refusing to fold them is what makes the ban hold.
    // The persisted banlist can never name the owner (the authority fold refuses
    // a ban whose target is position 0), so no owner exemption is needed here.
    if author_is_banned_here(channel_id, &event.opened().author) {
        return None;
    }
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
                // npub, not hex: the frontend's whole reaction contract (profile
                // name resolve + the own-reaction highlight vs strPubkey) keys on
                // bech32 — exactly what v1 stores.
                author_id: opened.author.to_bech32().unwrap_or_else(|_| opened.author.to_hex()),
                emoji: emoji.clone(),
                emoji_url: emoji_url.clone(),
            };
            let (_c, added) = state.add_reaction_to_message(&target_id, reaction)?;
            added.then(|| state.find_message(&target_id).map(|(_c, m)| ChatPersist::Updated { message: m, edit_event: None }))?
        }
        ChatEvent::Edit { opened, target, new_content } => {
            // Dedup by the edit's own rumor id (its MESSAGE_EDIT row below): the
            // in-message `apply_edit` dedups silently, so without this a re-wrapped
            // duplicate would still return Updated and re-fire the handler — the
            // replay hole the persist-gated callbacks exist to close.
            if crate::db::events::event_exists(&opened.rumor_id.to_hex()).unwrap_or(false) {
                return None;
            }
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

/// Apply an already-opened chat event to STATE + the shared store — the LIVE
/// counterpart of [`crate::VectorCore::v2_backfill_channel`]'s catch-up persistence.
/// The dispatcher opened the wrap (so nothing decrypts twice); the returned outcome
/// is what the caller's callbacks fire from — a duplicate, a non-resident target,
/// or a forged edit/delete yields `None` and nothing re-fires.
pub async fn persist_chat_event(
    event: &ChatEvent,
    channel_id: &str,
    my_pubkey: &PublicKey,
    session: &crate::state::SessionGuard,
) -> Option<ChatPersist> {
    let outcome = {
        let mut st = crate::state::STATE.lock().await;
        // A swap can land on the lock await: only mutate THIS account's STATE.
        if !session.is_valid() {
            return None;
        }
        apply_chat_to_state(&mut st, event, channel_id, my_pubkey)
    }?;
    // Resolve a reply's preview (content/npub) from the DB before persist + emit
    // (v1 parity): the parent is often persisted but outside the in-memory window,
    // and without this the live render shows a reply with no context.
    let outcome = match outcome {
        ChatPersist::New(mut m) => {
            if !m.replied_to.is_empty() {
                let _ = crate::db::events::populate_reply_context(&mut m).await;
            }
            ChatPersist::New(m)
        }
        o => o,
    };
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

/// The typed outcome of dispatching one v2 wrap.
#[derive(Debug, Clone)]
pub enum DispatchedV2 {
    /// An OPENED chat event (message/reaction/edit/delete) on `channel_id` (hex),
    /// NOT yet applied. The realtime layer runs it through [`persist_chat_event`]
    /// and fires the matching callback from the outcome — so a re-wrapped
    /// duplicate (any keyholder can re-wrap a signed seal into a fresh 1059) or a
    /// forged edit/delete never re-fires a handler, exactly v1's model.
    Chat { channel_id: String, event: Box<ChatEvent> },
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

/// Dispatch a received kind-1059 wrap for `community`: route it to the plane it
/// opens under. Chat events are returned OPENED (the realtime layer persists,
/// then fires callbacks from the outcome); only the non-persisted kinds (typing,
/// guestbook presence) fire their callback inline here. Purely in-memory — so
/// this stays offline-testable.
pub fn dispatch_wrap(
    wrap: &nostr_sdk::Event,
    community: &CommunityV2,
    _my_pubkey: &PublicKey,
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
        return dispatch_chat_event(event, &channel_id, handler);
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

fn dispatch_chat_event(event: ChatEvent, channel_id: &str, handler: &dyn InboundEventHandler) -> DispatchedV2 {
    match event {
        // Typing is ephemeral (never persisted) — the one chat kind fired inline,
        // so it carries its own CORD-04 banned-author drop (the persisted kinds
        // get theirs in `apply_chat_to_state`).
        ChatEvent::Typing { opened } => {
            if author_is_banned_here(channel_id, &opened.author) {
                return DispatchedV2::Ignored;
            }
            let npub = opened.author.to_bech32().unwrap_or_default();
            let until = opened.at_ms / 1000 + 30;
            handler.on_community_typing(channel_id, &npub, until);
            DispatchedV2::Typing { channel_id: channel_id.to_string(), npub }
        }
        ChatEvent::Webxdc { .. } => DispatchedV2::Ignored,
        // Message/Reaction/Edit/Delete all persist first; their callbacks fire from
        // the outcome (dedup + author checks), never optimistically.
        event => DispatchedV2::Chat { channel_id: channel_id.to_string(), event: Box::new(event) },
    }
}

fn dispatch_guestbook(ev: &GuestbookEntry, community: &CommunityV2, handler: &dyn InboundEventHandler) -> DispatchedV2 {
    // CORD-04 §4: a banned npub's presence announcements vanish with the rest of
    // their events (the memberlist fold subtracts them separately).
    if let GuestbookEntry::Join { member, .. } | GuestbookEntry::Leave { member, .. } = ev {
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let banned = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default();
        if banned.contains(&member.to_hex()) {
            return DispatchedV2::Ignored;
        }
    }
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
    async fn a_received_message_wrap_opens_then_fires_from_the_persist_outcome() {
        use nostr_sdk::prelude::Timestamp;
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "In", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);

        // ANOTHER member (holds the root) posts — the incoming case, so no local
        // send echo pre-persisted it.
        let member = Keys::generate();
        let group = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let rumor = chat::build_message_rumor(member.public_key(), &general, community.root_epoch, "ping", None, &[], vec![], 5_000);
        let (wrap, _) = chat::seal_chat_rumor(&rumor, &group, &member, Timestamp::from_secs(5), false).unwrap();

        // Dispatch OPENS the event but fires no message callback — that belongs to
        // the persist outcome (dedup + author checks), v1's model.
        let rec = Recorder::default();
        let dispatched = dispatch_wrap(&wrap, &community, &me.public_key(), &rec);
        assert!(rec.messages.lock().unwrap().is_empty(), "no optimistic message callback");
        let DispatchedV2::Chat { channel_id, event } = dispatched else {
            panic!("a chat wrap dispatches as Chat");
        };
        assert_eq!(channel_id, cid);

        let session = crate::state::SessionGuard::capture();
        let outcome = persist_chat_event(&event, &channel_id, &me.public_key(), &session).await;
        let Some(ChatPersist::New(msg)) = outcome else {
            panic!("the first delivery persists as New");
        };
        assert_eq!(msg.content, "ping");
        assert!(!msg.mine, "authored by the other member");

        // A RE-WRAP of the same signed rumor (any keyholder can mint one) is a
        // fresh outer event, but the persist dedups on the inner id — no re-fire.
        let (rewrap, _) = chat::seal_chat_rumor(&rumor, &group, &member, Timestamp::from_secs(6), false).unwrap();
        assert_ne!(rewrap.id, wrap.id, "a re-wrap is a distinct outer event");
        let DispatchedV2::Chat { event: dup, .. } = dispatch_wrap(&rewrap, &community, &me.public_key(), &rec) else {
            panic!("the re-wrap still opens");
        };
        assert!(
            persist_chat_event(&dup, &channel_id, &me.public_key(), &session).await.is_none(),
            "a re-wrapped duplicate yields no outcome (nothing re-fires)"
        );
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
        service::send_reaction(&relay, &community, &general, &msg_id, &me_hex, super::super::kind::MESSAGE, "🔥", None).await.unwrap();

        // The SEND ECHO persisted both immediately — send-then-read works with no
        // listen loop (the INT-W3 contract).
        assert!(crate::db::events::event_exists(&msg_id).unwrap(), "the send echo persisted the message row");
        let reacted = {
            let st = crate::state::STATE.lock().await;
            st.find_message(&msg_id).map(|(_, m)| m.reactions.iter().any(|r| r.emoji == "🔥")).unwrap_or(false)
        };
        assert!(reacted, "the send echo aggregated the reaction onto the stored message");

        // The relay's copies of our own sends then arrive — every one dedups
        // against the echoed rows (no double rows, no re-fires).
        let q = crate::community::transport::Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], ..Default::default() };
        let wraps = relay.fetch(&q, &community.relays).await.unwrap();
        let mut events: Vec<ChatEvent> = wraps.iter().filter_map(|w| chat::open_chat_event(w, &group, &general, community.root_epoch).ok()).collect();
        events.sort_by_key(|e| e.opened().at_ms);
        assert!(!events.is_empty());
        for ev in &events {
            let outcome = {
                let mut st = crate::state::STATE.lock().await;
                apply_chat_to_state(&mut st, ev, &cid, &me.public_key())
            };
            assert!(outcome.is_none(), "the relay echo of an already-echoed send dedups");
        }
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
    async fn a_reaction_after_a_rekey_aggregates_onto_a_prior_epoch_message() {
        // Sync stability across a rekey: a message written at epoch 0 and a reaction
        // to it written at epoch 1 (its wrap sealed + bound under the NEW epoch key)
        // must still aggregate — STATE keys by rumor id, not epoch, so a reaction to
        // pre-refound history lands. The reaction's OWN binding is epoch 1; the
        // target's is epoch 0.
        use nostr_sdk::prelude::Timestamp;
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "Rekeyed", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);
        let session = crate::state::SessionGuard::capture();

        // Epoch-0 message, opened under the epoch-0 public key, persisted.
        let member = Keys::generate();
        let g0 = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let msg = chat::build_message_rumor(member.public_key(), &general, community.root_epoch, "before the rekey", None, &[], vec![], 5_000);
        let msg_id = msg.id.unwrap().to_hex();
        let (mw, _) = chat::seal_chat_rumor(&msg, &g0, &member, Timestamp::from_secs(5), false).unwrap();
        let ev = chat::open_chat_event(&mw, &g0, &general, community.root_epoch).unwrap();
        assert!(matches!(persist_chat_event(&ev, &cid, &me.public_key(), &session).await, Some(ChatPersist::New(_))));

        // Epoch-1 reaction (same root, next epoch → a distinct channel key) to that
        // epoch-0 message, opened under the epoch-1 key.
        let next = crate::community::Epoch(community.root_epoch.0 + 1);
        let g1 = super::super::derive::channel_group_key(&community.community_root, &general, next);
        let reaction = chat::build_reaction_rumor(member.public_key(), &general, next, &msg_id, &member.public_key().to_hex(), super::super::kind::MESSAGE, "🎉", None, 6_000);
        let (rw, _) = chat::seal_chat_rumor(&reaction, &g1, &member, Timestamp::from_secs(6), false).unwrap();
        let rev = chat::open_chat_event(&rw, &g1, &general, next).unwrap();
        let outcome = persist_chat_event(&rev, &cid, &me.public_key(), &session).await;
        assert!(matches!(outcome, Some(ChatPersist::Updated { .. })), "the cross-epoch reaction updates the target");

        let reaction_author = {
            let st = crate::state::STATE.lock().await;
            st.find_message(&msg_id)
                .and_then(|(_, m)| m.reactions.iter().find(|r| r.emoji == "🎉").map(|r| r.author_id.clone()))
        };
        let author = reaction_author.expect("the epoch-1 reaction aggregated onto the epoch-0 message");
        // npub, never hex: the frontend resolves the reactor's profile and detects
        // "my reaction" by comparing against the user's npub.
        assert_eq!(author, member.public_key().to_bech32().unwrap(), "reaction author is stored as bech32");
    }

    #[tokio::test]
    async fn a_v2_message_with_an_imeta_attachment_surfaces_as_an_attachment() {
        use nostr_sdk::prelude::Timestamp;
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "Files", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);
        let group = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let session = crate::state::SessionGuard::capture();

        // Build a valid NIP-92 imeta tag via the same encoder the v2 file pipeline
        // uses, so the round-trip mirrors production exactly.
        let attachment = crate::types::Attachment {
            id: "a".repeat(64),
            key: "0".repeat(64),
            nonce: "1".repeat(32),
            extension: "png".into(),
            name: "photo.png".into(),
            url: "https://blossom.example/abc".into(),
            path: String::new(),
            size: 4096,
            img_meta: None,
            downloading: false,
            downloaded: false,
            webxdc_topic: None,
            group_id: None,
            original_hash: Some("b".repeat(64)),
            scheme_version: None,
            mls_filename: None,
        };
        let imeta = crate::community::attachments::attachment_to_imeta(&attachment);
        let member = Keys::generate();
        let rumor = chat::build_message_rumor(member.public_key(), &general, community.root_epoch, "here's a file", None, &[], vec![imeta], 5_000);
        let (wrap, _) = chat::seal_chat_rumor(&rumor, &group, &member, Timestamp::from_secs(5), false).unwrap();
        let ev = chat::open_chat_event(&wrap, &group, &general, community.root_epoch).unwrap();

        let outcome = persist_chat_event(&ev, &cid, &me.public_key(), &session).await;
        let Some(ChatPersist::New(msg)) = outcome else {
            panic!("the file message persists as new");
        };
        assert_eq!(msg.attachments.len(), 1, "the imeta tag parsed into one attachment");
        let att = &msg.attachments[0];
        assert!(att.url.contains("blossom.example"), "attachment url carried through: {}", att.url);
        assert_eq!(att.extension, "png", "extension carried through");
    }

    #[tokio::test]
    async fn a_banned_members_every_chat_event_is_dropped_on_sight() {
        // CORD-04 §4: a banned npub vanishes — message, reaction, edit, delete,
        // typing, and presence alike. Severance only cuts their READ; this fold
        // gate is what makes the ban hold against old-epoch keys they still have.
        use nostr_sdk::prelude::Timestamp;
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "BanGate", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let group = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let session = crate::state::SessionGuard::capture();
        let rogue = Keys::generate();

        // Pre-ban: the rogue's message folds like anyone's.
        let m1 = chat::build_message_rumor(rogue.public_key(), &general, community.root_epoch, "pre-ban", None, &[], vec![], 5_000);
        let m1_id = m1.id.unwrap().to_hex();
        let (w1, _) = chat::seal_chat_rumor(&m1, &group, &rogue, Timestamp::from_secs(5), false).unwrap();
        let ev = chat::open_chat_event(&w1, &group, &general, community.root_epoch).unwrap();
        assert!(matches!(persist_chat_event(&ev, &cid, &me.public_key(), &session).await, Some(ChatPersist::New(_))));

        // The ban lands (the fold's persisted banlist).
        crate::db::community::set_community_banlist(&cid_hex, &[rogue.public_key().to_hex()], 1_000).unwrap();

        // Post-ban: every kind they author drops.
        let m2 = chat::build_message_rumor(rogue.public_key(), &general, community.root_epoch, "post-ban", None, &[], vec![], 6_000);
        let (w2, _) = chat::seal_chat_rumor(&m2, &group, &rogue, Timestamp::from_secs(6), false).unwrap();
        let ev = chat::open_chat_event(&w2, &group, &general, community.root_epoch).unwrap();
        assert!(persist_chat_event(&ev, &cid, &me.public_key(), &session).await.is_none(), "a banned message is dropped");

        let edit = chat::build_edit_rumor(rogue.public_key(), &general, community.root_epoch, &m1_id, "rewritten", 7_000);
        let (we, _) = chat::seal_chat_rumor(&edit, &group, &rogue, Timestamp::from_secs(7), false).unwrap();
        let ev = chat::open_chat_event(&we, &group, &general, community.root_epoch).unwrap();
        assert!(persist_chat_event(&ev, &cid, &me.public_key(), &session).await.is_none(), "a banned edit is dropped");

        let del = chat::build_delete_rumor(rogue.public_key(), &general, community.root_epoch, &m1_id, super::super::kind::MESSAGE, 8_000);
        let (wd, _) = chat::seal_chat_rumor(&del, &group, &rogue, Timestamp::from_secs(8), false).unwrap();
        let ev = chat::open_chat_event(&wd, &group, &general, community.root_epoch).unwrap();
        assert!(persist_chat_event(&ev, &cid, &me.public_key(), &session).await.is_none(), "a banned delete is dropped");
        assert!(
            crate::state::STATE.lock().await.find_message(&m1_id).is_some(),
            "their pre-ban message survives their own post-ban delete"
        );

        // Typing + presence fire inline — the dispatcher's own gate covers them.
        let rec = Recorder::default();
        let typ = chat::build_typing_rumor(rogue.public_key(), &general, community.root_epoch, 9_000);
        let (wt, _) = chat::seal_chat_rumor(&typ, &group, &rogue, Timestamp::from_secs(9), true).unwrap();
        assert!(matches!(dispatch_wrap(&wt, &community, &me.public_key(), &rec), DispatchedV2::Ignored));
        let gb = super::super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
        let join = guestbook::build_join_rumor(rogue.public_key(), None, 10_000);
        let (wj, _) = guestbook::seal_guestbook_rumor(&join, &gb, &rogue, Timestamp::from_secs(10)).unwrap();
        assert!(matches!(dispatch_wrap(&wj, &community, &me.public_key(), &rec), DispatchedV2::Ignored));
        assert!(rec.presence.lock().unwrap().is_empty(), "no presence callback for a banned join");

        // An innocent author still folds normally.
        let innocent = Keys::generate();
        let m3 = chat::build_message_rumor(innocent.public_key(), &general, community.root_epoch, "innocent", None, &[], vec![], 11_000);
        let (w3, _) = chat::seal_chat_rumor(&m3, &group, &innocent, Timestamp::from_secs(11), false).unwrap();
        let ev = chat::open_chat_event(&w3, &group, &general, community.root_epoch).unwrap();
        assert!(matches!(persist_chat_event(&ev, &cid, &me.public_key(), &session).await, Some(ChatPersist::New(_))));
    }

    #[tokio::test]
    async fn an_armada_threaded_reply_persists_and_fires_as_an_inline_reply() {
        use nostr_sdk::prelude::Timestamp;
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "Thread", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);
        let group = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let session = crate::state::SessionGuard::capture();

        // A member posts a root message, then a kind-1111 threaded reply to it
        // (the shape Armada sends).
        let member = Keys::generate();
        let root = chat::build_message_rumor(member.public_key(), &general, community.root_epoch, "thread root", None, &[], vec![], 5_000);
        let root_id = root.id.unwrap().to_hex();
        let (rw, _) = chat::seal_chat_rumor(&root, &group, &member, Timestamp::from_secs(5), false).unwrap();
        let reply = chat::build_comment_rumor(
            member.public_key(),
            &general,
            community.root_epoch,
            "thread reply",
            &root_id,
            super::super::kind::MESSAGE,
            &member.public_key().to_hex(),
            None,
            &[],
            6_000,
        );
        let reply_id = reply.id.unwrap().to_hex();
        let (tw, _) = chat::seal_chat_rumor(&reply, &group, &member, Timestamp::from_secs(6), false).unwrap();

        for w in [&rw, &tw] {
            let ev = chat::open_chat_event(w, &group, &general, community.root_epoch).unwrap();
            let outcome = persist_chat_event(&ev, &cid, &me.public_key(), &session).await;
            assert!(matches!(outcome, Some(ChatPersist::New(_))), "both persist as new messages");
        }
        // The reply row carries its parent as inline reply context, resolved
        // from the persisted root (v1's reply-preview parity).
        let held = {
            let st = crate::state::STATE.lock().await;
            st.find_message(&reply_id).map(|(_, m)| m)
        }
        .expect("the threaded reply is resident");
        assert_eq!(held.replied_to, root_id, "the immediate parent is the reply context");
        assert_eq!(held.content, "thread reply");

        // Its author deletes it — target kind 1111 (the delete e/k grammar).
        let del = chat::build_delete_rumor(member.public_key(), &general, community.root_epoch, &reply_id, super::super::kind::COMMENT, 7_000);
        let (dw, _) = chat::seal_chat_rumor(&del, &group, &member, Timestamp::from_secs(7), false).unwrap();
        let ev = chat::open_chat_event(&dw, &group, &general, community.root_epoch).unwrap();
        let outcome = persist_chat_event(&ev, &cid, &me.public_key(), &session).await;
        assert!(matches!(outcome, Some(ChatPersist::Removed(id)) if id == reply_id), "the author's delete removes their thread reply");
    }

    #[tokio::test]
    async fn an_edit_replay_never_refires() {
        use nostr_sdk::prelude::Timestamp;
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "EditReplay", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);
        let group = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let session = crate::state::SessionGuard::capture();

        // Another member posts, then edits their own message.
        let member = Keys::generate();
        let msg = chat::build_message_rumor(member.public_key(), &general, community.root_epoch, "v1 text", None, &[], vec![], 5_000);
        let msg_id = msg.id.unwrap().to_hex();
        let (mw, _) = chat::seal_chat_rumor(&msg, &group, &member, Timestamp::from_secs(5), false).unwrap();
        let edit = chat::build_edit_rumor(member.public_key(), &general, community.root_epoch, &msg_id, "v2 text", 6_000);
        let (ew, _) = chat::seal_chat_rumor(&edit, &group, &member, Timestamp::from_secs(6), false).unwrap();
        for w in [&mw, &ew] {
            if let Ok(ev) = chat::open_chat_event(w, &group, &general, community.root_epoch) {
                let _ = persist_chat_event(&ev, &cid, &me.public_key(), &session).await;
            }
        }

        // A RE-WRAP of the same signed EDIT (fresh outer id) must not re-fire: the
        // MESSAGE_EDIT row dedups it, exactly like the other three chat kinds.
        let (replay, _) = chat::seal_chat_rumor(&edit, &group, &member, Timestamp::from_secs(7), false).unwrap();
        assert_ne!(replay.id, ew.id, "a re-wrap is a distinct outer event");
        let ev = chat::open_chat_event(&replay, &group, &general, community.root_epoch).unwrap();
        assert!(
            persist_chat_event(&ev, &cid, &me.public_key(), &session).await.is_none(),
            "a replayed edit yields no outcome (no handler re-fire)"
        );
    }

    #[tokio::test]
    async fn a_forged_edit_from_a_non_author_is_ignored() {
        // Author-scoping on edits (the counterpart to the forged-delete guard): a
        // member (holds the channel key) forges an EDIT of someone else's message.
        // It must not rewrite the content.
        use nostr_sdk::prelude::Timestamp;
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "EditGuard", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let cid = crate::simd::hex::bytes_to_hex_32(&general.0);
        let group = super::super::derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let session = crate::state::SessionGuard::capture();

        // The real author posts a message.
        let author = Keys::generate();
        let msg = chat::build_message_rumor(author.public_key(), &general, community.root_epoch, "original", None, &[], vec![], 5_000);
        let msg_id = msg.id.unwrap().to_hex();
        let (mw, _) = chat::seal_chat_rumor(&msg, &group, &author, Timestamp::from_secs(5), false).unwrap();
        let ev = chat::open_chat_event(&mw, &group, &general, community.root_epoch).unwrap();
        persist_chat_event(&ev, &cid, &me.public_key(), &session).await;

        // A stranger (member, holds the key) forges an edit of the author's message.
        let stranger = Keys::generate();
        let edit = chat::build_edit_rumor(stranger.public_key(), &general, community.root_epoch, &msg_id, "TAMPERED", 6_000);
        let (ew, _) = chat::seal_chat_rumor(&edit, &group, &stranger, Timestamp::from_secs(6), false).unwrap();
        let ev = chat::open_chat_event(&ew, &group, &general, community.root_epoch).unwrap();
        assert!(persist_chat_event(&ev, &cid, &me.public_key(), &session).await.is_none(), "a forged edit yields no outcome");

        let content = {
            let st = crate::state::STATE.lock().await;
            st.find_message(&msg_id).map(|(_, m)| m.content)
        };
        assert_eq!(content.as_deref(), Some("original"), "the message content is unchanged by the forged edit");
    }

    #[tokio::test]
    async fn a_reaction_cannot_be_injected_across_channels() {
        // A member holds BOTH channels' keys, so they can seal a valid reaction in
        // channel A whose target is a message resident in channel B. The
        // cross-channel guard (a reaction lands only on a same-channel target) must
        // drop it — else reactions could be injected onto messages in channels the
        // reaction was never sealed under.
        use nostr_sdk::prelude::Timestamp;
        let (_tmp, _guard, me) = init();
        let relay = MemoryRelay::new();
        let mut community = service::create_community(&relay, "TwoChan", vec!["wss://r".into()], None).await.unwrap();
        let chan_a = community.channels[0].id;
        let chan_b = service::create_public_channel(&relay, &community, "b").await.unwrap();
        community = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        let a_hex = crate::simd::hex::bytes_to_hex_32(&chan_a.0);
        let session = crate::state::SessionGuard::capture();

        // A message lives in channel B.
        let author = Keys::generate();
        let gb = super::super::derive::channel_group_key(&community.community_root, &chan_b, community.root_epoch);
        let msg = chat::build_message_rumor(author.public_key(), &chan_b, community.root_epoch, "in B", None, &[], vec![], 5_000);
        let msg_id = msg.id.unwrap().to_hex();
        let (mw, _) = chat::seal_chat_rumor(&msg, &gb, &author, Timestamp::from_secs(5), false).unwrap();
        let bev = chat::open_chat_event(&mw, &gb, &chan_b, community.root_epoch).unwrap();
        persist_chat_event(&bev, &crate::simd::hex::bytes_to_hex_32(&chan_b.0), &me.public_key(), &session).await;

        // A reaction sealed in channel A, targeting the channel-B message.
        let ga = super::super::derive::channel_group_key(&community.community_root, &chan_a, community.root_epoch);
        let reaction = chat::build_reaction_rumor(author.public_key(), &chan_a, community.root_epoch, &msg_id, &author.public_key().to_hex(), super::super::kind::MESSAGE, "💥", None, 6_000);
        let (rw, _) = chat::seal_chat_rumor(&reaction, &ga, &author, Timestamp::from_secs(6), false).unwrap();
        let aev = chat::open_chat_event(&rw, &ga, &chan_a, community.root_epoch).unwrap();
        // Applied under channel A (where it was sealed) — the target is in B.
        let outcome = persist_chat_event(&aev, &a_hex, &me.public_key(), &session).await;
        assert!(outcome.is_none(), "a cross-channel reaction is dropped");
        let reacted = {
            let st = crate::state::STATE.lock().await;
            st.find_message(&msg_id).map(|(_, m)| !m.reactions.is_empty()).unwrap_or(false)
        };
        assert!(!reacted, "the channel-B message gained no reaction from the channel-A injection");
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
        assert!(matches!(dispatch_wrap(&wrap, &community, &me.public_key(), &rec), DispatchedV2::NotOurs));
        assert!(rec.messages.lock().unwrap().is_empty());
    }
}
