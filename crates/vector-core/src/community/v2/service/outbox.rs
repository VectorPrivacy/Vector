//! Messages no relay has confirmed. Each is kept as signed until a relay takes it or is
//! seen holding it, so it only ever goes out as itself: a background resend, a Retry and a
//! late landing all carry the same wrap, and nothing posts twice. A send is reported failed
//! only after the relays were asked and none held it, and one that lands later shows as sent.

use super::*;
use crate::community::transport::{Evidence, LiveTransport};
use crate::state::ChatState;
use std::time::Duration;

/// Waits between background resends of an unconfirmed message.
const RESEND_BACKOFF_SECS: [u64; 6] = [10, 30, 90, 300, 900, 1800];

fn key(message_id: &str) -> String {
    format!("unconfirmed_wrap:{message_id}")
}

/// Keep a message's wrap until a relay confirms it.
pub(super) fn keep(message_id: &str, wrap: &Event) {
    let _ = crate::db::set_sql_setting(key(message_id), wrap.as_json());
}

pub(super) fn release(message_id: &str) {
    let _ = crate::db::remove_setting(&key(message_id));
}

pub(super) fn kept(message_id: &str) -> Option<Event> {
    crate::db::get_sql_setting(key(message_id)).ok().flatten().and_then(|json| crate::event_json::event(&json).ok())
}

/// Whether any relay holds `wrap`, asked as its plane (gating relays serve a plane only to it).
pub(super) async fn on_relays<T: Transport + ?Sized>(transport: &T, plane: &Keys, wrap: &Event, relays: &[String]) -> bool {
    let query = Query {
        kinds: vec![wrap.kind.as_u16()],
        authors: vec![wrap.pubkey.to_hex()],
        ids: vec![wrap.id.to_hex()],
        evidence: Evidence::Fast,
        ..Default::default()
    };
    transport.fetch_plane(plane, &query, relays).await.is_ok_and(|f| f.events.iter().any(|e| e.id == wrap.id))
}

/// How soon a send must be refused to count as never having reached a socket.
const REFUSED_AT_ONCE: Duration = Duration::from_secs(3);
/// How long the pool gets to reconnect before such a send goes once more.
const RECONNECT_GRACE: Duration = Duration::from_secs(4);

/// A send refused at once found no socket to go out on (a phone back from the file picker,
/// a laptop just woken): the pool is reconnecting, so it goes once more before it is called
/// failed. One that waited its relays out is not held up a second time.
pub(super) async fn again_once_reconnected<T: Transport + ?Sized>(
    transport: &T,
    plane: &Keys,
    wrap: &Event,
    relays: &[String],
    began: web_time::Instant,
) -> bool {
    if began.elapsed() > REFUSED_AT_ONCE {
        return false;
    }
    crate::rt::time::sleep(RECONNECT_GRACE).await;
    transport.publish(wrap, relays).await.is_ok() || on_relays(transport, plane, wrap, relays).await
}

/// Resend a kept message in the background, backing off, until it lands or is given up on.
pub(super) fn resend_later(message_id: String) {
    if crate::state::nostr_client().is_none() {
        return;
    }
    crate::db::spawn_bound(async move {
        for wait in RESEND_BACKOFF_SECS {
            crate::rt::time::sleep(Duration::from_secs(wait)).await;
            if crate::db::session_stopped() || !matches!(resend(&message_id).await, Resend::Pending) {
                return;
            }
        }
    });
}

pub enum Resend {
    /// Nothing kept: it landed already, or was given up on.
    Nothing,
    Landed,
    Pending,
}

/// One resend of a kept message, read back when no relay answers. Landing shows it as sent.
pub async fn resend(message_id: &str) -> Resend {
    let Some(wrap) = kept(message_id) else { return Resend::Nothing };
    let Ok(Some((plane, _, relays))) = crate::db::community::get_message_key(message_id) else { return Resend::Nothing };
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    if transport.publish(&wrap, &relays).await.is_err() && !on_relays(&transport, &plane, &wrap, &relays).await {
        return Resend::Pending;
    }
    let mut state = crate::state::STATE.lock().await;
    if !confirm_in(&mut state, message_id) {
        release(message_id);
    }
    Resend::Landed
}

/// Clear an own message's sending or failed mark: it's on a relay. The row is saved and
/// repainted behind the lock. Returns whether there was a mark to clear.
pub fn confirm_in(state: &mut ChatState, message_id: &str) -> bool {
    if !state.find_message(message_id).is_some_and(|(_, m)| m.mine && (m.failed || m.pending)) {
        return false;
    }
    release(message_id);
    if let Some((chat_id, msg)) = state.update_message(message_id, |m| {
        m.set_failed(false);
        m.set_pending(false);
    }) {
        crate::db::spawn_bound(async move {
            let _ = crate::db::events::save_message(&chat_id, &msg).await;
            crate::traits::emit_event(
                "message_update",
                &serde_json::json!({ "old_id": msg.id, "message": msg, "chat_id": chat_id }),
            );
        });
    }
    true
}

/// Mark the row shown for a send failed, under the id its message was signed with. The row
/// on screen is then that message: a retry resends it as itself, a delete takes it back, and
/// its landing later shows it as sent. Returns the row as it now stands.
pub async fn mark_unsent(channel_hex: &str, shown_id: &str, unsent: &Unsent) -> Option<crate::types::Message> {
    let mut state = crate::state::STATE.lock().await;
    let id = match unsent.kept.as_deref() {
        Some(signed) if signed != shown_id => {
            state.finalize_pending_message(channel_hex, shown_id, signed);
            signed
        }
        _ => shown_id,
    };
    state
        .update_message(id, |m| {
            m.set_failed(true);
            m.set_pending(false);
        })
        .map(|(_, msg)| msg)
}

async fn set_mark(message_id: &str, failed: bool) {
    let row = crate::state::STATE.lock().await.update_message(message_id, |m| {
        m.set_failed(failed);
        m.set_pending(!failed);
    });
    if let Some((chat_id, msg)) = row {
        crate::traits::emit_event(
            "message_update",
            &serde_json::json!({ "old_id": msg.id, "message": msg, "chat_id": chat_id }),
        );
    }
}

/// Retry a failed message as itself. `false` when nothing was kept, and a fresh send is
/// then the only way: no wrap of it went out under this send.
pub async fn retry_unconfirmed(message_id: &str) -> bool {
    if kept(message_id).is_none() {
        return false;
    }
    set_mark(message_id, false).await;
    if let Resend::Pending = resend(message_id).await {
        set_mark(message_id, true).await;
        resend_later(message_id.to_string());
    }
    true
}

/// Give up on a failed message: stop resending it, and take back any copy that landed.
/// Returns whether nothing of it can still be read: no message was ever signed under this
/// id, or its delete is on a relay. A failed mark is no proof it never landed, so a signed
/// message is taken back whether or not it is still kept.
pub async fn discard_unconfirmed(channel_hex: &str, message_id: &str) -> bool {
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    discard_unconfirmed_over(&transport, channel_hex, message_id).await
}

pub(super) async fn discard_unconfirmed_over<T: Transport + ?Sized>(transport: &T, channel_hex: &str, message_id: &str) -> bool {
    if crate::simd::hex::hex_to_bytes_32_checked(message_id).is_none() {
        return true;
    }
    release(message_id);
    let Ok(Some(community_hex)) = crate::db::community::community_id_for_channel(channel_hex) else { return false };
    let Some(cid) = crate::simd::hex::hex_to_bytes_32_checked(&community_hex).map(crate::community::CommunityId) else { return false };
    let Ok(Some(community)) = crate::db::community::load_community_v2(&cid) else { return false };
    let Some(ch) = crate::simd::hex::hex_to_bytes_32_checked(channel_hex).map(ChannelId) else { return false };
    match delete_own(transport, &community, &ch, message_id, crate::community::v2::kind::MESSAGE).await {
        Ok(_) => true,
        Err(e) => {
            crate::log_net_fail!("[Send] couldn't take back an unconfirmed message: {e}");
            false
        }
    }
}
