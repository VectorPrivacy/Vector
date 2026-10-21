//! Deleting what the local user sent to a Community, from its relays.
//!
//! Every wrap on a plane is signed by the plane's shared key, so a deletion by that key could
//! come from any member: CORD-01 §Deletions has relays refuse it and honour the wrap's
//! `p`-tagged owner instead. Vector derives that `p` key from the author's key and the message
//! ([`derive::wrap_owner_key`]), so every device of the author can delete any of their
//! messages. A remote-signer account can't derive it, so its sending device keeps a random one.
//! Owners are kept in `community_wrap_owners`, plane keys in `community_message_keys`.
//!
//! A delete sends the owner's deletion and, best-effort, the plane key's: a relay that only
//! honours authors drops the wrap on that one, and a relay following the spec ignores it. A
//! message sent before owners were kept has only its plane key, so those relays keep its wrap
//! and the in-plane delete alone hides it.

use nostr_sdk::prelude::*;

use super::transport::Transport;
use super::v2::{chat::ChatEvent, derive, stream};

/// How long the in-plane delete of a message its owner deleted from relays stays on them.
const OWNED_DELETE_EXPIRY_SECS: u64 = crate::deletion::COOPERATIVE_HIDE_EXPIRY_SECS;

/// The `p` key for my wrap of `rumor_id` at `address`: derived when this device holds the
/// identity's key, otherwise a fresh one only this device will hold.
pub(crate) fn own_wrap_owner(author: &PublicKey, address: &PublicKey, rumor_id: &EventId) -> Keys {
    derived_owner(author, address, rumor_id).unwrap_or_else(Keys::generate)
}

fn derived_owner(author: &PublicKey, address: &PublicKey, rumor_id: &EventId) -> Option<Keys> {
    crate::state::MY_SECRET_KEY
        .with_keys(|keys| {
            let secret: &[u8; 32] = keys.secret_key().as_secret_bytes().try_into().ok()?;
            (keys.public_key() == *author).then(|| derive::wrap_owner_key(secret, address, rumor_id.as_bytes()))
        })
        .flatten()
}

/// Keep what deletes my own `wrap`, met on a relay and sent from this device or another: its
/// owner when it is the key this identity derives, and `plane`, the key that signed it, when
/// the caller holds it. Fills only a confirmed gap. Returns whether its owner is newly kept.
pub(crate) fn heal_own_wrap(event: &ChatEvent, wrap: &Event, plane: Option<&Keys>, relays: &[String]) -> bool {
    if !matches!(event, ChatEvent::Message { .. } | ChatEvent::Reaction { .. } | ChatEvent::Edit { .. }) {
        return false;
    }
    let opened = event.opened();
    if crate::state::my_public_key() != Some(opened.author) {
        return false;
    }
    let (rumor_hex, wrap_hex) = (opened.rumor_id.to_hex(), wrap.id.to_hex());
    if let Some(plane) = plane {
        if matches!(crate::db::community::has_message_key(&rumor_hex), Ok(false)) {
            let _ = crate::db::community::store_message_key(&rumor_hex, &wrap_hex, plane, relays);
        }
    }
    if !matches!(crate::db::community::has_wrap_owner(&rumor_hex), Ok(false)) {
        return false;
    }
    let Some(owner) = derived_owner(&opened.author, &wrap.pubkey, &opened.rumor_id) else {
        return false;
    };
    stream::wrap_owner(wrap) == Some(owner.public_key())
        && crate::db::community::store_wrap_owner(&rumor_hex, &wrap_hex, &owner, relays).is_ok()
}

/// What deleting one of my own events from relays reached.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Scrubbed {
    /// Its owner's deletion went out, which every relay following CORD-01 honours.
    pub by_owner: bool,
    /// Its plane key's deletion was sent: a relay judging by author honours it, and one following
    /// CORD-01 refuses it, which is why its answer counts for nothing.
    pub by_plane: bool,
}

impl Scrubbed {
    /// Whether every relay that honours a deletion at all was asked in the way it honours.
    pub fn everywhere(&self) -> bool {
        self.by_owner && self.by_plane
    }
}

/// Delete my own `message_id` from relays, with its stored edits (they carry its text), by
/// every key kept for their wraps: one deletion per wrap, so none ties wraps together for a
/// relay. `relays` is where its Community lives now: a relay move copies wraps over. An owner
/// key is dropped once its deletion went out, a plane key once its deletion was sent.
pub async fn scrub_own<T: Transport + ?Sized>(transport: &T, relays: &[String], message_id: &str) -> Result<Scrubbed, String> {
    crate::db::scoped(async move {
        if !crate::db::current_session().is_live() {
            return Err("account changed; aborting delete".to_string());
        }
        let mut targets = vec![message_id.to_string()];
        targets.extend(crate::db::events::edits_of(message_id).unwrap_or_default());

        let mut scrubbed = Scrubbed::default();
        for target in &targets {
            let owners = crate::db::community::wrap_owners(target)?;
            let by_owner = owners.iter().map(|k| deletion_by(k, relays)).collect::<Result<Vec<_>, _>>()?;
            let by_plane = crate::db::community::get_message_key(target)?.map(|k| deletion_by(&k, relays)).transpose()?;
            // The owner's deletion must land. The plane key's is a single best-effort send: a relay
            // following CORD-01 refuses it, and a durable publish would wait out that refusal.
            let (owner_sent, plane_sent) = futures_util::future::join(
                futures_util::future::join_all(by_owner.iter().map(|(d, to)| transport.publish_durable(d, to))),
                async {
                    match &by_plane {
                        Some((d, to)) => Some(transport.publish(d, to).await),
                        None => None,
                    }
                },
            )
            .await;
            if let Some(e) = owner_sent.iter().find_map(|r| r.as_ref().err()) {
                crate::log_warn!("[Community] an owner's deletion of {target} failed: {e}");
            }
            if !owners.is_empty() && owner_sent.iter().all(Result::is_ok) {
                crate::db::community::forget_wrap_owners(target)?;
                scrubbed.by_owner |= target == message_id;
            }
            if let Some(sent) = plane_sent {
                if let Err(e) = sent {
                    crate::log_debug!("[Community] the plane key's deletion of {target} was not taken: {e}");
                }
                crate::db::community::delete_message_key(target)?;
                scrubbed.by_plane |= target == message_id;
            }
        }
        Ok(scrubbed)
    })
    .await
}

/// When the in-plane delete of `message_id` may leave relays, given what its scrub reached:
/// once the wrap was deleted [`Scrubbed::everywhere`], 30 days on, or with the message when
/// that comes sooner. Otherwise never, since it alone hides a wrap a relay still holds.
pub async fn in_plane_delete_expiry(message_id: &str, scrubbed: Scrubbed) -> Option<u64> {
    if !scrubbed.everywhere() {
        return None;
    }
    let until = Timestamp::now().as_secs().saturating_add(OWNED_DELETE_EXPIRY_SECS);
    let message = crate::self_destruct::edit_expiry(message_id).await.ok().flatten();
    Some(message.map_or(until, |m| m.min(until)))
}

/// The deletion of one wrap by the key kept for it, and the relays it goes to.
fn deletion_by((keys, outer, sent_to): &(Keys, String, Vec<String>), relays: &[String]) -> Result<(Event, Vec<String>), String> {
    let wrap = EventId::from_hex(outer).map_err(|e| e.to_string())?;
    let deletion = crate::deletion::wrap_deletion(&[wrap]).finalize(keys).map_err(|e| e.to_string())?;
    Ok((deletion, union(relays, sent_to)))
}

fn union(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = a.to_vec();
    for r in b {
        if !out.contains(r) {
            out.push(r.clone());
        }
    }
    out
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

    /// A wrap as a plane publishes it: signed by the shared `plane` key, `p`-tagged to `owner`.
    async fn published(relay: &MemoryRelay, plane: &Keys, owner: &Keys) -> Event {
        let wrap = EventBuilder::new(Kind::Custom(1059), "sealed").tag(Tag::public_key(owner.public_key())).finalize(plane).unwrap();
        relay.publish_durable(&wrap, &relays()).await.unwrap();
        wrap
    }

    async fn still_there(relay: &MemoryRelay, id: EventId) -> bool {
        let q = Query { ids: vec![id.to_hex()], ..Default::default() };
        !relay.fetch(&q, &relays()).await.unwrap().is_empty()
    }

    #[tokio::test]
    async fn a_relay_following_cord01_takes_a_wraps_deletion_only_from_its_owner() {
        let relay = MemoryRelay::new();
        relay.follow_cord01(&relays());
        let (plane, owner) = (Keys::generate(), Keys::generate());
        let wrap = published(&relay, &plane, &owner).await;

        let by_plane = crate::deletion::wrap_deletion(&[wrap.id]).finalize(&plane).unwrap();
        relay.publish_durable(&by_plane, &relays()).await.unwrap();
        assert!(still_there(&relay, wrap.id).await, "any member holds the plane key, so it deletes nothing");

        let by_owner = crate::deletion::wrap_deletion(&[wrap.id]).finalize(&owner).unwrap();
        relay.publish_durable(&by_owner, &["r1".to_string()]).await.unwrap();
        assert!(still_there(&relay, wrap.id).await, "a deletion on a subset leaves it on the rest");
        relay.publish_durable(&by_owner, &relays()).await.unwrap();
        assert!(!still_there(&relay, wrap.id).await);
    }

    #[tokio::test]
    async fn a_scrub_deletes_by_owner_and_plane_and_spends_both_keys() {
        let (_tmp, _guard) = init_test_db();
        let (cord01, stock) = (MemoryRelay::new(), MemoryRelay::new());
        cord01.follow_cord01(&relays());
        let (plane, owner) = (Keys::generate(), Keys::generate());
        let message_id = "00".repeat(32);
        for relay in [&cord01, &stock] {
            let wrap = published(relay, &plane, &owner).await;
            crate::db::community::store_message_key(&message_id, &wrap.id.to_hex(), &plane, &relays()).unwrap();
            crate::db::community::store_wrap_owner(&message_id, &wrap.id.to_hex(), &owner, &relays()).unwrap();

            assert_eq!(scrub_own(relay, &relays(), &message_id).await.unwrap(), Scrubbed { by_owner: true, by_plane: true });
            assert!(!still_there(relay, wrap.id).await, "the owner deletes it where CORD-01 holds, the plane key elsewhere");
            assert!(!crate::db::community::has_wrap_owner(&message_id).unwrap());
            assert!(!crate::db::community::has_message_key(&message_id).unwrap());
        }
    }

    /// Records which publish each event went through, accepting everything.
    #[derive(Default)]
    struct Recording {
        durable: std::sync::Mutex<Vec<PublicKey>>,
        once: std::sync::Mutex<Vec<PublicKey>>,
    }
    #[async_trait::async_trait]
    impl Transport for Recording {
        async fn fetch_plane(&self, _plane: &Keys, _query: &Query, _relays: &[String]) -> Result<PlaneFetch, String> {
            Ok(PlaneFetch::whole(Vec::new()))
        }
        async fn publish(&self, event: &Event, _relays: &[String]) -> Result<(), String> {
            self.once.lock().unwrap().push(event.pubkey);
            Ok(())
        }
        async fn publish_durable(&self, event: &Event, _relays: &[String]) -> Result<(), String> {
            self.durable.lock().unwrap().push(event.pubkey);
            Ok(())
        }
        async fn fetch(&self, _query: &Query, _relays: &[String]) -> Result<Vec<Event>, String> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn only_the_owners_deletion_waits_to_land() {
        let (_tmp, _guard) = init_test_db();
        let (plane, owner) = (Keys::generate(), Keys::generate());
        let message_id = "66".repeat(32);
        crate::db::community::store_message_key(&message_id, &"77".repeat(32), &plane, &relays()).unwrap();
        crate::db::community::store_wrap_owner(&message_id, &"77".repeat(32), &owner, &relays()).unwrap();
        let relay = Recording::default();
        scrub_own(&relay, &relays(), &message_id).await.unwrap();
        assert_eq!(*relay.durable.lock().unwrap(), vec![owner.public_key()]);
        assert_eq!(*relay.once.lock().unwrap(), vec![plane.public_key()], "a CORD-01 relay refuses it, so nothing waits on it");
    }

    #[tokio::test]
    async fn an_owner_deletion_no_relay_took_keeps_its_key_and_expires_nothing() {
        let (_tmp, _guard) = init_test_db();
        let message_id = "44".repeat(32);
        crate::db::community::store_message_key(&message_id, &"55".repeat(32), &Keys::generate(), &relays()).unwrap();
        crate::db::community::store_wrap_owner(&message_id, &"55".repeat(32), &Keys::generate(), &relays()).unwrap();

        let scrubbed = scrub_own(&FailingRelay, &relays(), &message_id).await.unwrap();
        assert_eq!(scrubbed, Scrubbed { by_owner: false, by_plane: true }, "a refused plane deletion still counts as sent");
        assert!(crate::db::community::has_wrap_owner(&message_id).unwrap(), "the owner key stays for another try");
        assert!(!crate::db::community::has_message_key(&message_id).unwrap());
        assert_eq!(in_plane_delete_expiry(&message_id, scrubbed).await, None);
    }

    #[tokio::test]
    async fn a_message_sent_before_owners_were_kept_stays_on_a_cord01_relay() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        relay.follow_cord01(&relays());
        let plane = Keys::generate();
        let wrap = published(&relay, &plane, &Keys::generate()).await;
        let message_id = "11".repeat(32);
        crate::db::community::store_message_key(&message_id, &wrap.id.to_hex(), &plane, &relays()).unwrap();

        let scrubbed = scrub_own(&relay, &relays(), &message_id).await.unwrap();
        assert_eq!(scrubbed, Scrubbed { by_owner: false, by_plane: true });
        assert!(still_there(&relay, wrap.id).await);
        assert_eq!(in_plane_delete_expiry(&message_id, scrubbed).await, None, "its in-plane delete never leaves");
        let owner_only = Scrubbed { by_owner: true, by_plane: false };
        assert_eq!(in_plane_delete_expiry(&message_id, owner_only).await, None, "nor while a relay judging by author holds it");
    }

    #[tokio::test]
    async fn a_scrub_reaches_the_communitys_relays_beyond_those_it_was_sent_to() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let moved = vec!["r9".to_string()];
        relay.follow_cord01(&relays());
        relay.follow_cord01(&moved);
        let (plane, owner) = (Keys::generate(), Keys::generate());
        let wrap = published(&relay, &plane, &owner).await;
        relay.inject(&wrap, &moved);
        let message_id = "22".repeat(32);
        crate::db::community::store_wrap_owner(&message_id, &wrap.id.to_hex(), &owner, &relays()).unwrap();

        scrub_own(&relay, &moved, &message_id).await.unwrap();
        let q = Query { ids: vec![wrap.id.to_hex()], ..Default::default() };
        assert!(relay.fetch(&q, &moved).await.unwrap().is_empty(), "the copy a relay move made goes too");
        assert!(!still_there(&relay, wrap.id).await);
    }

    #[tokio::test]
    async fn an_owned_delete_leaves_relays_after_30_days() {
        let (_tmp, _guard) = init_test_db();
        let now = Timestamp::now().as_secs();
        let until = in_plane_delete_expiry(&"33".repeat(32), Scrubbed { by_owner: true, by_plane: true }).await.unwrap();
        assert!((now + OWNED_DELETE_EXPIRY_SECS..=now + OWNED_DELETE_EXPIRY_SECS + 5).contains(&until));
    }

    #[tokio::test]
    async fn scrubbing_a_message_we_hold_no_keys_for_reaches_nothing() {
        let (_tmp, _guard) = init_test_db();
        assert_eq!(scrub_own(&MemoryRelay::new(), &relays(), &"00".repeat(32)).await.unwrap(), Scrubbed::default());
    }
}
