//! Mini apps (WebXDC). The page runs each app in a sandboxed iframe on its own
//! origin (`web/miniapps.js`); this side reads packages, keeps history and
//! permissions, and carries realtime channels over Iroh.

mod marketplace;
mod package;
pub(crate) mod realtime;
mod url;

/// Whether this browser can run mini apps at all (each needs a service worker),
/// as the page reports at boot.
static AVAILABLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_available(on: bool) {
    AVAILABLE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Warm the marketplace after login, where apps can run: its listings and their
/// icons are downloads nobody could use otherwise.
pub async fn preload_marketplace() {
    if AVAILABLE.load(std::sync::atomic::Ordering::Relaxed) {
        marketplace::preload().await;
    }
}

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::db;
use vector_core::event_ext::FinalizeUnsignedWithId;
use vector_core::stored_event::{event_kind, StoredEvent};
use vector_core::{state, STATE};

use crate::commands::Args;
use crate::emitter;
use realtime::TopicId;

/// An open app window, keyed by its label (`miniapp:<chat>:<message>`, as desktop).
struct Instance {
    chat_id: String,
    topic: Option<TopicId>,
}

static OPEN: Mutex<Option<HashMap<String, Instance>>> = Mutex::new(None);
/// Who is playing on each topic, as far as their signals say.
static SESSION_PEERS: Mutex<Option<HashMap<TopicId, Vec<String>>>> = Mutex::new(None);
/// Addresses advertised before we joined; dialled when we do.
static CACHED_ADDRS: Mutex<Option<HashMap<TopicId, Vec<EndpointAddr>>>> = Mutex::new(None);

use iroh::EndpointAddr;

fn with<K, V, R>(m: &Mutex<Option<HashMap<K, V>>>, f: impl FnOnce(&mut HashMap<K, V>) -> R) -> R {
    f(m.lock().unwrap().get_or_insert_with(HashMap::new))
}

fn my_npub() -> Option<String> {
    vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok())
}

fn session_peers(topic: &TopicId) -> Vec<String> {
    with(&SESSION_PEERS, |m| m.get(topic).cloned().unwrap_or_default())
}

fn add_session_peer(topic: TopicId, npub: String) {
    with(&SESSION_PEERS, |m| {
        let v = m.entry(topic).or_default();
        if !v.contains(&npub) {
            v.push(npub);
        }
    });
}

fn remove_session_peer(topic: &TopicId, npub: &str) {
    with(&SESSION_PEERS, |m| {
        if let Some(v) = m.get_mut(topic) {
            v.retain(|n| n != npub);
        }
    });
}

/// The chat a topic is played in, for advertising; solo play has none.
fn chat_for_topic(topic: &TopicId) -> Option<String> {
    with(&OPEN, |m| {
        m.values()
            .find(|i| i.topic.as_ref() == Some(topic) && !i.chat_id.is_empty() && i.chat_id != "solo")
            .map(|i| i.chat_id.clone())
    })
}

fn topic_is_open(topic: &TopicId) -> bool {
    with(&OPEN, |m| m.values().any(|i| i.topic.as_ref() == Some(topic)))
}

/// The lobby state for a topic, for the chat's app card.
pub(crate) fn emit_status(topic_encoded: &str, _peer_count: usize, is_active: bool) {
    let Ok(topic) = realtime::decode_topic_id(topic_encoded) else { return };
    let peers = session_peers(&topic);
    emitter::emit(
        "miniapp_realtime_status",
        &json!({
            "topic": topic_encoded, "peer_count": peers.len(), "peers": peers,
            "is_active": is_active, "has_pending_peers": !peers.is_empty(),
        }),
    );
}

/// A peer dropped out of the mesh: tell the chat again where we are, so it can find us.
pub(crate) fn readvertise_later(topic_encoded: String) {
    db::spawn_bound(async move {
        vector_core::rt::time::sleep(std::time::Duration::from_secs(2)).await;
        let Ok(topic) = realtime::decode_topic_id(&topic_encoded) else { return };
        let Some(chat_id) = chat_for_topic(&topic) else { return };
        let Some(iroh) = realtime::try_iroh().await else { return };
        if let Ok(addr) = realtime::encode_node_addr(&iroh.node_addr()) {
            send_signal(&chat_id, &topic_encoded, Some(&addr)).await;
        }
    });
}

// ─── Package info ───────────────────────────────────────────────────────────

fn icon_data_url(icon: &Option<(Vec<u8>, &'static str)>) -> Value {
    match icon {
        Some((bytes, mime)) => json!(format!("data:{mime};base64,{}", base64_simd::STANDARD.encode_to_string(bytes))),
        None => Value::Null,
    }
}

fn info_json(pkg: &package::Package, uses_realtime: bool) -> Value {
    json!({
        "id": pkg.file_hash, "name": pkg.manifest.name, "description": pkg.manifest.description,
        "version": pkg.manifest.version, "has_icon": pkg.icon.is_some(), "icon_data": icon_data_url(&pkg.icon),
        "source_code_url": pkg.manifest.source_code_url, "file_hash": pkg.file_hash, "uses_realtime": uses_realtime,
    })
}

fn stem(path: &str) -> String {
    Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("Mini App").to_string()
}

async fn read_package(path: &str) -> Result<(Vec<u8>, package::Package), String> {
    let bytes = vector_core::files::read(Path::new(path)).await?;
    let pkg = package::parse(&bytes, &stem(path))?;
    Ok((bytes, pkg))
}

async fn load_info(path: &str) -> Result<Value, String> {
    let (_, pkg) = read_package(path).await?;
    Ok(info_json(&pkg, pkg.uses_realtime))
}

fn load_info_from_cached() -> Result<Value, String> {
    let (bytes, name) = crate::files::pasted_bytes().ok_or("No cached file")?;
    let pkg = package::parse(&bytes, &stem(&name))?;
    Ok(info_json(&pkg, false))
}

// ─── Opening ────────────────────────────────────────────────────────────────

fn self_name(npub: &str) -> String {
    let state = STATE.try_lock();
    let name = state.ok().and_then(|s| {
        s.get_profile(npub).map(|p| {
            [p.nickname().to_string(), p.display_name.to_string(), p.name.to_string()].into_iter().find(|n| !n.is_empty())
        })
    });
    name.flatten().unwrap_or_else(|| npub.to_string())
}

fn window_label(path: &str, chat_id: &str, message_id: &str) -> String {
    if chat_id == "solo" || (chat_id.is_empty() && message_id.is_empty()) {
        let hash = vector_core::simd::hex::bytes_to_hex_string(&vector_core::crypto::sha256::digest(path.as_bytes()));
        format!("miniapp:solo:miniapp_{}", &hash[..16])
    } else {
        format!("miniapp:{chat_id}:{message_id}")
    }
}

/// Everything the page needs to open an app window. Warms the realtime channel
/// when the app uses one, so peers are found while it loads.
async fn prepare(a: &Args) -> Result<Value, String> {
    let path = a.str("filePath")?;
    let chat_id = a.opt_str("chatId").unwrap_or_default();
    let message_id = a.opt_str("messageId").unwrap_or_default();
    let (_, pkg) = read_package(&path).await?;
    let me = my_npub().ok_or("Not logged in")?;

    let listing = marketplace::app_by_hash(&pkg.file_hash);
    let partition = package::partition(&pkg.file_hash, listing.as_ref().map(|l| l.id.as_str()));
    let label = window_label(&path, &chat_id, &message_id);

    let categories = listing.as_ref().map(|l| l.categories.join(",")).unwrap_or_default();
    let _ = db::miniapps::record_miniapp_opened_with_metadata(
        pkg.manifest.name.clone(),
        path.clone(),
        path.clone(),
        categories,
        listing.as_ref().map(|l| l.id.clone()),
        listing.as_ref().map(|l| l.version.clone()),
    );

    // Solo play still gets a channel (apps expect one); it just isn't advertised.
    let solo = label.starts_with("miniapp:solo:");
    let topic = if pkg.uses_realtime {
        Some(match a.opt_str("topicId").and_then(|t| realtime::decode_topic_id(&t).ok()) {
            Some(t) => t,
            None => realtime::derive_topic_id(&pkg.manifest.name, &chat_id, &message_id),
        })
    } else {
        None
    };
    with(&OPEN, |m| m.insert(label.clone(), Instance { chat_id: chat_id.clone(), topic }));
    if let Some(topic) = topic {
        preconnect(topic, (!solo).then(|| chat_id.clone()), me.clone());
    }

    let granted = db::miniapps::get_miniapp_granted_permissions(&pkg.file_hash).unwrap_or_default();
    // The frame grants what the header allows: autoplay and gamepads, plus the user's grants.
    let mut allow = vec!["autoplay".to_string(), "gamepad".to_string()];
    for p in vector_core::webxdc_permissions::parse_permissions(&granted) {
        let name = p.policy_name().to_string();
        if !allow.contains(&name) {
            allow.push(name);
        }
    }
    Ok(json!({
        "label": label, "partition": partition, "name": pkg.manifest.name,
        "icon_data": icon_data_url(&pkg.icon), "file_hash": pkg.file_hash,
        "self_addr": me, "self_name": self_name(&me),
        "policy": vector_core::webxdc_permissions::build_permissions_policy(&granted), "allow": allow.join("; "),
        "realtime": topic.is_some(),
    }))
}

/// Join the topic with no window attached (events buffer), advertise ourselves,
/// and dial everyone already known to be playing.
fn preconnect(topic: TopicId, chat_id: Option<String>, me: String) {
    db::spawn_bound(async move {
        let topic_encoded = realtime::encode_topic_id(&topic);
        let iroh = match realtime::iroh().await {
            Ok(i) => i,
            Err(e) => return vector_core::log_warn!("[WEBXDC] {e}"),
        };
        let mut peers: Vec<EndpointAddr> = with(&CACHED_ADDRS, |m| m.remove(&topic).unwrap_or_default());
        for ad in db::miniapps::get_active_peer_advertisements(&topic_encoded, &me).unwrap_or_default() {
            if let Ok(addr) = realtime::decode_node_addr(&ad.node_addr_encoded) {
                add_session_peer(topic, ad.npub);
                if !peers.iter().any(|p| p.id == addr.id) {
                    peers.push(addr);
                }
            }
        }
        if let Err(e) = iroh.join(topic, peers, None, topic_encoded.clone()).await {
            return vector_core::log_warn!("[WEBXDC] join failed: {e}");
        }
        if let (Some(chat_id), Ok(addr)) = (chat_id, realtime::encode_node_addr(&iroh.node_addr())) {
            send_signal(&chat_id, &topic_encoded, Some(&addr)).await;
        }
        add_session_peer(topic, me);
        emit_status(&topic_encoded, 0, true);
    });
}

fn instance_topic(label: &str) -> Option<TopicId> {
    with(&OPEN, |m| m.get(label).and_then(|i| i.topic))
}

async fn rt_join(label: String) -> Result<Value, String> {
    let topic = instance_topic(&label).ok_or("This mini app has no realtime channel")?;
    let iroh = realtime::iroh().await?;
    // Normally warm already; this attaches the window and flushes what buffered.
    for _ in 0..100 {
        if iroh.has_channel(&topic).await {
            break;
        }
        vector_core::rt::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let encoded = realtime::encode_topic_id(&topic);
    iroh.join(topic, Vec::new(), Some(label), encoded.clone()).await.map_err(|e| e.to_string())?;
    Ok(json!({ "topic": encoded }))
}

pub async fn rt_send(label: &str, bytes: Vec<u8>) -> Result<Value, String> {
    if bytes.len() > 128_000 {
        return Err("Realtime message too large".into());
    }
    let topic = instance_topic(label).ok_or("No realtime channel")?;
    let iroh = realtime::try_iroh().await.ok_or("No realtime channel")?;
    iroh.send(&topic, bytes).await.map_err(|e| e.to_string())?;
    Ok(Value::Null)
}

/// The window is gone: leave the topic and tell the chat we stopped playing.
async fn closed(label: String) -> Result<Value, String> {
    let Some(inst) = with(&OPEN, |m| m.remove(&label)) else { return Ok(Value::Null) };
    let Some(topic) = inst.topic else { return Ok(Value::Null) };
    if topic_is_open(&topic) {
        return Ok(Value::Null);
    }
    if let Some(iroh) = realtime::try_iroh().await {
        iroh.leave(&topic).await;
    }
    let encoded = realtime::encode_topic_id(&topic);
    if let Some(me) = my_npub() {
        remove_session_peer(&topic, &me);
    }
    emit_status(&encoded, 0, false);
    if !label.starts_with("miniapp:solo:") {
        db::spawn_bound(async move { send_signal(&inst.chat_id, &encoded, None).await; });
    }
    Ok(Value::Null)
}

async fn realtime_status(topic_encoded: &str) -> Result<Value, String> {
    let topic = realtime::decode_topic_id(topic_encoded)?;
    let active = match realtime::try_iroh().await {
        Some(i) => i.has_channel(&topic).await,
        None => false,
    };
    let peers = session_peers(&topic);
    Ok(json!({ "active": active, "peer_count": peers.len(), "pending_peer_count": 0, "topic_id": topic_encoded, "peers": peers }))
}

// ─── Peer signals over Nostr ────────────────────────────────────────────────

/// Advertise our node on a topic (`Some(addr)`) or announce we left (`None`),
/// to a DM peer by gift wrap or into a community channel.
async fn send_signal(chat_id: &str, topic: &str, node_addr: Option<&str>) -> bool {
    let (Some(client), Some(me)) = (state::nostr_client(), vector_core::my_public_key()) else { return false };
    match PublicKey::from_bech32(chat_id) {
        Ok(pk) => {
            let mut b = EventBuilder::new(Kind::ApplicationSpecificData, if node_addr.is_some() { "peer-advertisement" } else { "peer-left" })
                .tag(Tag::public_key(pk))
                .tag(Tag::custom("d", vec!["vector-webxdc-peer"]))
                .tag(Tag::custom("webxdc-topic", vec![topic.to_string()]));
            if let Some(addr) = node_addr {
                b = b.tag(Tag::custom("webxdc-node-addr", vec![addr.to_string()]));
            }
            let rumor = b.finalize_unsigned_with_id(me);
            let relays = state::active_trusted_relays().await;
            vector_core::send_gift_wrap(&client, relays, &pk, rumor, []).await.is_ok()
        }
        Err(_) => match send_community_signal(chat_id, topic, node_addr).await {
            Ok(()) => true,
            Err(e) => {
                vector_core::log_warn!("[WEBXDC] community peer signal failed: {e}");
                false
            }
        },
    }
}

/// A peer signal sealed into a (v2) community channel as kind 3310.
async fn send_community_signal(channel_id: &str, topic: &str, node_addr: Option<&str>) -> Result<(), String> {
    use vector_core::community::{v2, ChannelId, CommunityId, ConcordProtocol};
    use vector_core::simd::hex::hex_to_bytes_32;
    let cid = db::community::community_id_for_channel(channel_id)?.ok_or("Not a community channel")?;
    let community_id = CommunityId(hex_to_bytes_32(&cid));
    if db::community::community_protocol(&community_id)? != Some(ConcordProtocol::V2) {
        return Err("Legacy communities are not available on Vector Web".into());
    }
    let community = db::community::load_community_v2(&community_id)?.ok_or("Community not found")?;
    let transport = vector_core::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
    v2::service::send_webxdc_signal(&transport, &community, &ChannelId(hex_to_bytes_32(channel_id)), topic, node_addr)
        .await
        .map_err(|e| e.to_string())
}

/// A peer's signal, from a DM or a community channel. Persisted for later joins,
/// then applied live only if it is still that peer's latest word on the topic.
pub fn on_signal(contact: String, npub: String, topic_id: String, node_addr: Option<String>, event_id: String, created_at: u64) {
    db::spawn_bound(async move {
        let Ok(topic) = realtime::decode_topic_id(&topic_id) else { return };
        let addr = match node_addr.as_deref().map(realtime::decode_node_addr) {
            Some(Ok(a)) => Some(a),
            Some(Err(_)) => return,
            None => None,
        };
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // A forged far-future signal must not outrank every later genuine one.
        let created_at = created_at.min(now + 300);
        persist_signal(&event_id, &topic_id, node_addr.as_deref(), &npub, created_at, &contact).await;
        if !db::miniapps::peer_signal_is_current(&topic_id, &npub, created_at, addr.is_some()).unwrap_or(false) {
            return;
        }
        let live = match realtime::try_iroh().await {
            Some(i) if i.has_channel(&topic).await => Some(i),
            _ => None,
        };
        match addr {
            Some(addr) => {
                add_session_peer(topic, npub);
                match &live {
                    Some(iroh) => {
                        if let Err(e) = iroh.add_peer(topic, addr).await {
                            vector_core::log_warn!("[WEBXDC] could not reach advertised peer: {e}");
                        }
                    }
                    None => with(&CACHED_ADDRS, |m| m.entry(topic).or_default().push(addr)),
                }
            }
            None => remove_session_peer(&topic, &npub),
        }
        emit_status(&topic_id, 0, live.is_some());
    });
}

async fn persist_signal(event_id: &str, topic_id: &str, node_addr: Option<&str>, npub: &str, created_at: u64, contact: &str) {
    if db::events::event_exists(event_id).unwrap_or(true) {
        return;
    }
    let Ok(chat_id) = db::id_cache::get_or_create_chat_id(contact) else { return };
    let mut tags = vec![vec!["webxdc-topic".to_string(), topic_id.to_string()]];
    if let Some(addr) = node_addr {
        tags.push(vec!["webxdc-node-addr".to_string(), addr.to_string()]);
    }
    tags.push(vec!["d".to_string(), "vector-webxdc-peer".to_string()]);
    let event = StoredEvent {
        id: event_id.to_string(),
        kind: event_kind::APPLICATION_SPECIFIC,
        chat_id,
        user_id: None,
        content: if node_addr.is_some() { "peer-advertisement" } else { "peer-left" }.to_string(),
        tags,
        reference_id: Some(topic_id.to_string()),
        created_at,
        received_at: web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        mine: false,
        pending: false,
        failed: false,
        wrapper_event_id: None,
        npub: Some(npub.to_string()),
        preview_metadata: None,
    };
    if let Err(e) = db::events::save_event(&event).await {
        vector_core::log_warn!("[WEBXDC] could not persist peer signal: {e}");
    }
}

// ─── History and permissions ────────────────────────────────────────────────

fn available_permissions() -> Value {
    serde_json::to_value(vector_core::webxdc_permissions::get_all_permission_info()).unwrap_or_default()
}

fn to_value<T: serde::Serialize>(v: T) -> Result<Value, String> {
    crate::commands::to_value(v)
}

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        if cmd.starts_with("marketplace_") {
            return marketplace::dispatch(cmd, a).await;
        }
        Some(match cmd {
            "miniapp_load_info" => match a.str("filePath") {
                Ok(p) => load_info(&p).await,
                Err(e) => Err(e),
            },
            "miniapp_load_info_from_cached_file" => load_info_from_cached(),
            "miniapp_prepare" => prepare(a).await,
            "miniapp_rt_join" => match a.str("label") {
                Ok(l) => rt_join(l).await,
                Err(e) => Err(e),
            },
            "miniapp_resolve_url_xdc" => match a.str("url") {
                Ok(u) => url::resolve(u, a.opt_str("msgId").unwrap_or_default(), a.bool("download").unwrap_or(false)).await,
                Err(e) => Err(e),
            },
            "miniapp_closed" => match a.str("label") {
                Ok(l) => closed(l).await,
                Err(e) => Err(e),
            },
            "miniapp_get_realtime_status" => match a.str("topicId") {
                Ok(t) => realtime_status(&t).await,
                Err(e) => Err(e),
            },
            "miniapp_list_open" => Ok(json!([])),
            "send_webxdc_peer_advertisement" => match (a.str("receiver"), a.str("topicId"), a.str("nodeAddr")) {
                (Ok(r), Ok(t), Ok(n)) => Ok(json!(send_signal(&r, &t, Some(&n)).await)),
                _ => Err("missing arguments".into()),
            },
            "miniapp_record_opened" => (|| {
                db::miniapps::record_miniapp_opened(a.str("name")?, a.str("srcUrl")?, a.opt_str("attachmentRef").unwrap_or_default())
            })()
            .map(|_| Value::Null),
            "miniapp_get_history" => {
                db::miniapps::get_miniapps_history(a.get("limit").and_then(Value::as_i64)).and_then(to_value)
            }
            "miniapp_remove_from_history" => match a.str("name") {
                Ok(n) => db::miniapps::remove_miniapp_from_history(&n).map(|_| Value::Null),
                Err(e) => Err(e),
            },
            "miniapp_toggle_favorite" => match a.get("id").and_then(Value::as_i64) {
                Some(id) => db::miniapps::toggle_miniapp_favorite(id).map(|v| json!(v)),
                None => Err("missing argument `id`".into()),
            },
            "miniapp_set_favorite" => match a.get("id").and_then(Value::as_i64) {
                Some(id) => db::miniapps::set_miniapp_favorite(id, a.bool("isFavorite").unwrap_or(false)).map(|_| Value::Null),
                None => Err("missing argument `id`".into()),
            },
            "miniapp_get_available_permissions" => Ok(available_permissions()),
            "miniapp_get_granted_permissions" => match a.str("fileHash") {
                Ok(h) => db::miniapps::get_miniapp_granted_permissions(&h).map(|s| json!(s)),
                Err(e) => Err(e),
            },
            "miniapp_set_permission" => (|| {
                db::miniapps::set_miniapp_permission(&a.str("fileHash")?, &a.str("permission")?, a.bool("granted").unwrap_or(false))
            })()
            .map(|_| Value::Null),
            "miniapp_set_permissions" => (|| {
                let perms: Vec<(String, bool)> = a.de("permissions")?;
                let perms: Vec<(&str, bool)> = perms.iter().map(|(p, g)| (p.as_str(), *g)).collect();
                db::miniapps::set_miniapp_permissions(&a.str("fileHash")?, &perms)
            })()
            .map(|_| Value::Null),
            "miniapp_has_permission_prompt" => match a.str("fileHash") {
                Ok(h) => db::miniapps::has_miniapp_permission_prompt(&h).map(|v| json!(v)),
                Err(e) => Err(e),
            },
            "miniapp_revoke_all_permissions" => match a.str("fileHash") {
                Ok(h) => db::miniapps::revoke_all_miniapp_permissions(&h).map(|_| Value::Null),
                Err(e) => Err(e),
            },
            _ => return None,
        })
    })
}
