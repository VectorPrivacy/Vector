//! Inbound processing: turn a verified, opened Community message into a `Message`
//! in `STATE` under its channel chat (→ app state). Pure conversion
//! (`build_message`) is separated from the STATE mutation (`ingest_message`) so the
//! conversion is unit-testable without any global state.

use nostr_sdk::prelude::{Event, PublicKey};
use nostr_sdk::ToBech32;

use super::envelope::{open_message_multi, OpenedMessage};
use super::Channel;
use crate::state::ChatState;
use crate::stored_event::event_kind;
use crate::types::Message;

/// Convert a verified [`OpenedMessage`] into a STATE `Message` via the SHARED content parser.
///
/// A Concord 3300 normalizes to a text rumor and runs through `rumor::process_rumor` — the exact same
/// path a NIP-17 DM text message takes — so content, reply ref, emoji, ms (incl. the future-clamp),
/// and author-by-conversation-type are parsed in ONE place for every transport. The only Concord-
/// specific layering is attachments: Concord carries NIP-92 `imeta` (multi-file + caption), already
/// parsed in `open_message`, so they're set on top of the shared text result.
/// Build a normalized `(RumorEvent, RumorContext)` from an opened Concord inner so it can run through
/// the SHARED `rumor::process_rumor`. `kind` is the canonical content kind the sub-kind maps to
/// (3300→14, 3301→reaction, 3302→edit, 3305→deletion). The binding/banlist/authority checks already
/// happened at the transport layer; this is purely the bridge to the shared content parser.
fn concord_rumor(
    opened: &OpenedMessage,
    kind: nostr_sdk::Kind,
    my_pubkey: &PublicKey,
) -> (crate::rumor::RumorEvent, crate::rumor::RumorContext) {
    use crate::rumor::{ConversationType, RumorContext, RumorEvent};
    (
        RumorEvent {
            id: opened.message_id,
            kind,
            content: opened.content.clone(),
            tags: opened.tags.clone(),
            created_at: opened.created_at,
            pubkey: opened.author,
        },
        RumorContext {
            sender: opened.author,
            is_mine: opened.author == *my_pubkey,
            conversation_id: opened.channel_id.to_hex(),
            conversation_type: ConversationType::Community,
        },
    )
}

pub fn build_message(opened: &OpenedMessage, my_pubkey: &PublicKey) -> Message {
    use crate::rumor::{process_rumor, RumorProcessingResult};
    let (rumor, ctx) = concord_rumor(opened, nostr_sdk::Kind::PrivateDirectMessage, my_pubkey);
    let mut msg = match process_rumor(rumor, ctx, &crate::db::get_download_dir()) {
        Ok(RumorProcessingResult::TextMessage(m)) => m,
        // A 3300 is always a caption/text message, so this never fires — but never drop a message on
        // a parser quirk: fall back to the minimal direct fields.
        _ => Message {
            id: opened.message_id.to_hex(),
            content: opened.content.clone(),
            at: opened.ms.unwrap_or_else(|| opened.created_at.as_secs().saturating_mul(1000)),
            mine: opened.author == *my_pubkey,
            npub: opened.author.to_bech32().ok(),
            ..Default::default()
        },
    };
    // Transport-specific: Concord attachments are NIP-92 imeta (already parsed). Link the outer wire
    // id for the shared dedup. The shared parser already set content/reply/emoji/ms/npub.
    msg.attachments = opened.attachments.clone();
    msg.wrapper_event_id = Some(opened.wrapper_id.to_hex());
    msg
}

/// Ingest a verified Community message into STATE under its channel chat, creating
/// the chat (as `ChatType::Community`) if absent. Returns the added `Message` (so the
/// caller can persist + emit it), or `None` if it was a duplicate (dedup on the inner
/// message id).
pub fn ingest_message(
    state: &mut ChatState,
    opened: &OpenedMessage,
    my_pubkey: &PublicKey,
) -> Option<Message> {
    let chat_id = opened.channel_id.to_hex();
    let msg = build_message(opened, my_pubkey);
    // DB-level dedup: an inner id already in the events table is KNOWN — don't re-ingest into
    // STATE or re-emit it. A boot/catch-up sweep re-fetches the whole channel page, but in-memory
    // STATE only holds the per-chat hydration window, so it can't dedup the tail on its own. Without
    // this, replayed sends (incl. our own) resurface as "new", re-firing reads/notifications. Mirrors
    // the DM pipeline: outer dedup (wrapper-id cache) + inner dedup (events table). Known events live
    // in the DB and load from there.
    if crate::db::events::event_exists(&msg.id).unwrap_or(false) {
        return None;
    }
    state.ensure_community_chat(&chat_id);
    if state.add_message_to_chat(&chat_id, msg.clone()) {
        Some(msg)
    } else {
        None
    }
}

/// The result of processing an inbound wire event: a brand-new message (3300), an update to
/// an existing message (a reaction 3301 / edit 3302 applied to its target), or a tombstone
/// (a delete 3305 that removed its target). New/Updated surface as a UI `message_new` /
/// `message_update`; Removed surfaces as a `message_removed`.
pub enum IncomingEvent {
    NewMessage(Message),
    /// An existing message changed (reaction or edit). `message` is the live-updated view for the
    /// UI. `edit_event` is `Some` only for edits: the `MESSAGE_EDIT` StoredEvent the caller persists
    /// (event-sourced, folded on reload like DM edits) instead of overwriting the row. Reactions
    /// leave it `None` and the caller re-saves the message row (which carries the new reaction).
    Updated { target_id: String, message: Message, edit_event: Option<Box<crate::stored_event::StoredEvent>> },
    Removed { target_id: String },
    /// A reaction was revoked by its author (a 3305 tombstone whose target is a reaction id).
    /// The caller drops the reaction's kind-7 row and re-emits `message` so chips refresh live.
    /// Distinct from `Removed` (whole message) and `Updated` (re-saves the row, which is additive
    /// and so can't express a removal). `message_id` is the parent for the UI update.
    ReactionRemoved { message_id: String, reaction_id: String, message: Message },
    /// A join/leave presence announcement (kind 3306). `npub` is the announcing member; the
    /// caller persists + surfaces it as a `MemberJoined`/`MemberLeft` system event. `event_id`
    /// is the inner id (dedup key). `created_at` is the inner's authenticated timestamp (secs) so a
    /// HISTORICALLY-synced join/leave lands at the right place in the timeline, not at ingest-time
    /// "now". `invited_by`/`invited_label` carry attribution on an invite-join (who/which-link
    /// brought them) — `None` for a plain join/leave. Not a message.
    Presence { npub: String, joined: bool, event_id: String, created_at: u64, invited_by: Option<String>, invited_label: Option<String> },
    /// A cooperative kick (3309) targeting THE LOCAL USER, authorized (signer held `KICK` + outranked
    /// us). The caller performs the self-removal teardown (wipe local chat data, RETAIN the held
    /// epoch keys). A kick of ANOTHER member surfaces as `Presence { joined: false }`
    /// instead, so it falls out of the observed member list without a dedicated arm.
    Kicked { community_id: String },
    /// A voluntary leave-presence (3306, content "leave") whose inner author IS the local npub — i.e. a
    /// leave I (or another of my devices) published. route to the same self-removal teardown as
    /// `Kicked`/ban so a leave on device A tears the community down on device B too. Safe because the
    /// presence inner is real-npub-signed (only my own devices can author a leave for my npub). The
    /// teardown is idempotent, so the publishing device tearing down on its own echoed leave is a no-op.
    SelfLeft { community_id: String },
    /// A WebXDC realtime peer signal (3310): a member advertising their Iroh node for a Mini App
    /// session (`node_addr` = Some) or announcing they stopped playing (`node_addr` = None). The
    /// caller persists it (kind-30078 row keyed by `topic_id`, the DM-parity shape) and — when a
    /// realtime channel for the topic is live — feeds the peer to the gossip layer. Not a message.
    WebxdcPeer {
        npub: String,
        topic_id: String,
        /// Base32 iroh node address — `Some` for an advertisement, `None` for peer-left.
        node_addr: Option<String>,
        event_id: String,
        created_at: u64,
    },
    /// A typing indicator (3311): a member is composing in this channel. Ephemeral — never persisted
    /// or folded; the caller feeds it to the live typing tracker and emits `typing-update`. `until` is
    /// the unix-secs the typer should stop being shown as active (receiver-computed, ~30s out).
    Typing { npub: String, until: u64 },
}

/// Open a single incoming wire event against `channel`, verify the binding, and apply
/// it to STATE by sub-kind: a message is ingested, a reaction/edit is applied to its
/// target. Events that fail to open (wrong key, splice, forged sig, bad version) or that
/// dedup/target-miss are dropped (returns `None`). This is the per-event handler the
/// real-time subscription routes each arriving 3300/3301/3302 event through.
pub fn process_incoming(
    state: &mut ChatState,
    event: &Event,
    channel: &Channel,
    my_pubkey: &PublicKey,
) -> Option<IncomingEvent> {
    // Outer-event dedup, shared with DMs via the cross-transport ledger: a wire event we've already
    // processed is either recorded as some inner's `wrapper_event_id` (row-creating sub-kinds) or in
    // the `processed_wrappers` ledger (non-row sub-kinds). Skip it BEFORE decryption — the same role
    // the wrapper-id cache plays for gift-wraps. The per-inner-id check in ingest_message is the
    // backstop for the same inner re-published under a fresh wire event.
    let outer_bytes = event.id.to_bytes();
    if crate::db::events::wrapper_event_exists(&event.id.to_hex()).unwrap_or(false)
        || crate::db::wrappers::processed_wrapper_exists(&outer_bytes)
    {
        return None;
    }
    // binary seal: a dissolved community DROPS every subsequent event — control or message, any author
    // (owner included), any claimed time. NO timestamp comparison: the seal is the flag, not a time.
    // Already-persisted events stay (no retroactive purge); this only stops NEW events from landing.
    // CARVE-OUT: own-message DELETIONS (3305) always pass — data ownership means anyone can scrub their own
    // content from a dead community, and a delete only removes the author's OWN message (it can't inject, so
    // it doesn't reopen the backdating attack the seal exists to stop). `apply_delete` restricts a dissolved
    // community's deletes to SELF-deletes (moderation-hides are blocked).
    if channel.dissolved && event.kind.as_u16() != event_kind::COMMUNITY_DELETE {
        return None;
    }
    // Select the decryption key by the event's epoch pseudonym across ALL held epochs (post-rekey
    // catch-up), so a message posted under an older epoch still opens. Falls back to the head epoch for
    // send-built/test channels (read_epoch_keys).
    let opened = match open_message_multi(event, &channel.id, &channel.read_epoch_keys()) {
        Ok(o) => o,
        Err(e) => {
            crate::log_debug!("[community] inbound drop {}: {}", event.id.to_hex(), e);
            return None;
        }
    };
    // Banlist (the "anti-memberlist"): drop EVERY event kind from a banned author — message,
    // reaction, edit, delete, presence — so a banned member vanishes entirely, presence and all.
    if channel.banned.contains(&opened.author) {
        crate::log_debug!("[community] dropped event from banned author {}", opened.author.to_hex());
        return None;
    }
    let outcome = match opened.kind {
        k if k == event_kind::COMMUNITY_MESSAGE => {
            ingest_message(state, &opened, my_pubkey).map(IncomingEvent::NewMessage)
        }
        k if k == event_kind::COMMUNITY_REACTION => apply_reaction(state, &opened, my_pubkey),
        k if k == event_kind::COMMUNITY_EDIT => apply_edit(state, &opened, my_pubkey),
        k if k == event_kind::COMMUNITY_DELETE => apply_delete(state, &opened, channel, my_pubkey),
        k if k == event_kind::COMMUNITY_PRESENCE => apply_presence(&opened, channel, my_pubkey),
        k if k == event_kind::COMMUNITY_KICK => apply_kick(&opened, channel, my_pubkey),
        k if k == event_kind::COMMUNITY_WEBXDC => apply_webxdc(&opened, my_pubkey),
        k if k == event_kind::COMMUNITY_TYPING => apply_typing(&opened, my_pubkey),
        _ => None,
    };
    // Record the outer id in the shared ledger for NON-message sub-kinds, which have no inner row to
    // carry a `wrapper_event_id` (messages are covered atomically by that column on save). These are
    // idempotent on replay, so recording at process time is safe. Gives every sub-kind the same
    // pre-decryption skip on a re-fetch that messages already get. Typing is exempt — it's a frequent,
    // realtime-only ephemeral signal; recording every keystroke ping would bloat the ledger for no gain.
    if let Some(ref evt) = outcome {
        if !matches!(evt, IncomingEvent::NewMessage(_) | IncomingEvent::Typing { .. }) {
            let _ = crate::db::wrappers::save_processed_wrapper(
                &outer_bytes, event.created_at.as_secs(), crate::db::wrappers::TRANSPORT_CONCORD,
            );
        }
    }
    outcome
}

/// Interpret a presence announcement (3306). The inner author is the member; content "leave"
/// marks a departure, anything else (e.g. "join") an arrival. No STATE mutation here — the
/// caller turns this into a persisted system event (which is where dedup-by-id happens).
fn apply_presence(opened: &OpenedMessage, channel: &Channel, my_pubkey: &PublicKey) -> Option<IncomingEvent> {
    // Content is "leave", plain "join", or an attributed-join JSON `{"by":"<npub>","l":"<label>"}`.
    let joined = opened.content != "leave";
    // voluntary-leave self-teardown: a leave whose inner author is MY npub means I (or another of my
    // devices) left → route to the same self-removal teardown as a kick/ban, so the leave propagates to
    // every device that syncs it. Safe because the inner is real-npub-signed (only my devices author it).
    if !joined && opened.author == *my_pubkey {
        if let Ok(Some(cid)) = crate::db::community::community_id_for_channel(&channel.id.to_hex()) {
            // Only a leave NEWER than the current join is a real teardown. A leave OLDER than the join is
            // a historical leave from a PRIOR membership cycle (re-accepting an invite writes a fresh,
            // later join time) — it must NOT tear down the re-join (mirrors apply_kick's staleness gate),
            // but it SHOULD still surface as a "left" system event so my own join/leave history matches
            // what other members see. So: fresh leave → SelfLeft (teardown); stale leave → fall through to
            // the normal Presence emission below (a MemberLeft history line). saturating_mul: the inner
            // created_at isn't relay-clamped.
            let cid_bytes = crate::community::CommunityId(crate::simd::hex::hex_to_bytes_32(&cid));
            let join_ms = crate::db::community::community_created_at_ms(&cid_bytes).unwrap_or(0);
            if opened.created_at.as_secs().saturating_mul(1000) > join_ms {
                return Some(IncomingEvent::SelfLeft { community_id: cid });
            }
            crate::log_debug!("[community] self-leave predates this join — rendering as history, not teardown");
        }
    }
    let (invited_by, invited_label) = if joined {
        serde_json::from_str::<serde_json::Value>(&opened.content)
            .ok()
            .map(|v| {
                // `by` is attacker-controlled (the joiner builds their own presence), so VALIDATE it is a
                // real pubkey before surfacing — else a hostile join could inject arbitrary text into the
                // member list as a fake "inviter". Drop a non-pubkey. Bound the free-text label too.
                let by = v.get("by").and_then(|b| b.as_str())
                    .filter(|s| PublicKey::parse(s).is_ok())
                    .map(str::to_string);
                let label = v.get("l").and_then(|l| l.as_str())
                    .map(|s| s.chars().take(48).collect::<String>())
                    .filter(|s| !s.is_empty());
                (by, label)
            })
            .unwrap_or((None, None))
    } else {
        (None, None)
    };
    Some(IncomingEvent::Presence {
        npub: opened.author.to_bech32().ok()?,
        joined,
        event_id: opened.message_id.to_hex(),
        created_at: clamp_inner_secs(opened.created_at.as_secs()),
        invited_by,
        invited_label,
    })
}

/// Clamp an author-controlled inner timestamp before it becomes a persisted
/// sort key — a forged far-future stamp would otherwise pin the event at the
/// timeline edge forever (mirrors the webxdc handlers' clamp).
fn clamp_inner_secs(secs: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.min(now + 300)
}

/// Interpret a WebXDC peer signal (3310). JSON content: `{"op":"ad","topic":...,"addr":...}` for an
/// advertisement, `{"op":"left","topic":...}` for a departure. The inner author is the player (real-npub
/// signed, so a member can't forge another's presence). Own-device echoes are dropped — the local
/// realtime layer already tracks itself. Both fields are author-controlled: the topic must be a
/// 52-char base32 TopicId and the addr is size-bounded (the realtime layer's decode is the final word).
fn apply_webxdc(opened: &OpenedMessage, my_pubkey: &PublicKey) -> Option<IncomingEvent> {
    if opened.author == *my_pubkey {
        return None;
    }
    let (topic_id, node_addr) = crate::webxdc::parse_peer_signal(&opened.content)?;
    Some(IncomingEvent::WebxdcPeer {
        npub: opened.author.to_bech32().ok()?,
        topic_id,
        node_addr,
        event_id: opened.message_id.to_hex(),
        created_at: opened.created_at.as_secs(),
    })
}

/// Interpret a typing indicator (3311). Content is "typing"; the inner author is the typer (real-npub
/// signed). Own-device echoes are dropped. `until` is computed receiver-side (now + 30s) rather than
/// trusting the sender's clock — typing is realtime, so a fixed local window is both simpler and immune
/// to a forged far-future timestamp pinning a phantom typer.
fn apply_typing(opened: &OpenedMessage, my_pubkey: &PublicKey) -> Option<IncomingEvent> {
    if opened.author == *my_pubkey {
        return None;
    }
    if opened.content != "typing" {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(IncomingEvent::Typing {
        npub: opened.author.to_bech32().ok()?,
        until: now + 30,
    })
}

/// Apply an inbound cooperative kick (3309). The kicker's REAL npub is the inner author; `content` is the
/// target member's hex pubkey. Honored only when the kicker (a) cites a grant we've SYNCED
/// (`actor_authority_pinned`) AND (b) holds `KICK` + strictly outranks the target in the
/// floor-protected roster (`can_act_on_member`; the owner is never a valid target there, so owner-
/// protection falls out with no hardcoded carve-out, and a self-kick is refused since you don't strictly
/// outrank yourself). NOT a
/// rekey and NOT persisted: a kick of THE LOCAL USER yields `Kicked` (the caller tears down locally); a
/// kick of another member reuses `Presence { joined: false }` so they drop out of the observed member
/// list. An unauthorized or uncited kick is dropped. Per, only a kick NEWER than this account's join
/// is obeyed, so re-accepting an invite cleanly overrides a stale kick replayed from channel history.
fn apply_kick(opened: &OpenedMessage, channel: &Channel, my_pubkey: &PublicKey) -> Option<IncomingEvent> {
    use crate::community::roles::Permissions;
    let target = PublicKey::parse(opened.content.trim()).ok()?;
    let target_hex = target.to_hex();
    let kicker_hex = opened.author.to_hex();
    let owner_hex = channel.protected.first().map(|pk| pk.to_hex());
    let pinned = actor_authority_pinned(channel, owner_hex.as_deref(), &kicker_hex, opened.citation.as_ref());
    if !(pinned && channel.roster.can_act_on_member(&kicker_hex, owner_hex.as_deref(), &target_hex, Permissions::KICK)) {
        crate::log_debug!("[community] dropped kick: {kicker_hex} not authorized to kick {target_hex}");
        return None;
    }
    let cid_hex = crate::db::community::community_id_for_channel(&channel.id.to_hex()).ok().flatten()?;
    // "obey the latest kick newer than my current join": ignore any kick older than this account's
    // join (the community row's first-save time, in ms). The inner's non-randomized `created_at` is
    // seconds, so scale to ms. A kick tears the row down, so re-accepting an invite writes a fresh, later
    // join time → a stale kick from before the re-join is cleanly overridden, no kicklist to maintain.
    let cid = crate::community::CommunityId(crate::simd::hex::hex_to_bytes_32(&cid_hex));
    let join_ms = crate::db::community::community_created_at_ms(&cid).unwrap_or(0);
    // saturating: the inner `created_at` is NOT relay-clamped (it rides inside the ciphertext), so a
    // hostile (already-authorized) kicker could set it near u64::MAX and overflow the ms scale.
    if opened.created_at.as_secs().saturating_mul(1000) <= join_ms {
        crate::log_debug!("[community] dropped stale kick of {target_hex} (predates this join)");
        return None;
    }
    if target == *my_pubkey {
        return Some(IncomingEvent::Kicked { community_id: cid_hex });
    }
    // Kicked someone else → reflect it in the observed member list as a leave (no dedicated arm).
    Some(IncomingEvent::Presence {
        npub: target.to_bech32().ok()?,
        joined: false,
        event_id: opened.message_id.to_hex(),
        created_at: clamp_inner_secs(opened.created_at.as_secs()),
        invited_by: None,
        invited_label: None,
    })
}

/// Does this wire event authenticate against the channel's keys — now (open +
/// MAC) or on a prior sight (dedup ledgers)? The outer `created_at` is
/// otherwise unauthenticated relay input: sync cursors must only ever advance
/// over events that pass this, or one junk event stamped far-future/past at
/// the channel's cleartext pseudonym wedges the session's fetch floor/ceiling.
/// Ledger hits skip decryption, so steady-state re-syncs stay cheap.
///
/// CAVEAT: the ledger half is keyed by event id, NOT channel-scoped — a relay
/// replaying channel A's real events into channel B's page authenticates here.
/// Cursor skew from that is bounded by genuinely-authored (signature-pinned)
/// times, roughly equivalent to the relay's existing withholding power; do not
/// repurpose this as a channel-membership check.
pub fn event_authenticates(event: &Event, channel: &Channel) -> bool {
    if crate::db::events::wrapper_event_exists(&event.id.to_hex()).unwrap_or(false)
        || crate::db::wrappers::processed_wrapper_exists(&event.id.to_bytes())
    {
        return true;
    }
    open_message_multi(event, &channel.id, &channel.read_epoch_keys()).is_ok()
}

/// Process a fetched batch of raw channel events (backfill / cold-start) in a SAFE order:
/// messages (3300) first so a reaction/edit (3301/3302) finds its target already in STATE
/// before its reference applies — relay return order is arbitrary, and a control event whose
/// target hasn't been ingested yet is silently dropped. Each event goes through
/// [`process_incoming`] (open + verify + dedup), so undecryptable/forged/duplicate events
/// yield nothing. Returns the applied events in processing order for the caller to persist + emit.
///
/// KNOWN LIMITATION (cross-page): the two-pass ordering only covers targets WITHIN this batch.
/// A reaction/edit on one page whose target message lives on an older, not-yet-fetched page is
/// dropped and not re-applied when that page later arrives (the older page won't re-contain the
/// reaction). Acceptable while history is shallow; when deep scroll-backfill matures this wants a
/// pending-control buffer keyed by target id (re-drained on target ingest), mirroring the DM
/// `PENDING_EVENTS` path.
pub fn process_channel_batch(
    state: &mut ChatState,
    events: &[Event],
    channel: &Channel,
    my_pubkey: &PublicKey,
) -> Vec<IncomingEvent> {
    let mut out = Vec::new();
    // Pass 1: messages; Pass 2: control events (reactions/edits). The outer kind mirrors the
    // inner kind (seal enforces it), so it's a reliable partition key without decrypting.
    for want_message in [true, false] {
        for ev in events {
            let is_message = ev.kind.as_u16() == event_kind::COMMUNITY_MESSAGE;
            if is_message != want_message {
                continue;
            }
            if let Some(evt) = process_incoming(state, ev, channel, my_pubkey) {
                out.push(evt);
            }
        }
    }
    out
}

/// Apply an inbound reaction (3301) to its target message. The reaction is PARSED by the shared
/// `process_rumor` (3301→kind 7: target, emoji, NIP-30 image); only the STATE apply (dedup on the
/// reaction's inner id, so local + relay echoes collapse) is Concord-side.
fn apply_reaction(state: &mut ChatState, opened: &OpenedMessage, my_pubkey: &PublicKey) -> Option<IncomingEvent> {
    use crate::rumor::{process_rumor, RumorProcessingResult};
    let (rumor, ctx) = concord_rumor(opened, nostr_sdk::Kind::Reaction, my_pubkey);
    let reaction = match process_rumor(rumor, ctx, &crate::db::get_download_dir()) {
        Ok(RumorProcessingResult::Reaction(r)) => r,
        _ => return None,
    };
    let target_id = reaction.reference_id.clone();
    // Cross-channel guard: a reaction may only land on a target resident in the SAME channel it was
    // sealed under. The reaction's binding triad authenticates its own channel/epoch but says nothing
    // about where its target lives, so without this a member holding one channel's key could inject
    // reactions onto another channel's messages. (Community channel chats are keyed by channel-id hex.)
    let expected_chat = opened.channel_id.to_hex();
    if !matches!(state.find_message(&target_id), Some((chat, _)) if chat.id == expected_chat) {
        return None;
    }
    let (_chat_id, was_added) = state.add_reaction_to_message(&target_id, reaction)?;
    if !was_added {
        return None;
    }
    let (_chat, message) = state.find_message(&target_id)?;
    Some(IncomingEvent::Updated { target_id, message, edit_event: None })
}

/// Apply an inbound edit (3302) to its target message. PARSED by the shared `process_rumor`
/// (3302→edit: target, new content, edited_at via the shared ms resolver). The author-scoped gate
/// (only the original author may edit their own message) + the canonical edit applier are Concord-side.
fn apply_edit(state: &mut ChatState, opened: &OpenedMessage, my_pubkey: &PublicKey) -> Option<IncomingEvent> {
    use crate::rumor::{process_rumor, RumorProcessingResult};
    let (rumor, ctx) = concord_rumor(opened, nostr_sdk::Kind::from(event_kind::MESSAGE_EDIT), my_pubkey);
    let (target_id, new_content, edited_at, emoji_tags, edit_event) = match process_rumor(rumor, ctx, &crate::db::get_download_dir()) {
        Ok(RumorProcessingResult::Edit { message_id, new_content, edited_at, emoji_tags, event }) => (message_id, new_content, edited_at, emoji_tags, event),
        _ => return None,
    };
    // Author-scoped: you can't edit someone else's message (not a parser concern — needs the resident
    // target's author).
    let editor_npub = opened.author.to_bech32().ok()?;
    let target_author = state.find_message(&target_id).and_then(|(_, m)| m.npub)?;
    if target_author != editor_npub {
        crate::log_debug!("[community] dropped edit from non-author of {}", target_id);
        return None;
    }
    // The canonical edit applier seeds history with the original ONCE, dedups by `edited_at` (a
    // relay-replayed edit is a no-op, not history corruption), sorts, and swaps the content.
    let (_chat_id, message) = state.update_message(&target_id, |m| {
        m.apply_edit(new_content.clone(), edited_at, emoji_tags.clone());
    })?;
    // Persist the edit as a folded MESSAGE_EDIT event (caller sets chat_id), mirroring DMs —
    // no row overwrite, no JSON snapshot.
    Some(IncomingEvent::Updated { target_id, message, edit_event: Some(Box::new(edit_event)) })
}

/// version-pinned authority for a directed authority action (moderation-hide 3305, kick 3309):
/// does the actor's cited grant prove authority we've actually SYNCED? The owner is supreme (cites
/// nothing). A non-owner must cite the grant that authorizes them, and we must hold that grant in our
/// persisted heads at ≥ the cited version (with the cited hash at the tip) — else fail closed (don't
/// honor an action claiming authority we can't confirm). This is the COMPLETENESS half only; the actual
/// permission + outrank is the SEPARATE `can_act_on_member` check against the floor-protected roster (so
/// a since-demoted actor is refused there: refuse-superseded). The block-until-synced FETCH escalation
/// isn't possible in this sync inbound path; an action citing a version we haven't synced is dropped and
/// re-evaluated on the next roster sync (the documented sync-path limit).
fn actor_authority_pinned(
    channel: &Channel,
    owner_hex: Option<&str>,
    actor_hex: &str,
    citation: Option<&super::edition::AuthorityCitation>,
) -> bool {
    if owner_hex == Some(actor_hex) {
        return true; // owner is supreme and cites nothing
    }
    if citation.is_none() {
        return false; // a non-owner authority action MUST carry a citation
    }
    let Ok(Some(cid)) = crate::db::community::community_id_for_channel(&channel.id.to_hex()) else {
        return false; // can't resolve the community → can't confirm the cited grant → fail closed
    };
    let cid_bytes = crate::simd::hex::hex_to_bytes_32(&cid);
    let actor_bytes = crate::simd::hex::hex_to_bytes_32(actor_hex);
    let grant_hex = crate::simd::hex::bytes_to_hex_32(&super::derive::grant_locator(
        &crate::community::CommunityId(cid_bytes),
        &actor_bytes,
    ));
    let head: Vec<super::roster::EntityHead> = crate::db::community::get_edition_head(&cid, &grant_hex)
        .ok()
        .flatten()
        .map(|(version, self_hash)| super::roster::EntityHead { entity_hex: grant_hex.clone(), version, self_hash, inner_id: [0u8; 32], citation: None })
        .into_iter()
        .collect();
    super::roster::authority_citation_satisfied(&head, owner_hex, actor_hex, &grant_hex, citation)
}

fn apply_delete(state: &mut ChatState, opened: &OpenedMessage, channel: &Channel, my_pubkey: &PublicKey) -> Option<IncomingEvent> {
    use crate::community::roles::Permissions;
    use crate::rumor::{process_rumor, RumorProcessingResult};
    // Target PARSED by the shared deletion parser (3305→kind 5; rejects an ambiguous multi-`e` target).
    // The author-delete vs moderation-hide AUTHORITY decision below stays Concord-side — it reads the
    // synced roster, which the parser knows nothing about, and never mutates consensus.
    let (rumor, ctx) = concord_rumor(opened, nostr_sdk::Kind::EventDeletion, my_pubkey);
    let target_id = match process_rumor(rumor, ctx, &crate::db::get_download_dir()) {
        Ok(RumorProcessingResult::DeletionRequest { target_event_id }) => target_event_id,
        _ => return None,
    };
    let deleter = opened.author;
    let deleter_hex = deleter.to_hex();

    // A 3305 may target a REACTION rather than a message (both are event ids). Reactions are
    // author-revocable only in v1 (no moderation-strip): the deleter must be the reactor. Handled
    // before the message path so a reaction id never falls through to message-removal logic.
    if let Some((_chat_id, message_id, author_npub, _is_comm)) = state.find_reaction(&target_id) {
        let reactor_ok = PublicKey::parse(&author_npub).map(|pk| pk == deleter).unwrap_or(false);
        if !reactor_ok {
            crate::log_debug!("[community] dropped reaction-revoke: {deleter_hex} is not the reactor of {target_id}");
            return None;
        }
        return state
            .remove_reaction_from_message(&message_id, &target_id)
            .map(|(_cid, message)| IncomingEvent::ReactionRemoved {
                message_id,
                reaction_id: target_id,
                message,
            });
    }

    let owner_hex = channel.protected.first().map(|pk| pk.to_hex());
    // pinned authority for a moderation-hide: the deleter's cited grant must be one we've synced
    // (fail closed otherwise). Orthogonal to the outrank below; self-deletes don't consult it.
    let pinned = actor_authority_pinned(channel, owner_hex.as_deref(), &deleter_hex, opened.citation.as_ref());

    // Resolve the target's author from the resident copy (needed for both the self-delete check and the
    // hide outrank check). If the target isn't resident (an older page), we can't resolve the author.
    let target_author = state
        .find_message(&target_id)
        .and_then(|(_, m)| m.npub.clone())
        .and_then(|n| PublicKey::parse(&n).ok());

    if let Some(author) = target_author {
        // Self-delete: the message's own author removes it.
        if author == deleter {
            return state.remove_message(&target_id).map(|_| IncomingEvent::Removed { target_id });
        }
        // Moderation-hide: deleter must (a) cite an authorizing grant we've synced (`pinned`) AND
        // (b) hold MANAGE_MESSAGES + outrank the target's author. The owner is never a valid target of
        // `can_act_on_member` (so owner-protection falls out — no hardcoded carve-out), and a
        // peer/superior can't be hidden either. Blocked once dissolved: a dead community accepts no
        // new authority actions — only SELF-deletes (handled above) survive the seal.
        if pinned && !channel.dissolved && channel.roster.can_act_on_member(&deleter_hex, owner_hex.as_deref(), &author.to_hex(), Permissions::MANAGE_MESSAGES) {
            return state.remove_message(&target_id).map(|_| IncomingEvent::Removed { target_id });
        }
        crate::log_debug!("[community] dropped delete: {deleter_hex} not authorized to remove {target_id}");
        return None;
    }

    // Target not resident in memory. If it's still in the DB (just paged out of the window), authorize
    // against its REAL author — owner-protection and the outrank both apply exactly as for a resident
    // target, so an admin can't tombstone the owner's (or a peer's) paged-out message.
    if let Ok(Some(author_npub)) = crate::db::events::event_author(&target_id) {
        if let Ok(author) = PublicKey::parse(&author_npub) {
            // Dissolved gate mirrors the resident path: the seal blocks
            // moderation-hides over paged-out targets too — only self-deletes
            // survive in a dead community.
            let ok = author == deleter
                || (pinned
                    && !channel.dissolved
                    && channel.roster.can_act_on_member(&deleter_hex, owner_hex.as_deref(), &author.to_hex(), Permissions::MANAGE_MESSAGES));
            if ok {
                return Some(IncomingEvent::Removed { target_id });
            }
            crate::log_debug!("[community] dropped out-of-window delete: {deleter_hex} not authorized over {target_id}");
            return None;
        }
    }

    // Author unknown (in neither STATE nor DB — a hide racing ahead of its target). There's nothing to
    // remove and no real author to authorize against yet, so DON'T act and DON'T let this record in the
    // dedup ledger (returning None keeps `process_incoming` from recording the outer id). The hide stays
    // un-deduped and RE-APPLIES on a later sync once the target is resident — the resident path then
    // authorizes against the real author. (A speculative `Removed` here would be a no-op emit AND, via the
    // ledger, dedup the hide forever → the message would never get hidden once it arrived: bypass.)
    // Mirrors reaction/edit, which also return None on an absent target.
    None
}

/// Route an incoming wire event by its `z` pseudonym tag to the matching channel in
/// `routes`, then open + ingest it. Returns the added `Message`, or `None` if the
/// event carries no `z` tag, names a pseudonym we don't route, or fails to open/dedup.
/// Pure over the passed `state` + `routes`, so the routing is unit-testable without
/// the live subscription loop.
pub fn route_incoming(
    state: &mut ChatState,
    event: &Event,
    routes: &std::collections::HashMap<String, Channel>,
    my_pubkey: &PublicKey,
) -> Option<IncomingEvent> {
    let pseudonym = event.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == "z").then(|| s[1].clone())
    })?;
    let channel = routes.get(&pseudonym)?;
    process_incoming(state, event, channel, my_pubkey)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::derive::channel_pseudonym;
    use std::collections::HashMap;
    use crate::community::envelope::{build_inner_full, build_inner_typed, open_message, seal_message, seal_with_signed_inner};
    use crate::community::edition::AuthorityCitation;
    use crate::community::{Channel, ChannelId, ChannelKey, Epoch};
    use crate::state::ChatState;
    use nostr_sdk::prelude::{Keys, Tag};

    /// A DB-backed community owned by `owner` with `admin` granted the Admin role (MANAGE_MESSAGES,
    /// position 1) and the admin's grant head recorded at v1 — so `apply_delete` can resolve the
    /// community (`community_id_for_channel`) and verify a moderation hide's pinned authority against the
    /// persisted grant head. Returns the reloaded channel (carrying the denormalized roster + protected
    /// owner) and a valid citation pinning the admin's v1 grant. Holds the DB test guard for the test.
    fn db_roster_channel(
        owner: &Keys,
        admin: &PublicKey,
    ) -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>, Channel, AuthorityCitation) {
        use crate::community::roles::{CommunityRoles, MemberGrant, Role};
        use nostr_sdk::{JsonUtil, ToBech32};
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let tmp = tempfile::tempdir().unwrap();
        let account = owner.public_key().to_bech32().unwrap();
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        crate::state::MY_SECRET_KEY.store_from_keys(owner, &[]);
        crate::state::set_my_public_key(owner.public_key());

        let mut community = crate::community::Community::create("HQ", "general", vec!["r".into()]);
        let cid = community.id.to_hex();
        community.owner_attestation = Some(
            crate::community::owner::build_owner_attestation_unsigned(owner.public_key(), &cid)
                .sign_with_keys(owner)
                .unwrap()
                .as_json(),
        );
        crate::db::community::save_community(&community).unwrap();

        // Grant the admin MANAGE_MESSAGES (Admin role) and cache it so the reloaded channel's roster
        // ranks them; record their grant head at v1 so the citation below verifies.
        let role = Role::admin("a".repeat(64));
        let roster = CommunityRoles {
            grants: vec![MemberGrant { member: admin.to_hex(), role_ids: vec![role.role_id.clone()] }],
            roles: vec![role],
        };
        crate::db::community::set_community_roles(&cid, &roster, 0).unwrap();
        let entity_id = crate::community::derive::grant_locator(&community.id, &admin.to_bytes());
        let entity_hex = crate::simd::hex::bytes_to_hex_32(&entity_id);
        let hash = [0x5Au8; 32];
        crate::db::community::set_edition_head(&cid, &entity_hex, 1, &hash).unwrap();

        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        let channel = reloaded.channels[0].clone();
        (tmp, guard, channel, AuthorityCitation { entity_id, version: 1, edition_hash: hash })
    }

    /// Seal a 3305 moderation hide of `target` as `author`, optionally carrying a `vac` citation.
    fn seal_hide(channel: &Channel, author: &Keys, target: &str, ms: u64, citation: Option<&AuthorityCitation>) -> Event {
        let extra: Vec<Tag> = citation.iter().map(|c| c.to_tag()).collect();
        let inner = build_inner_full(
            author.public_key(), &channel.id, channel.epoch, event_kind::COMMUNITY_DELETE, "", ms, Some(target), &[], &extra,
        )
        .sign_with_keys(author)
        .unwrap();
        seal_with_signed_inner(&Keys::generate(), &inner, &channel.key, &channel.id, channel.epoch).unwrap()
    }

    /// Ingest a message authored by `author` into `channel`, returning its inner id.
    fn ingest_msg_in(state: &mut ChatState, channel: &Channel, author: &Keys, content: &str, ms: u64, viewer: &Keys) -> String {
        let outer = seal_message(author, &channel.key, &channel.id, channel.epoch, content, ms).unwrap();
        match process_incoming(state, &outer, channel, &viewer.public_key()) {
            Some(IncomingEvent::NewMessage(m)) => m.id,
            _ => panic!("expected a new message"),
        }
    }

    fn opened_from(author: &Keys, content: &str, ms: u64) -> OpenedMessage {
        let key = ChannelKey([0x33u8; 32]);
        let chan = ChannelId([0x44u8; 32]);
        let outer = seal_message(author, &key, &chan, Epoch(0), content, ms).unwrap();
        open_message(&outer, &key, &chan, Epoch(0)).unwrap()
    }

    fn test_channel() -> Channel {
        Channel { id: ChannelId([0x44u8; 32]), key: ChannelKey([0x33u8; 32]), epoch: Epoch(0), name: "t".into(), banned: Vec::new(), protected: Vec::new(), roster: Default::default(), epoch_keys: Vec::new(), dissolved: false }
    }

    /// Seal a typed control event (reaction/edit) referencing `target`, as `author`.
    fn seal_typed(author: &Keys, kind: u16, content: &str, ms: u64, target: &str) -> Event {
        let c = test_channel();
        let inner = build_inner_typed(author.public_key(), &c.id, c.epoch, kind, content, ms, Some(target), &[])
            .sign_with_keys(author)
            .unwrap();
        seal_with_signed_inner(&Keys::generate(), &inner, &c.key, &c.id, c.epoch).unwrap()
    }

    fn ingest_msg(state: &mut ChatState, author: &Keys, content: &str, ms: u64, viewer: &Keys) -> String {
        let c = test_channel();
        let outer = seal_message(author, &c.key, &c.id, c.epoch, content, ms).unwrap();
        match process_incoming(state, &outer, &c, &viewer.public_key()) {
            Some(IncomingEvent::NewMessage(m)) => m.id,
            _ => panic!("expected a new message"),
        }
    }

    #[test]
    fn inbound_reaction_applies_to_target_and_dedups() {
        use crate::stored_event::event_kind;
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let bob = Keys::generate();
        let target = ingest_msg(&mut state, &alice, "hi", 1, &bob);

        let react = seal_typed(&bob, event_kind::COMMUNITY_REACTION, "🔥", 2, &target);
        match process_incoming(&mut state, &react, &test_channel(), &bob.public_key()) {
            Some(IncomingEvent::Updated { target_id, message, edit_event: None }) => {
                assert_eq!(target_id, target);
                assert!(message.reactions.iter().any(|r| r.emoji == "🔥"), "reaction applied to target");
            }
            _ => panic!("expected a reaction update"),
        }
        // The exact same reaction event again → deduped (no second update).
        assert!(process_incoming(&mut state, &react, &test_channel(), &bob.public_key()).is_none());
    }

    #[test]
    fn bot_routing_tag_rides_the_v1_inner_into_addressed_bots() {
        use nostr_sdk::prelude::ToBech32;
        use crate::community::envelope::{build_inner_full, seal_with_signed_inner};
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let bot = Keys::generate();
        let c = test_channel();

        // A picker send: the `bot` routing tag rides the inner verbatim.
        let inner = build_inner_full(
            alice.public_key(),
            &c.id,
            c.epoch,
            crate::stored_event::event_kind::COMMUNITY_MESSAGE,
            "/roll 20",
            5,
            None,
            &[],
            &[crate::bot_interface::bot_tag(&bot.public_key())],
        )
        .sign_with_keys(&alice)
        .unwrap();
        let outer = seal_with_signed_inner(&Keys::generate(), &inner, &c.key, &c.id, c.epoch).unwrap();

        // The real open + ingest path lifts the tag into `addressed_bots` —
        // the field SDK bots consult to skip commands picked for another bot.
        let opened = open_message(&outer, &c.key, &c.id, c.epoch).unwrap();
        let msg = build_message(&opened, &alice.public_key());
        assert_eq!(msg.addressed_bots, vec![bot.public_key().to_bech32().unwrap()]);

        match process_incoming(&mut state, &outer, &c, &alice.public_key()) {
            Some(IncomingEvent::NewMessage(m)) => {
                assert_eq!(m.addressed_bots.len(), 1, "ingest keeps the routing tag");
            }
            _ => panic!("expected a new message"),
        }
    }

    #[test]
    fn reaction_cross_channel_is_rejected() {
        // A member holding one channel's key must not be able to seal a reaction under THAT channel
        // that lands on a message resident in ANOTHER channel.
        use crate::stored_event::event_kind;
        use crate::community::envelope::{build_inner_typed, seal_with_signed_inner};
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let bob = Keys::generate();
        let chan_a = test_channel();
        let chan_b = Channel {
            id: ChannelId([0x55u8; 32]), key: ChannelKey([0x66u8; 32]), epoch: Epoch(0),
            name: "b".into(), banned: Vec::new(), protected: Vec::new(),
            roster: Default::default(), epoch_keys: Vec::new(), dissolved: false,
        };
        // Target message lives in channel A.
        let target = ingest_msg_in(&mut state, &chan_a, &alice, "hi", 1, &bob);
        // Bob seals a reaction under channel B that points at A's message.
        let inner = build_inner_typed(
            bob.public_key(), &chan_b.id, chan_b.epoch, event_kind::COMMUNITY_REACTION, "🔥", 2, Some(&target), &[],
        ).sign_with_keys(&bob).unwrap();
        let outer = seal_with_signed_inner(&Keys::generate(), &inner, &chan_b.key, &chan_b.id, chan_b.epoch).unwrap();
        // Opened under channel B, but the target is in A → rejected, nothing applied.
        assert!(
            process_incoming(&mut state, &outer, &chan_b, &bob.public_key()).is_none(),
            "a reaction sealed under another channel must not apply to this channel's message"
        );
        let (_c, msg) = state.find_message(&target).unwrap();
        assert!(msg.reactions.is_empty(), "cross-channel reaction must not be applied");
    }

    #[test]
    fn inbound_message_carries_multi_attachments() {
        use crate::stored_event::event_kind;
        use crate::community::attachments::attachment_to_imeta;
        use crate::community::envelope::build_inner_full;
        use crate::types::Attachment;
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let bob = Keys::generate();
        let c = test_channel();

        let mk = |n: &str, ext: &str| Attachment {
            id: "x".into(), key: "0".repeat(64), nonce: format!("{:0<24}", crate::simd::hex::bytes_to_hex_string(n.as_bytes())),
            extension: ext.into(), name: n.into(), url: format!("https://b/{n}"),
            path: String::new(), size: 9, img_meta: None, downloading: false, downloaded: false,
            webxdc_topic: None, group_id: None, original_hash: Some("a".repeat(64)),
            scheme_version: None, mls_filename: None,
        };
        let imetas = vec![attachment_to_imeta(&mk("a.png", "png")), attachment_to_imeta(&mk("b.txt", "txt"))];
        let inner = build_inner_full(
            alice.public_key(), &c.id, c.epoch, event_kind::COMMUNITY_MESSAGE,
            "caption", 5, None, &[], &imetas,
        ).sign_with_keys(&alice).unwrap();
        let outer = seal_with_signed_inner(&Keys::generate(), &inner, &c.key, &c.id, c.epoch).unwrap();

        match process_incoming(&mut state, &outer, &c, &bob.public_key()) {
            Some(IncomingEvent::NewMessage(m)) => {
                assert_eq!(m.content, "caption", "caption + attachments coexist in one event");
                assert_eq!(m.attachments.len(), 2);
                assert_eq!(m.attachments[0].name, "a.png");
                assert_eq!(m.attachments[1].name, "b.txt");
                assert!(m.attachments.iter().all(|a| a.group_id.is_none()));
            }
            _ => panic!("expected new message with attachments"),
        }
    }

    #[test]
    fn inbound_edit_only_honored_from_original_author() {
        use crate::stored_event::event_kind;
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let target = ingest_msg(&mut state, &alice, "original", 1, &alice);

        // Author edits her own message → applied.
        let edit = seal_typed(&alice, event_kind::COMMUNITY_EDIT, "edited!", 2, &target);
        match process_incoming(&mut state, &edit, &test_channel(), &alice.public_key()) {
            Some(IncomingEvent::Updated { message, edit_event, .. }) => {
                assert_eq!(message.content, "edited!");
                assert!(message.edited);
                // Event-sourced: the edit rides a foldable MESSAGE_EDIT event, not a row overwrite.
                let ev = edit_event.expect("edit surfaces a MESSAGE_EDIT event to persist");
                assert_eq!(ev.kind, event_kind::MESSAGE_EDIT);
                assert_eq!(ev.reference_id.as_deref(), Some(target.as_str()));
                assert_eq!(ev.content, "edited!");
            }
            _ => panic!("expected an edit update"),
        }

        // A different author trying to edit alice's message → dropped, content unchanged.
        let mallory = Keys::generate();
        let hijack = seal_typed(&mallory, event_kind::COMMUNITY_EDIT, "hijacked", 3, &target);
        assert!(process_incoming(&mut state, &hijack, &test_channel(), &alice.public_key()).is_none());
        assert_eq!(state.find_message(&target).unwrap().1.content, "edited!");
    }

    #[test]
    fn cooperative_delete_only_honored_from_original_author() {
        use crate::stored_event::event_kind;
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let mallory = Keys::generate();
        let target = ingest_msg(&mut state, &alice, "secret", 1, &alice);

        // Someone else's delete of alice's message → dropped, message survives.
        let hijack = seal_typed(&mallory, event_kind::COMMUNITY_DELETE, "", 2, &target);
        assert!(process_incoming(&mut state, &hijack, &test_channel(), &alice.public_key()).is_none());
        assert!(state.find_message(&target).is_some(), "non-author delete must not remove");

        // Author's own delete (signed by a FRESH key, no retained message key needed) → removed.
        let del = seal_typed(&alice, event_kind::COMMUNITY_DELETE, "", 3, &target);
        match process_incoming(&mut state, &del, &test_channel(), &alice.public_key()) {
            Some(IncomingEvent::Removed { target_id }) => assert_eq!(target_id, target),
            _ => panic!("expected a removal"),
        }
        assert!(state.find_message(&target).is_none(), "message gone after author delete");

        // Replaying the delete (or one arriving for an already-gone target) → silent no-op.
        let replay = seal_typed(&alice, event_kind::COMMUNITY_DELETE, "", 4, &target);
        assert!(process_incoming(&mut state, &replay, &test_channel(), &alice.public_key()).is_none());
    }

    #[test]
    fn dissolved_community_still_honors_an_own_message_delete() {
        use crate::stored_event::event_kind;
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let target = ingest_msg(&mut state, &alice, "alice's own message", 1, &alice);
        let mut ch = test_channel();
        ch.dissolved = true;
        // carve-out (data ownership): the binary seal blocks all NEW content, but a member can always
        // scrub their OWN past message even from a dead community — a 3305 self-delete passes the seal.
        let del = seal_typed(&alice, event_kind::COMMUNITY_DELETE, "", 2, &target);
        match process_incoming(&mut state, &del, &ch, &alice.public_key()) {
            Some(IncomingEvent::Removed { target_id }) => assert_eq!(target_id, target),
            _ => panic!("a self-delete must be honored in a dissolved community"),
        }
        assert!(state.find_message(&target).is_none(), "own message scrubbed from the dead community");
    }

    #[test]
    fn admin_moderation_hide_removes_any_message() {
        let owner = Keys::generate();
        let admin = Keys::generate(); // granted MANAGE_MESSAGES in the roster
        let (_tmp, _guard, c, cite) = db_roster_channel(&owner, &admin.public_key());
        let alice = Keys::generate(); // author of the target
        let mallory = Keys::generate(); // unprivileged member
        let mut state = ChatState::new();
        let target = ingest_msg_in(&mut state, &c, &alice, "spicy take", 1, &alice);

        // A member with no MANAGE_MESSAGES role (and no citation to offer) cannot hide someone else's.
        let hijack = seal_hide(&c, &mallory, &target, 2, None);
        assert!(process_incoming(&mut state, &hijack, &c, &alice.public_key()).is_none());
        assert!(state.find_message(&target).is_some(), "unprivileged hide rejected");

        // The admin (hide signed by their REAL npub, citing their synced grant) hides a member's message.
        let hide = seal_hide(&c, &admin, &target, 3, Some(&cite));
        match process_incoming(&mut state, &hide, &c, &alice.public_key()) {
            Some(IncomingEvent::Removed { target_id }) => assert_eq!(target_id, target),
            _ => panic!("expected admin moderation-hide to remove the message"),
        }
        assert!(state.find_message(&target).is_none(), "admin hide removed the message");
    }

    #[test]
    fn admin_hide_without_a_citation_is_dropped() {
        // a non-owner moderation hide MUST cite the grant that authorizes them. An admin who holds
        // MANAGE_MESSAGES but ships an UNCITED hide is dropped (fail closed — we never act on authority
        // that isn't pinned to a synced grant version).
        let owner = Keys::generate();
        let admin = Keys::generate();
        let (_tmp, _guard, c, _cite) = db_roster_channel(&owner, &admin.public_key());
        let alice = Keys::generate();
        let mut state = ChatState::new();
        let target = ingest_msg_in(&mut state, &c, &alice, "spicy take", 1, &alice);

        let hide = seal_hide(&c, &admin, &target, 2, None); // no citation
        assert!(process_incoming(&mut state, &hide, &c, &alice.public_key()).is_none());
        assert!(state.find_message(&target).is_some(), "an uncited admin hide is dropped");
    }

    #[test]
    fn hide_citing_an_unsynced_grant_version_is_dropped() {
        // The sync-floor: an admin who cites a grant version we have NOT synced (ahead of our persisted
        // head) is dropped — we can't confirm the authority, so we don't act (block-until-synced degrades
        // to drop in the sync inbound path; it re-evaluates once the grant syncs).
        let owner = Keys::generate();
        let admin = Keys::generate();
        let (_tmp, _guard, c, cite) = db_roster_channel(&owner, &admin.public_key());
        let alice = Keys::generate();
        let mut state = ChatState::new();
        let target = ingest_msg_in(&mut state, &c, &alice, "spicy take", 1, &alice);

        // Our held head for the admin's grant is v1; the hide cites a future v2 nobody has yet.
        let ahead = AuthorityCitation { version: 2, ..cite };
        let hide = seal_hide(&c, &admin, &target, 2, Some(&ahead));
        assert!(process_incoming(&mut state, &hide, &c, &alice.public_key()).is_none());
        assert!(state.find_message(&target).is_some(), "a hide citing an unsynced version is dropped");
    }

    #[test]
    fn hide_with_a_forged_citation_hash_is_dropped() {
        // fork guard: an admin citing their real grant entity + version but the WRONG hash is dropped.
        let owner = Keys::generate();
        let admin = Keys::generate();
        let (_tmp, _guard, c, cite) = db_roster_channel(&owner, &admin.public_key());
        let alice = Keys::generate();
        let mut state = ChatState::new();
        let target = ingest_msg_in(&mut state, &c, &alice, "spicy take", 1, &alice);

        let forged = AuthorityCitation { edition_hash: [0xEE; 32], ..cite };
        let hide = seal_hide(&c, &admin, &target, 2, Some(&forged));
        assert!(process_incoming(&mut state, &hide, &c, &alice.public_key()).is_none());
        assert!(state.find_message(&target).is_some(), "a forged-hash citation is dropped");
    }

    #[test]
    fn protected_owner_cannot_be_moderation_hidden_but_others_can() {
        let owner = Keys::generate(); // protected, implicit position 0
        let admin = Keys::generate(); // granted MANAGE_MESSAGES
        let (_tmp, _guard, c, cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();

        // An admin-signed hide targeting the OWNER's message is dropped — the owner outranks every admin.
        let owners_msg = ingest_msg_in(&mut state, &c, &owner, "owner speaks", 1, &owner);
        let hide_owner = seal_hide(&c, &admin, &owners_msg, 2, Some(&cite));
        assert!(process_incoming(&mut state, &hide_owner, &c, &owner.public_key()).is_none());
        assert!(state.find_message(&owners_msg).is_some(), "owner's message is protected");

        // A non-protected member's message CAN be moderation-hidden by the same admin.
        let member = Keys::generate();
        let members_msg = ingest_msg_in(&mut state, &c, &member, "member speaks", 3, &owner);
        let hide_member = seal_hide(&c, &admin, &members_msg, 4, Some(&cite));
        match process_incoming(&mut state, &hide_member, &c, &owner.public_key()) {
            Some(IncomingEvent::Removed { target_id }) => assert_eq!(target_id, members_msg),
            _ => panic!("a non-protected member's message should be hideable"),
        }
    }

    #[test]
    fn admin_hide_of_absent_target_defers_until_resident() {
        // A hide for a target resident in NEITHER STATE nor DB (racing ahead of its message) returns None
        // — there's nothing to remove and no real author to outrank yet. Critically it must NOT emit a
        // speculative Removed: that emit is a no-op (delete_event on an absent id does nothing) AND, via
        // the cross-transport dedup ledger, would dedup the hide forever, so the message would never get
        // hidden once it finally arrived (a moderation bypass). Returning None keeps the hide
        // un-deduped so it RE-APPLIES on a later sync once the target pages in (resident path authorizes
        // against the real author). Mirrors reaction/edit on an absent target.
        let owner = Keys::generate();
        let admin = Keys::generate();
        let (_tmp, _guard, c, cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();
        let absent_target = "f".repeat(64); // never ingested into STATE or DB

        let hide = seal_hide(&c, &admin, &absent_target, 1, Some(&cite));
        assert!(
            process_incoming(&mut state, &hide, &c, &Keys::generate().public_key()).is_none(),
            "a hide of an absent target defers (None) rather than falsely tombstoning + self-deduping",
        );

        // A NON-privileged hide of an out-of-window message stays a no-op (no MANAGE_MESSAGES grant).
        let mallory = Keys::generate();
        let hijack = seal_hide(&c, &mallory, &absent_target, 2, None);
        assert!(process_incoming(&mut state, &hijack, &c, &mallory.public_key()).is_none());

        // And a PRIVILEGED admin (holds MANAGE_MESSAGES) who ships an UNCITED hide of the unknown target
        // is ALSO dropped — the author-unknown branch is gated on `pinned`, not the permission bit alone.
        let uncited = seal_hide(&c, &admin, &absent_target, 3, None);
        assert!(
            process_incoming(&mut state, &uncited, &c, &Keys::generate().public_key()).is_none(),
            "an admin's uncited hide of an unknown target is dropped (pinned gates the author-unknown path)"
        );
    }

    #[tokio::test]
    async fn out_of_window_hide_authorizes_against_db_author() {
        use crate::types::Message;
        // A paged-out target (in the DB, not resident in memory) is authorized against its REAL author:
        // an admin can hide a regular member's paged-out message, but NOT the owner's (owner-protection
        // holds even when the message is out of the in-memory window).
        use nostr_sdk::ToBech32;
        let owner = Keys::generate();
        let admin = Keys::generate(); // granted MANAGE_MESSAGES
        let member = Keys::generate();
        let (_tmp, _guard, c, cite) = db_roster_channel(&owner, &admin.public_key());

        // Persist two messages to the DB only — a fresh ChatState holds neither.
        let owner_msg = "a".repeat(64);
        let member_msg = "b".repeat(64);
        let mk = |id: &str, author: &Keys, at: u64| {
            let mut m = Message::default();
            m.id = id.to_string();
            m.npub = Some(author.public_key().to_bech32().unwrap());
            m.at = at;
            m
        };
        crate::db::events::save_message("chatoow", &mk(&owner_msg, &owner, 1)).await.unwrap();
        crate::db::events::save_message("chatoow", &mk(&member_msg, &member, 2)).await.unwrap();

        let mut state = ChatState::new();
        // Admin hide of the OWNER's paged-out message → dropped (owner is supreme, never a target).
        let hide_owner = seal_hide(&c, &admin, &owner_msg, 3, Some(&cite));
        assert!(
            process_incoming(&mut state, &hide_owner, &c, &member.public_key()).is_none(),
            "owner's paged-out message must not be hideable by an admin"
        );
        // Admin hide of a regular member's paged-out message → Removed (tombstoned).
        let hide_member = seal_hide(&c, &admin, &member_msg, 4, Some(&cite));
        match process_incoming(&mut state, &hide_member, &c, &owner.public_key()) {
            Some(IncomingEvent::Removed { target_id }) => assert_eq!(target_id, member_msg),
            _ => panic!("admin should hide a member's paged-out message"),
        }

        // Dissolved seal covers paged-out targets too: an admin moderation-hide of a
        // member's DB-only message is dropped in a dead community, while the author's
        // own self-delete of their paged-out message still passes (data ownership).
        let member_msg2 = "c".repeat(64);
        crate::db::events::save_message("chatoow", &mk(&member_msg2, &member, 5)).await.unwrap();
        let mut sealed = c.clone();
        sealed.dissolved = true;
        let hide_sealed = seal_hide(&sealed, &admin, &member_msg2, 6, Some(&cite));
        assert!(
            process_incoming(&mut state, &hide_sealed, &sealed, &owner.public_key()).is_none(),
            "a dissolved community accepts no moderation-hide, resident or paged-out"
        );
        let self_del = seal_hide(&sealed, &member, &member_msg2, 7, None);
        match process_incoming(&mut state, &self_del, &sealed, &owner.public_key()) {
            Some(IncomingEvent::Removed { target_id }) => assert_eq!(target_id, member_msg2),
            _ => panic!("a self-delete of a paged-out message must survive the dissolved seal"),
        }
        crate::db::close_database();
    }

    /// Seal a 3309 cooperative kick of `target_hex` as `author`, optionally carrying a `vac` citation.
    fn seal_kick(channel: &Channel, author: &Keys, target_hex: &str, ms: u64, citation: Option<&AuthorityCitation>) -> Event {
        let extra: Vec<Tag> = citation.iter().map(|c| c.to_tag()).collect();
        let inner = build_inner_full(
            author.public_key(), &channel.id, channel.epoch, event_kind::COMMUNITY_KICK, target_hex, ms, None, &[], &extra,
        )
        .sign_with_keys(author)
        .unwrap();
        seal_with_signed_inner(&Keys::generate(), &inner, &channel.key, &channel.id, channel.epoch).unwrap()
    }

    /// A kick inner timestamp safely AFTER `db_roster_channel`'s community save, so the join-time
    /// guard honors it. `build_inner_full` derives `created_at = ms / 1000`, so we add a 5s margin.
    fn post_join_ms() -> u64 {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        (now + 5) * 1000
    }

    #[test]
    fn cited_admin_kick_of_local_user_yields_self_removal() {
        let owner = Keys::generate();
        let admin = Keys::generate();
        let member = Keys::generate();
        let (_tmp, _g, channel, cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();
        // Admin kicks `member`; the LOCAL viewer is `member` → Kicked (the caller tears down locally).
        let kick = seal_kick(&channel, &admin, &member.public_key().to_hex(), post_join_ms(), Some(&cite));
        match process_incoming(&mut state, &kick, &channel, &member.public_key()) {
            Some(IncomingEvent::Kicked { community_id }) => assert!(!community_id.is_empty()),
            _ => panic!("expected Kicked"),
        }
        crate::db::close_database();
    }

    #[test]
    fn cited_admin_kick_of_other_member_is_a_leave() {
        use nostr_sdk::ToBech32;
        let owner = Keys::generate();
        let admin = Keys::generate();
        let member = Keys::generate();
        let (_tmp, _g, channel, cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();
        // Admin kicks `member`; the LOCAL viewer is the owner → reflected as a leave Presence so `member`
        // drops out of the observed member list (no dedicated arm).
        let kick = seal_kick(&channel, &admin, &member.public_key().to_hex(), post_join_ms(), Some(&cite));
        match process_incoming(&mut state, &kick, &channel, &owner.public_key()) {
            Some(IncomingEvent::Presence { npub, joined, .. }) => {
                assert!(!joined);
                assert_eq!(npub, member.public_key().to_bech32().unwrap());
            }
            _ => panic!("expected leave Presence"),
        }
        crate::db::close_database();
    }

    #[test]
    fn uncited_kick_is_dropped() {
        let owner = Keys::generate();
        let admin = Keys::generate();
        let member = Keys::generate();
        let (_tmp, _g, channel, _cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();
        let kick = seal_kick(&channel, &admin, &member.public_key().to_hex(), 1, None);
        assert!(process_incoming(&mut state, &kick, &channel, &member.public_key()).is_none(),
            "a non-owner kick without a citation is dropped");
        crate::db::close_database();
    }

    #[test]
    fn unprivileged_kick_is_dropped() {
        let owner = Keys::generate();
        let admin = Keys::generate();
        let mallory = Keys::generate();
        let member = Keys::generate();
        let (_tmp, _g, channel, cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();
        // Mallory holds no grant; even replaying the admin's citation, mallory's own grant locator has no
        // synced head AND the roster doesn't rank them with KICK → dropped (double-gated).
        let kick = seal_kick(&channel, &mallory, &member.public_key().to_hex(), 1, Some(&cite));
        assert!(process_incoming(&mut state, &kick, &channel, &member.public_key()).is_none(),
            "a kick from an unranked actor is dropped");
        crate::db::close_database();
    }

    #[test]
    fn kick_of_owner_is_dropped() {
        let owner = Keys::generate();
        let admin = Keys::generate();
        let (_tmp, _g, channel, cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();
        // The owner is never a valid target of an authority action (owner-protection, no hardcoded carve-out).
        let kick = seal_kick(&channel, &admin, &owner.public_key().to_hex(), post_join_ms(), Some(&cite));
        assert!(process_incoming(&mut state, &kick, &channel, &owner.public_key()).is_none(),
            "an admin cannot kick the owner");
        crate::db::close_database();
    }

    #[test]
    fn stale_kick_predating_join_is_dropped() {
        let owner = Keys::generate();
        let admin = Keys::generate();
        let member = Keys::generate();
        let (_tmp, _g, channel, cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();
        // A fully-authorized kick whose inner timestamp PREDATES this account's join (ms=1 → created_at 0)
        // is ignored, so a re-accepted invite isn't undone by a stale kick replayed from history.
        let kick = seal_kick(&channel, &admin, &member.public_key().to_hex(), 1, Some(&cite));
        assert!(process_incoming(&mut state, &kick, &channel, &member.public_key()).is_none(),
            "a kick older than the current join is dropped");
        crate::db::close_database();
    }

    #[test]
    fn webxdc_signals_parse_ad_and_left_and_reject_garbage() {
        use crate::stored_event::event_kind;
        use nostr_sdk::ToBech32;
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let c = test_channel();
        let viewer = Keys::generate();
        let mk = |content: &str, ms: u64| {
            let inner = build_inner_typed(alice.public_key(), &c.id, c.epoch, event_kind::COMMUNITY_WEBXDC, content, ms, None, &[])
                .sign_with_keys(&alice).unwrap();
            seal_with_signed_inner(&Keys::generate(), &inner, &c.key, &c.id, c.epoch).unwrap()
        };
        let topic = crate::webxdc::mint_topic_id("game-hash", "sender");

        // Advertisement: topic + addr surface, author attributed.
        let ad = serde_json::json!({ "op": "ad", "topic": topic, "addr": "BASE32NODEADDR" }).to_string();
        match process_incoming(&mut state, &mk(&ad, 1), &c, &viewer.public_key()) {
            Some(IncomingEvent::WebxdcPeer { npub, topic_id, node_addr, .. }) => {
                assert_eq!(npub, alice.public_key().to_bech32().unwrap(), "player is the inner author");
                assert_eq!(topic_id, topic);
                assert_eq!(node_addr.as_deref(), Some("BASE32NODEADDR"));
            }
            _ => panic!("expected a webxdc advertisement"),
        }

        // Peer-left: no addr.
        let left = serde_json::json!({ "op": "left", "topic": topic }).to_string();
        match process_incoming(&mut state, &mk(&left, 2), &c, &viewer.public_key()) {
            Some(IncomingEvent::WebxdcPeer { node_addr, .. }) => {
                assert!(node_addr.is_none(), "peer-left carries no addr");
            }
            _ => panic!("expected a webxdc peer-left"),
        }

        // Own echo is dropped — the local realtime layer already tracks itself.
        assert!(
            process_incoming(&mut state, &mk(&ad, 3), &c, &alice.public_key()).is_none(),
            "own webxdc signal must be ignored"
        );

        // Garbage: malformed topic (author-controlled), unknown op, ad missing addr, non-JSON.
        let bad_topic = serde_json::json!({ "op": "ad", "topic": "../../etc", "addr": "X" }).to_string();
        assert!(process_incoming(&mut state, &mk(&bad_topic, 4), &c, &viewer.public_key()).is_none());
        let bad_op = serde_json::json!({ "op": "explode", "topic": topic }).to_string();
        assert!(process_incoming(&mut state, &mk(&bad_op, 5), &c, &viewer.public_key()).is_none());
        let no_addr = serde_json::json!({ "op": "ad", "topic": topic }).to_string();
        assert!(process_incoming(&mut state, &mk(&no_addr, 6), &c, &viewer.public_key()).is_none());
        assert!(process_incoming(&mut state, &mk("not json", 7), &c, &viewer.public_key()).is_none());
    }

    #[test]
    fn typing_indicator_parses_drops_own_echo_and_rejects_garbage() {
        use crate::stored_event::event_kind;
        use nostr_sdk::ToBech32;
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let c = test_channel();
        let viewer = Keys::generate();
        let mk = |content: &str, ms: u64| {
            let inner = build_inner_typed(alice.public_key(), &c.id, c.epoch, event_kind::COMMUNITY_TYPING, content, ms, None, &[])
                .sign_with_keys(&alice).unwrap();
            seal_with_signed_inner(&Keys::generate(), &inner, &c.key, &c.id, c.epoch).unwrap()
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

        // A "typing" signal from another member surfaces, attributed to the inner author, with a
        // receiver-computed near-future `until` (not the sender's clock).
        match process_incoming(&mut state, &mk("typing", 1), &c, &viewer.public_key()) {
            Some(IncomingEvent::Typing { npub, until }) => {
                assert_eq!(npub, alice.public_key().to_bech32().unwrap(), "typer is the inner author");
                assert!(until >= now && until <= now + 31, "until is receiver-computed (~now + 30s)");
            }
            _ => panic!("expected a typing indicator"),
        }

        // Own echo is dropped — we never show ourselves typing.
        assert!(
            process_incoming(&mut state, &mk("typing", 2), &c, &alice.public_key()).is_none(),
            "own typing signal must be ignored"
        );

        // Wrong content (a 3311 carrying anything but "typing") is rejected.
        assert!(process_incoming(&mut state, &mk("nope", 3), &c, &viewer.public_key()).is_none());
    }

    #[test]
    fn presence_announcements_parse_join_and_leave() {
        use crate::stored_event::event_kind;
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let c = test_channel();
        let viewer = Keys::generate();
        let mk = |content: &str, ms: u64| {
            let inner = build_inner_typed(alice.public_key(), &c.id, c.epoch, event_kind::COMMUNITY_PRESENCE, content, ms, None, &[])
                .sign_with_keys(&alice).unwrap();
            seal_with_signed_inner(&Keys::generate(), &inner, &c.key, &c.id, c.epoch).unwrap()
        };
        match process_incoming(&mut state, &mk("join", 1), &c, &viewer.public_key()) {
            Some(IncomingEvent::Presence { npub, joined, .. }) => {
                assert!(joined, "content 'join' → joined");
                assert_eq!(npub, alice.public_key().to_bech32().unwrap(), "announcer is the inner author");
            }
            _ => panic!("expected a join presence"),
        }
        match process_incoming(&mut state, &mk("leave", 2), &c, &viewer.public_key()) {
            Some(IncomingEvent::Presence { joined, invited_by, .. }) => {
                assert!(!joined, "content 'leave' → not joined");
                assert!(invited_by.is_none(), "a plain leave carries no attribution");
            }
            _ => panic!("expected a leave presence"),
        }
        // attributed join: content is `{"by":"<npub>","l":"<label>"}` → invited_by/label surface
        // (only when `by` is a REAL pubkey — a forged non-npub is dropped).
        let jean = Keys::generate().public_key().to_bech32().unwrap();
        let attributed = serde_json::json!({ "by": jean, "l": "Reddit" }).to_string();
        match process_incoming(&mut state, &mk(&attributed, 3), &c, &viewer.public_key()) {
            Some(IncomingEvent::Presence { joined, invited_by, invited_label, .. }) => {
                assert!(joined, "an attributed-join JSON is still a join");
                assert_eq!(invited_by.as_deref(), Some(jean.as_str()), "valid inviter npub surfaced");
                assert_eq!(invited_label.as_deref(), Some("Reddit"), "link label surfaced");
            }
            _ => panic!("expected an attributed join presence"),
        }
        // A forged non-pubkey `by` is dropped (no arbitrary text leaks into attribution).
        let forged = serde_json::json!({ "by": "haha not an npub", "l": "x" }).to_string();
        match process_incoming(&mut state, &mk(&forged, 4), &c, &viewer.public_key()) {
            Some(IncomingEvent::Presence { invited_by, .. }) => assert!(invited_by.is_none(), "forged inviter dropped"),
            _ => panic!("expected a join presence"),
        }
    }

    #[test]
    fn leave_presence_authored_by_local_npub_yields_self_left() {
        use crate::stored_event::event_kind;
        // a leave-presence whose inner author IS the local npub is a self-removal → SelfLeft, so the
        // leave propagates to every device. A DB-backed channel is needed (community_id resolution).
        let owner = Keys::generate();
        let admin = Keys::generate();
        let (_tmp, _g, channel, _cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();
        // A FRESH self-leave (newer than the join) is the teardown case — build it after the
        // community's recorded join time so the staleness gate treats it as a real SelfLeft.
        let cid = crate::db::community::community_id_for_channel(&channel.id.to_hex()).unwrap().unwrap();
        let cid_bytes = crate::community::CommunityId(crate::simd::hex::hex_to_bytes_32(&cid));
        let leave_ms = crate::db::community::community_created_at_ms(&cid_bytes).unwrap_or(0) + 10_000;
        let leave = {
            let inner = build_inner_typed(owner.public_key(), &channel.id, channel.epoch, event_kind::COMMUNITY_PRESENCE, "leave", leave_ms, None, &[])
                .sign_with_keys(&owner).unwrap();
            seal_with_signed_inner(&Keys::generate(), &inner, &channel.key, &channel.id, channel.epoch).unwrap()
        };
        match process_incoming(&mut state, &leave, &channel, &owner.public_key()) {
            Some(IncomingEvent::SelfLeft { community_id }) => assert!(!community_id.is_empty()),
            _ => panic!("expected SelfLeft"),
        }
        crate::db::close_database();
    }

    #[test]
    fn leave_presence_authored_by_another_npub_stays_a_plain_leave() {
        use crate::stored_event::event_kind;
        use nostr_sdk::ToBech32;
        // A leave by SOMEONE ELSE is just a member-list departure, NOT a self-removal.
        let owner = Keys::generate();
        let admin = Keys::generate();
        let other = Keys::generate();
        let (_tmp, _g, channel, _cite) = db_roster_channel(&owner, &admin.public_key());
        let mut state = ChatState::new();
        let leave = {
            let inner = build_inner_typed(other.public_key(), &channel.id, channel.epoch, event_kind::COMMUNITY_PRESENCE, "leave", 2, None, &[])
                .sign_with_keys(&other).unwrap();
            seal_with_signed_inner(&Keys::generate(), &inner, &channel.key, &channel.id, channel.epoch).unwrap()
        };
        // LOCAL viewer is the owner; the leave is `other`'s → plain Presence{joined:false}.
        match process_incoming(&mut state, &leave, &channel, &owner.public_key()) {
            Some(IncomingEvent::Presence { npub, joined, .. }) => {
                assert!(!joined);
                assert_eq!(npub, other.public_key().to_bech32().unwrap());
            }
            _ => panic!("expected plain leave Presence"),
        }
        crate::db::close_database();
    }

    #[test]
    fn self_delete_still_applies_after_keep_keys_teardown() {
        // after a self-removal teardown that RETAINS the epoch keys, a 3305 self-delete of one's own
        // past message still works — the channel is reconstructed from the retained key and the delete opens.
        let owner = Keys::generate();
        let admin = Keys::generate();
        let (_tmp, _g, channel, _cite) = db_roster_channel(&owner, &admin.public_key());
        let cid = crate::db::community::community_id_for_channel(&channel.id.to_hex()).unwrap().unwrap();
        let chan_hex = channel.id.to_hex();
        let epoch = channel.epoch.0;

        // The local user posts a message under the channel's current epoch.
        let mut state = ChatState::new();
        let target = ingest_msg_in(&mut state, &channel, &owner, "mine", 1, &owner);

        // Self-removal teardown that retains keys, then reconstruct the channel from the RETAINED key.
        crate::db::community::delete_community_retain_keys(&cid).unwrap();
        let retained = crate::db::community::held_epoch_key(&cid, &chan_hex, epoch).unwrap()
            .expect("epoch key retained after keep-keys teardown");
        let mut rebuilt = channel.clone();
        rebuilt.key = ChannelKey(retained);
        rebuilt.epoch = Epoch(epoch);

        // A 3305 self-delete authored by the local user opens under the retained key and removes the message.
        let del = seal_hide(&rebuilt, &owner, &target, 2, None);
        match process_incoming(&mut state, &del, &rebuilt, &owner.public_key()) {
            Some(IncomingEvent::Removed { target_id }) => assert_eq!(target_id, target),
            _ => panic!("expected the self-delete to apply under the retained key"),
        }
        crate::db::close_database();
    }

    #[test]
    fn banned_author_events_are_dropped_including_presence() {
        use crate::stored_event::event_kind;
        let mut state = ChatState::new();
        let alice = Keys::generate(); // will be banned
        let bob = Keys::generate();
        let mut c = test_channel();
        c.banned = vec![alice.public_key()];

        // A banned author's message is dropped before any STATE mutation.
        let spam = seal_message(&alice, &c.key, &c.id, c.epoch, "spam", 1).unwrap();
        assert!(process_incoming(&mut state, &spam, &c, &bob.public_key()).is_none(), "banned message dropped");

        // A banned author's PRESENCE is dropped too (the anti-memberlist must hide them entirely).
        let pres_inner = build_inner_typed(alice.public_key(), &c.id, c.epoch, event_kind::COMMUNITY_PRESENCE, "join", 2, None, &[])
            .sign_with_keys(&alice).unwrap();
        let pres = seal_with_signed_inner(&Keys::generate(), &pres_inner, &c.key, &c.id, c.epoch).unwrap();
        assert!(process_incoming(&mut state, &pres, &c, &bob.public_key()).is_none(), "banned presence dropped");

        // A non-banned author is unaffected.
        let ok = seal_message(&bob, &c.key, &c.id, c.epoch, "hi", 3).unwrap();
        assert!(matches!(process_incoming(&mut state, &ok, &c, &bob.public_key()), Some(IncomingEvent::NewMessage(_))), "non-banned applied");
    }

    #[test]
    fn cooperative_delete_applies_after_message_in_batch_order() {
        use crate::stored_event::event_kind;
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let c = test_channel();

        // A 3300 message and its author's 3305 delete, fed as ONE batch with the delete listed
        // first — process_channel_batch must ingest the message (pass 1) before the delete
        // (pass 2), so the tombstone lands on a present target.
        let msg_outer = seal_message(&alice, &c.key, &c.id, c.epoch, "bye", 1).unwrap();
        let opened = open_message(&msg_outer, &c.key, &c.id, c.epoch).unwrap();
        let inner_id = opened.message_id.to_hex();
        let del_outer = seal_typed(&alice, event_kind::COMMUNITY_DELETE, "", 2, &inner_id);

        let applied = process_channel_batch(&mut state, &[del_outer, msg_outer], &c, &alice.public_key());
        assert!(applied.iter().any(|e| matches!(e, IncomingEvent::NewMessage(_))));
        assert!(applied.iter().any(|e| matches!(e, IncomingEvent::Removed { .. })));
        assert!(state.find_message(&inner_id).is_none(), "delete applied despite arriving first");
    }

    #[test]
    fn build_message_sets_mine_and_author() {
        let me = Keys::generate();
        let opened = opened_from(&me, "hello", 4242);
        let msg = build_message(&opened, &me.public_key());
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.at, 4242);
        assert!(msg.mine, "author == me → mine");
        assert_eq!(msg.npub, me.public_key().to_bech32().ok());
        assert_eq!(msg.id, opened.message_id.to_hex());

        // A message from someone else is not mine.
        let other_view = build_message(&opened, &Keys::generate().public_key());
        assert!(!other_view.mine);
    }

    #[test]
    fn ingest_creates_community_chat_and_adds_message() {
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let opened = opened_from(&alice, "gm", 1);

        assert!(ingest_message(&mut state, &opened, &alice.public_key()).is_some());
        // A Community chat now exists, keyed by the channel id, typed Community.
        let chat = state.chats.iter().find(|c| c.id == opened.channel_id.to_hex()).expect("chat");
        assert!(chat.is_community(), "channel chat must be ChatType::Community");
    }

    #[test]
    fn process_incoming_ingests_valid_drops_foreign() {
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let key = ChannelKey([0x33u8; 32]);
        let chan = ChannelId([0x44u8; 32]);
        let channel = Channel { id: chan, key: key.clone(), epoch: Epoch(0), name: "g".into(), banned: Vec::new(), protected: Vec::new(), roster: Default::default(), epoch_keys: Vec::new(), dissolved: false };

        // A valid event for this channel lands.
        let outer = seal_message(&alice, &key, &chan, Epoch(0), "real", 1).unwrap();
        assert!(process_incoming(&mut state, &outer, &channel, &alice.public_key()).is_some());
        assert!(state.chats.iter().any(|c| c.is_community()));

        // An event for a DIFFERENT channel (wrong key) is dropped, no chat created.
        let other_key = ChannelKey([0x99u8; 32]);
        let other_chan = ChannelId([0xaau8; 32]);
        let foreign = seal_message(&alice, &other_key, &other_chan, Epoch(0), "nope", 1).unwrap();
        assert!(process_incoming(&mut state, &foreign, &channel, &alice.public_key()).is_none());
        assert_eq!(state.chats.iter().filter(|c| c.is_community()).count(), 1);
    }

    #[test]
    fn ingest_dedups_on_message_id() {
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let opened = opened_from(&alice, "once", 1);

        assert!(ingest_message(&mut state, &opened, &alice.public_key()).is_some(), "first add");
        assert!(ingest_message(&mut state, &opened, &alice.public_key()).is_none(), "duplicate not re-added");
        // Still exactly one Community chat.
        assert_eq!(state.chats.iter().filter(|c| c.is_community()).count(), 1);
    }

    #[test]
    fn dedup_keys_on_inner_id_across_distinct_outer_events() {
        // The real invariant: a re-broadcast of the SAME inner message (same
        // inner id) wrapped in a DIFFERENT outer event must dedup. Sealing twice with
        // identical params yields the same inner event (created_at is derived from
        // ms, so it's deterministic) but distinct outer events (fresh ephemeral key +
        // nonce). The second must NOT add a second message.
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let key = ChannelKey([0x33u8; 32]);
        let chan = ChannelId([0x44u8; 32]);
        let channel = Channel { id: chan, key: key.clone(), epoch: Epoch(0), name: "g".into(), banned: Vec::new(), protected: Vec::new(), roster: Default::default(), epoch_keys: Vec::new(), dissolved: false };

        let outer_a = seal_message(&alice, &key, &chan, Epoch(0), "dup", 7).unwrap();
        let outer_b = seal_message(&alice, &key, &chan, Epoch(0), "dup", 7).unwrap();
        assert_ne!(outer_a.id, outer_b.id, "distinct outer events (fresh ephemeral + nonce)");

        assert!(process_incoming(&mut state, &outer_a, &channel, &alice.public_key()).is_some());
        assert!(
            process_incoming(&mut state, &outer_b, &channel, &alice.public_key()).is_none(),
            "same inner message id must dedup despite a different outer event"
        );
    }

    #[test]
    fn route_incoming_routes_by_pseudonym() {
        let mut state = ChatState::new();
        let alice = Keys::generate();
        let key = ChannelKey([0x33u8; 32]);
        let chan = ChannelId([0x44u8; 32]);
        let channel = Channel { id: chan, key: key.clone(), epoch: Epoch(0), name: "g".into(), banned: Vec::new(), protected: Vec::new(), roster: Default::default(), epoch_keys: Vec::new(), dissolved: false };

        // Routing table keyed by the channel's epoch pseudonym.
        let mut routes = HashMap::new();
        routes.insert(channel_pseudonym(&key, &chan, Epoch(0)).to_hex(), channel.clone());

        // An event tagged with that pseudonym routes + lands.
        let outer = seal_message(&alice, &key, &chan, Epoch(0), "routed", 1).unwrap();
        assert!(route_incoming(&mut state, &outer, &routes, &alice.public_key()).is_some());

        // An event for an UNROUTED pseudonym (different channel) is ignored.
        let other_key = ChannelKey([0x55u8; 32]);
        let other_chan = ChannelId([0x66u8; 32]);
        let unrouted = seal_message(&alice, &other_key, &other_chan, Epoch(0), "x", 1).unwrap();
        assert!(route_incoming(&mut state, &unrouted, &routes, &alice.public_key()).is_none());
    }

    #[test]
    fn ms_none_falls_back_to_created_at() {
        // Directly construct an OpenedMessage with no ms tag → `at` = created_at*1000.
        use nostr_sdk::prelude::{EventId, Timestamp, Tags};
        let author = Keys::generate();
        let opened = OpenedMessage {
            message_id: EventId::all_zeros(),
            author: author.public_key(),
            content: "no ms".into(),
            channel_id: ChannelId([1u8; 32]),
            epoch: Epoch(0),
            ms: None,
            created_at: Timestamp::from_secs(1500),
            kind: 3300,
            attachments: vec![],
            citation: None,
            wrapper_id: EventId::all_zeros(),
            tags: Tags::new(),
        };
        assert_eq!(build_message(&opened, &author.public_key()).at, 1_500_000);
    }
}
