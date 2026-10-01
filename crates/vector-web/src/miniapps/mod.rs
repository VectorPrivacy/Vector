//! Mini apps (WebXDC). The page runs each app in a sandboxed iframe on its own
//! origin (`web/miniapps.js`); this side reads packages, keeps history and
//! permissions, and carries realtime channels (vector-core's `xdc`).

mod marketplace;
mod package;
mod realtime;
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use nostr_sdk::prelude::*;
use serde_json::{json, Value};
use vector_core::db;
use vector_core::xdc::wire;
use vector_core::STATE;

use crate::commands::Args;
use crate::emitter;

/// An open app window, keyed by its label (`miniapp:<chat>:<message>`, as desktop).
struct Instance {
    /// Tells a reopen of the label from the window it replaced.
    id: u64,
    chat_id: String,
    /// Canonical base32, for an app that uses realtime.
    topic: Option<String>,
}

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
/// The worker serves one account (a swap reloads it), so these are its.
static OPEN: Mutex<Option<HashMap<String, Instance>>> = Mutex::new(None);
/// Who is playing on each topic, as far as their signals say.
static SESSION_PEERS: Mutex<Option<HashMap<String, Vec<String>>>> = Mutex::new(None);

fn with<K, V, R>(m: &Mutex<Option<HashMap<K, V>>>, f: impl FnOnce(&mut HashMap<K, V>) -> R) -> R {
    f(m.lock().unwrap().get_or_insert_with(HashMap::new))
}

fn my_npub() -> Option<String> {
    vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok())
}

fn session_peers(topic: &str) -> Vec<String> {
    with(&SESSION_PEERS, |m| m.get(topic).cloned().unwrap_or_default())
}

fn add_session_peer(topic: &str, npub: String) {
    with(&SESSION_PEERS, |m| {
        let v = m.entry(topic.to_string()).or_default();
        if !v.contains(&npub) {
            v.push(npub);
        }
    });
}

fn remove_session_peer(topic: &str, npub: &str) {
    with(&SESSION_PEERS, |m| {
        if let Some(v) = m.get_mut(topic) {
            v.retain(|n| n != npub);
        }
    });
}

fn topic_is_open(topic: &str) -> bool {
    with(&OPEN, |m| m.values().any(|i| i.topic.as_deref() == Some(topic)))
}

fn is_solo(chat_id: &str) -> bool {
    chat_id.is_empty() || chat_id == "solo"
}

/// The lobby state for a topic, for the chat's app card.
pub(crate) fn emit_status(topic: &str, is_active: bool) {
    let peers = session_peers(topic);
    emitter::emit(
        "miniapp_realtime_status",
        &json!({
            "topic": topic, "peer_count": peers.len(), "peers": peers,
            "is_active": is_active, "has_pending_peers": !peers.is_empty(),
        }),
    );
}

/// End every realtime session as the account that joined them, while it can
/// still announce the departures: before a reload swaps the account.
pub(crate) async fn end_sessions() {
    vector_core::xdc::session::leave_all(std::time::Duration::from_secs(4)).await;
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
    let topic = pkg.uses_realtime.then(|| {
        let bytes = a
            .opt_str("topicId")
            .and_then(|t| wire::decode_topic(&t).ok())
            .unwrap_or_else(|| wire::fallback_topic(&pkg.manifest.name, &chat_id, &message_id));
        wire::encode_topic(&bytes)
    });
    // The page prepares an open window again only to focus it; its session stays.
    let id = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
    let fresh = with(&OPEN, |m| match m.entry(label.clone()) {
        std::collections::hash_map::Entry::Occupied(_) => false,
        std::collections::hash_map::Entry::Vacant(v) => {
            v.insert(Instance { id, chat_id: chat_id.clone(), topic: topic.clone() });
            true
        }
    });
    if let (true, Some(topic)) = (fresh, topic.clone()) {
        preconnect(label.clone(), id, chat_id.clone(), topic, me.clone());
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
    let isolated = pkg.manifest.cross_origin_isolated;
    let policy = if isolated {
        // Chromium only isolates a cross-origin frame the embedder delegates it to.
        allow.push("cross-origin-isolated".to_string());
        vector_core::webxdc_permissions::build_isolated_permissions_policy(&granted)
    } else {
        vector_core::webxdc_permissions::build_permissions_policy(&granted)
    };
    Ok(json!({
        "label": label, "partition": partition, "name": pkg.manifest.name,
        "icon_data": icon_data_url(&pkg.icon), "file_hash": pkg.file_hash,
        "self_addr": me, "self_name": self_name(&me),
        "policy": policy, "allow": allow.join("; "),
        "realtime": topic.is_some(), "isolated": isolated,
    }))
}

/// Join the window's session before the app asks (events buffer until it
/// does), and put us in the topic's lobby.
fn preconnect(label: String, id: u64, chat_id: String, topic: String, me: String) {
    db::spawn_bound(async move {
        let w = realtime::Window { label: &label, id, chat_id: &chat_id, topic: &topic, advertise: !is_solo(&chat_id) };
        if let Err(e) = realtime::open(w, false).await {
            return vector_core::log_warn!("[WEBXDC] realtime join failed: {e}");
        }
        if !is_solo(&chat_id) {
            for ad in db::miniapps::get_active_peer_advertisements_in(&topic, &chat_id, &me, 32).unwrap_or_default() {
                add_session_peer(&topic, ad.npub);
            }
        }
        add_session_peer(&topic, me);
        emit_status(&topic, true);
    });
}

fn instance(label: &str) -> Option<(u64, String, String)> {
    with(&OPEN, |m| m.get(label).and_then(|i| Some((i.id, i.chat_id.clone(), i.topic.clone()?))))
}

async fn rt_join(label: String) -> Result<Value, String> {
    let (id, chat_id, topic) = instance(&label).ok_or("This mini app has no realtime channel")?;
    // Normally joined already by the open; this attaches the app and flushes what buffered.
    let w = realtime::Window { label: &label, id, chat_id: &chat_id, topic: &topic, advertise: !is_solo(&chat_id) };
    realtime::open(w, true).await?;
    Ok(json!({ "topic": topic }))
}

pub async fn rt_send(label: &str, bytes: Vec<u8>) -> Result<Value, String> {
    if bytes.len() > 128_000 {
        return Err("Realtime message too large".into());
    }
    realtime::send(label, bytes).await?;
    Ok(Value::Null)
}

/// The window is gone: end its session (announcing the departure) and leave the lobby.
async fn closed(label: String) -> Result<Value, String> {
    let Some(inst) = with(&OPEN, |m| m.remove(&label)) else { return Ok(Value::Null) };
    let Some(topic) = inst.topic else { return Ok(Value::Null) };
    realtime::close(&label, inst.id).await;
    if topic_is_open(&topic) {
        return Ok(Value::Null);
    }
    if let Some(me) = my_npub() {
        remove_session_peer(&topic, &me);
    }
    emit_status(&topic, false);
    Ok(Value::Null)
}

async fn realtime_status(topic_encoded: &str) -> Result<Value, String> {
    let topic = wire::encode_topic(&wire::decode_topic(topic_encoded)?);
    let peers = session_peers(&topic);
    Ok(json!({
        "active": realtime::is_active(&topic), "peer_count": peers.len(), "pending_peer_count": 0,
        "topic_id": topic_encoded, "peers": peers,
    }))
}

// ─── Peer signals over Nostr ────────────────────────────────────────────────

/// A peer's signal, from a DM or a community channel: vector-core persists it
/// and dials the peer into a session on its topic; the lobby here shows who is playing.
pub fn on_signal(contact: String, npub: String, topic_id: String, node_addr: Option<String>, event_id: String, created_at: u64) {
    db::spawn_bound(async move {
        let Some(sig) = vector_core::xdc::on_signal(&contact, &npub, &topic_id, node_addr.as_deref(), &event_id, created_at).await else {
            return;
        };
        // History stays persisted; the lobby follows the present.
        if !sig.current {
            return;
        }
        if sig.node_addr.is_some() {
            add_session_peer(&sig.topic, sig.npub);
        } else {
            remove_session_peer(&sig.topic, &sig.npub);
        }
        emit_status(&sig.topic, realtime::is_active(&sig.topic));
    });
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
            "miniapp_rt_leave" => a.str("label").map(|l| {
                realtime::detach(&l);
                Value::Null
            }),
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
