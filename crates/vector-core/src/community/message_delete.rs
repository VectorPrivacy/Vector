//! Deleting a Community message the local user sent: a NIP-09 deletion signed by the
//! ephemeral key that signed the outer event (only that key may delete it), retained
//! when the message was published.

use nostr_sdk::prelude::*;

use super::transport::Transport;

/// Delete an event the sender previously published, via its retained ephemeral key.
pub async fn delete_own_message<T: Transport + ?Sized>(
    transport: &T,
    relays: &[String],
    ephemeral: &Keys,
    outer_event_id: EventId,
) -> Result<(), String> {
    let deletion = EventDeletionRequest::new()
        .ids([outer_event_id])
        .finalize(ephemeral)
        .map_err(|e| e.to_string())?;
    transport.publish_durable(&deletion, relays).await
}

/// Delete a message the local user previously sent, by its INNER message id (what the UI
/// holds). Loads the retained ephemeral key + the outer event id it points at, then
/// NIP-09-deletes that outer event. Errors if no key is retained (not ours, or already
/// deleted).
pub async fn delete_message<T: Transport + ?Sized>(transport: &T, message_id: &str) -> Result<(), String> {
    crate::db::scoped(async move {
        let session = crate::db::current_session();
        if !session.is_live() {
            return Err("account changed; aborting delete".to_string());
        }
        // PEEK the key (don't consume it yet): the NIP-09 publish below is fallible, and the
        // key is single-use — consuming it before a failed publish would leave the message
        // permanently undeletable. Remove it only after the deletion actually goes out.
        let (ephemeral, outer_event_id_hex, relays) = match crate::db::community::get_message_key(message_id)? {
            Some(v) => v,
            None => return Err("no retained key for this message (not yours, or already deleted)".to_string()),
        };
        let id = EventId::from_hex(&outer_event_id_hex).map_err(|e| e.to_string())?;
        delete_own_message(transport, &relays, &ephemeral, id).await?;
        crate::db::community::delete_message_key(message_id)?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::transport::memory::MemoryRelay;
    use crate::community::transport::{PlaneFetch, Query};

    /// A transport whose publish always fails (fetch returns nothing).
    struct FailingRelay;
    #[async_trait::async_trait]
    impl Transport for FailingRelay {
        async fn fetch_plane(&self, _plane: &Keys, query: &Query, relays: &[String]) -> Result<PlaneFetch, String> {
            Ok(PlaneFetch::whole(self.fetch(query, relays).await?))
        }
        async fn publish(&self, _event: &Event, _relays: &[String]) -> Result<(), String> {
            Err("relay unreachable".to_string())
        }
        async fn publish_durable(&self, _event: &Event, _relays: &[String]) -> Result<(), String> {
            Err("relay unreachable".to_string())
        }
        async fn fetch(&self, _query: &Query, _relays: &[String]) -> Result<Vec<Event>, String> {
            Ok(Vec::new())
        }
    }

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(7700);

    fn make_test_npub(n: u32) -> String {
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut payload = vec![b'q'; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        format!("npub1{}", std::str::from_utf8(&payload).unwrap())
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let account = make_test_npub(TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    fn relays() -> Vec<String> {
        vec!["r1".into(), "r2".into(), "r3".into()]
    }

    /// Publish a plain event signed by a fresh ephemeral key, as a sent message's outer event.
    async fn published(relay: &MemoryRelay) -> (Event, Keys) {
        let ephemeral = Keys::generate();
        let outer = EventBuilder::new(Kind::Custom(1059), "sealed").finalize(&ephemeral).unwrap();
        relay.publish_durable(&outer, &relays()).await.unwrap();
        (outer, ephemeral)
    }

    async fn still_there(relay: &MemoryRelay, id: EventId) -> bool {
        let q = Query { ids: vec![id.to_hex()], ..Default::default() };
        !relay.fetch(&q, &relays()).await.unwrap().is_empty()
    }

    #[tokio::test]
    async fn only_the_signing_key_deletes_and_a_deletion_must_reach_every_relay() {
        let relay = MemoryRelay::new();
        let (outer, ephemeral) = published(&relay).await;

        delete_own_message(&relay, &relays(), &Keys::generate(), outer.id).await.unwrap();
        assert!(still_there(&relay, outer.id).await, "a foreign key must not delete it");

        delete_own_message(&relay, &["r1".to_string()], &ephemeral, outer.id).await.unwrap();
        assert!(still_there(&relay, outer.id).await, "a deletion on a subset leaves it on the rest");

        delete_own_message(&relay, &relays(), &ephemeral, outer.id).await.unwrap();
        assert!(!still_there(&relay, outer.id).await);
    }

    #[tokio::test]
    async fn a_retained_key_deletes_once_and_survives_a_failed_publish() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let (outer, ephemeral) = published(&relay).await;
        let message_id = "00".repeat(32);
        crate::db::community::store_message_key(&message_id, &outer.id.to_hex(), &ephemeral, &relays()).unwrap();

        assert!(delete_message(&FailingRelay, &message_id).await.is_err());
        assert!(still_there(&relay, outer.id).await);

        delete_message(&relay, &message_id).await.unwrap();
        assert!(!still_there(&relay, outer.id).await);
        assert!(delete_message(&relay, &message_id).await.is_err(), "the key is single-use");
    }

    #[tokio::test]
    async fn deleting_a_message_we_never_sent_errors() {
        let (_tmp, _guard) = init_test_db();
        assert!(delete_message(&MemoryRelay::new(), &"00".repeat(32)).await.is_err());
    }
}
