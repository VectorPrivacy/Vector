//! Event signatures checked many at a time.
//!
//! One BIP-340 batch equation (nostr-verify) accepts a whole slice of signatures for much
//! less than one libsecp256k1 verification each: 1.7x at 16 signatures, 2.2x at 64 on an
//! M4. Parsed keys are cached per thread, so a sender seen before skips its square root.
//! A slice that fails is narrowed down and its signatures checked one at a time by
//! libsecp256k1, so every `false` is the individual verifier's verdict.
//!
//! Only signatures: callers check the event id first, as `Event::verify` does.

use nostr_sdk::prelude::Event;
use std::cell::RefCell;

thread_local! {
    static VERIFIER: RefCell<nostr_verify::Verifier> = RefCell::new(nostr_verify::Verifier::new());
}

/// Whether each event's signature is valid for its id and author, in order.
pub fn signatures(events: &[&Event]) -> Vec<bool> {
    // Too few to batch (a live DM): rust-nostr's own check, exactly as before batching.
    if events.len() < nostr_verify::MIN_BATCH {
        return events.iter().map(|e| e.verify_signature()).collect();
    }
    let refs: Vec<nostr_verify::SigRef<'_>> = events
        .iter()
        .map(|e| nostr_verify::SigRef { pubkey: e.pubkey.as_bytes(), msg: e.id.as_bytes(), sig: e.sig.as_bytes() })
        .collect();
    let mut ok = Vec::with_capacity(refs.len());
    VERIFIER.with_borrow_mut(|v| v.verify_batch(&refs, &mut ok));
    ok
}

/// This thread's counters: batches and how many held, key-cache hits and misses.
pub fn stats() -> nostr_verify::Stats {
    VERIFIER.with_borrow(|v| v.stats())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::*;

    fn note(keys: &Keys, i: usize) -> Event {
        EventBuilder::new(Kind::TextNote, format!("note {i}")).finalize(keys).unwrap()
    }

    #[test]
    fn verdicts_match_each_event_verified_alone() {
        let keys: Vec<Keys> = (0..3).map(|_| Keys::generate()).collect();
        let mut events: Vec<Event> = (0..40).map(|i| note(&keys[i % 3], i)).collect();
        // Signatures moved onto other events: valid signatures, wrong messages.
        for i in [5, 17, 33] {
            events[i].sig = events[i + 1].sig;
        }
        let refs: Vec<&Event> = events.iter().collect();
        let ours = signatures(&refs);
        let theirs: Vec<bool> = events.iter().map(Event::verify_signature).collect();
        assert_eq!(ours, theirs);
        assert_eq!(ours.iter().filter(|ok| !**ok).count(), 3);
        assert!(signatures(&[]).is_empty());
    }
}
