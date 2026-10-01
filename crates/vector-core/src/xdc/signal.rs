//! Peer signals over Nostr: how players find each other's nodes. An
//! advertisement says "my node is on this topic, dial it"; a departure says
//! "I left". DMs carry them as gift-wrapped rumors, Community channels as
//! sealed kind-3310 events, exactly as the app sends them.

use nostr_sdk::prelude::*;

use crate::event_ext::FinalizeUnsignedWithId;
use crate::stored_event::{event_kind, StoredEvent};

/// A peer signal as it arrived, already persisted.
#[derive(Debug, Clone)]
pub struct Signal {
    /// The DM npub or Community channel id it arrived in.
    pub chat_id: String,
    pub npub: String,
    pub topic: String,
    /// `Some` for an advertisement, `None` for a departure.
    pub node_addr: Option<iroh::EndpointAddr>,
    pub created_at: u64,
    /// False when a newer signal from the same peer already superseded this one
    /// (a replayed or out-of-order event): history, not the present.
    pub current: bool,
}

/// Advertise our node on `topic` to `chat_id`, or announce we left (`node_addr: None`).
pub async fn send(chat_id: &str, topic: &str, node_addr: Option<&str>) -> Result<(), String> {
    let session = crate::db::current_session();
    let client = crate::state::nostr_client().ok_or("Not logged in")?;
    let me = crate::state::my_public_key().ok_or("Not logged in")?;
    match PublicKey::from_bech32(chat_id) {
        Ok(pk) => {
            let content = if node_addr.is_some() { "peer-advertisement" } else { "peer-left" };
            let mut b = EventBuilder::new(Kind::ApplicationSpecificData, content)
                .tag(Tag::public_key(pk))
                .tag(Tag::custom("d", vec!["vector-webxdc-peer"]))
                .tag(Tag::custom("webxdc-topic", vec![topic.to_string()]));
            if let Some(addr) = node_addr {
                b = b.tag(Tag::custom("webxdc-node-addr", vec![addr.to_string()]));
            }
            let rumor = b.finalize_unsigned_with_id(me);
            let relays = crate::state::active_trusted_relays().await;
            // The signer is process-wide: after a swap it would seal this as another account.
            if !session.is_live() {
                return Err("account changed before the signal was sent".into());
            }
            crate::send_gift_wrap(&client, relays, &pk, rumor, []).await.map(|_| ()).map_err(|e| e.to_string())
        }
        Err(_) => {
            if !session.is_live() {
                return Err("account changed before the signal was sent".into());
            }
            send_to_channel(chat_id, topic, node_addr).await
        }
    }
}

async fn send_to_channel(channel_id: &str, topic: &str, node_addr: Option<&str>) -> Result<(), String> {
    use crate::community::{v2, ChannelId, CommunityId, ConcordProtocol};
    use crate::simd::hex::hex_to_bytes_32;
    let cid = crate::db::community::community_id_for_channel(channel_id)?.ok_or("Not a Community channel")?;
    let community_id = CommunityId(hex_to_bytes_32(&cid));
    let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
    if crate::db::community::community_protocol(&community_id)? == Some(ConcordProtocol::V2) {
        let community = crate::db::community::load_community_v2(&community_id)?.ok_or("Community not found")?;
        let channel = ChannelId(hex_to_bytes_32(channel_id));
        return v2::service::send_webxdc_signal(&transport, &community, &channel, topic, node_addr).await;
    }
    if cfg!(target_arch = "wasm32") {
        return Err("Legacy communities are not available on Vector Web".into());
    }
    let community = crate::db::community::load_community(&community_id)?.ok_or("Community not found")?;
    let channel = community
        .channels
        .iter()
        .find(|c| c.id.to_hex() == channel_id)
        .cloned()
        .ok_or("Channel not found in Community")?;
    crate::community::service::publish_webxdc_signal(&transport, &community, &channel, topic, node_addr).await
}

/// Take in a peer signal from a DM (`chat_id` = the contact's npub) or a
/// Community channel: validate, persist it for later joins, and say whether
/// it is still that peer's latest word on the topic. `None` drops garbage.
pub async fn ingest(
    chat_id: &str,
    npub: &str,
    topic: &str,
    node_addr: Option<&str>,
    event_id: &str,
    created_at: u64,
) -> Option<Signal> {
    // One spelling, so every lookup and lobby keyed on it agrees.
    let topic = &super::wire::encode_topic(&super::wire::decode_topic(topic).ok()?);
    let addr = match node_addr.map(super::wire::decode_node_addr) {
        Some(Ok(a)) => Some(a),
        Some(Err(_)) => return None,
        None => None,
    };
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Sender-claimed: a forged far-future signal must not outrank every later genuine one.
    let created_at = created_at.min(now + 300);
    persist(chat_id, npub, topic, node_addr, event_id, created_at).await;
    let current = crate::db::miniapps::peer_signal_is_current(topic, npub, created_at, addr.is_some()).unwrap_or(false);
    Some(Signal { chat_id: chat_id.to_string(), npub: npub.to_string(), topic: topic.to_string(), node_addr: addr, created_at, current })
}

async fn persist(chat_id: &str, npub: &str, topic: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
    if crate::db::events::event_exists(event_id).unwrap_or(true) {
        return;
    }
    let Ok(chat) = crate::db::id_cache::get_or_create_chat_id(chat_id) else { return };
    let mut tags = vec![
        vec!["webxdc-topic".to_string(), topic.to_string()],
        vec!["d".to_string(), "vector-webxdc-peer".to_string()],
    ];
    if let Some(addr) = node_addr {
        tags.push(vec!["webxdc-node-addr".to_string(), addr.to_string()]);
    }
    let event = StoredEvent {
        id: event_id.to_string(),
        kind: event_kind::APPLICATION_SPECIFIC,
        chat_id: chat,
        user_id: None,
        content: if node_addr.is_some() { "peer-advertisement" } else { "peer-left" }.to_string(),
        tags,
        reference_id: Some(topic.to_string()),
        created_at,
        received_at: web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        mine: false,
        pending: false,
        failed: false,
        wrapper_event_id: None,
        npub: Some(npub.to_string()),
        preview_metadata: None,
    };
    if let Err(e) = crate::db::events::save_event(&event).await {
        crate::log_warn!("[xdc] could not persist peer signal: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let account = Keys::generate().public_key().to_bech32().unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    fn addr_with_direct_ip() -> String {
        let id = iroh::SecretKey::from([9u8; 32]).public();
        let relay: iroh::RelayUrl = "https://relay.example./".parse().unwrap();
        let addr = iroh::EndpointAddr::new(id)
            .with_relay_url(relay)
            .with_ip_addr("203.0.113.7:4433".parse().unwrap());
        super::super::wire::encode_node_addr(&addr).unwrap()
    }

    #[tokio::test]
    async fn a_signal_persists_and_only_the_latest_word_is_current() {
        let (_tmp, _guard) = init_test_db();
        let topic = crate::webxdc::mint_topic_id("h", "s");
        let npub = Keys::generate().public_key().to_bech32().unwrap();
        let addr = addr_with_direct_ip();
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap().as_secs();

        let ad = ingest("npub1chat", &npub, &topic, Some(&addr), "ev_ad", now - 20).await.expect("valid ad");
        assert!(ad.current);
        let node = ad.node_addr.expect("an advertisement carries a node");
        assert!(node.addrs.iter().all(|a| matches!(a, iroh::TransportAddr::Relay(_))), "a peer-nominated direct IP is never kept");

        let left = ingest("npub1chat", &npub, &topic, None, "ev_left", now - 10).await.expect("valid departure");
        assert!(left.current && left.node_addr.is_none());

        // An older advertisement replayed by a sync is history, not presence.
        let stale = ingest("npub1chat", &npub, &topic, Some(&addr), "ev_old_ad", now - 30).await.unwrap();
        assert!(!stale.current);

        // Only the departure-free latest ads are offered to a joiner.
        assert!(crate::db::miniapps::get_active_peer_advertisements(&topic, "npub1me").unwrap().is_empty());
        ingest("npub1chat", &npub, &topic, Some(&addr), "ev_back", now).await.unwrap();
        assert_eq!(crate::db::miniapps::get_active_peer_advertisements(&topic, "npub1me").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_joiner_bootstraps_only_from_its_own_chat() {
        let (_tmp, _guard) = init_test_db();
        let topic = crate::webxdc::mint_topic_id("h", "s");
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap().as_secs();
        let (player, stranger) = (Keys::generate().public_key().to_bech32().unwrap(), Keys::generate().public_key().to_bech32().unwrap());
        ingest("npub1channel", &player, &topic, Some(&addr_with_direct_ip()), "ev_in", now).await.unwrap();
        // The same topic advertised to us through a stranger's DM.
        ingest(&stranger, &stranger, &topic, Some(&addr_with_direct_ip()), "ev_out", now).await.unwrap();

        let here = crate::db::miniapps::get_active_peer_advertisements_in(&topic, "npub1channel", "npub1me", 32).unwrap();
        assert_eq!(here.iter().map(|r| r.npub.as_str()).collect::<Vec<_>>(), vec![player.as_str()]);
        assert!(crate::db::miniapps::get_active_peer_advertisements_in(&topic, "npub1unknown", "npub1me", 32).unwrap().is_empty());

        // The same player's later word through another chat neither hides them
        // here nor adds that chat's node.
        let other_node = {
            let id = iroh::SecretKey::from([8u8; 32]).public();
            let relay: iroh::RelayUrl = "https://relay.example./".parse().unwrap();
            super::super::wire::encode_node_addr(&iroh::EndpointAddr::new(id).with_relay_url(relay)).unwrap()
        };
        ingest(&player, &player, &topic, Some(&other_node), "ev_dm_later", now + 5).await.unwrap();
        ingest(&player, &player, &topic, Some(&other_node), "ev_dm_same", now).await.unwrap();
        let here = crate::db::miniapps::get_active_peer_advertisements_in(&topic, "npub1channel", "npub1me", 32).unwrap();
        assert_eq!(here.len(), 1, "one row, from this chat");
        assert_eq!(here[0].node_addr_encoded, addr_with_direct_ip());
    }

    #[tokio::test]
    async fn garbage_signals_are_dropped_before_they_persist() {
        let (_tmp, _guard) = init_test_db();
        let topic = crate::webxdc::mint_topic_id("h", "s");
        assert!(ingest("npub1chat", "npub1x", "not-a-topic", None, "ev1", 1).await.is_none());
        assert!(ingest("npub1chat", "npub1x", &topic, Some("!!"), "ev2", 1).await.is_none());
        assert!(!crate::db::events::event_exists("ev1").unwrap());
        assert!(!crate::db::events::event_exists("ev2").unwrap());
    }

    #[tokio::test]
    async fn a_topic_is_kept_in_its_one_spelling() {
        let (_tmp, _guard) = init_test_db();
        let topic = crate::webxdc::mint_topic_id("h", "s");
        let sig = ingest("npub1chat", "npub1x", &topic.to_lowercase(), Some(&addr_with_direct_ip()), "ev_case", 1).await.unwrap();
        assert_eq!(sig.topic, topic);
        let here = crate::db::miniapps::get_active_peer_advertisements_in(&topic, "npub1chat", "npub1me", 32).unwrap();
        assert_eq!(here.len(), 1);
    }

    #[tokio::test]
    async fn a_far_future_timestamp_is_clamped() {
        let (_tmp, _guard) = init_test_db();
        let topic = crate::webxdc::mint_topic_id("h", "s");
        let sig = ingest("npub1chat", "npub1x", &topic, None, "ev_future", u64::MAX / 2).await.unwrap();
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap().as_secs();
        assert!(sig.created_at <= now + 300);
    }
}
