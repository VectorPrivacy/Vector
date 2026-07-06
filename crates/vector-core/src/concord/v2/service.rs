//! Concord v2 network service: creation, boot sync, channel paging, sending,
//! and the realtime authors-routed subscription.
//!
//! v2 planes live at derived stream *addresses* (`authors` filters on kind
//! 1059/21059 wraps), so routing is a pubkey→route map — a different seam
//! from v1's `z`-tag pseudonyms, and one that never collides with DM gift
//! wraps (those have random ephemeral authors and `p`-tag the recipient).
//!
//! Multi-account: every entry point captures a `SessionGuard` and re-checks
//! it across each network await before any STATE/DB write.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::{Mutex, RwLock};

use super::community::{self, Community};
use super::control::{ControlFold, FoldMode};
use super::db;
use super::derive::GroupKey;
use super::edition::parse_edition;
use super::stream::{self, Opened, SealForm};
use super::{kind, now_ms, vsk, ChannelId, CommunityId, Epoch};
use crate::state::SessionGuard;
use crate::types::Message;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const PAGE_LIMIT: usize = 50;

// ============================================================================
// Routes & subscription
// ============================================================================

/// Where an inbound wrap belongs, resolved by its author (the plane address).
/// The `GroupKey` rides along so dispatch decrypts without a DB round-trip.
#[derive(Clone)]
enum Route {
    /// Control and dissolution motion both funnel into a full community
    /// re-sync, which re-derives its own keys.
    Control { community: CommunityId },
    Chat { community: CommunityId, channel: ChannelId, epoch: Epoch, group: GroupKey },
    Guestbook { community: CommunityId, group: GroupKey },
    Dissolved { community: CommunityId },
}

static ROUTES: LazyLock<RwLock<HashMap<PublicKey, Route>>> = LazyLock::new(|| RwLock::new(HashMap::new()));
static SUB_ID: LazyLock<Mutex<Option<SubscriptionId>>> = LazyLock::new(|| Mutex::new(None));
static POOLWIDE_SUB_ID: LazyLock<Mutex<Option<SubscriptionId>>> = LazyLock::new(|| Mutex::new(None));

/// Session-swap hygiene: drop every route and live subscription id.
pub async fn clear() {
    ROUTES.write().await.clear();
    *SUB_ID.lock().await = None;
    *POOLWIDE_SUB_ID.lock().await = None;
}

/// Rebuild the pubkey→route map from persisted communities. Returns the
/// route authors (hex) and the union of community relays.
pub async fn rebuild_routes() -> (Vec<PublicKey>, Vec<String>) {
    let communities = db::list_communities().unwrap_or_default();
    let mut map: HashMap<PublicKey, Route> = HashMap::new();
    let mut relays: Vec<String> = Vec::new();
    for c in &communities {
        let control = c.control_key();
        map.insert(control.public_key(), Route::Control { community: c.id });
        let guestbook = c.guestbook_key();
        map.insert(guestbook.public_key(), Route::Guestbook { community: c.id, group: guestbook });
        let dissolved = c.dissolved_key();
        map.insert(dissolved.public_key(), Route::Dissolved { community: c.id });
        for chan in c.channels.iter().filter(|ch| !ch.deleted) {
            if let (Some(group), Some(epoch)) = (c.channel_key(&chan.id), c.channel_epoch(&chan.id)) {
                map.insert(
                    group.public_key(),
                    Route::Chat { community: c.id, channel: chan.id, epoch, group },
                );
            }
        }
        for r in &c.relays {
            if !relays.contains(r) {
                relays.push(r.clone());
            }
        }
    }
    let authors: Vec<PublicKey> = map.keys().copied().collect();
    *ROUTES.write().await = map;
    (authors, relays)
}

/// (Re)open the live subscription: kind 1059/21059 by plane authors, on the
/// community relays (added GOSSIP|PING) plus pool-wide for Android parity.
pub async fn refresh_subscription(client: &Client) {
    let (authors, relays) = rebuild_routes().await;

    // Serialize concurrent refreshers across the unsubscribe+subscribe.
    let mut sub_guard = SUB_ID.lock().await;
    if let Some(old) = sub_guard.take() {
        client.unsubscribe(&old).await;
    }
    if authors.is_empty() {
        if let Some(old) = POOLWIDE_SUB_ID.lock().await.take() {
            client.unsubscribe(&old).await;
        }
        return;
    }

    for r in &relays {
        let _ = client.pool().add_relay(r.as_str(), crate::community_relay_options()).await;
    }
    client.connect().await;

    let filter = Filter::new()
        .kinds([Kind::Custom(kind::WRAP), Kind::Custom(kind::WRAP_EPHEMERAL)])
        .authors(authors)
        .limit(0);

    {
        let mut pw = POOLWIDE_SUB_ID.lock().await;
        if let Some(old) = pw.take() {
            client.unsubscribe(&old).await;
        }
        if let Ok(out) = client.subscribe(filter.clone(), None).await {
            *pw = Some(out.val);
        }
    }
    if !relays.is_empty() {
        if let Ok(out) = client.subscribe_to(relays, filter, None).await {
            *sub_guard = Some(out.val);
        }
    }
}

// ============================================================================
// Signing & publishing
// ============================================================================

/// Seal (via the account's `NostrSigner` — local keys or bunker) and wrap a
/// rumor at a plane's address.
async fn seal_and_wrap(
    client: &Client,
    group: &GroupKey,
    rumor: &UnsignedEvent,
    form: SealForm,
    wrap_kind: u16,
) -> Result<Event, String> {
    let signer = client.signer().await.map_err(|e| format!("signer unavailable: {e}"))?;
    let unsigned_seal =
        stream::build_seal(group, rumor.pubkey, rumor, form).map_err(|e| e.to_string())?;
    let seal = signer
        .sign_event(unsigned_seal)
        .await
        .map_err(|e| format!("seal signing failed: {e}"))?;
    stream::wrap_signed_seal(group, &seal, wrap_kind, Keys::generate().public_key())
        .map_err(|e| e.to_string())
}

async fn publish(client: &Client, relays: &[String], event: &Event) -> Result<EventId, String> {
    for r in relays {
        let _ = client.pool().add_relay(r.as_str(), crate::community_relay_options()).await;
    }
    client.connect().await;
    let out = if relays.is_empty() {
        client.send_event(event).await.map_err(|e| e.to_string())?
    } else {
        client
            .send_event_to(relays.iter().cloned(), event)
            .await
            .map_err(|e| e.to_string())?
    };
    if out.success.is_empty() {
        return Err("no relay accepted the event".into());
    }
    Ok(event.id)
}

/// Seal, wrap, and publish one chat rumor into a channel. Returns the outer
/// wrap id.
pub async fn publish_chat_rumor(
    client: &Client,
    community: &Community,
    channel: &ChannelId,
    rumor: &UnsignedEvent,
    ephemeral_kind: bool,
) -> Result<EventId, String> {
    let group = community
        .channel_key(channel)
        .ok_or("channel key not held")?;
    let wrap_kind = if ephemeral_kind { kind::WRAP_EPHEMERAL } else { kind::WRAP };
    let wrap = seal_and_wrap(client, &group, rumor, SealForm::Encrypted, wrap_kind).await?;
    publish(client, &community.relays, &wrap).await
}

// ============================================================================
// Creation
// ============================================================================

/// Found and publish a v2 Community: mint keys + `#general`, persist locally
/// FIRST (fresh-random keys must never be lost to a publish failure), then
/// publish the genesis Control Plane.
pub async fn create_community(client: &Client, name: &str, relays: Vec<String>) -> Result<Community, String> {
    let session = SessionGuard::capture();
    let signer = client.signer().await.map_err(|e| format!("signer unavailable: {e}"))?;
    let my_pk = signer.get_public_key().await.map_err(|e| e.to_string())?;

    let community::Founded { community, genesis } =
        Community::found(my_pk, name, relays, now_ms())?;

    if !session.is_valid() {
        return Err("account changed during creation".into());
    }
    db::save_community(&community)?;

    let control = community.control_key();
    for rumor in &genesis {
        let wrap = seal_and_wrap(client, &control, rumor, SealForm::Plaintext, kind::WRAP).await?;
        publish(client, &community.relays, &wrap).await?;
        if let Ok(ed) = parse_edition(rumor) {
            if session.is_valid() {
                // The seal survives inside the wrap; persist its JSON for the
                // boot fold. Reconstruct it the same way the wrap carried it.
                let seal = stream::build_seal(&control, rumor.pubkey, rumor, SealForm::Plaintext)
                    .map_err(|e| e.to_string())?;
                let _ = db::save_edition_seal(&community.id, &ed.eid, ed.version, &seal.as_json());
            }
        }
    }

    if !session.is_valid() {
        return Err("account changed during creation".into());
    }
    sync_chats(&community).await;
    refresh_subscription(client).await;
    Ok(community)
}

// ============================================================================
// STATE / chat rows
// ============================================================================

/// Mirror every channel into STATE as a Community chat row + persist the slim
/// rows, so v2 channels load uniformly with DMs and v1 channels at startup.
pub async fn sync_chats(community: &Community) {
    let session = SessionGuard::capture();
    let my_pk = crate::state::my_public_key();
    let is_owner = my_pk.map(|pk| pk == community.owner).unwrap_or(false);
    let owner_npub = community.owner.to_bech32().ok();
    let created_at_ms = db::community_created_at_ms(&community.id);
    let dissolved = db::is_dissolved(&community.id);
    let slims = {
        let mut state = crate::state::STATE.lock().await;
        let mut slims = Vec::new();
        for ch in community.channels.iter().filter(|c| !c.deleted) {
            let channel_id = ch.id.to_hex();
            state.upsert_community_chat(
                &channel_id,
                &community.name,
                community.description.as_deref().unwrap_or(""),
                &community.id.to_hex(),
                is_owner,
                false,
                owner_npub.as_deref(),
                created_at_ms,
                dissolved,
            );
            if let Some(chat) = state.chats.iter().find(|c| c.id == channel_id) {
                slims.push(crate::db::chats::SlimChatDB::from_chat(chat, &state.interner));
            }
        }
        slims
    };
    if !session.is_valid() {
        return;
    }
    for slim in &slims {
        let _ = crate::db::chats::save_slim_chat(slim);
    }
}

/// Local teardown: STATE chats + chat rows + concord2 rows.
pub async fn teardown_local(community: &Community) {
    let channel_ids: Vec<String> = community.channels.iter().map(|c| c.id.to_hex()).collect();
    {
        let mut state = crate::state::STATE.lock().await;
        state.chats.retain(|c| !channel_ids.contains(&c.id));
    }
    for id in &channel_ids {
        let _ = crate::db::chats::delete_chat(id);
    }
    let _ = db::delete_community(&community.id);
}

// ============================================================================
// Control fold
// ============================================================================

fn fold_from_seal_jsons(community: &Community, seals: &[String]) -> ControlFold {
    // Persisted seals were verified on ingest; fresh joins are handled by the
    // invite path (future pass), so Tracking is the boot default.
    let mut fold = ControlFold::new(community.id, community.owner, FoldMode::Tracking);
    let mut editions = Vec::new();
    for json in seals {
        let Ok(seal) = Event::from_json(json) else { continue };
        if seal.verify().is_err() {
            continue;
        }
        let Ok(mut rumor) = UnsignedEvent::from_json(seal.content.as_bytes()) else { continue };
        rumor.ensure_id();
        if rumor.pubkey != seal.pubkey {
            continue;
        }
        if let Ok(ed) = parse_edition(&rumor) {
            editions.push(ed);
        }
    }
    fold.ingest(editions);
    fold
}

/// Fetch + fold the Control Plane and the dissolution coordinate for one
/// community, apply the result, and persist. Returns the refreshed Community.
pub async fn sync_community(client: &Client, mut community: Community) -> Result<Community, String> {
    let session = SessionGuard::capture();
    let control = community.control_key();
    let dissolved_key = community.dissolved_key();

    // Rebuild the fold from persisted editions, then extend it from relays.
    let persisted = db::load_edition_seals(&community.id).unwrap_or_default();
    let mut fold = fold_from_seal_jsons(&community, &persisted);

    let filter = Filter::new()
        .kind(Kind::Custom(kind::WRAP))
        .authors([control.public_key(), dissolved_key.public_key()]);
    let events = fetch(client, &community.relays, filter).await?;

    if !session.is_valid() {
        return Err("account changed during community sync".into());
    }

    for event in &events {
        if event.pubkey == dissolved_key.public_key() {
            if let Ok(opened) = stream::open(&dissolved_key, event) {
                if let Ok(ed) = parse_edition(&opened.rumor) {
                    if ed.vsk == vsk::DISSOLVED && fold.ingest_dissolution(&ed) {
                        let _ = db::set_dissolved(&community.id);
                    }
                }
            }
            continue;
        }
        if let Ok(opened) = stream::open(&control, event) {
            if opened.seal_form != SealForm::Plaintext {
                continue;
            }
            if let Ok(ed) = parse_edition(&opened.rumor) {
                // Persist the signed seal verbatim (fold rebuild + compaction).
                let seal_json = decrypt_seal_json(&control, event);
                if let Some(json) = seal_json {
                    let _ = db::save_edition_seal(&community.id, &ed.eid, ed.version, &json);
                }
                fold.ingest([ed]);
            }
        }
    }

    community.apply_control(&fold);
    if !session.is_valid() {
        return Err("account changed during community sync".into());
    }
    db::save_community(&community)?;
    sync_chats(&community).await;
    Ok(community)
}

/// The signed seal JSON inside a wrap (for verbatim persistence).
fn decrypt_seal_json(group: &GroupKey, wrap: &Event) -> Option<String> {
    use nostr_sdk::nips::nip44::v2::decrypt_to_bytes;
    let ct = base64_simd::STANDARD.decode_to_vec(wrap.content.as_bytes()).ok()?;
    let seal_bytes = decrypt_to_bytes(group.conversation_key(), &ct).ok()?;
    String::from_utf8(seal_bytes).ok()
}

// ============================================================================
// Fetching & message ingest
// ============================================================================

async fn fetch(client: &Client, relays: &[String], filter: Filter) -> Result<Vec<Event>, String> {
    for r in relays {
        let _ = client.pool().add_relay(r.as_str(), crate::community_relay_options()).await;
    }
    client.connect().await;
    let events = if relays.is_empty() {
        client.fetch_events(filter, FETCH_TIMEOUT).await
    } else {
        client
            .fetch_events_from(relays.iter().cloned(), filter, FETCH_TIMEOUT)
            .await
    }
    .map_err(|e| format!("fetch failed: {e}"))?;
    Ok(events.into_iter().collect())
}

/// Build a UI `Message` from an opened chat rumor. `None` for a malformed
/// `ms` tag (dropped, never interpreted).
fn message_from_opened(opened: &Opened, my_pk: &PublicKey) -> Option<Message> {
    let at = opened.timestamp_ms()?;
    let replied_to = opened
        .rumor
        .tags
        .iter()
        .find(|t| t.kind() == TagKind::q())
        .and_then(|t| t.content())
        .unwrap_or("")
        .to_string();
    let emoji_tags: Vec<crate::types::EmojiTag> = opened
        .rumor
        .tags
        .iter()
        .filter(|t| t.kind() == TagKind::Custom("emoji".into()))
        .filter_map(|t| {
            let s = t.as_slice();
            Some(crate::types::EmojiTag { shortcode: s.get(1)?.clone(), url: s.get(2)?.clone() })
        })
        .collect();
    Some(Message {
        id: opened.rumor.id?.to_hex(),
        content: opened.rumor.content.clone(),
        replied_to,
        at,
        mine: opened.author == *my_pk,
        npub: opened.author.to_bech32().ok(),
        wrapper_event_id: Some(opened.wrap_id.to_hex()),
        emoji_tags,
        ..Default::default()
    })
}

/// Ingest one opened kind-9 rumor into STATE + DB under its channel chat.
/// Returns the added message, or `None` on dedup/malformed.
async fn ingest_message(opened: &Opened, channel: &ChannelId, my_pk: &PublicKey) -> Option<Message> {
    let msg = message_from_opened(opened, my_pk)?;
    // Inner-id dedup mirrors the DM pipeline: known events load from the DB.
    if crate::db::events::event_exists(&msg.id).unwrap_or(false) {
        return None;
    }
    let chat_id = channel.to_hex();
    let added = {
        let mut state = crate::state::STATE.lock().await;
        state.ensure_community_chat(&chat_id);
        state.add_message_to_chat(&chat_id, msg.clone())
    };
    if !added {
        return None;
    }
    let _ = crate::db::events::save_message(&chat_id, &msg);
    Some(msg)
}

/// A dispatched inbound outcome the shell surfaces to the UI.
pub enum Inbound {
    NewMessage { chat_id: String, message: Message },
    Typing { chat_id: String, npub: String, until: u64 },
    /// Handled internally (dedup, drop, self-echo, control re-sync spawn).
    Handled,
}

/// Route one wrap by its author. Returns `None` if the author is not a v2
/// plane (the caller falls through to the DM pipeline).
pub async fn dispatch_event(session: &SessionGuard, client: &Client, event: &Event) -> Option<Inbound> {
    let route = ROUTES.read().await.get(&event.pubkey).cloned()?;
    // Once the author matched a v2 plane, the event is OURS: every early exit
    // below is `Handled` (a drop), never `None` (which would leak a v2 wrap
    // into the DM gift-wrap pipeline).
    if !session.is_valid() {
        return Some(Inbound::Handled);
    }
    let Some(my_pk) = crate::state::my_public_key() else {
        return Some(Inbound::Handled);
    };

    match route {
        Route::Chat { community, channel, epoch, group } => {
            // Outer dedup before decryption (shared cross-transport ledger).
            let outer = event.id.to_bytes();
            if crate::db::events::wrapper_event_exists(&event.id.to_hex()).unwrap_or(false)
                || crate::db::wrappers::processed_wrapper_exists(&outer)
            {
                return Some(Inbound::Handled);
            }
            if db::is_dissolved(&community) {
                return Some(Inbound::Handled);
            }
            let Ok(opened) = stream::open(&group, event) else {
                return Some(Inbound::Handled);
            };
            if stream::check_binding(&opened.rumor, &channel, epoch).is_err() {
                return Some(Inbound::Handled);
            }
            match opened.rumor.kind.as_u16() {
                kind::MESSAGE => match ingest_message(&opened, &channel, &my_pk).await {
                    Some(msg) => Some(Inbound::NewMessage { chat_id: channel.to_hex(), message: msg }),
                    None => Some(Inbound::Handled),
                },
                kind::TYPING => {
                    if opened.author == my_pk {
                        return Some(Inbound::Handled);
                    }
                    match opened.author.to_bech32() {
                        Ok(npub) => Some(Inbound::Typing {
                            chat_id: channel.to_hex(),
                            npub,
                            until: (now_ms() / 1000) + 30,
                        }),
                        Err(_) => Some(Inbound::Handled),
                    }
                }
                // Reactions/edits/deletes land with the moderation pass.
                _ => Some(Inbound::Handled),
            }
        }
        Route::Control { community } | Route::Dissolved { community } => {
            // Any control-plane motion triggers a full re-sync (small plane,
            // convergence-driving, must stay complete). Spawned — a re-sync is
            // seconds of network I/O and must not stall the notification loop.
            let task_session = SessionGuard::capture();
            let client = client.clone();
            tokio::spawn(async move {
                if !task_session.is_valid() {
                    return;
                }
                let Some(loaded) = db::load_community(&community).ok().flatten() else { return };
                let Ok(refreshed) = sync_community(&client, loaded).await else { return };
                if !task_session.is_valid() {
                    return;
                }
                refresh_subscription(&client).await;
                crate::traits::emit_event(
                    "community_refreshed",
                    &serde_json::json!({ "community_id": refreshed.id.to_hex() }),
                );
            });
            Some(Inbound::Handled)
        }
        Route::Guestbook { community, group } => {
            // Observed membership only in this pass: record the author's
            // presence motion into the ledger so replays skip early.
            let outer = event.id.to_bytes();
            if crate::db::wrappers::processed_wrapper_exists(&outer) {
                return Some(Inbound::Handled);
            }
            if stream::open(&group, event).is_ok() {
                let _ = crate::db::wrappers::save_processed_wrapper(
                    &outer,
                    event.created_at.as_secs(),
                    crate::db::wrappers::TRANSPORT_CONCORD2,
                );
            }
            let _ = community;
            Some(Inbound::Handled)
        }
    }
}

// ============================================================================
// Boot & paging
// ============================================================================

/// Boot sweep: control fold + latest channel page for every held community,
/// then the realtime subscription.
pub async fn boot_sync(client: &Client) -> Result<(), String> {
    let session = SessionGuard::capture();
    let ids = db::list_community_ids()?;
    for id in ids {
        if !session.is_valid() {
            return Err("account changed during boot sync".into());
        }
        let Some(community) = db::load_community(&id)? else { continue };
        let community = match sync_community(client, community).await {
            Ok(c) => c,
            Err(e) => {
                crate::log_warn!("[concord2] boot sync of {} failed: {}", id.to_hex(), e);
                continue;
            }
        };
        for chan in community.channels.iter().filter(|c| !c.deleted) {
            if !session.is_valid() {
                return Err("account changed during boot sync".into());
            }
            let _ = sync_channel_page_inner(client, &community, &chan.id, None, true).await;
        }
    }
    refresh_subscription(client).await;
    Ok(())
}

/// One page of channel history. `before_ms` pages backwards (scroll-up).
/// Returns (new_messages, reached_start, oldest_ms).
pub async fn sync_channel_page(
    client: &Client,
    channel_hex: &str,
    before_ms: Option<u64>,
) -> Result<(u32, bool, Option<u64>), String> {
    let community_hex = db::community_id_for_channel(channel_hex)?.ok_or("unknown v2 channel")?;
    let id = CommunityId::from_hex(&community_hex).ok_or("bad community id")?;
    let community = db::load_community(&id)?.ok_or("community not found")?;
    let channel = ChannelId::from_hex(channel_hex).ok_or("bad channel id")?;
    sync_channel_page_inner(client, &community, &channel, before_ms, false).await
}

async fn sync_channel_page_inner(
    client: &Client,
    community: &Community,
    channel: &ChannelId,
    before_ms: Option<u64>,
    emit_new: bool,
) -> Result<(u32, bool, Option<u64>), String> {
    let session = SessionGuard::capture();
    let group = community.channel_key(channel).ok_or("channel key not held")?;
    let epoch = community.channel_epoch(channel).ok_or("channel not found")?;
    let my_pk = crate::state::my_public_key().ok_or("public key not set")?;

    let mut filter = Filter::new()
        .kind(Kind::Custom(kind::WRAP))
        .author(group.public_key())
        .limit(PAGE_LIMIT);
    if let Some(before) = before_ms {
        // Wrap created_at is untweaked seconds (CORD-01), so second-floor works.
        filter = filter.until(Timestamp::from_secs(before / 1000));
    }
    let events = fetch(client, &community.relays, filter).await?;
    if !session.is_valid() {
        return Err("account changed during channel sync".into());
    }

    let fetched = events.len();
    let mut new_messages = 0u32;
    let mut oldest: Option<u64> = None;
    for event in &events {
        oldest = Some(oldest.map_or(event.created_at.as_secs() * 1000, |o: u64| {
            o.min(event.created_at.as_secs() * 1000)
        }));
        let outer = event.id.to_bytes();
        if crate::db::events::wrapper_event_exists(&event.id.to_hex()).unwrap_or(false)
            || crate::db::wrappers::processed_wrapper_exists(&outer)
        {
            continue;
        }
        let Ok(opened) = stream::open(&group, event) else { continue };
        if stream::check_binding(&opened.rumor, channel, epoch).is_err() {
            continue;
        }
        if opened.rumor.kind.as_u16() != kind::MESSAGE {
            continue;
        }
        if !session.is_valid() {
            return Err("account changed during channel sync".into());
        }
        if let Some(msg) = ingest_message(&opened, channel, &my_pk).await {
            new_messages += 1;
            if emit_new {
                crate::traits::emit_event(
                    "message_new",
                    &serde_json::json!({ "message": &msg, "chat_id": channel.to_hex() }),
                );
            }
        }
    }
    Ok((new_messages, fetched < PAGE_LIMIT, oldest))
}

// ============================================================================
// Lifecycle
// ============================================================================

/// Leave: publish a self-signed Guestbook Leave, then tear down locally.
pub async fn leave_community(client: &Client, community_hex: &str) -> Result<(), String> {
    let session = SessionGuard::capture();
    let id = CommunityId::from_hex(community_hex).ok_or("bad community id")?;
    let community = db::load_community(&id)?.ok_or("community not found")?;
    let my_pk = crate::state::my_public_key().ok_or("public key not set")?;

    let rumor = super::guestbook::build_join_leave(my_pk, false, now_ms(), None);
    let guestbook = community.guestbook_key();
    if let Ok(wrap) = seal_and_wrap(client, &guestbook, &rumor, SealForm::Encrypted, kind::WRAP).await {
        let _ = publish(client, &community.relays, &wrap).await;
    }

    if !session.is_valid() {
        return Err("account changed during leave".into());
    }
    teardown_local(&community).await;
    refresh_subscription(client).await;
    Ok(())
}

/// Dissolve (owner only): publish the tombstone at the dissolution
/// coordinate, mark local state read-only.
pub async fn dissolve_community(client: &Client, community_hex: &str) -> Result<(), String> {
    let session = SessionGuard::capture();
    let id = CommunityId::from_hex(community_hex).ok_or("bad community id")?;
    let community = db::load_community(&id)?.ok_or("community not found")?;
    let my_pk = crate::state::my_public_key().ok_or("public key not set")?;
    if my_pk != community.owner {
        return Err("only the owner can dissolve a community".into());
    }

    let rumor = super::edition::build_dissolved_rumor(my_pk, now_ms() / 1000);
    let dissolved = community.dissolved_key();
    let wrap = seal_and_wrap(client, &dissolved, &rumor, SealForm::Plaintext, kind::WRAP).await?;
    publish(client, &community.relays, &wrap).await?;

    if !session.is_valid() {
        return Err("account changed during dissolution".into());
    }
    db::set_dissolved(&id)?;
    sync_chats(&community).await;
    Ok(())
}

/// Publish a typing indicator (ephemeral wrap, nothing stored anywhere).
pub async fn send_typing(client: &Client, channel_hex: &str) -> Result<(), String> {
    let community_hex = db::community_id_for_channel(channel_hex)?.ok_or("unknown v2 channel")?;
    let id = CommunityId::from_hex(&community_hex).ok_or("bad community id")?;
    let community = db::load_community(&id)?.ok_or("community not found")?;
    let channel = ChannelId::from_hex(channel_hex).ok_or("bad channel id")?;
    let epoch = community.channel_epoch(&channel).ok_or("channel not found")?;
    let my_pk = crate::state::my_public_key().ok_or("public key not set")?;
    let rumor = community::build_typing(my_pk, &channel, epoch, now_ms());
    publish_chat_rumor(client, &community, &channel, &rumor, true).await?;
    Ok(())
}

/// Publish the next Community-metadata edition (rename and/or description).
/// Owner-only in this pass — delegated editors land with the roles/vac work.
pub async fn update_metadata(
    client: &Client,
    community_hex: &str,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<(), String> {
    let session = SessionGuard::capture();
    let id = CommunityId::from_hex(community_hex).ok_or("bad community id")?;
    let mut community = db::load_community(&id)?.ok_or("community not found")?;
    let my_pk = crate::state::my_public_key().ok_or("public key not set")?;
    if my_pk != community.owner {
        return Err("only the owner can edit metadata (for now)".into());
    }
    if db::is_dissolved(&id) {
        return Err("this community has been dissolved".into());
    }

    // Fold the persisted chain to find the current head — the next edition
    // chains on its hash and round-trips fields it doesn't touch.
    let persisted = db::load_edition_seals(&id)?;
    let fold = fold_from_seal_jsons(&community, &persisted);
    let head = fold.head(&id.0).ok_or("metadata head not folded yet")?.clone();
    let mut metadata: super::control::CommunityMetadata =
        serde_json::from_str(&head.content).map_err(|e| format!("metadata head: {e}"))?;
    if let Some(n) = name {
        metadata.name = n.to_string();
    }
    if let Some(d) = description {
        // Empty string clears the description.
        metadata.description = if d.is_empty() { None } else { Some(d.to_string()) };
    }
    metadata.validate()?;
    let content = serde_json::to_string(&metadata).map_err(|e| e.to_string())?;

    let rumor = super::edition::build_edition_rumor(
        my_pk,
        vsk::COMMUNITY_METADATA,
        &id.0,
        head.version + 1,
        Some(&head.hash()),
        &content,
        now_ms() / 1000,
        None,
    );
    let control = community.control_key();
    let wrap = seal_and_wrap(client, &control, &rumor, SealForm::Plaintext, kind::WRAP).await?;
    publish(client, &community.relays, &wrap).await?;

    if !session.is_valid() {
        return Err("account changed during metadata update".into());
    }
    if let Some(json) = decrypt_seal_json(&control, &wrap) {
        let _ = db::save_edition_seal(&id, &id.0, head.version + 1, &json);
    }
    community.name = metadata.name.clone();
    community.description = metadata.description.clone();
    db::save_community(&community)?;
    sync_chats(&community).await;
    Ok(())
}
