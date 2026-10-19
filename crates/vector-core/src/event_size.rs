//! The relay size ceiling: every event Vector publishes, its outer wrap included, serializes
//! to at most 65,536 bytes (CORD-02 Appendix B), since a relay refuses a larger one whole.
//!
//! NIP-44 pads stepwise (powers of two to 256, then eighths of the next power of two), so
//! the encoded size jumps: the thresholds below are exact, measured, never extrapolated.

use nostr_sdk::prelude::{Event, UnsignedEvent};

/// The common relay cap on one serialized event.
pub const MAX_EVENT_BYTES: usize = 65_536;

/// The largest rumor whose seal and wrap still fit: at 28,672 bytes the wrap is 55,121,
/// one byte more pads both layers a step up and the wrap lands at 66,045.
pub const MAX_SEALED_RUMOR_BYTES: usize = 28_672;

/// The largest plaintext one NIP-44 layer carries in a fitting event (~55,060 encoded).
pub const MAX_LAYER_PLAINTEXT_BYTES: usize = 40_960;

/// The refusal a user sees: what they sent is too big for any relay to take.
pub fn too_large(bytes: usize) -> String {
    format!("Too large to send: {} once encrypted, relays take at most 64 KB", crate::crypto::format_bytes(bytes as u64))
}

/// Refuse an event no relay would store.
pub fn check(event: &Event) -> Result<(), String> {
    let n = event.as_json().len();
    if n > MAX_EVENT_BYTES { Err(too_large(n)) } else { Ok(()) }
}

/// Refuse a rumor whose sealed and wrapped form would not fit, before any key is used.
pub fn check_rumor(rumor: &UnsignedEvent) -> Result<(), String> {
    let n = rumor.as_json().len();
    if n > MAX_SEALED_RUMOR_BYTES { Err(too_large(projected_wrap(n))) } else { Ok(()) }
}

/// Whether one NIP-44 layer over `plaintext` bytes stays in a fitting event.
pub fn layer_fits(plaintext: usize) -> bool {
    plaintext <= MAX_LAYER_PLAINTEXT_BYTES
}

/// NIP-44 v2's padded length for a plaintext of `len` bytes.
pub fn nip44_padded(len: usize) -> usize {
    if len <= 32 {
        return 32;
    }
    let next_power = 1usize << (usize::BITS - (len - 1).leading_zeros());
    let chunk = if next_power <= 256 { 32 } else { next_power / 8 };
    chunk * ((len - 1) / chunk + 1)
}

/// The base64 length of one NIP-44 v2 payload over `len` plaintext bytes.
pub fn nip44_encoded(len: usize) -> usize {
    // version[1] + nonce[32] + length prefix[2] + padded + mac[32]
    (1 + 32 + 2 + nip44_padded(len) + 32).div_ceil(3) * 4
}

/// A sealed-and-wrapped rumor's approximate wrap size, for the refusal's wording.
fn projected_wrap(rumor_len: usize) -> usize {
    // A seal and a wrap each add ~400 bytes of envelope around their NIP-44 content.
    nip44_encoded(nip44_encoded(rumor_len) + 400) + 400
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_ext::FinalizeUnsignedWithId;
    use nostr_sdk::prelude::*;

    fn rumor_of(len: usize, author: &Keys) -> UnsignedEvent {
        let at = Timestamp::from_secs(1_800_000_000);
        let build = |content: String| EventBuilder::new(Kind::PrivateDirectMessage, content).custom_created_at(at).finalize_unsigned_with_id(author.public_key());
        let base = build(String::new()).as_json().len();
        let r = build("x".repeat(len - base));
        assert_eq!(r.as_json().len(), len);
        r
    }

    #[test]
    fn padding_matches_nip44() {
        assert_eq!(nip44_padded(1), 32);
        assert_eq!(nip44_padded(33), 64);
        assert_eq!(nip44_padded(257), 320);
        assert_eq!(nip44_padded(28_672), 28_672);
        assert_eq!(nip44_padded(28_673), 32_768);
        assert_eq!(nip44_padded(40_960), 40_960);
        assert_eq!(nip44_padded(40_961), 49_152);
    }

    #[test]
    fn the_sealed_rumor_ceiling_is_exact() {
        let me = Keys::generate();
        let them = Keys::generate();
        let fits = rumor_of(MAX_SEALED_RUMOR_BYTES, &me);
        assert!(check_rumor(&fits).is_ok());
        let wrap = GiftWrapBuilder::new(them.public_key(), fits).finalize(&me).unwrap();
        assert!(check(&wrap).is_ok(), "the ceiling's wrap is {} bytes", wrap.as_json().len());

        let over = rumor_of(MAX_SEALED_RUMOR_BYTES + 1, &me);
        assert!(check_rumor(&over).is_err());
        let wrap = GiftWrapBuilder::new(them.public_key(), over).finalize(&me).unwrap();
        assert!(check(&wrap).is_err(), "one byte past the ceiling must not fit");
    }

    #[test]
    fn the_layer_ceiling_is_exact() {
        let k = Keys::generate();
        let conv = nip44::v2::ConversationKey::derive(k.secret_key(), &k.public_key()).unwrap();
        for (len, fits) in [(MAX_LAYER_PLAINTEXT_BYTES, true), (MAX_LAYER_PLAINTEXT_BYTES + 1, false)] {
            let ct = crate::community::cipher::encrypt_with_random_nonce(&conv, "x".repeat(len).as_bytes()).unwrap();
            let content = crate::simd::base64::STANDARD.encode_to_string(&ct);
            assert_eq!(content.len(), nip44_encoded(len));
            let ev = EventBuilder::new(Kind::Custom(30078), content).tags([Tag::identifier("x")]).finalize(&k).unwrap();
            assert_eq!(check(&ev).is_ok(), fits, "{len}: {} bytes", ev.as_json().len());
            assert_eq!(layer_fits(len), fits);
        }
    }
}
