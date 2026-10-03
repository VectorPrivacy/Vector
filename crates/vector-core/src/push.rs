//! Sealed push, Vector's half of it: the tickets this device hands its contacts, the tickets
//! contacts hand us, and the notification a delivered DM sends to the recipient's devices.
//! The protocol is the `vector-push` crate.

use std::collections::HashMap;
use std::sync::Arc;

use nostr_sdk::prelude::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
pub use vector_push::ticket::TicketList;
pub use vector_push::{Notice, Ticket};

use crate::db::settings::{get_sql_setting, remove_setting, set_sql_setting};
use crate::event_ext::FinalizeUnsignedWithId;
use crate::tags::TagsExt;
use crate::ClientRelayExt;
use crate::state::{my_public_key, nostr_client, STATE};

/// This device's push subscription, as the browser (or a distributor) made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// Names the subscription in every ticket: a new one replaces the old at each contact.
    pub id: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    /// The VAPID private key the subscription was made with.
    pub vapid: String,
    pub origin: String,
}

/// What this device minted for one contact.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Issued {
    cap: String,
    h: String,
    mk: String,
    /// The device id this contact last received a ticket for.
    #[serde(default)]
    sent: String,
    /// The hold last confirmed at the pusher, unix seconds (0 = none).
    #[serde(default)]
    held: u64,
}

const DEVICE_KEY: &str = "push_device";
const ISSUED_KEY: &str = "push_issued";
const HELD_KEY: &str = "push_held";
/// A contact keeps at most this many devices' tickets.
const MAX_TICKETS_PER_CONTACT: usize = 8;
/// Upper bound of the random wait between delivering a DM and asking for its push.
const SEND_JITTER_MS: u64 = 4000;
/// How long a push service holds a notification for a device that is offline.
const TTL: u32 = 2 * 24 * 3600;

/// Serialises the read-modify-write of the three JSON settings.
struct PushLock;
fn lock() -> Arc<tokio::sync::Mutex<()>> {
    crate::db::current_session().scoped::<PushLock, _>()
}

fn load<T: DeserializeOwned + Default>(key: &str) -> T {
    get_sql_setting(key.into()).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save<T: Serialize>(key: &str, value: &T) -> Result<(), String> {
    set_sql_setting(key.into(), serde_json::to_string(value).map_err(|e| e.to_string())?)
}

fn random_hex<const N: usize>() -> String {
    use rand::RngCore;
    let mut b = [0u8; N];
    rand::thread_rng().fill_bytes(&mut b);
    crate::simd::hex::bytes_to_hex_string(&b)
}

// ============================================================================
// This device: receiving notifications
// ============================================================================

pub fn device() -> Option<Device> {
    load::<Option<Device>>(DEVICE_KEY)
}

/// Adopt this device's subscription. Contacts holding a ticket for an older one get the new
/// one the next time we write to them.
pub async fn set_device(device: Device) -> Result<(), String> {
    let _g = lock().lock_owned().await;
    save(DEVICE_KEY, &Some(device))
}

/// Notifications off: tell every contact holding our ticket to forget it.
pub async fn disable() -> Result<(), String> {
    let (old, issued) = {
        let _g = lock().lock_owned().await;
        let old = device();
        let issued: HashMap<String, Issued> = load(ISSUED_KEY);
        remove_setting(DEVICE_KEY)?;
        remove_setting(ISSUED_KEY)?;
        (old, issued)
    };
    let Some(old) = old else { return Ok(()) };
    let holders: Vec<String> = issued.into_iter().filter(|(_, i)| i.sent.starts_with(&old.id)).map(|(npub, _)| npub).collect();
    crate::db::spawn_bound(async move {
        let list = TicketList { v: 1, tickets: vec![], revoked: vec![old.id] };
        for npub in holders {
            if let Err(e) = send_list(&npub, &list).await {
                crate::log_warn!("[Push] revoking at {}: {}", npub, e);
            }
        }
    });
    Ok(())
}

/// Hand this device's ticket to `npub` unless they already hold the current one.
/// Returns whether a ticket was sent.
pub async fn share_with(npub: &str) -> Result<bool, String> {
    let Some(device) = device() else { return Ok(false) };
    if my_public_key().and_then(|pk| pk.to_bech32().ok()).as_deref() == Some(npub) || is_blocked(npub).await {
        return Ok(false);
    }
    let ticket = {
        let _g = lock().lock_owned().await;
        let mut issued: HashMap<String, Issued> = load(ISSUED_KEY);
        let version = ticket_version(&device);
        if issued.get(npub).is_some_and(|i| i.sent == version) {
            return Ok(false);
        }
        let fresh = !issued.contains_key(npub);
        let entry = issued.entry(npub.to_string()).or_insert_with(new_issued);
        let ticket = mint(&device, entry)?;
        save(ISSUED_KEY, &issued)?;
        if fresh {
            crate::emit_event("push_contacts_changed", &serde_json::json!({}));
            // Someone muted before they ever held a ticket starts out held.
            controls_changed();
        }
        ticket
    };
    send_list(npub, &TicketList { v: 1, tickets: vec![ticket], revoked: vec![] }).await?;
    let _g = lock().lock_owned().await;
    let mut issued: HashMap<String, Issued> = load(ISSUED_KEY);
    if let Some(i) = issued.get_mut(npub) {
        i.sent = ticket_version(&device);
    }
    save(ISSUED_KEY, &issued)?;
    Ok(true)
}

fn new_issued() -> Issued {
    Issued { cap: random_hex::<16>(), h: random_hex::<8>(), mk: random_hex::<32>(), sent: String::new(), held: 0 }
}

/// Make the handles and keys for these contacts now, so the device's service worker knows
/// them before their tickets have even gone out.
pub async fn prepare(npubs: &[String]) -> Result<(), String> {
    let _g = lock().lock_owned().await;
    let mut issued: HashMap<String, Issued> = load(ISSUED_KEY);
    for npub in npubs {
        issued.entry(npub.clone()).or_insert_with(new_issued);
    }
    save(ISSUED_KEY, &issued)
}

/// What a contact's ticket must match to be current: this subscription, at this pusher, on
/// these relays. Any change re-issues tickets with the next message to each contact.
fn ticket_version(device: &Device) -> String {
    let (key, relays) = pusher();
    format!("{}|{}|{}", device.id, &key[..16], relays.join(","))
}

/// The pusher tickets name. A build can point at its own (`VECTOR_PUSHER`, a hex pubkey,
/// and `VECTOR_PUSHER_RELAYS`, comma separated).
fn pusher() -> (&'static str, Vec<String>) {
    let key = option_env!("VECTOR_PUSHER").unwrap_or(vector_push::DEFAULT_PUSHER);
    let relays = match option_env!("VECTOR_PUSHER_RELAYS") {
        Some(r) => r.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        None => vector_push::DEFAULT_PUSHER_RELAYS.iter().map(|r| r.to_string()).collect(),
    };
    (key, relays)
}

fn mint(device: &Device, issued: &Issued) -> Result<Ticket, String> {
    let (key, relays) = pusher();
    let pusher = PublicKey::from_hex(key).map_err(|e| e.to_string())?;
    let cap = vector_push::Capability { endpoint: device.endpoint.clone(), vapid: device.vapid.clone(), cap: issued.cap.clone() };
    Ok(Ticket {
        id: device.id.clone(),
        pusher: key.to_string(),
        relays,
        sealed: vector_push::ticket::seal(&pusher, &cap).map_err(|e| e.to_string())?,
        p256dh: device.p256dh.clone(),
        auth: device.auth.clone(),
        origin: device.origin.clone(),
        h: issued.h.clone(),
        mk: issued.mk.clone(),
    })
}

async fn send_list(npub: &str, list: &TicketList) -> Result<(), String> {
    let client = nostr_client().ok_or("Not connected")?;
    let me = my_public_key().ok_or("Not logged in")?;
    let to = PublicKey::from_bech32(npub).map_err(|e| e.to_string())?;
    let rumor = EventBuilder::new(Kind::ApplicationSpecificData, serde_json::to_string(list).map_err(|e| e.to_string())?)
        .tag(Tag::public_key(to))
        .tag(Tag::identifier(vector_push::TICKET_RUMOR_D))
        .finalize_unsigned_with_id(me);
    let out = crate::inbox_relays::send_gift_wrap(&client, &to, rumor, []).await?;
    if out.success.is_empty() {
        return Err("no relay took the ticket".into());
    }
    Ok(())
}

/// Every contact holding a ticket, keyed by the handle a notice names them by: what the
/// device's service worker needs to name and verify a sender.
pub async fn worker_contacts() -> serde_json::Value {
    let issued: HashMap<String, Issued> = {
        let _g = lock().lock_owned().await;
        load(ISSUED_KEY)
    };
    let state = STATE.lock().await;
    let mut out = serde_json::Map::new();
    for (npub, i) in issued {
        let name = state
            .get_profile(&npub)
            .map(|p| {
                if !p.nickname().is_empty() {
                    p.nickname().to_string()
                } else if !p.display_name.is_empty() {
                    p.display_name.to_string()
                } else {
                    p.name.to_string()
                }
            })
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("{}…", &npub[..npub.len().min(12)]));
        out.insert(i.h, serde_json::json!({ "npub": npub, "name": name, "mk": i.mk }));
    }
    serde_json::Value::Object(out)
}

// ============================================================================
// Holds: muted, blocked, or on screen right now
// ============================================================================

/// How long "I'm looking at this chat" holds pushes without being renewed. The page renews it
/// well inside this, and a lost release (the app frozen as it left) runs out on its own.
const VIEWING_LEASE: u64 = 45;

/// The chat on screen on this device, and until when that holds its pushes.
struct Viewing;
fn viewing_slot() -> Arc<std::sync::Mutex<Option<(String, u64)>>> {
    crate::db::current_session().scoped::<Viewing, _>()
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

async fn is_blocked(npub: &str) -> bool {
    STATE.lock().await.get_profile(npub).is_some_and(|p| p.flags.is_blocked())
}

/// The chat on screen, or None once it isn't. Renewing the same chat extends its lease.
pub async fn viewing(npub: Option<String>) -> Result<(), String> {
    *viewing_slot().lock().unwrap() = npub.map(|n| (n, now_secs() + VIEWING_LEASE));
    reconcile_holds().await
}

/// How long `npub`'s pushes should be held: forever if blocked or muted indefinitely, until a
/// timed mute ends, or while their chat is on screen, whichever is longest.
async fn wanted_hold(npub: &str, now: u64) -> u64 {
    if is_blocked(npub).await {
        return vector_push::HOLD_FOREVER;
    }
    let mute = crate::notify::prefs(npub).mute_until;
    let muted = match mute {
        -1 => vector_push::HOLD_FOREVER,
        ms if ms > 0 && (ms / 1000) as u64 > now => (ms / 1000) as u64,
        _ => 0,
    };
    let seen = viewing_slot().lock().unwrap().as_ref().filter(|(n, until)| n == npub && *until > now).map_or(0, |(_, until)| *until);
    muted.max(seen)
}

/// Mutes, blocks or the chat on screen moved: bring the pusher's holds in line.
pub fn controls_changed() {
    if device().is_none() {
        return;
    }
    crate::db::spawn_bound(async {
        if let Err(e) = reconcile_holds().await {
            crate::log_warn!("[Push] holds not updated: {}", e);
        }
    });
}

/// Tell the pusher every hold that differs from what it last confirmed. A hold that ran out on
/// its own already matches "none", so expiry costs nothing.
pub async fn reconcile_holds() -> Result<(), String> {
    if device().is_none() {
        return Ok(());
    }
    let now = now_secs();
    let issued: HashMap<String, Issued> = {
        let _g = lock().lock_owned().await;
        load(ISSUED_KEY)
    };
    let mut changes: Vec<(String, vector_push::Hold)> = Vec::new();
    for (npub, i) in &issued {
        let want = wanted_hold(npub, now).await;
        let have = if i.held != vector_push::HOLD_FOREVER && i.held <= now { 0 } else { i.held };
        if want != have {
            changes.push((npub.clone(), vector_push::Hold { c: i.cap.clone(), until: want }));
        }
    }
    if changes.is_empty() {
        return Ok(());
    }
    let (key, relays) = pusher();
    let pusher_pk = PublicKey::from_hex(key).map_err(|e| e.to_string())?;
    let event = vector_push::Control::new(changes.iter().map(|(_, h)| h.clone()).collect())
        .event(&pusher_pk)
        .map_err(|e| e.to_string())?;
    let client = nostr_client().ok_or("Not connected")?;
    publish(&client, &relays, &event).await?;
    let _g = lock().lock_owned().await;
    let mut issued: HashMap<String, Issued> = load(ISSUED_KEY);
    for (npub, hold) in changes {
        if let Some(i) = issued.get_mut(&npub).filter(|i| i.cap == hold.c) {
            i.held = hold.until;
        }
    }
    save(ISSUED_KEY, &issued)
}

// ============================================================================
// Contacts' devices: sending notifications
// ============================================================================

/// A contact's tickets arrived: keep them, replacing older ones for the same device.
pub(crate) async fn store_tickets(from_npub: &str, content: &str) {
    let Ok(list) = serde_json::from_str::<TicketList>(content) else { return };
    let _g = lock().lock_owned().await;
    let mut held: HashMap<String, Vec<Ticket>> = load(HELD_KEY);
    let entry = held.entry(from_npub.to_string()).or_default();
    entry.retain(|t| !list.revoked.contains(&t.id) && !list.tickets.iter().any(|n| n.id == t.id));
    for t in list.tickets {
        match t.validate() {
            Ok(()) => entry.push(t),
            Err(e) => crate::log_warn!("[Push] ticket from {} refused: {}", from_npub, e),
        }
    }
    if entry.len() > MAX_TICKETS_PER_CONTACT {
        let excess = entry.len() - MAX_TICKETS_PER_CONTACT;
        entry.drain(..excess);
    }
    if entry.is_empty() {
        held.remove(from_npub);
    }
    if let Err(e) = save(HELD_KEY, &held) {
        crate::log_warn!("[Push] saving tickets: {}", e);
    }
}

/// A DM reached `receiver_npub`: wake their devices, and hand them our own ticket if they
/// don't hold the current one.
pub(crate) fn after_delivery(receiver_npub: &str, rumor: &UnsignedEvent) {
    let receiver = receiver_npub.to_string();
    let notice = notice_for(rumor);
    crate::db::spawn_bound(async move {
        if let Some(notice) = notice {
            // A beat apart from the DM landing on the recipient's relays, so the two aren't
            // paired by arrival time alone.
            let jitter = rand::Rng::gen_range(&mut rand::thread_rng(), 0..SEND_JITTER_MS);
            crate::rt::time::sleep(std::time::Duration::from_millis(jitter)).await;
            notify(&receiver, &notice).await;
        }
        if let Err(e) = share_with(&receiver).await {
            crate::log_warn!("[Push] sharing ticket with {}: {}", receiver, e);
        }
    });
}

fn notice_for(rumor: &UnsignedEvent) -> Option<Notice> {
    let (kind, text) = match rumor.kind {
        Kind::PrivateDirectMessage => ("dm", preview_text(&rumor.content)),
        k if k.as_u16() == 15 => {
            let mime = rumor.tags.find_kind("file-type").and_then(|t| t.content()).unwrap_or("");
            ("file", format!("Sent {}", file_label(mime)))
        }
        _ => return None,
    };
    Some(Notice {
        h: String::new(),
        kind: kind.into(),
        msg: rumor.id?.to_hex(),
        ts: rumor.created_at.as_secs() * 1000,
        text,
    })
}

/// Plain text for a preview: markup and line breaks flattened, mentions left for the
/// receiver's own names. The notice is cut to fit later.
fn preview_text(content: &str) -> String {
    let flat: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(600).collect()
}

fn file_label(mime: &str) -> &'static str {
    match mime {
        "image/gif" => "a GIF",
        m if m.starts_with("image/") => "a Picture",
        m if m.starts_with("video/") => "a Video",
        "audio/wav" | "audio/x-wav" => "a Voice Message",
        m if m.starts_with("audio/") => "an Audio Clip",
        _ => "a File",
    }
}

async fn notify(receiver: &str, notice: &Notice) {
    let tickets: Vec<Ticket> = {
        let _g = lock().lock_owned().await;
        let held: HashMap<String, Vec<Ticket>> = load(HELD_KEY);
        held.get(receiver).cloned().unwrap_or_default()
    };
    if tickets.is_empty() {
        return;
    }
    let Some(client) = nostr_client() else { return };
    for t in tickets {
        let event = match t.request(notice, TTL) {
            Ok(e) => e,
            Err(e) => {
                crate::log_warn!("[Push] building request: {}", e);
                continue;
            }
        };
        if let Err(e) = publish(&client, &t.relays, &event).await {
            crate::log_warn!("[Push] request not published: {}", e);
        }
    }
}

/// Publish to the pusher's relays we're already connected to; join them just for this only
/// when we hold none, so a request travels the connections this client already keeps.
async fn publish(client: &Client, relays: &[String], event: &Event) -> Result<(), String> {
    use crate::inbox_relays::normalize_relay_url;
    let pool = client.relays().all().await;
    let wanted: Vec<String> = relays.iter().map(|r| normalize_relay_url(r)).collect();
    let mut targets: Vec<Relay> = pool
        .iter()
        .filter(|(u, r)| wanted.contains(&normalize_relay_url(&u.to_string())) && r.status() == RelayStatus::Connected)
        .map(|(_, r)| r.clone())
        .collect();
    let mut transient: Vec<RelayUrl> = Vec::new();
    if targets.is_empty() {
        for r in relays {
            let norm = normalize_relay_url(r);
            if let Some((_, relay)) = pool.iter().find(|(u, _)| normalize_relay_url(&u.to_string()) == norm) {
                let _ = relay.try_connect().timeout(crate::relay_connect_timeout(std::time::Duration::from_secs(6))).await;
                targets.push(relay.clone());
                continue;
            }
            if client.add_managed_relay(r.as_str()).await.is_ok() {
                if let Ok(Some(relay)) = client.relay(r.as_str()).await {
                    let _ = relay.try_connect().timeout(crate::relay_connect_timeout(std::time::Duration::from_secs(6))).await;
                    transient.push(relay.url().clone());
                    targets.push(relay);
                }
            }
        }
    }
    let results = futures_util::future::join_all(targets.iter().map(|r| async move { r.send_event(event).await })).await;
    for url in transient {
        let _ = client.remove_relay(&url).await;
    }
    if results.iter().any(|r| r.is_ok()) {
        Ok(())
    } else {
        Err(results.into_iter().filter_map(|r| r.err()).map(|e| e.to_string()).next().unwrap_or_else(|| "no relays".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_flatten_and_files_get_a_label() {
        assert_eq!(preview_text("hi\n\nthere   you"), "hi there you");
        assert_eq!(preview_text(&"x".repeat(900)).len(), 600);
        assert_eq!(file_label("image/png"), "a Picture");
        assert_eq!(file_label("image/gif"), "a GIF");
        assert_eq!(file_label("audio/wav"), "a Voice Message");
        assert_eq!(file_label("application/zip"), "a File");
    }
}
