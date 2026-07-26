//! Event-builder helpers.

use nostr_sdk::prelude::*;

/// Finalize into an `UnsignedEvent` with the event id already computed.
///
/// nostr 0.44's `EventBuilder::build` populated `id`; 0.45's `finalize_unsigned`
/// deliberately leaves it `None` so a caller can mine NIP-13 PoW before the id is
/// fixed. Vector never mines rumors and reads `.id` throughout the send pipeline
/// (pending-message keys, retry payloads, edit ids), where a `None` is a
/// runtime failure the compiler can't see. This restores the old semantics at
/// every call site that wants an addressable rumor.
pub trait FinalizeUnsignedWithId {
    /// Finalize and compute the id.
    fn finalize_unsigned_with_id(self, public_key: PublicKey) -> UnsignedEvent;
}

impl FinalizeUnsignedWithId for EventBuilder {
    #[inline]
    fn finalize_unsigned_with_id(self, public_key: PublicKey) -> UnsignedEvent {
        let mut unsigned = self.finalize_unsigned(public_key);
        unsigned.ensure_id();
        unsigned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_populated_and_matches_the_signed_event() {
        let keys = Keys::generate();
        let unsigned = EventBuilder::text_note("hi").finalize_unsigned_with_id(keys.public_key());
        let id = unsigned.id.expect("id computed eagerly");
        let signed = unsigned.finalize(&keys).expect("sign");
        assert_eq!(id, signed.id, "eager id must equal the signed event id");
    }

    #[test]
    fn plain_finalize_unsigned_still_leaves_it_none() {
        // Guards the reason this trait exists: if upstream ever populates the id
        // again, this fails and the wrapper can go.
        let keys = Keys::generate();
        let unsigned = EventBuilder::text_note("hi").finalize_unsigned(keys.public_key());
        assert!(unsigned.id.is_none());
    }
}
