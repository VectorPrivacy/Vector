//! One NIP-77 exchange with one relay: which of a filter's events each side lacks, without either
//! sending the events.
//!
//! Driven here, not through nostr-sdk's sync, so an exchange can stop at a count of named ids and
//! keep what it has named, and so a refusal is told apart from a gate and from silence.

use std::borrow::Cow;
use std::time::Duration;

use negentropy::{Id, Negentropy, NegentropyStorageBase, NegentropyStorageVector};
use nostr_sdk::prelude::*;

use super::transport::{Diff, ReconcileFail};

/// Half a relay's usual frame limit (the message goes out hex-encoded), less the frame around it.
const FRAME_LIMIT: u64 = 60_000;

/// How the relay ended an exchange it did not finish.
pub(crate) enum Ended {
    Fail(ReconcileFail),
    /// Nothing came back at all: no NIP-77 here, or no relay.
    Silent,
}

fn failed(why: impl std::fmt::Display) -> Ended {
    Ended::Fail(ReconcileFail::Failed(why.to_string()))
}

/// What a relay's notice says of a NEG-OPEN it would not take, as relays without NIP-77 word it.
fn refuses(notice: &str) -> bool {
    let notice = notice.to_ascii_lowercase();
    // A limit on how much is asked at once is no answer about NIP-77.
    if ["too many", "rate", "limit", "slow down", "busy"].iter().any(|limit| notice.contains(limit)) {
        return false;
    }
    (notice.contains("bad msg") || notice.contains("unknown") || notice.contains("unrecognized") || notice.contains("unsupported") || notice.contains("invalid"))
        && (notice.contains("neg") || notice.contains("unknown cmd") || notice.contains("unknown message") || notice.contains("unrecognized message") || notice.contains("unrecognised"))
}

/// The exchange over `held`, and the message that opens it: one fingerprint of the whole set, so
/// a set the relay holds exactly costs a few dozen bytes each way. A relay holding anything else
/// answers with the split a standard opening starts from, one round later.
pub(crate) fn open(held: &[(EventId, u64)]) -> Result<(Negentropy<'static, NegentropyStorageVector>, Vec<u8>), negentropy::Error> {
    let mut storage = NegentropyStorageVector::with_capacity(held.len());
    for (id, at) in held {
        storage.insert(*at, Id::from_byte_array(id.to_bytes()))?;
    }
    storage.seal()?;
    let size = storage.size()?;
    if size == 0 {
        // Nothing to fingerprint: the standard opening is as short, and asks for all of it at once.
        let mut negentropy = Negentropy::owned(storage, FRAME_LIMIT)?;
        let opening = negentropy.initiate()?;
        return Ok((negentropy, opening));
    }
    let whole = storage.fingerprint(0, size)?.to_bytes();
    let mut negentropy = Negentropy::owned(storage, FRAME_LIMIT)?;
    negentropy.set_initiator();
    // The version, a range reaching to the end (no timestamp, no id prefix), fingerprint mode.
    let mut opening = vec![negentropy::PROTOCOL_VERSION as u8, 0, 0, 1];
    opening.extend_from_slice(&whole);
    Ok((negentropy, opening))
}

/// Reconcile `held` against what `relay` holds of `filter`, naming at most `budget` ids we lack.
/// `first` bounds the wait for the relay's first answer, `idle` each wait after it.
pub(crate) async fn reconcile(relay: &Relay, filter: Filter, held: &[(EventId, u64)], budget: usize, first: Duration, idle: Duration) -> Result<Diff, Ended> {
    let (mut negentropy, opening) = open(held).map_err(failed)?;

    // Listening before the open, so its answer can't slip past.
    let mut notifications = relay.notifications();
    let sub_id = SubscriptionId::generate();
    // The relay holds the exchange open until told: close it on every way out.
    struct CloseGuard(Relay, SubscriptionId);
    impl Drop for CloseGuard {
        fn drop(&mut self) {
            let (relay, id) = (self.0.clone(), self.1.clone());
            // spawn-detached: sends a relay NEG-CLOSE, touches no account state.
            crate::rt::spawn(async move {
                let _ = relay.send_msg(ClientMessage::NegClose { subscription_id: Cow::Owned(id) }).await;
            });
        }
    }
    let _close = CloseGuard(relay.clone(), sub_id.clone());
    relay
        .send_msg(ClientMessage::NegOpen {
            subscription_id: Cow::Borrowed(&sub_id),
            filter: Cow::Owned(filter),
            initial_message: Cow::Owned(crate::simd::hex::bytes_to_hex_string(&opening)),
        })
        .await
        .map_err(|_| Ended::Fail(ReconcileFail::Unreached))?;

    let mut diff = Diff::default();
    let mut answered = false;
    loop {
        let deadline = crate::rt::time::Instant::now() + if answered { idle } else { first };
        let notification = match crate::rt::time::timeout_at(deadline, notifications.next()).await {
            Ok(Some(n)) => n,
            Ok(None) => return Err(Ended::Fail(ReconcileFail::Unreached)),
            Err(_) if answered => return Err(failed("the relay stopped answering")),
            Err(_) => return Err(Ended::Silent),
        };
        let message = match notification {
            RelayNotification::Message { message } => *message,
            RelayNotification::RelayStatus { status: RelayStatus::Shutdown | RelayStatus::Terminated | RelayStatus::Banned } => {
                return Err(Ended::Fail(ReconcileFail::Unreached));
            }
            _ => continue,
        };
        match message {
            RelayMessage::NegMsg { subscription_id, message } if *subscription_id == sub_id => {
                answered = true;
                let Some(bytes) = crate::simd::hex::hex_string_to_bytes_checked(&message) else {
                    return Err(failed("a reconciliation message that is not hex"));
                };
                let (mut have, mut need) = (Vec::new(), Vec::new());
                let next = negentropy.reconcile_with_ids(&bytes, &mut have, &mut need).map_err(failed)?;
                diff.lack.extend(have.into_iter().map(|id| EventId::from_byte_array(id.to_bytes())));
                diff.need.extend(need.into_iter().map(|id| EventId::from_byte_array(id.to_bytes())));
                match next {
                    None => {
                        diff.complete = diff.need.len() <= budget;
                        diff.need.truncate(budget);
                        return Ok(diff);
                    }
                    // Enough named for one pass: what is named already is still worth fetching.
                    Some(_) if diff.need.len() >= budget => {
                        diff.need.truncate(budget);
                        return Ok(diff);
                    }
                    Some(next) => {
                        relay
                            .send_msg(ClientMessage::NegMsg { subscription_id: Cow::Borrowed(&sub_id), message: Cow::Owned(crate::simd::hex::bytes_to_hex_string(&next)) })
                            .await
                            .map_err(|_| Ended::Fail(ReconcileFail::Unreached))?;
                    }
                }
            }
            RelayMessage::NegErr { subscription_id, message } if *subscription_id == sub_id => {
                return Err(Ended::Fail(match MachineReadablePrefix::parse(&message) {
                    Some(MachineReadablePrefix::AuthRequired | MachineReadablePrefix::Restricted) => ReconcileFail::Gated,
                    // "blocked:" refuses this exchange, not NIP-77: too many sessions open at once
                    // (asked again later), or a set too big to reconcile (read by pages now).
                    Some(MachineReadablePrefix::Blocked) if message.contains("too many") => ReconcileFail::Failed(message.into_owned()),
                    Some(MachineReadablePrefix::Blocked) => ReconcileFail::Gated,
                    _ => ReconcileFail::Failed(message.into_owned()),
                }));
            }
            RelayMessage::Closed { subscription_id, message } if *subscription_id == sub_id => {
                return Err(Ended::Fail(match MachineReadablePrefix::parse(&message) {
                    Some(MachineReadablePrefix::AuthRequired | MachineReadablePrefix::Restricted) => ReconcileFail::Gated,
                    _ => ReconcileFail::Failed(message.into_owned()),
                }));
            }
            RelayMessage::Notice(notice) if !answered && refuses(&notice) => {
                return Err(Ended::Fail(ReconcileFail::Unsupported));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wraps(ids: impl IntoIterator<Item = u16>) -> Vec<(EventId, u64)> {
        ids.into_iter()
            .map(|i| {
                // Hashed, as event ids are: a fingerprint is a sum, and patterned ids cancel out.
                let id: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(i.to_be_bytes()).into();
                (EventId::from_byte_array(id), 1_000 + (i as u64 % 40))
            })
            .collect()
    }

    /// Both ends run here. Returns what each side lacks, the rounds taken, and the bytes the
    /// opening and the relay's answers came to.
    fn exchange(held: &[(EventId, u64)], theirs: &[(EventId, u64)]) -> (Vec<EventId>, Vec<EventId>, usize, (usize, usize)) {
        let (mut ours, opening) = open(held).unwrap();
        let mut storage = NegentropyStorageVector::new();
        for (id, at) in theirs {
            storage.insert(*at, Id::from_byte_array(id.to_bytes())).unwrap();
        }
        storage.seal().unwrap();
        let mut relay = Negentropy::owned(storage, FRAME_LIMIT).unwrap();
        let (mut need, mut lack, mut rounds, size, mut answered) = (Vec::new(), Vec::new(), 0, opening.len(), 0);
        let mut message = Some(opening);
        while let Some(out) = message.take() {
            rounds += 1;
            let answer = relay.reconcile(&out).unwrap();
            answered += answer.len();
            let (mut have, mut want) = (Vec::new(), Vec::new());
            message = ours.reconcile_with_ids(&answer, &mut have, &mut want).unwrap();
            lack.extend(have.iter().map(|id| EventId::from_byte_array(id.to_bytes())));
            need.extend(want.iter().map(|id| EventId::from_byte_array(id.to_bytes())));
        }
        need.sort();
        lack.sort();
        (need, lack, rounds, (size, answered))
    }

    fn ids(wraps: &[(EventId, u64)]) -> Vec<EventId> {
        let mut ids: Vec<EventId> = wraps.iter().map(|(id, _)| *id).collect();
        ids.sort();
        ids
    }

    #[test]
    fn a_set_the_relay_holds_exactly_is_settled_by_one_fingerprint() {
        for n in [1u16, 20, 400] {
            let held = wraps(0..n);
            assert_eq!(exchange(&held, &held), (vec![], vec![], 1, (20, 1)), "{n} wraps: twenty bytes out, one back");
        }
    }

    #[test]
    fn a_set_that_differs_names_exactly_what_each_side_lacks() {
        for n in [5u16, 31, 32, 33, 400, 3_000] {
            let held = wraps((0..n).filter(|i| i % 7 != 3));
            let theirs = wraps((0..n).filter(|i| i % 11 != 5));
            let (need, lack, ..) = exchange(&held, &theirs);
            assert_eq!(need, ids(&wraps((0..n).filter(|i| i % 7 == 3 && i % 11 != 5))), "{n} wraps");
            assert_eq!(lack, ids(&wraps((0..n).filter(|i| i % 11 == 5 && i % 7 != 3))), "{n} wraps");
        }
        // One side empty, either way round.
        let all = wraps(0..50);
        assert_eq!(exchange(&[], &all).0, ids(&all));
        assert_eq!(exchange(&all, &[]).1, ids(&all));
        assert_eq!(exchange(&[], &[]).3 .0, 5);
    }

    #[test]
    fn a_relay_without_nip77_is_known_by_its_notice() {
        for notice in [
            "ERROR: bad msg: unknown cmd",
            "ERROR: bad msg: negentropy disabled",
            "invalid: unknown message type NEG-OPEN",
            "unrecognized message: NEG-OPEN",
            "unsupported: NEG-OPEN",
        ] {
            assert!(refuses(notice), "{notice}");
        }
        for notice in ["rate-limited: slow down", "ERROR: bad msg: invalid filter", "restricted: not on the allow list", "ERROR: too many open negentropy sessions", "invalid: negentropy rate limit"] {
            assert!(!refuses(notice), "{notice}");
        }
    }
}
