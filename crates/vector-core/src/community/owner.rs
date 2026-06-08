//! Owner attestation — the unforgeable binding of a Community to its owner's identity (
//! "anchored to the owner's identity key").
//!
//! At creation the owner signs, with their IDENTITY key, a statement binding the Community's random
//! `community_id`. The proven owner is then DERIVED from the signature (`event.pubkey`), never
//! asserted separately — so:
//!   - you cannot frame an innocent npub (that requires their key to sign), and
//!   - the binding can't be transplanted to another community (the unique id is inside the signed
//!     payload, so an attestation for community X can't be replayed as community Y's).
//! Members verify it against the very community id they already hold. Server-root encrypted in
//! transit, so outsiders learn nothing; only members see who the owner is. (The community is
//! keyless — there is no management/authority key to bind; `community_id` alone is the anchor.)

use crate::stored_event::event_kind;
use nostr_sdk::prelude::*;

/// Tag binding the attestation to its community. `["vco", community_id]`.
const TAG_OWNER: &str = "vco";

/// The unsigned owner-attestation event. Sign it with the owner's IDENTITY signer (local or
/// bunker — it's a normal event, so NIP-46 works) at community creation.
pub fn build_owner_attestation_unsigned(
    owner_pubkey: PublicKey,
    community_id: &str,
) -> UnsignedEvent {
    EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), "")
        .tags([Tag::custom(
            TagKind::Custom(TAG_OWNER.into()),
            [community_id.to_string()],
        )])
        .build(owner_pubkey)
}

/// Verify an owner-attestation event (JSON). Returns the PROVEN owner pubkey iff the signature
/// is valid AND it binds exactly this `community_id`. `None` on any missing/mismatched/forged
/// input — the caller then treats ownership as unverified (no crown).
pub fn verify_owner_attestation(
    attestation_json: &str,
    community_id: &str,
) -> Option<PublicKey> {
    let ev: Event = serde_json::from_str(attestation_json).ok()?;
    ev.verify().ok()?; // id + Schnorr signature
    let bound = ev.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == TAG_OWNER).then(|| s[1].clone())
    })?;
    (bound == community_id).then_some(ev.pubkey)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attestation_round_trips_binds_and_rejects_forgery() {
        let owner = Keys::generate();
        let cid = "a".repeat(64);

        let signed = build_owner_attestation_unsigned(owner.public_key(), &cid)
            .sign_with_keys(&owner)
            .unwrap();
        let json = signed.as_json();

        // Valid → the proven owner is the signer.
        assert_eq!(verify_owner_attestation(&json, &cid), Some(owner.public_key()));
        // Can't transplant to another community.
        assert_eq!(verify_owner_attestation(&json, &"b".repeat(64)), None);
        // Garbage in → None, never a panic.
        assert_eq!(verify_owner_attestation("not json", &cid), None);

        // Framing defense: a forger can only ever attest THEMSELVES — verify returns the
        // forger's pubkey, never the victim's, so they can't make the UI crown someone else.
        let mallory = Keys::generate();
        let forged = build_owner_attestation_unsigned(mallory.public_key(), &cid)
            .sign_with_keys(&mallory)
            .unwrap()
            .as_json();
        assert_eq!(verify_owner_attestation(&forged, &cid), Some(mallory.public_key()));
        assert_ne!(verify_owner_attestation(&forged, &cid), Some(owner.public_key()));
    }

    #[test]
    fn attestation_tag_layout_is_frozen() {
        // the attestation's signed form is FROZEN. It is verify-only (other clients check the
        // owner's signature, never reconstruct the event — created_at is non-deterministic), so the
        // pinned interop contract is the TAG layout: exactly one `["vco", <community_id>]` tag, empty
        // content. A drift here (extra element, reordered, renamed) breaks every verifier reading idx 1.
        let owner = Keys::generate();
        let cid = "c".repeat(64);
        let unsigned = build_owner_attestation_unsigned(owner.public_key(), &cid);
        assert_eq!(unsigned.content, "");
        let tags: Vec<Vec<String>> = unsigned.tags.iter().map(|t| t.as_slice().to_vec()).collect();
        assert_eq!(tags, vec![vec!["vco".to_string(), cid]], "exactly one [\"vco\", community_id] tag");
    }
}
